//! Single MCP server connection with lifecycle management.

use crate::mcp::types::{McpServerConfig, McpTransport};
use rmcp::model::Tool;
use rmcp::service::ServerSink;
use rmcp::transport::child_process::ConfigureCommandExt;
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Connection state for a single MCP server.
#[derive(Debug, Clone)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected {
        server_name: String,
        tool_count: usize,
    },
    Failed {
        error: String,
        attempts: usize,
    },
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Disconnected => write!(f, "disconnected"),
            ConnectionState::Connecting => write!(f, "connecting"),
            ConnectionState::Connected {
                server_name,
                tool_count,
            } => write!(f, "connected ({server_name}, {tool_count} tools)"),
            ConnectionState::Failed { error, attempts } => {
                write!(f, "failed after {attempts} attempts: {error}")
            }
        }
    }
}

/// Cached tool data from a connected MCP server.
struct ConnectedServer {
    tools: Vec<Tool>,
    sink: ServerSink,
}

/// Manages a single MCP server connection.
///
/// Handles transport setup, tool discovery, and reconnection. Each agent
/// gets its own `McpClient` per server for isolation — a shared server
/// definition results in independent connections per agent.
pub struct McpClient {
    config: McpServerConfig,
    state: RwLock<ConnectionState>,
    connected: RwLock<Option<ConnectedServer>>,
}

impl McpClient {
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            state: RwLock::new(ConnectionState::Disconnected),
            connected: RwLock::new(None),
        }
    }

    pub fn server_id(&self) -> &str {
        &self.config.id
    }

    pub async fn state(&self) -> ConnectionState {
        self.state.read().await.clone()
    }

    /// Connect to the MCP server, discover tools, and cache the results.
    pub async fn connect(&self) -> Result<(), crate::error::McpError> {
        *self.state.write().await = ConnectionState::Connecting;

        match self.config.transport {
            McpTransport::Stdio => self.connect_stdio().await,
            McpTransport::Sse => Err(crate::error::McpError::ConnectionFailed {
                server_id: self.config.id.clone(),
                reason: "SSE transport not yet implemented (Phase 2)".into(),
            }),
        }
    }

    async fn connect_stdio(&self) -> Result<(), crate::error::McpError> {
        let command = self.config.command.as_deref().ok_or_else(|| {
            crate::error::McpError::ConnectionFailed {
                server_id: self.config.id.clone(),
                reason: "stdio transport requires a command".into(),
            }
        })?;

        let env_vars = self.config.env.clone();
        let args = self.config.args.clone();
        let command_owned = command.to_string();

        let transport = TokioChildProcess::new(
            tokio::process::Command::new(&command_owned).configure(|cmd| {
                cmd.args(&args);
                for (key, value) in &env_vars {
                    cmd.env(key, value);
                }
            }),
        )
        .map_err(|error| crate::error::McpError::ConnectionFailed {
            server_id: self.config.id.clone(),
            reason: format!("failed to spawn process: {error}"),
        })?;

        let client = ().serve(transport).await.map_err(|error| {
            crate::error::McpError::ConnectionFailed {
                server_id: self.config.id.clone(),
                reason: format!("MCP handshake failed: {error}"),
            }
        })?;

        let server_name = client
            .peer_info()
            .map(|info| info.server_info.name.clone())
            .unwrap_or_else(|| self.config.id.clone());

        let tools_result = client
            .list_tools(None)
            .await
            .map_err(|error| crate::error::McpError::DiscoveryFailed {
                server_id: self.config.id.clone(),
                reason: format!("list_tools failed: {error}"),
            })?;

        let tools = tools_result.tools;
        let tool_count = tools.len();
        let sink = client.peer().clone();

        tracing::info!(
            server_id = %self.config.id,
            server_name = %server_name,
            tool_count,
            "MCP server connected"
        );

        *self.connected.write().await = Some(ConnectedServer { tools, sink });
        *self.state.write().await = ConnectionState::Connected {
            server_name,
            tool_count,
        };

        Ok(())
    }

    /// Returns cached tool definitions and the server sink for creating rig McpTool instances.
    ///
    /// Returns `None` if not connected.
    pub async fn tools(&self) -> Option<(Vec<Tool>, ServerSink)> {
        let guard = self.connected.read().await;
        guard
            .as_ref()
            .map(|server| (server.tools.clone(), server.sink.clone()))
    }

    /// Disconnect from the MCP server and clean up resources.
    pub async fn disconnect(&self) {
        *self.connected.write().await = None;
        *self.state.write().await = ConnectionState::Disconnected;
        tracing::info!(server_id = %self.config.id, "MCP server disconnected");
    }

    /// Attempt reconnection with exponential backoff.
    ///
    /// Follows the same retry pattern as `MessagingManager::spawn_retry_task`:
    /// 5s initial delay, doubling up to 60s cap, max 12 attempts.
    pub async fn connect_with_retry(self: &Arc<Self>) -> bool {
        const MAX_ATTEMPTS: usize = 12;
        const INITIAL_DELAY_SECS: u64 = 5;
        const MAX_DELAY_SECS: u64 = 60;

        let mut delay_secs = INITIAL_DELAY_SECS;

        for attempt in 1..=MAX_ATTEMPTS {
            match self.connect().await {
                Ok(()) => return true,
                Err(error) => {
                    tracing::warn!(
                        server_id = %self.config.id,
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        %error,
                        retry_in_secs = delay_secs,
                        "MCP connection failed, retrying"
                    );
                    *self.state.write().await = ConnectionState::Failed {
                        error: error.to_string(),
                        attempts: attempt,
                    };
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    delay_secs = (delay_secs * 2).min(MAX_DELAY_SECS);
                }
            }
        }

        tracing::error!(
            server_id = %self.config.id,
            "MCP connection failed after {MAX_ATTEMPTS} attempts, giving up"
        );
        false
    }
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server_id", &self.config.id)
            .finish_non_exhaustive()
    }
}
