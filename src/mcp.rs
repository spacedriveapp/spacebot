//! MCP (Model Context Protocol) client integration.
//!
//! Types are always available for config parsing. The runtime client and
//! manager are compiled only with the `mcp` feature flag.

#[cfg(feature = "mcp")]
pub mod client;
#[cfg(feature = "mcp")]
pub mod manager;
pub mod types;

#[cfg(feature = "mcp")]
pub use client::McpClient;
#[cfg(feature = "mcp")]
pub use manager::McpManager;
pub use types::{McpConfig, McpServerConfig, McpTransport};
