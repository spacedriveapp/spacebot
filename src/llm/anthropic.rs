//! Anthropic API integration: auth routing, tool normalization, and caching.

pub mod auth;
pub mod cache;
pub mod params;
pub mod tools;

pub use auth::{AnthropicAuthPath, apply_auth_headers, detect_auth_path};
pub use cache::{CacheRetention, get_cache_control, resolve_cache_retention};
pub use params::build_anthropic_request;
pub use tools::{from_claude_code_name, to_claude_code_name};
