//! Per-agent MCP client registry and tool registration.

use crate::mcp::client::{ConnectionState, McpClient};
use crate::mcp::types::{McpConfig, McpServerConfig};
use rmcp::model::Tool;
use rmcp::service::ServerSink;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Per-server connection status for API reporting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpServerStatus {
    pub server_id: String,
    pub state: String,
    pub tool_count: usize,
}

/// Per-agent registry of MCP server connections.
///
/// Each agent gets its own `McpManager` for isolation — the same server
/// definition results in independent connections per agent. Tools from all
/// connected servers are merged and prefixed with the server ID to prevent
/// name collisions with built-in tools.
pub struct McpManager {
    clients: RwLock<HashMap<String, Arc<McpClient>>>,
    server_configs: Arc<Vec<McpServerConfig>>,
}

impl McpManager {
    pub fn new(server_configs: Arc<Vec<McpServerConfig>>) -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            server_configs,
        }
    }

    /// Connect to all MCP servers listed in the agent's config.
    ///
    /// Servers that fail to connect are retried in background tasks.
    /// This method returns once all initial connection attempts complete
    /// (successfully or not) — it does not block on retries.
    pub async fn start(&self, mcp_config: &McpConfig) {
        if mcp_config.servers.is_empty() {
            return;
        }

        let configs_by_id: HashMap<&str, &McpServerConfig> = self
            .server_configs
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();

        for server_id in &mcp_config.servers {
            let Some(config) = configs_by_id.get(server_id.as_str()) else {
                tracing::warn!(
                    server_id,
                    "MCP server referenced in agent config but not defined in [[mcp_servers]]"
                );
                continue;
            };

            let client = Arc::new(McpClient::new((*config).clone()));

            match client.connect().await {
                Ok(()) => {
                    self.clients
                        .write()
                        .await
                        .insert(server_id.clone(), client);
                }
                Err(error) => {
                    tracing::warn!(
                        server_id,
                        %error,
                        "MCP server initial connection failed, starting background retry"
                    );
                    self.clients
                        .write()
                        .await
                        .insert(server_id.clone(), client.clone());

                    tokio::spawn(async move {
                        client.connect_with_retry().await;
                    });
                }
            }
        }
    }

    /// Add a server at runtime (hot-reload path).
    pub async fn add_server(&self, server_id: &str) {
        let config = self
            .server_configs
            .iter()
            .find(|c| c.id == server_id)
            .cloned();

        let Some(config) = config else {
            tracing::warn!(server_id, "cannot add unknown MCP server");
            return;
        };

        let client = Arc::new(McpClient::new(config));
        self.clients
            .write()
            .await
            .insert(server_id.to_string(), client.clone());

        tokio::spawn(async move {
            if client.connect().await.is_err() {
                client.connect_with_retry().await;
            }
        });
    }

    /// Remove a server at runtime (hot-reload path).
    pub async fn remove_server(&self, server_id: &str) {
        if let Some(client) = self.clients.write().await.remove(server_id) {
            client.disconnect().await;
        }
    }

    /// Collect all connected MCP tools as (prefixed_tool, server_sink) pairs.
    ///
    /// Tool names are prefixed with `{server_id}_` to prevent collisions
    /// with built-in tools (e.g., `github_create_issue`).
    pub async fn tools(&self) -> Vec<(Tool, ServerSink)> {
        let clients = self.clients.read().await;
        let mut result = Vec::new();

        for (server_id, client) in clients.iter() {
            if let Some((tools, sink)) = client.tools().await {
                for mut tool in tools {
                    tool.name = format!("{}_{}", server_id, tool.name).into();
                    result.push((tool, sink.clone()));
                }
            }
        }

        result
    }

    /// Per-server connection status for API reporting.
    pub async fn status(&self) -> Vec<McpServerStatus> {
        let clients = self.clients.read().await;
        let mut statuses = Vec::with_capacity(clients.len());

        for (server_id, client) in clients.iter() {
            let state = client.state().await;
            let tool_count = match &state {
                ConnectionState::Connected { tool_count, .. } => *tool_count,
                _ => 0,
            };
            statuses.push(McpServerStatus {
                server_id: server_id.clone(),
                state: state.to_string(),
                tool_count,
            });
        }

        statuses
    }

    /// Disconnect all clients.
    pub async fn shutdown(&self) {
        let clients = self.clients.write().await;
        for (_, client) in clients.iter() {
            client.disconnect().await;
        }
    }
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager").finish_non_exhaustive()
    }
}
