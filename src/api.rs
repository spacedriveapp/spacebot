//! HTTP API server for the Spacebot control interface.
//!
//! Serves the embedded frontend assets and provides a JSON API for
//! managing agents, viewing status, and interacting with the system.
//! Includes an SSE endpoint for realtime event streaming.

mod agents;
mod bindings;
mod channels;
mod config;
mod cortex;
mod cron;
mod ingest;
mod memories;
mod messaging;
mod models;
mod providers;
mod server;
mod settings;
mod skills;
mod state;
mod system;

pub use server::start_http_server;
pub use state::{AgentInfo, ApiEvent, ApiState};
