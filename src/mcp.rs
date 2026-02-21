//! MCP client connections and tool discovery for workers.

use crate::config::{McpServerConfig, McpTransport};

use anyhow::{Context as _, Result, anyhow};
use axum::http::{HeaderName, HeaderValue};
use rmcp::ClientHandler;
use rmcp::service::{NotificationContext, RoleClient, RunningService, ServiceError};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock};

type McpClientSession = RunningService<RoleClient, McpClientHandler>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Connecting,
    Connected,
    Failed(String),
    Disconnected,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub enabled: bool,
    pub transport: String,
    pub state: McpConnectionState,
}

#[derive(Clone)]
struct McpClientHandler {
    tool_list_changed: Arc<AtomicBool>,
    client_info: rmcp::model::ClientInfo,
}

impl McpClientHandler {
    fn new(tool_list_changed: Arc<AtomicBool>) -> Self {
        let client_info = rmcp::model::ClientInfo {
            meta: None,
            protocol_version: rmcp::model::ProtocolVersion::default(),
            capabilities: rmcp::model::ClientCapabilities::default(),
            client_info: rmcp::model::Implementation {
                name: "spacebot".to_string(),
                title: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: Some("Spacebot MCP client".to_string()),
                icons: None,
                website_url: None,
            },
        };

        Self {
            tool_list_changed,
            client_info,
        }
    }
}

impl ClientHandler for McpClientHandler {
    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.tool_list_changed.store(true, Ordering::SeqCst);
        std::future::ready(())
    }

    fn get_info(&self) -> rmcp::model::ClientInfo {
        self.client_info.clone()
    }
}

pub struct McpConnection {
    name: String,
    config: McpServerConfig,
    state: RwLock<McpConnectionState>,
    client: Mutex<Option<McpClientSession>>,
    tools: RwLock<Vec<rmcp::model::Tool>>,
    tool_list_changed: Arc<AtomicBool>,
}

impl McpConnection {
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            config,
            state: RwLock::new(McpConnectionState::Disconnected),
            client: Mutex::new(None),
            tools: RwLock::new(Vec::new()),
            tool_list_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn state(&self) -> McpConnectionState {
        self.state.read().await.clone()
    }

    pub async fn is_connected(&self) -> bool {
        matches!(self.state().await, McpConnectionState::Connected)
    }

    pub async fn connect(&self) -> Result<()> {
        {
            let mut state = self.state.write().await;
            *state = McpConnectionState::Connecting;
        }

        let session_result = self.connect_session().await;
        let mut client_guard = self.client.lock().await;

        match session_result {
            Ok(session) => {
                let tools_result = session
                    .list_all_tools()
                    .await
                    .with_context(|| format!("failed to list tools for '{}'", self.name));

                let tools = match tools_result {
                    Ok(tools) => tools,
                    Err(error) => {
                        *client_guard = None;
                        drop(client_guard);

                        {
                            let mut cached_tools = self.tools.write().await;
                            cached_tools.clear();
                        }

                        let error_message = error.to_string();
                        let mut state = self.state.write().await;
                        *state = McpConnectionState::Failed(error_message.clone());
                        return Err(anyhow!(error_message));
                    }
                };
                *client_guard = Some(session);
                drop(client_guard);

                {
                    let mut cached_tools = self.tools.write().await;
                    *cached_tools = tools;
                }
                self.tool_list_changed.store(false, Ordering::SeqCst);

                let mut state = self.state.write().await;
                *state = McpConnectionState::Connected;
                Ok(())
            }
            Err(error) => {
                *client_guard = None;
                drop(client_guard);

                {
                    let mut cached_tools = self.tools.write().await;
                    cached_tools.clear();
                }

                let error_message = error.to_string();
                let mut state = self.state.write().await;
                *state = McpConnectionState::Failed(error_message.clone());
                Err(anyhow!(error_message))
            }
        }
    }

    pub async fn disconnect(&self) {
        let mut client_guard = self.client.lock().await;
        let mut session = client_guard.take();
        drop(client_guard);

        if let Some(client) = session.as_mut() {
            if let Err(error) = client.close().await {
                tracing::warn!(
                    server = %self.name,
                    %error,
                    "failed to close mcp session"
                );
            }
        }

        {
            let mut cached_tools = self.tools.write().await;
            cached_tools.clear();
        }
        self.tool_list_changed.store(false, Ordering::SeqCst);

        let mut state = self.state.write().await;
        *state = McpConnectionState::Disconnected;
    }

