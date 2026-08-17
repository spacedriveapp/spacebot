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
            return self.detached_worker_output(worker_id).await;
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
                return Err(RouteError(format!(
                    "Worker {worker_id} stopped accepting input before the follow-up was delivered."
                )));
            }
            WorkerRouteResult::Routed { operation }
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
            // A worker that left the registry between the snapshot and the send
            // resolves against durable state rather than reporting a bare miss.
            WorkerRouteResult::Terminal { .. }
            | WorkerRouteResult::Unavailable { .. }
            | WorkerRouteResult::NotFound => self.detached_worker_output(worker_id).await,
        }
    }
}

impl RouteTool {
    /// Describe a worker the registry does not hold. A terminal worker reports
    /// its durable outcome, a nonterminal row without controls reports that it
    /// is unavailable, and an unknown ID is an error.
    async fn detached_worker_output(
        &self,
        worker_id: WorkerId,
    ) -> std::result::Result<RouteOutput, RouteError> {
        let resolved = resolve_detached_worker(&self.state.process_run_logger, worker_id)
            .await
            .map_err(|error| RouteError(error.to_string()))?;
        match resolved {
            WorkerRouteResult::Terminal { lifecycle, result } => Ok(RouteOutput {
                routed: false,
                worker_id,
                message: match result {
                    Some(result) => format!(
                        "Worker {worker_id} already finished ({}). It cannot accept follow-ups. Its result was:\n\n{result}",
                        lifecycle.as_str()
                    ),
                    None => format!(
                        "Worker {worker_id} already finished ({}). It cannot accept follow-ups.",
                        lifecycle.as_str()
                    ),
                },
            }),
            WorkerRouteResult::Unavailable { lifecycle } => Ok(RouteOutput {
                routed: false,
                worker_id,
                message: format!(
                    "Worker {worker_id} is recorded as {} but has no live controls in this agent, \
                     so it cannot be routed to. Inspect its transcript instead of retrying.",
                    lifecycle.as_str()
                ),
            }),
            _ => Err(RouteError(format!(
                "Worker {worker_id} was not found in this agent."
            ))),
        }
    }
}

/// Resolve a worker that has no live registry entry against durable state.
async fn resolve_detached_worker(
    run_logger: &crate::conversation::ProcessRunLogger,
    worker_id: WorkerId,
) -> crate::Result<WorkerRouteResult> {
    let Some(lifecycle) = run_logger.read_worker_lifecycle(worker_id).await? else {
        return Ok(WorkerRouteResult::NotFound);
    };
    if !lifecycle.is_terminal() {
        return Ok(WorkerRouteResult::Unavailable { lifecycle });
    }
    let result = run_logger
        .read_worker_terminal(worker_id)
        .await?
        .map(|terminal| terminal.result);
    Ok(WorkerRouteResult::Terminal { lifecycle, result })
}

#[cfg(test)]
mod tests {
    use super::{WorkerRouteResult, resolve_detached_worker};
    use crate::conversation::{
        ProcessRunLogger, WorkerLifecycle, WorkerOutcomeKind, WorkerTerminalOwner,
    };
    use std::sync::Arc;

    async fn logger() -> ProcessRunLogger {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO channels (id, platform) VALUES ('channel-a', 'test')")
            .execute(&pool)
            .await
            .unwrap();
        ProcessRunLogger::new(pool)
    }

    async fn start_worker(logger: &ProcessRunLogger, worker_id: crate::WorkerId) {
        logger
            .log_worker_started(
                Some(&Arc::from("channel-a")),
                worker_id,
                "task-a",
                "builtin",
                &Arc::from("agent"),
                true,
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }

    /// An ID with no durable row is the only genuine "not found".
    #[tokio::test]
    async fn unknown_worker_is_not_found() {
        let logger = logger().await;

        assert_eq!(
            resolve_detached_worker(&logger, uuid::Uuid::new_v4())
                .await
                .unwrap(),
            WorkerRouteResult::NotFound
        );
    }

    /// A durable nonterminal row without live controls is unavailable, not
    /// missing — the distinction the August 16 incident turned on.
    #[tokio::test]
    async fn detached_nonterminal_worker_is_unavailable() {
        let logger = logger().await;
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id).await;
        logger.log_worker_idle(worker_id).await.unwrap();

        assert_eq!(
            resolve_detached_worker(&logger, worker_id).await.unwrap(),
            WorkerRouteResult::Unavailable {
                lifecycle: WorkerLifecycle::WaitingForInput,
            }
        );
    }

    /// A terminal worker reports its outcome so the caller stops retrying.
    #[tokio::test]
    async fn terminal_worker_reports_its_durable_outcome() {
        let logger = logger().await;
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id).await;
        crate::agent::channel_dispatch::commit_worker_outcome(
            &logger,
            worker_id,
            WorkerOutcomeKind::Succeeded,
            "the answer is 42",
            None,
            WorkerTerminalOwner::Worker,
        )
        .await
        .unwrap();

        assert_eq!(
            resolve_detached_worker(&logger, worker_id).await.unwrap(),
            WorkerRouteResult::Terminal {
                lifecycle: WorkerLifecycle::Succeeded,
                result: Some("the answer is 42".to_string()),
            }
        );
    }
}
