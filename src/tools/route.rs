//! Route tool for sending follow-ups to active workers.

use crate::WorkerId;
use crate::agent::channel::ChannelState;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for routing messages to workers.
#[derive(Debug, Clone)]
pub struct RouteTool {
    state: ChannelState,
}

impl RouteTool {
    /// Create a new route tool with access to channel state.
    pub fn new(state: ChannelState) -> Self {
        Self { state }
    }
}

/// Error type for route tool.
#[derive(Debug, thiserror::Error)]
#[error("Route failed: {0}")]
pub struct RouteError(String);

/// Arguments for route tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RouteArgs {
    /// The ID of the worker to route to (UUID format).
    pub worker_id: String,
    /// The message to send to the worker.
    pub message: String,
}

/// Output from route tool.
#[derive(Debug, Serialize)]
pub struct RouteOutput {
    /// Whether the message was routed successfully.
    pub routed: bool,
    /// The worker ID.
    pub worker_id: WorkerId,
    /// Status message.
    pub message: String,
}

impl Tool for RouteTool {
    const NAME: &'static str = "route";

    type Error = RouteError;
    type Args = RouteArgs;
    type Output = RouteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/route").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "worker_id": {
                        "type": "string",
                        "description": "The worker ID to route to (from spawn_worker result)"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message to send to the worker"
                    }
                },
                "required": ["worker_id", "message"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let worker_id = args
            .worker_id
            .parse::<WorkerId>()
            .map_err(|e| RouteError(format!("Invalid worker ID: {e}")))?;

        // Look up the input sender for this worker
        let inputs = self.state.worker_inputs.read().await;
        let input_tx = inputs.get(&worker_id)
            .ok_or_else(|| RouteError(format!(
                "Worker {worker_id} not found or not interactive. Only interactive workers accept follow-up messages."
            )))?
            .clone();
        drop(inputs);

        // Deliver the message
        input_tx.send(args.message).await.map_err(|_| {
            RouteError(format!(
                "Worker {worker_id} has stopped accepting input (channel closed)"
            ))
        })?;

        tracing::info!(
            worker_id = %worker_id,
            channel_id = %self.state.channel_id,
            "message routed to worker"
        );

        Ok(RouteOutput {
            routed: true,
            worker_id,
            message: format!("Message delivered to worker {worker_id}."),
        })
    }
}