    pub async fn list_tools(&self) -> Vec<rmcp::model::Tool> {
        if self.tool_list_changed.swap(false, Ordering::SeqCst) {
            if let Err(error) = self.refresh_tools().await {
                tracing::warn!(server = %self.name, %error, "failed to refresh mcp tools");
            }
        }

        self.tools.read().await.clone()
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<rmcp::model::CallToolResult> {
        let arguments = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            _ => {
                return Err(anyhow!("mcp tool arguments must be a JSON object or null"));
            }
        };

        let client_guard = self.client.lock().await;
        let Some(client) = client_guard.as_ref() else {
            return Err(anyhow!("mcp server '{}' is not connected", self.name));
        };

        let params = rmcp::model::CallToolRequestParams {
            meta: None,
            name: Cow::Owned(tool_name.to_string()),
            arguments,
            task: None,
        };

        client
            .call_tool(params)
            .await
            .map_err(service_error_to_anyhow)
    }

    async fn refresh_tools(&self) -> Result<()> {
        let client_guard = self.client.lock().await;
        let Some(client) = client_guard.as_ref() else {
            return Err(anyhow!("mcp server '{}' is not connected", self.name));
        };
        let tools = client
            .list_all_tools()
            .await
            .map_err(service_error_to_anyhow)?;
        drop(client_guard);

        let mut cached_tools = self.tools.write().await;
        *cached_tools = tools;
        Ok(())
    }

    async fn connect_session(&self) -> Result<McpClientSession> {
        let handler = McpClientHandler::new(self.tool_list_changed.clone());

        match &self.config.transport {
            McpTransport::Stdio { command, args, env } => {
                let resolved_command = interpolate_env_placeholders(command);
                let resolved_args = args
                    .iter()
                    .map(|arg| interpolate_env_placeholders(arg))
                    .collect::<Vec<_>>();
                let resolved_env = env
                    .iter()
                    .map(|(key, value)| (key.clone(), interpolate_env_placeholders(value)))
                    .collect::<HashMap<_, _>>();

                let mut child_command = tokio::process::Command::new(&resolved_command);
                child_command.args(&resolved_args);
                child_command.envs(&resolved_env);

                let transport = rmcp::transport::TokioChildProcess::new(child_command)
                    .with_context(|| format!("failed to spawn stdio mcp server '{}'", self.name))?;

                rmcp::serve_client(handler, transport)
                    .await
                    .with_context(|| format!("failed to initialize mcp server '{}'", self.name))
            }
            McpTransport::Http { url, headers } => {
                let resolved_url = interpolate_env_placeholders(url);
                let resolved_headers = headers
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            interpolate_env_placeholders(value).trim().to_string(),
                        )
                    })
                    .collect::<HashMap<_, _>>();

                let mut custom_headers = HashMap::new();
                for (header_name, header_value) in resolved_headers {
                    let parsed_name = HeaderName::from_str(&header_name).with_context(|| {
                        format!(
                            "invalid mcp header name '{}' for server '{}'",
                            header_name, self.name
                        )
                    })?;
                    let parsed_value = HeaderValue::from_str(&header_value).with_context(|| {
                        format!(
                            "invalid mcp header value for '{}' on server '{}'",
                            header_name, self.name
                        )
                    })?;
                    custom_headers.insert(parsed_name, parsed_value);
                }

                let transport_config =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                        resolved_url,
                    )
                    .custom_headers(custom_headers);

                let transport =
                    rmcp::transport::StreamableHttpClientTransport::from_config(transport_config);

                rmcp::serve_client(handler, transport)
                    .await
                    .with_context(|| format!("failed to initialize mcp server '{}'", self.name))
            }
        }
    }
}

pub struct McpManager {
    connections: RwLock<HashMap<String, Arc<McpConnection>>>,
    configs: RwLock<Vec<McpServerConfig>>,
}

