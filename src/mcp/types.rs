//! MCP configuration types.

use serde::Deserialize;
use std::collections::HashMap;

/// Transport protocol for connecting to an MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Spawn a child process and communicate over stdin/stdout.
    Stdio,
    /// Connect to a server-sent events endpoint (Phase 2).
    Sse,
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
    /// SSE endpoint URL (SSE transport only, Phase 2).
    pub url: Option<String>,
    /// HTTP headers for SSE transport. Values support `env:VAR_NAME`.
    pub headers: HashMap<String, String>,
}

/// Per-agent MCP configuration.
///
/// Lists which MCP server IDs this agent should connect to. Resolved from
/// `[defaults.mcp]` merged with `[[agents]].mcp` following the same
/// inheritance pattern as routing and browser config.
#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    pub servers: Vec<String>,
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
}

fn default_transport() -> String {
    "stdio".into()
}

/// TOML deserialization target for `[defaults.mcp]` and `[[agents]].mcp`.
#[derive(Deserialize, Default)]
pub(crate) struct TomlMcpConfig {
    #[serde(default)]
    pub servers: Vec<String>,
}
