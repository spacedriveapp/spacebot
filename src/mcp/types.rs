//! MCP configuration types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Transport protocol for connecting to an MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Spawn a child process and communicate over stdin/stdout.
    Stdio,
    /// Connect via streamable HTTP (SSE-based) endpoint.
    StreamableHttp,
}

/// Process types that can receive MCP tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    Worker,
    Branch,
    Channel,
    CortexChat,
}

/// Default scopes when none are specified.
pub fn default_scopes() -> Vec<McpScope> {
    vec![McpScope::Worker, McpScope::CortexChat]
}

/// Instance-level MCP server definition.
///
/// Shared across agents — each agent gets its own connection to the same
/// server definition. Analogous to how LLM providers are defined once and
/// referenced per-agent.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub id: String,
    pub transport: McpTransport,
    /// Command to spawn (stdio transport only).
    pub command: Option<String>,
    /// Arguments for the spawned command (stdio transport only).
    pub args: Vec<String>,
    /// Environment variables for the spawned process. Values support
    /// `env:VAR_NAME` references resolved at startup.
    pub env: HashMap<String, String>,
    /// Streamable HTTP endpoint URL.
    pub url: Option<String>,
    /// HTTP headers for streamable HTTP transport. Values support `env:VAR_NAME`.
    pub headers: HashMap<String, String>,
    /// Which process types can use this server's tools.
    /// Empty means inherit from agent-level `McpConfig.scopes`.
    pub scopes: Vec<McpScope>,
}

/// Per-agent MCP configuration.
///
/// Lists which MCP server IDs this agent should connect to. Resolved from
/// `[defaults.mcp]` merged with `[[agents]].mcp` following the same
/// inheritance pattern as routing and browser config.
#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    pub servers: Vec<String>,
    /// Default scopes inherited by all servers that don't specify their own.
    pub scopes: Vec<McpScope>,
}

/// TOML deserialization target for `[[mcp_servers]]`.
#[derive(Deserialize)]
pub(crate) struct TomlMcpServerConfig {
    pub id: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub scopes: Vec<McpScope>,
}

fn default_transport() -> String {
    "stdio".into()
}

/// TOML deserialization target for `[defaults.mcp]` and `[[agents]].mcp`.
#[derive(Deserialize, Default)]
pub(crate) struct TomlMcpConfig {
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<McpScope>,
}