impl McpManager {
    pub fn new(configs: Vec<McpServerConfig>) -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            configs: RwLock::new(configs),
        }
    }

    pub async fn connect_all(&self) {
        let configs = self.configs.read().await.clone();
        for config in configs {
            if !config.enabled {
                continue;
            }

            let connection = self.upsert_connection(config).await;
            if let Err(error) = connection.connect().await {
                tracing::warn!(
                    server = %connection.name(),
                    %error,
                    "failed to connect mcp server"
                );
            }
        }
    }

    pub async fn disconnect_all(&self) {
        let connections = self
            .connections
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for connection in connections {
            connection.disconnect().await;
        }
    }

    pub async fn get_tools(&self) -> Vec<crate::tools::mcp::McpToolAdapter> {
        let connections = self
            .connections
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();

        let mut adapters = Vec::new();
        for connection in connections {
            if !connection.is_connected().await {
                continue;
            }

            let server_name = connection.name().to_string();
            let tools = connection.list_tools().await;
            for tool in tools {
                adapters.push(crate::tools::mcp::McpToolAdapter::new(
                    server_name.clone(),
                    tool,
                    connection.clone(),
                ));
            }
        }

        adapters
    }

    pub async fn reconnect(&self, name: &str) -> Result<()> {
        let config = self
            .configs
            .read()
            .await
            .iter()
            .find(|config| config.name == name)
            .cloned()
            .ok_or_else(|| anyhow!("mcp server '{}' is not configured", name))?;

        let (old_connection, connection) = {
            let mut connections = self.connections.write().await;
            let connection = Arc::new(McpConnection::new(config.clone()));
            let old_connection = connections.insert(name.to_string(), connection.clone());
            (old_connection, connection)
        };

        if let Some(old_connection) = old_connection {
            old_connection.disconnect().await;
        }

        if !config.enabled {
            return Ok(());
        }

        connection.connect().await
    }

    pub async fn reconcile(
        &self,
        old_configs: &[McpServerConfig],
        new_configs: &[McpServerConfig],
    ) {
        {
            let mut configs = self.configs.write().await;
            *configs = new_configs.to_vec();
        }

        let old_names = old_configs
            .iter()
            .map(|config| config.name.clone())
            .collect::<HashSet<_>>();
        let new_names = new_configs
            .iter()
            .map(|config| config.name.clone())
            .collect::<HashSet<_>>();

        for removed_name in old_names.difference(&new_names) {
            let removed_connection = self.connections.write().await.remove(removed_name);
            if let Some(connection) = removed_connection {
                connection.disconnect().await;
            }
        }

        let old_map = old_configs
            .iter()
            .map(|config| (config.name.clone(), config))
            .collect::<HashMap<_, _>>();

        for new_config in new_configs {
            if !new_config.enabled {
                let removed = self.connections.write().await.remove(&new_config.name);
                if let Some(connection) = removed {
                    connection.disconnect().await;
                }
                continue;
            }

            let should_reconnect = old_map
                .get(&new_config.name)
                .is_none_or(|old_config| *old_config != new_config);

            if should_reconnect {
                let removed = self.connections.write().await.remove(&new_config.name);
                if let Some(connection) = removed {
                    connection.disconnect().await;
                }

                let connection = self.upsert_connection(new_config.clone()).await;
                if let Err(error) = connection.connect().await {
                    tracing::warn!(
                        server = %new_config.name,
                        %error,
                        "failed to reconnect mcp server after config reload"
                    );
                }
                continue;
            }

            let connection = self.connections.read().await.get(&new_config.name).cloned();
            if let Some(connection) = connection {
                if !connection.is_connected().await {
                    if let Err(error) = connection.connect().await {
                        tracing::warn!(
                            server = %new_config.name,
                            %error,
                            "failed to connect unchanged mcp server"
                        );
                    }
                }
            } else {
                let connection = self.upsert_connection(new_config.clone()).await;
                if let Err(error) = connection.connect().await {
                    tracing::warn!(
                        server = %new_config.name,
                        %error,
                        "failed to connect missing mcp server"
                    );
                }
            }
        }
    }

    pub async fn statuses(&self) -> Vec<McpServerStatus> {
        let configs = self.configs.read().await.clone();
        let connections = self.connections.read().await.clone();

        let mut statuses = Vec::with_capacity(configs.len());
        for config in configs {
            let state = if let Some(connection) = connections.get(&config.name) {
                connection.state().await
            } else {
                McpConnectionState::Disconnected
            };

            statuses.push(McpServerStatus {
                name: config.name,
                enabled: config.enabled,
                transport: config.transport.kind().to_string(),
                state,
            });
        }

        statuses
    }

    async fn upsert_connection(&self, config: McpServerConfig) -> Arc<McpConnection> {
        let mut connections = self.connections.write().await;
        connections
            .entry(config.name.clone())
            .or_insert_with(|| Arc::new(McpConnection::new(config)))
            .clone()
    }
}

fn service_error_to_anyhow(error: ServiceError) -> anyhow::Error {
    anyhow!(error.to_string())
}

fn interpolate_env_placeholders(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;

    while let Some(start_offset) = value[cursor..].find("${") {
        let start = cursor + start_offset;
        output.push_str(&value[cursor..start]);

        let placeholder_start = start + 2;
        let Some(end_offset) = value[placeholder_start..].find('}') else {
            output.push_str(&value[start..]);
            return output;
        };

        let end = placeholder_start + end_offset;
        let var_name = &value[placeholder_start..end];
        if var_name.is_empty() {
            output.push_str("${}");
        } else {
            let resolved = std::env::var(var_name).unwrap_or_default();
            output.push_str(&resolved);
        }

        cursor = end + 1;
    }

    output.push_str(&value[cursor..]);
    output
}
