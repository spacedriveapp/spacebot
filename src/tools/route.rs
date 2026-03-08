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

        // Check the status block to determine the worker's actual state.
        // Using sender map presence alone is unreliable: interactive workers
        // register both `worker_inputs` and `worker_injections` at spawn
        // time, so the input sender is always present regardless of whether
        // the worker is idle or running.
        let worker_is_idle = {
            let status = self.state.status_block.read().await;
            status
                .active_workers
                .iter()
                .find(|w| w.id == worker_id)
                .map(|w| w.status == "idle")
        };

        match worker_is_idle {
            // Worker is idle (WaitingForInput) — deliver as interactive follow-up.
            Some(true) => {
                let inputs = self.state.worker_inputs.read().await;
                if let Some(input_tx) = inputs.get(&worker_id).cloned() {
                    drop(inputs);

                    input_tx.send(args.message).await.map_err(|_| {
                        RouteError(format!(
                            "Worker {worker_id} has stopped accepting input (channel closed)"
                        ))
                    })?;

                    tracing::info!(
                        worker_id = %worker_id,
                        channel_id = %self.state.channel_id,
                        "message routed to interactive worker (input)"
                    );

                    return Ok(RouteOutput {
                        routed: true,
                        worker_id,
                        message: format!(
                            "Message delivered to worker {worker_id} (follow-up input)."
                        ),
                    });
                }
                drop(inputs);

                // Worker is idle but has no input channel — shouldn't happen
                // for interactive workers, but fall through to injection.
            }
            // Worker is running — use context injection.
            Some(false) => {
                let injections = self.state.worker_injections.read().await;
                if let Some(inject_tx) = injections.get(&worker_id).cloned() {
                    drop(injections);

                    inject_tx.send(args.message).await.map_err(|_| {
                        RouteError(format!(
                            "Worker {worker_id} has stopped running (injection channel closed)"
                        ))
                    })?;

                    tracing::info!(
                        worker_id = %worker_id,
                        channel_id = %self.state.channel_id,
                        "context injected into running worker"
                    );

                    return Ok(RouteOutput {
                        routed: true,
                        worker_id,
                        message: format!(
                            "Context injected into running worker {worker_id}. \
                             The worker will incorporate this at its next turn boundary."
                        ),
                    });
                }
                drop(injections);

                // Worker is running but has no injection channel (e.g. OpenCode
                // workers only support interactive follow-ups, not mid-flight
                // injection). Return a structured result so the LLM knows to
                // wait rather than falling through to "not found".
                let has_input = self
                    .state
                    .worker_inputs
                    .read()
                    .await
                    .contains_key(&worker_id);
                if has_input {
                    return Ok(RouteOutput {
                        routed: false,
                        worker_id,
                        message: format!(
                            "Worker {worker_id} is currently running and does not support \
                             mid-flight context injection. Wait for it to finish or become \
                             idle before sending follow-up input."
                        ),
                    });
                }
            }
            // Worker not found in status block.
            None => {}
        }

        Err(RouteError(format!(
            "Worker {worker_id} not found. It may have already completed or been cancelled."
        )))
    }
}
