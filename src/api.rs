//! HTTP API server for the Spacebot control interface.
//!
//! Serves the embedded frontend assets and provides a JSON API for
//! managing agents, viewing status, and interacting with the system.
//! Includes an SSE endpoint for realtime event streaming.

pub mod agents;
mod bindings;
mod channels;
mod codegraph;
mod config;
mod cortex;
mod cron;
mod factory;
mod fs;
mod ingest;
mod links;
mod mcp;
mod memories;
mod messaging;
mod models;
mod opencode_proxy;
mod projects;
mod providers;
mod schema_alias;
mod secrets;
mod server;
mod settings;
mod skills;
pub(crate) mod ssh;
pub mod state;
mod system;
mod tasks;
mod tools;
mod webchat;
mod workers;

pub use server::{api_router, start_http_server};
pub use state::{AgentInfo, ApiEvent, ApiState};
