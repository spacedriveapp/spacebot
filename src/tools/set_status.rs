//! Set status tool for workers.

use crate::conversation::ProcessRunLogger;
use crate::{AgentId, ChannelId, ProcessEvent, WorkerId};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Tool for setting worker status.
#[derive(Clone)]
pub struct SetStatusTool {
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
    process_run_logger: ProcessRunLogger,
    interactive: bool,
    callback: crate::agent::process_control::WorkerCallbackContext,
    process_control_registry: std::sync::Arc<crate::agent::process_control::ProcessControlRegistry>,
    /// Tool secret pairs for scrubbing status text before it reaches the channel.
    tool_secret_pairs: Vec<(String, String)>,
}

impl std::fmt::Debug for SetStatusTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SetStatusTool")
            .field("worker_id", &self.worker_id)
            .finish_non_exhaustive()
    }
}

impl SetStatusTool {
    /// Create a new set status tool.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        event_tx: broadcast::Sender<ProcessEvent>,
        process_run_logger: ProcessRunLogger,
        interactive: bool,
        callback: crate::agent::process_control::WorkerCallbackContext,
        process_control_registry: std::sync::Arc<
            crate::agent::process_control::ProcessControlRegistry,
        >,
    ) -> Self {
        Self {
            agent_id,
            worker_id,
            channel_id,
            event_tx,
            process_run_logger,
            interactive,
            callback,
            process_control_registry,
            tool_secret_pairs: Vec::new(),
        }
    }

    /// Set tool secret pairs for output scrubbing.
    pub fn with_tool_secrets(mut self, pairs: Vec<(String, String)>) -> Self {
        self.tool_secret_pairs = pairs;
        self
    }
}

/// Error type for set status tool.
#[derive(Debug, thiserror::Error)]
#[error("Failed to set status: {0}")]
pub struct SetStatusError(String);

/// The kind of status update.
///
/// `progress` (default) reports intermediate progress. `outcome` signals that
/// the worker has reached a terminal result — the task is done (or failed in a
/// way the worker can describe). Workers **must** emit an `outcome` status
/// before finishing; the system will nudge them back to work if they try to
/// stop without one.
///
/// NOTE: The outcome gate only checks *whether* an outcome was signaled, not
/// *whether all task steps are actually complete*. Premature outcome signaling
/// (e.g. after 2 of 7 steps) is handled via prompt-level instructions, not
/// structural enforcement. See the worker prompt for the anti-premature-exit
/// language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// Intermediate progress update (default).
    #[default]
    Progress,
    /// Terminal outcome — the task is complete or has a definitive result.
    Outcome,
}

/// Arguments for set status tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetStatusArgs {
    /// The status message to report.
    pub status: String,
    /// The kind of status update: "progress" (default) for intermediate
    /// updates, "outcome" when the task has reached a terminal result.
    #[serde(default)]
    pub kind: StatusKind,
}

/// Output from set status tool.
#[derive(Debug, Serialize)]
pub struct SetStatusOutput {
    /// Whether the status was set successfully.
    pub success: bool,
    /// The worker ID.
    pub worker_id: WorkerId,
    /// The status that was set.
    pub status: String,
    /// Full outcome text when `kind` is `outcome`, uncapped so the worker's
    /// terminal result survives into the durable completion record. Absent
    /// for progress updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The kind of status that was set.
    pub kind: StatusKind,
}

impl Tool for SetStatusTool {
    const NAME: &'static str = "set_status";

