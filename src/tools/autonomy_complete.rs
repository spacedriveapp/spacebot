//! Terminal completion tool for autonomy channel runs.
//!
//! Every autonomy run must end with a call to this tool. The recorded summary
//! and actions become the run history surfaced to future runs — the channel's
//! primary continuity mechanism. Modeled on `memory_persistence_complete`:
//! the channel enforces the call before exit and retries when it is missing.

use crate::agent::autonomy::AutonomyRunHandle;
use crate::wakes::AutonomyAction;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct AutonomyCompleteTool {
    handle: AutonomyRunHandle,
}

impl AutonomyCompleteTool {
    pub fn new(handle: AutonomyRunHandle) -> Self {
        Self { handle }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("autonomy_complete failed: {0}")]
pub struct AutonomyCompleteError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AutonomyCompleteArgs {
    /// 2-5 line summary of what this run observed and did.
    pub summary: String,
    /// One entry per task this run enriched, created, or executed.
    #[serde(default)]
    pub actions: Vec<AutonomyActionInput>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AutonomyActionInput {
    /// "enriched", "created", or "executed".
    pub kind: String,
    /// Task number the action touched, when applicable.
    #[serde(default)]
    pub task_number: Option<i64>,
    /// One-line description of what was done.
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct AutonomyCompleteOutput {
    pub success: bool,
    pub recorded_actions: usize,
}

impl Tool for AutonomyCompleteTool {
    const NAME: &'static str = "autonomy_complete";

    type Error = AutonomyCompleteError;
    type Args = AutonomyCompleteArgs;
    type Output = AutonomyCompleteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/autonomy_complete").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "2-5 line summary of what this run observed and did"
                    },
                    "actions": {
                        "type": "array",
                        "description": "One entry per task this run enriched, created, or executed",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": {
                                    "type": "string",
                                    "enum": ["enriched", "created", "executed"],
                                    "description": "What was done with the task"
                                },
                                "task_number": {
                                    "type": "integer",
                                    "description": "Task number the action touched, when applicable"
                                },
                                "detail": {
                                    "type": "string",
                                    "description": "One-line description of what was done"
                                }
                            },
                            "required": ["kind", "detail"]
                        }
                    }
                },
                "required": ["summary"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let summary = args.summary.trim();
        if summary.len() < 10 {
            return Err(AutonomyCompleteError(
                "summary must be a short 2-5 line description of what the run did".to_string(),
            ));
        }

        let mut actions = Vec::with_capacity(args.actions.len());
        for action in &args.actions {
            let kind = action.kind.trim();
            if !AutonomyAction::kind_is_valid(kind) {
                return Err(AutonomyCompleteError(format!(
                    "invalid action kind '{kind}'; expected one of {:?}",
                    AutonomyAction::KINDS
                )));
            }
            let detail = action.detail.trim();
            if detail.is_empty() {
                return Err(AutonomyCompleteError(
                    "every action needs a non-empty detail".to_string(),
                ));
            }
            actions.push(AutonomyAction {
                kind: kind.to_string(),
                task_number: action.task_number,
                detail: detail.to_string(),
            });
        }

        let recorded = self
            .handle
            .store
            .complete_run(&self.handle.run_id, summary, &actions)
            .await
            .map_err(|error| AutonomyCompleteError(error.to_string()))?;
        if !recorded {
            // The driver already finished this run (timeout wrap-up racing
            // the tool call). The summary is lost but the contract is met.
            tracing::warn!(
                run_id = %self.handle.run_id,
                "autonomy_complete called after the run was already finished"
            );
        }
        self.handle.mark_completed();

        Ok(AutonomyCompleteOutput {
            success: true,
            recorded_actions: actions.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wakes::{AutonomyRunStatus, AutonomyRunStore};
    use std::sync::Arc;

    async fn store() -> AutonomyRunStore {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        AutonomyRunStore::new(pool)
    }

    #[tokio::test]
    async fn records_summary_and_actions() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");
        let handle = AutonomyRunHandle::new(run_id.clone(), Arc::new(store.clone()), 2);
        let tool = AutonomyCompleteTool::new(handle.clone());

        let output = tool
            .call(AutonomyCompleteArgs {
                summary: "Surveyed 3 pending tasks and enriched task #7 with findings.".to_string(),
                actions: vec![AutonomyActionInput {
                    kind: "enriched".to_string(),
                    task_number: Some(7),
                    detail: "added dependency analysis".to_string(),
                }],
            })
            .await
            .expect("call should succeed");

        assert!(output.success);
        assert_eq!(output.recorded_actions, 1);
        assert!(handle.completed());

        let recent = store.recent(1).await.expect("recent");
        assert_eq!(recent[0].status, AutonomyRunStatus::Completed);
        assert_eq!(recent[0].actions[0].task_number, Some(7));
    }

    #[tokio::test]
    async fn rejects_invalid_action_kind() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");
        let handle = AutonomyRunHandle::new(run_id, Arc::new(store), 2);
        let tool = AutonomyCompleteTool::new(handle.clone());

        let error = tool
            .call(AutonomyCompleteArgs {
                summary: "A perfectly reasonable run summary.".to_string(),
                actions: vec![AutonomyActionInput {
                    kind: "deleted".to_string(),
                    task_number: None,
                    detail: "nope".to_string(),
                }],
            })
            .await
            .expect_err("invalid kind must fail");

        assert!(error.to_string().contains("invalid action kind"));
        assert!(!handle.completed());
    }

    #[tokio::test]
    async fn rejects_trivial_summary() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");
        let handle = AutonomyRunHandle::new(run_id, Arc::new(store), 2);
        let tool = AutonomyCompleteTool::new(handle);

        let error = tool
            .call(AutonomyCompleteArgs {
                summary: "ok".to_string(),
                actions: Vec::new(),
            })
            .await
            .expect_err("trivial summary must fail");

        assert!(error.to_string().contains("summary"));
    }
}
