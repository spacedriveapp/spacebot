//! Meta-tool for connecting to an MCP server mid-conversation.
//!
//! Workers can call this to discover and connect to a new MCP server at runtime
//! without modifying config.toml. Only streamable HTTP (SSE) URLs are allowed —
//! stdio would require arbitrary command execution.

use crate::mcp::McpManager;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use rig::tool::server::ToolServerHandle;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Tool that connects to a new MCP server at runtime and registers its tools.
#[derive(Clone)]
pub struct ConnectMcpTool {
    mcp_manager: Arc<McpManager>,
    tool_server: ToolServerHandle,
}

impl ConnectMcpTool {
    pub fn new(mcp_manager: Arc<McpManager>, tool_server: ToolServerHandle) -> Self {
        Self {
            mcp_manager,
            tool_server,
        }
    }
}

/// Error type for connect_mcp tool.
#[derive(Debug, thiserror::Error)]
pub enum ConnectMcpError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Tool registration failed: {0}")]
    RegistrationFailed(String),
}

/// Arguments for connect_mcp tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectMcpArgs {
    /// Server URL (streamable HTTP/SSE endpoint). Must be http:// or https://.
    pub url: String,
    /// Optional authorization header value (e.g. "Bearer token123").
    pub auth: Option<String>,
    /// Short identifier for this server (used as tool name prefix, e.g. "weather").
    pub server_id: String,
}

/// Output from connect_mcp tool.
#[derive(Debug, Serialize)]
pub struct ConnectMcpOutput {
    /// Whether the connection was successful.
    pub success: bool,
    /// The server identifier used for tool name prefixes.
    pub server_id: String,
    /// List of discovered tool names (already prefixed with server_id).
    pub tools: Vec<String>,
    /// Human-readable status message.
    pub message: String,
}

impl Tool for ConnectMcpTool {
    const NAME: &'static str = "connect_mcp";

    type Error = ConnectMcpError;
    type Args = ConnectMcpArgs;
    type Output = ConnectMcpOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/connect_mcp").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Server URL (streamable HTTP endpoint). Must be http:// or https://."
                    },
                    "auth": {
                        "type": "string",
                        "description": "Optional authorization header value (e.g. \"Bearer token123\")."
                    },
                    "server_id": {
                        "type": "string",
                        "description": "Short identifier for this server. Used as a prefix for tool names (e.g. \"weather\" makes tools like \"weather_get_forecast\")."
                    }
                },
                "required": ["url", "server_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate URL scheme
        let url_lower = args.url.to_lowercase();
        if !url_lower.starts_with("http://") && !url_lower.starts_with("https://") {
            return Err(ConnectMcpError::InvalidUrl(
                "Only http:// and https:// URLs are allowed".into(),
            ));
        }

        // Validate server_id
        if args.server_id.trim().is_empty() {
            return Err(ConnectMcpError::InvalidUrl(
                "server_id cannot be empty".into(),
            ));
        }

        // Build ephemeral config
        let mut headers = HashMap::new();
        if let Some(auth) = &args.auth {
            headers.insert("Authorization".to_string(), auth.clone());
        }

        let config = crate::mcp::McpServerConfig {
            id: args.server_id.clone(),
            transport: crate::mcp::McpTransport::StreamableHttp,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(args.url.clone()),
            headers,
            scopes: vec![crate::mcp::McpScope::Worker],
        };

        // Connect and discover tools
        let tool_names = self
            .mcp_manager
            .add_runtime_server(config)
            .await
            .map_err(|e| ConnectMcpError::ConnectionFailed(e.to_string()))?;

        // Register discovered tools onto the worker's tool server
        let registered_count = tool_names.len();
        let prefix = format!("{}_", args.server_id);
        for (tool, sink) in self
            .mcp_manager
            .tools_for_scope(crate::mcp::McpScope::Worker)
            .await
        {
            let name = tool.name.to_string();
            if name.starts_with(&prefix) {
                let mcp_tool = rig::tool::rmcp::McpTool::from_mcp_server(tool, sink);
                if let Err(error) = self.tool_server.add_tool(mcp_tool).await {
                    tracing::warn!(%error, name, "failed to register runtime MCP tool");
                }
            }
        }

        Ok(ConnectMcpOutput {
            success: true,
            server_id: args.server_id,
            tools: tool_names,
            message: format!(
                "Connected successfully. {registered_count} tools now available."
            ),
        })
    }
}

impl std::fmt::Debug for ConnectMcpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectMcpTool").finish_non_exhaustive()
    }
}
