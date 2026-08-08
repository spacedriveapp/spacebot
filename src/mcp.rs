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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionState {
    Connecting,
    Connected,
    Failed(String),
    Disconnected,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
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
        let implementation =
            rmcp::model::Implementation::new("spacebot", env!("CARGO_PKG_VERSION"))
                .with_description("Spacebot MCP client");

        let client_info = rmcp::model::ClientInfo::new(
            rmcp::model::ClientCapabilities::default(),
            implementation,
        );

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

#[cfg(feature = "metrics")]
fn set_mcp_connection_state(
    server_name: &str,
    connected: i64,
    connecting: i64,
    disconnected: i64,
    failed: i64,
    tools_registered: i64,
) {
    let metrics = crate::telemetry::Metrics::global();
    metrics
        .mcp_connections
        .with_label_values(&[server_name, "connected"])
        .set(connected);
    metrics
        .mcp_connections
        .with_label_values(&[server_name, "connecting"])
        .set(connecting);
    metrics
        .mcp_connections
        .with_label_values(&[server_name, "disconnected"])
        .set(disconnected);
    metrics
        .mcp_connections
        .with_label_values(&[server_name, "failed"])
        .set(failed);
    metrics
        .mcp_tools_registered
        .with_label_values(&[server_name])
        .set(tools_registered);
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
        #[cfg(feature = "metrics")]
        let connect_start = std::time::Instant::now();

        {
            let mut state = self.state.write().await;
            *state = McpConnectionState::Connecting;
        }

        #[cfg(feature = "metrics")]
        set_mcp_connection_state(&self.name, 0, 1, 0, 0, 0);

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

                        #[cfg(feature = "metrics")]
                        {
                            crate::telemetry::Metrics::global()
                                .mcp_connection_attempts_total
                                .with_label_values(&[&self.name, "failure"])
                                .inc();
                            set_mcp_connection_state(&self.name, 0, 0, 0, 1, 0);
                        }

                        return Err(anyhow!(error_message));
                    }
                };
                *client_guard = Some(session);
                drop(client_guard);

                #[cfg(feature = "metrics")]
                let tool_count = tools.len() as i64;

                {
                    let mut cached_tools = self.tools.write().await;
                    *cached_tools = tools;
                }
                self.tool_list_changed.store(false, Ordering::SeqCst);

                {
                    let mut state = self.state.write().await;
                    *state = McpConnectionState::Connected;
                }

                #[cfg(feature = "metrics")]
                {
                    let metrics = crate::telemetry::Metrics::global();
                    let elapsed = connect_start.elapsed().as_secs_f64();
                    metrics
                        .mcp_connection_attempts_total
                        .with_label_values(&[&self.name, "success"])
                        .inc();
                    metrics
                        .mcp_connection_duration_seconds
                        .with_label_values(&[&self.name])
                        .observe(elapsed);
                    set_mcp_connection_state(&self.name, 1, 0, 0, 0, tool_count);
                }

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

                #[cfg(feature = "metrics")]
                {
                    crate::telemetry::Metrics::global()
                        .mcp_connection_attempts_total
                        .with_label_values(&[&self.name, "failure"])
                        .inc();
                    set_mcp_connection_state(&self.name, 0, 0, 0, 1, 0);
                }

                Err(anyhow!(error_message))
            }
        }
    }

    /// Attempt connection with exponential backoff.
    ///
    /// 5s initial delay, doubling up to 60s cap, max 12 attempts. Follows the
    /// same retry pattern as `MessagingManager::spawn_retry_task`.
    pub async fn connect_with_retry(self: &Arc<Self>) -> bool {
        const MAX_ATTEMPTS: usize = 12;
        const INITIAL_DELAY_SECS: u64 = 5;
        const MAX_DELAY_SECS: u64 = 60;

        let mut delay_secs = INITIAL_DELAY_SECS;

        for attempt in 1..=MAX_ATTEMPTS {
            match self.connect().await {
                Ok(()) => {
                    #[cfg(feature = "metrics")]
                    if attempt > 1 {
                        let metrics = crate::telemetry::Metrics::global();
                        metrics
                            .mcp_reconnects_total
                            .with_label_values(&[&self.name])
                            .inc();
                    }
                    return true;
                }
                Err(error) => {
                    tracing::warn!(
                        server = %self.name,
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        %error,
                        retry_in_secs = delay_secs,
                        "mcp connection failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    delay_secs = (delay_secs * 2).min(MAX_DELAY_SECS);
                }
            }
        }

        tracing::error!(
            server = %self.name,
            "mcp connection failed after {MAX_ATTEMPTS} attempts, giving up"
        );
        false
    }

    pub async fn disconnect(&self) {
        let mut client_guard = self.client.lock().await;
        let mut session = client_guard.take();
        drop(client_guard);

        if let Some(client) = session.as_mut()
            && let Err(error) = client.close().await
        {
            tracing::warn!(
                server = %self.name,
                %error,
                "failed to close mcp session"
            );
        }

        {
            let mut cached_tools = self.tools.write().await;
            cached_tools.clear();
        }
        self.tool_list_changed.store(false, Ordering::SeqCst);

        let mut state = self.state.write().await;
        *state = McpConnectionState::Disconnected;

        #[cfg(feature = "metrics")]
        set_mcp_connection_state(&self.name, 0, 0, 1, 0, 0);
    }

    pub async fn list_tools(&self) -> Vec<rmcp::model::Tool> {
        if self.tool_list_changed.swap(false, Ordering::SeqCst)
            && let Err(error) = self.refresh_tools().await
        {
            tracing::warn!(server = %self.name, %error, "failed to refresh mcp tools");
        }

        self.tools.read().await.clone()
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<rmcp::model::CallToolResult> {
        #[cfg(feature = "metrics")]
        let call_start = std::time::Instant::now();

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

        let mut params = rmcp::model::CallToolRequestParams::new(Cow::Owned(tool_name.to_string()));
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }

        let result = client
            .call_tool(params)
            .await
            .map_err(service_error_to_anyhow);

        #[cfg(feature = "metrics")]
        {
            let metrics = crate::telemetry::Metrics::global();
            let elapsed = call_start.elapsed().as_secs_f64();
            metrics
                .mcp_tool_calls_total
                .with_label_values(&[&self.name, tool_name])
                .inc();
            metrics
                .mcp_tool_call_duration_seconds
                .with_label_values(&[&self.name, tool_name])
                .observe(elapsed);
        }

        result
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
                child_command.env_clear();
                // Keep PATH so stdio servers launched via shims/shebangs
                // (for example `npx` -> `/usr/bin/env node`) can still
                // resolve their runtime without inheriting unrelated secrets.
                if let Ok(path) = std::env::var("PATH") {
                    child_command.env("PATH", path);
                }
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
                let mut auth_header_value = None;
                for (header_name, header_value) in resolved_headers {
                    if header_name.eq_ignore_ascii_case("authorization") {
                        HeaderValue::from_str(&header_value).with_context(|| {
                            format!(
                                "invalid mcp header value for '{}' on server '{}'",
                                header_name, self.name
                            )
                        })?;
                        auth_header_value = Some(header_value);
                        continue;
                    }
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

                let mut transport_config =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                        resolved_url,
                    )
                    .custom_headers(custom_headers);

                if let Some(auth_value) = auth_header_value {
                    let token = parse_bearer_token(&auth_value, &self.name)?;
                    transport_config = transport_config.auth_header(&token);
                }

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
                    "mcp server initial connection failed, starting background retry"
                );
                let conn = connection.clone();
                tokio::spawn(async move {
                    conn.connect_with_retry().await;
                });
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

    /// Return namespaced tool names for all connected MCP servers.
    ///
    /// Used to inform the channel prompt about available MCP tools so the
    /// agent knows it can delegate work that uses them.
    pub async fn get_tool_names(&self) -> Vec<String> {
        let connections = self
            .connections
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();

        let mut names = Vec::new();
        for connection in connections {
            if !connection.is_connected().await {
                continue;
            }

            let server_name = connection.name().to_string();
            let tools = connection.list_tools().await;
            for tool in tools {
                let description = tool
                    .description
                    .as_ref()
                    .map(|d| d.as_ref().to_string())
                    .unwrap_or_default();
                names.push(format!(
                    "{} — {}",
                    tool.name,
                    if description.is_empty() {
                        format!("from {}", server_name)
                    } else {
                        description
                    }
                ));
            }
        }

        names.sort();
        names
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

        let result = connection.connect().await;

        #[cfg(feature = "metrics")]
        if result.is_ok() {
            let metrics = crate::telemetry::Metrics::global();
            metrics
                .mcp_reconnects_total
                .with_label_values(&[name])
                .inc();
        }

        result
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
                    let conn = connection.clone();
                    tokio::spawn(async move {
                        conn.connect_with_retry().await;
                    });
                }
                continue;
            }

            let connection = self.connections.read().await.get(&new_config.name).cloned();
            if let Some(connection) = connection {
                if !connection.is_connected().await
                    && let Err(error) = connection.connect().await
                {
                    tracing::warn!(
                        server = %new_config.name,
                        %error,
                        "failed to connect unchanged mcp server"
                    );
                    let conn = connection.clone();
                    tokio::spawn(async move {
                        conn.connect_with_retry().await;
                    });
                }
            } else {
                let connection = self.upsert_connection(new_config.clone()).await;
                if let Err(error) = connection.connect().await {
                    tracing::warn!(
                        server = %new_config.name,
                        %error,
                        "failed to connect missing mcp server"
                    );
                    let conn = connection.clone();
                    tokio::spawn(async move {
                        conn.connect_with_retry().await;
                    });
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

/// Parse a Bearer token from an Authorization header value.
///
/// rmcp's `auth_header()` uses reqwest's `.bearer_auth()` which always
/// prepends `"Bearer "`. This function strips any `Bearer` prefix
/// (case-insensitive) and rejects non-Bearer schemes and empty tokens.
fn parse_bearer_token(auth_value: &str, server_name: &str) -> anyhow::Result<String> {
    let trimmed = auth_value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty Authorization header value for mcp server '{server_name}'");
    }

    // Check if the value starts with a known scheme word (case-insensitive).
    // We split on the first space to detect "Bearer <token>" vs "Basic ..." etc.
    if let Some(space_pos) = trimmed.find(' ') {
        let scheme = &trimmed[..space_pos];
        if !scheme.eq_ignore_ascii_case("Bearer") {
            anyhow::bail!(
                "unsupported Authorization scheme '{scheme}' for mcp server \
                 '{server_name}': only Bearer tokens are supported (rmcp always \
                 sends Bearer auth). Remove the scheme prefix or use a Bearer token."
            );
        }
        let token = trimmed[space_pos + 1..].trim();
        if token.is_empty() {
            anyhow::bail!("empty Bearer token value for mcp server '{server_name}'");
        }
        Ok(token.to_string())
    } else if trimmed.eq_ignore_ascii_case("Bearer") {
        // Bare "Bearer" with no token value.
        anyhow::bail!("empty Bearer token value for mcp server '{server_name}'");
    } else {
        // Raw token with no scheme prefix — pass through as-is.
        Ok(trimmed.to_string())
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
            let resolved = std::env::var(var_name)
                .ok()
                .or_else(|| crate::config::resolve_env_value(&format!("secret:{var_name}")))
                .unwrap_or_default();
            output.push_str(&resolved);
        }

        cursor = end + 1;
    }

    output.push_str(&value[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mcp_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "test".to_string(),
                args: Vec::new(),
                env: HashMap::new(),
            },
        }
    }

    fn test_tool(name: &str, description: Option<&str>) -> rmcp::model::Tool {
        let mut tool = rmcp::model::Tool::default();
        tool.name = Cow::Owned(name.to_string());
        tool.description = description.map(|description| Cow::Owned(description.to_string()));
        tool
    }

    #[tokio::test]
    async fn get_tool_names_returns_deterministic_sorted_names() {
        let manager = McpManager::new(Vec::new());

        let later_connection = Arc::new(McpConnection::new(test_mcp_config("z_server")));
        {
            let mut tools = later_connection.tools.write().await;
            *tools = vec![test_tool("z_tool", Some("z desc"))];
        }
        {
            let mut state = later_connection.state.write().await;
            *state = McpConnectionState::Connected;
        }

        let earlier_connection = Arc::new(McpConnection::new(test_mcp_config("a_server")));
        {
            let mut tools = earlier_connection.tools.write().await;
            *tools = vec![
                test_tool("b_tool", None),
                test_tool("a_tool", Some("a desc")),
            ];
        }
        {
            let mut state = earlier_connection.state.write().await;
            *state = McpConnectionState::Connected;
        }

        {
            let mut connections = manager.connections.write().await;
            connections.insert("z_server".to_string(), later_connection);
            connections.insert("a_server".to_string(), earlier_connection);
        }

        assert_eq!(
            manager.get_tool_names().await,
            vec![
                "a_tool — a desc",
                "b_tool — from a_server",
                "z_tool — z desc"
            ]
        );
    }

    #[test]
    fn parse_bearer_token_strips_bearer_prefix() {
        let token = parse_bearer_token("Bearer abc123", "test").unwrap();
        assert_eq!(token, "abc123");
    }

    #[test]
    fn parse_bearer_token_case_insensitive() {
        assert_eq!(parse_bearer_token("bearer abc", "test").unwrap(), "abc");
        assert_eq!(parse_bearer_token("BEARER abc", "test").unwrap(), "abc");
        assert_eq!(parse_bearer_token("BeArEr abc", "test").unwrap(), "abc");
    }

    #[test]
    fn parse_bearer_token_raw_token_passthrough() {
        let token = parse_bearer_token("abc123", "test").unwrap();
        assert_eq!(token, "abc123");
    }

    #[test]
    fn parse_bearer_token_rejects_non_bearer_scheme() {
        let err = parse_bearer_token("Basic dXNlcjpwYXNz", "myserver").unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported Authorization scheme 'Basic'")
        );
        assert!(err.to_string().contains("myserver"));
    }

    #[test]
    fn parse_bearer_token_rejects_empty_value() {
        assert!(parse_bearer_token("", "test").is_err());
        assert!(parse_bearer_token("   ", "test").is_err());
    }

    #[test]
    fn parse_bearer_token_rejects_empty_bearer_value() {
        let err = parse_bearer_token("Bearer ", "test").unwrap_err();
        assert!(err.to_string().contains("empty Bearer token"));
    }

    #[test]
    fn parse_bearer_token_trims_whitespace() {
        assert_eq!(parse_bearer_token("  Bearer abc  ", "test").unwrap(), "abc");
        assert_eq!(
            parse_bearer_token("  raw_token  ", "test").unwrap(),
            "raw_token"
        );
    }

    #[test]
    fn interpolate_env_placeholders_no_placeholders() {
        assert_eq!(interpolate_env_placeholders("hello world"), "hello world");
    }

    #[test]
    fn interpolate_env_placeholders_literal_passthrough() {
        assert_eq!(interpolate_env_placeholders("no-vars-here"), "no-vars-here");
        assert_eq!(interpolate_env_placeholders("${"), "${");
        assert_eq!(interpolate_env_placeholders("${}"), "${}");
    }
}