    type Error = SetStatusError;
    type Args = SetStatusArgs;
    type Output = SetStatusOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/set_status").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "description": "A concise status message describing your current progress or final result (1-2 sentences)"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["progress", "outcome"],
                        "default": "progress",
                        "description": "Use \"progress\" for intermediate updates. Use \"outcome\" ONLY when ALL steps of the task have reached a terminal result (success or failure) and you are ready to finish. Do not signal outcome if there are remaining steps — premature outcome signaling causes the task to be incorrectly reported as complete."
                    }
                },
                "required": ["status"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Scrub tool secret values before the status leaves the worker.
        // Layer 1: exact-match redaction of known secrets from the store.
        // Layer 2: regex-based redaction of unknown secret patterns.
        // Scrubbing runs on the full text so the display cap below can't
        // truncate a secret out of exact-match range.
        let scrubbed = crate::secrets::scrub::scrub_secrets(&args.status, &self.tool_secret_pairs);
        let scrubbed = crate::secrets::scrub::scrub_leaks(&scrubbed);

        // Cap status length to prevent context bloat in the status block.
        // Status is rendered into every channel turn so it should stay short.
        let status = if scrubbed.len() > 256 {
            let end = scrubbed.floor_char_boundary(256);
            let boundary = scrubbed[..end].rfind(char::is_whitespace).unwrap_or(end);
            format!("{}...", &scrubbed[..boundary])
        } else {
            scrubbed.clone()
        };

        // An outcome status is the worker's terminal result, not just a
        // progress line: the full text rides in the tool output so the
        // completion path can deliver it, while the capped form feeds the
        // live status stream.
        let outcome = (args.kind == StatusKind::Outcome).then_some(scrubbed);

        let mutation = if args.kind == StatusKind::Outcome && !self.interactive {
            self.process_control_registry
                .claim_worker_outcome_status(
                    self.callback,
                    &self.process_run_logger,
                    status.clone(),
                )
                .await
                .map_err(|error| SetStatusError(error.to_string()))?
        } else {
            self.process_control_registry
                .update_worker_status(self.callback, status.clone())
                .await
        };
        if mutation != crate::agent::process_control::WorkerMutationResult::Applied {
            return Err(SetStatusError(format!(
                "worker registration rejected status update: {mutation:?}"
            )));
        }

        let event = ProcessEvent::WorkerStatus {
            agent_id: self.agent_id.clone(),
            worker_id: self.worker_id,
            worker_registration_id: self.callback.registration_id,
            channel_id: self.channel_id.clone(),
            status: status.clone(),
        };

        self.event_tx.send(event).ok();

        Ok(SetStatusOutput {
            success: true,
            worker_id: self.worker_id,
            status,
            outcome,
            kind: args.kind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SetStatusArgs, SetStatusTool, StatusKind};
    use crate::conversation::{
        ProcessRunLogger, WorkerLifecycle, WorkerOutcomeKind, WorkerTerminalOwner,
        WorkerTransitionResult,
    };
    use rig::tool::Tool as _;
    use std::sync::Arc;

    async fn setup(interactive: bool) -> (SetStatusTool, ProcessRunLogger, uuid::Uuid) {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        logger
            .log_worker_started(
                None,
                worker_id,
                "task",
                "builtin",
                &Arc::from("agent"),
                interactive,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(8);
        let registry = Arc::new(crate::agent::process_control::ProcessControlRegistry::new());
        let provenance = crate::agent::process_control::WorkerProvenance {
            origin_channel_id: None,
            origin_branch_id: None,
            task: "task".to_string(),
            task_id: None,
            autonomy_run_id: None,
            spawning_process: crate::ProcessId::Worker(worker_id),
        };
        let reservation = registry
            .reserve_worker_in_scope(worker_id, &provenance, Arc::from("test"), 1)
            .await
            .unwrap();
        let callback = reservation.callback_context();
        let operation = crate::agent::process_control::WorkerOperationContext {
            operation_id: crate::agent::process_control::WorkerOperationId::new(),
            requester: crate::agent::process_control::WorkerRequester::System,
            result_target: crate::agent::process_control::WorkerResultTarget::None,
            autonomy_run_id: None,
        };
        let control = crate::agent::process_control::WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
        )
        .0;
        registry
            .register_new_worker(
                reservation,
                provenance,
                crate::agent::process_control::WorkerBackend::Builtin,
                interactive,
                operation,
                "running",
                control,
            )
            .await
            .unwrap();
        assert_eq!(
            registry
                .update_worker_state(
                    callback,
                    crate::agent::process_control::WorkerRuntimeState::Running,
                )
                .await,
            crate::agent::process_control::WorkerMutationResult::Applied
        );
        (
            SetStatusTool::new(
                Arc::from("agent"),
                worker_id,
                None,
                event_tx,
                logger.clone(),
                interactive,
                callback,
                registry,
            ),
            logger,
            worker_id,
        )
    }

    #[tokio::test]
    async fn outcome_output_carries_full_text_while_status_stays_capped() {
        let (tool, _logger, _worker_id) = setup(false).await;
        let long = format!("Verified the deployment. {}", "detail ".repeat(60));
        let output = tool
            .call(SetStatusArgs {
                status: long.clone(),
                kind: StatusKind::Outcome,
            })
            .await
            .unwrap();
        assert_eq!(output.outcome.as_deref(), Some(long.as_str()));
        assert!(output.status.len() <= 260);
        assert!(output.status.ends_with("..."));
    }

    #[tokio::test]
    async fn progress_output_has_no_outcome_text() {
        let (tool, _logger, _worker_id) = setup(false).await;
        let output = tool
            .call(SetStatusArgs {
                status: "working on it".to_string(),
                kind: StatusKind::Progress,
            })
            .await
            .unwrap();
        assert!(output.outcome.is_none());
    }

    #[tokio::test]
    async fn non_interactive_outcome_claims_completing_once() {
        let (tool, logger, worker_id) = setup(false).await;
        let args = || SetStatusArgs {
            status: "finished".to_string(),
            kind: StatusKind::Outcome,
        };
        assert!(tool.call(args()).await.is_ok());
        assert!(tool.call(args()).await.is_err());
        assert_eq!(
            logger.read_worker_lifecycle(worker_id).await.unwrap(),
            Some(WorkerLifecycle::Completing)
        );
    }

    #[tokio::test]
    async fn outcome_claim_beats_concurrent_cancel_transition() {
        let (tool, logger, worker_id) = setup(false).await;
        tool.call(SetStatusArgs {
            status: "finished".to_string(),
            kind: StatusKind::Outcome,
        })
        .await
        .unwrap();
        assert_eq!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerLifecycle::Cancelling,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::Conflict {
                current: WorkerLifecycle::Completing,
            }
        );
        logger
            .complete_worker(
                worker_id,
                WorkerLifecycle::Completing,
                WorkerOutcomeKind::Succeeded,
                Some("finished"),
                "finished",
                None,
                0,
                WorkerTerminalOwner::Worker,
            )
            .await
            .unwrap();
        assert_eq!(
            logger
                .read_worker_terminal(worker_id)
                .await
                .unwrap()
                .unwrap()
                .outcome_kind,
            WorkerOutcomeKind::Succeeded
        );
    }

    #[tokio::test]
    async fn interactive_outcome_does_not_claim_terminal_lifecycle() {
        let (tool, logger, worker_id) = setup(true).await;
        tool.call(SetStatusArgs {
            status: "turn complete".to_string(),
            kind: StatusKind::Outcome,
        })
        .await
        .unwrap();
        assert_eq!(
            logger.read_worker_lifecycle(worker_id).await.unwrap(),
            Some(WorkerLifecycle::Running)
        );
    }

    #[tokio::test]
    async fn stale_set_status_callback_emits_no_event() {
        let (tool, _logger, _worker_id) = setup(true).await;
        let mut events = tool.event_tx.subscribe();
        assert!(
            tool.process_control_registry
                .remove_worker_if_registration_matches(tool.callback)
                .await
        );

        assert!(
            tool.call(SetStatusArgs {
                status: "stale".to_string(),
                kind: StatusKind::Progress,
            })
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), events.recv())
                .await
                .is_err()
        );
    }
}
