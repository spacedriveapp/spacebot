//! Route tool for sending follow-ups to agent-owned workers.

use crate::WorkerId;
use crate::agent::channel::ChannelState;
use crate::agent::process_control::{WorkerRequester, WorkerResultTarget, WorkerRouteResult};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct RouteTool {
    state: ChannelState,
}

impl RouteTool {
    pub fn new(state: ChannelState) -> Self {
        Self { state }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Route failed: {0}")]
pub struct RouteError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RouteArgs {
    pub worker_id: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct RouteOutput {
    pub routed: bool,
    pub worker_id: WorkerId,
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
                    "worker_id": { "type": "string", "description": "The worker ID" },
                    "message": { "type": "string", "description": "The message to send" }
                },
                "required": ["worker_id", "message"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let worker_id = args
            .worker_id
            .parse::<WorkerId>()
            .map_err(|error| RouteError(format!("Invalid worker ID: {error}")))?;
        let autonomy_run = self.state.autonomy_run();
        let requester = autonomy_run.as_ref().map_or_else(
            || WorkerRequester::Channel {
                channel_id: self.state.channel_id.clone(),
            },
            |run| WorkerRequester::Autonomy {
                run_id: run.run_id.clone(),
            },
        );
        let registry = &self.state.deps.process_control_registry;
        let Some(snapshot) = registry.worker_snapshot(worker_id).await else {
            return Err(RouteError(format!(
                "Worker {worker_id} was not found in this agent's live registry."
            )));
        };
        let result = if snapshot.state
            == crate::agent::process_control::WorkerRuntimeState::WaitingForInput
        {
            let follow_up = registry
                .claim_idle_follow_up(
                    worker_id,
                    requester,
                    WorkerResultTarget::Channel {
                        channel_id: self.state.channel_id.clone(),
                    },
                    autonomy_run.as_ref().map(|run| run.run_id.clone()),
                    args.message,
                )
                .await
                .map_err(|error| RouteError(error.to_string()))?;
            let operation = follow_up.operation.clone();
            let child = crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: operation.operation_id,
            };
            if let Some(run) = &autonomy_run
                && !run.register_child(child)
            {
                registry
                    .rollback_failed_follow_up(
                        crate::agent::process_control::WorkerCallbackContext {
                            worker_id,
                            registration_id: snapshot.registration_id,
                        },
                        operation.operation_id,
                    )
                    .await;
                return Err(RouteError(
                    "The autonomy epoch is finishing and cannot own this operation".to_string(),
                ));
            }
            let delivered = registry
                .deliver_claimed_follow_up(
                    crate::agent::process_control::WorkerCallbackContext {
                        worker_id,
                        registration_id: snapshot.registration_id,
                    },
                    follow_up,
                )
                .await;
            if delivered != crate::agent::process_control::WorkerMutationResult::Applied {
                if let Some(run) = &autonomy_run {
                    run.settle_child(child);
                }
                WorkerRouteResult::NotFound
            } else {
                WorkerRouteResult::Routed { operation }
            }
        } else if snapshot.state == crate::agent::process_control::WorkerRuntimeState::Running {
            registry.inject_running(worker_id, args.message).await
        } else {
            WorkerRouteResult::Busy {
                state: snapshot.state,
            }
        };

        match result {
            WorkerRouteResult::Routed { operation: _ } => Ok(RouteOutput {
                routed: true,
                worker_id,
                message: format!("Message delivered to worker {worker_id}."),
            }),
            WorkerRouteResult::Injected => Ok(RouteOutput {
                routed: true,
                worker_id,
                message: format!(
                    "Context injected into running worker {worker_id}; it remains part of the current operation."
                ),
            }),
            WorkerRouteResult::WaitUntilIdle => Ok(RouteOutput {
                routed: false,
                worker_id,
                message: format!(
                    "Worker {worker_id} is running and does not support injection. Wait until it is idle."
                ),
            }),
            WorkerRouteResult::Busy { state } => Ok(RouteOutput {
                routed: false,
                worker_id,
                message: format!("Worker {worker_id} is {state}."),
            }),
            WorkerRouteResult::NotFound => Err(RouteError(format!(
                "Worker {worker_id} was not found in this agent's live registry."
            ))),
        }
    }
}
