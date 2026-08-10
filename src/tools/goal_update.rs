//! Goal update tool for branch processes.

use crate::goals::{GoalStatus, GoalStore, UpdateGoalInput};
use crate::tasks::TaskPriority;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct GoalUpdateTool {
    goal_store: Arc<GoalStore>,
    /// Whether this registration may move a goal between statuses. Cleared for
    /// processes that run without a user present: closing or abandoning a goal
    /// is the user's decision, so the field is removed from the schema and
    /// rejected if it arrives anyway.
    allow_status_change: bool,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
    api_state: Option<Arc<crate::api::ApiState>>,
}

impl std::fmt::Debug for GoalUpdateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalUpdateTool").finish()
    }
}

impl GoalUpdateTool {
    pub fn new(goal_store: Arc<GoalStore>) -> Self {
        Self {
            goal_store,
            allow_status_change: true,
            working_memory: None,
            api_state: None,
        }
    }

    /// Restrict this registration to notes, priority, due date, and metadata.
    pub fn without_status_changes(mut self) -> Self {
        self.allow_status_change = false;
        self
    }

    pub fn with_working_memory(mut self, store: Arc<crate::memory::WorkingMemoryStore>) -> Self {
        self.working_memory = Some(store);
        self
    }

    /// Enables goal.updated wake emission across the agent registry.
    pub fn with_api_state(mut self, api_state: Arc<crate::api::ApiState>) -> Self {
        self.api_state = Some(api_state);
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("goal_update failed: {0}")]
pub struct GoalUpdateError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalUpdateArgs {
    pub id: String,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due_date: Option<String>,
    pub notes: Option<String>,
    pub metadata_patch: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct GoalUpdateOutput {
    pub success: bool,
    pub goal_id: String,
    pub status: String,
    pub message: String,
}

impl Tool for GoalUpdateTool {
    const NAME: &'static str = "goal_update";

    type Error = GoalUpdateError;
    type Args = GoalUpdateArgs;
    type Output = GoalUpdateOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = serde_json::json!({
                    "id": { "type": "string", "description": "Goal id" },
                    "priority": {
                        "type": "string",
                        "enum": TaskPriority::ALL.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                        "description": "Optional new priority"
                    },
                    "due_date": { "type": "string", "description": "New deadline as YYYY-MM-DD, or empty string to clear" },
                    "notes": { "type": "string", "description": "Replacement progress notes — the goal's current standing, not an append" },
                    "metadata_patch": { "type": "object", "description": "Metadata object deep-merged with current metadata" }
        });
        if self.allow_status_change {
            properties["status"] = serde_json::json!({
                "type": "string",
                "enum": GoalStatus::ALL.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "description": "Optional new status — only change on explicit user instruction",
            });
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/goal_update").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let status = match args.status.as_deref() {
            None => None,
            Some(value) => {
                if !self.allow_status_change {
                    return Err(GoalUpdateError(
                        "goal status is the user's call — record progress in notes instead"
                            .to_string(),
                    ));
                }
                Some(
                    GoalStatus::parse(value)
                        .ok_or_else(|| GoalUpdateError(format!("invalid status: {value}")))?,
                )
            }
        };
        let priority = match args.priority.as_deref() {
            None => None,
            Some(value) => Some(
                TaskPriority::parse(value)
                    .ok_or_else(|| GoalUpdateError(format!("invalid priority: {value}")))?,
            ),
        };

        let updated = self
            .goal_store
            .update(
                &args.id,
                UpdateGoalInput {
                    title: None,
                    description: None,
                    status,
                    priority,
                    due_date: args.due_date,
                    notes: args.notes,
                    metadata: args.metadata_patch,
                },
            )
            .await
            .map_err(|error| GoalUpdateError(format!("{error}")))?
            .ok_or_else(|| GoalUpdateError(format!("goal {} not found", args.id)))?;

        if let Some(working_memory) = &self.working_memory {
            working_memory
                .emit(
                    crate::memory::WorkingMemoryEventType::Decision,
                    format!(
                        "Goal updated: {} (status: {})",
                        updated.title, updated.status
                    ),
                )
                .importance(0.4)
                .record();
        }

        if let Some(api_state) = &self.api_state {
            crate::wakes::emit_to_all_agents(
                &api_state.wake_registry,
                crate::wakes::SystemEvent::GoalUpdated,
                &format!("goal:{}", updated.id),
                &serde_json::json!({
                    "goal_id": updated.id,
                    "title": updated.title,
                    "status": updated.status.to_string(),
                }),
            )
            .await;
        }

        Ok(GoalUpdateOutput {
            success: true,
            goal_id: updated.id,
            status: updated.status.to_string(),
            message: format!("Updated goal: {}", updated.title),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::goals::{CreateGoalInput, store::setup_test_store};

    async fn create_goal(store: &GoalStore, title: &str) -> crate::goals::Goal {
        store
            .create(CreateGoalInput {
                title: title.to_string(),
                description: None,
                priority: TaskPriority::Medium,
                due_date: None,
                metadata: serde_json::json!({}),
            })
            .await
            .expect("goal should be created")
    }

    #[tokio::test]
    async fn goal_update_replaces_notes() {
        let goal_store = Arc::new(setup_test_store().await);
        let goal = create_goal(&goal_store, "notes goal").await;
        let tool = GoalUpdateTool::new(goal_store.clone());

        tool.call(GoalUpdateArgs {
            id: goal.id.clone(),
            status: None,
            priority: None,
            due_date: None,
            notes: Some("Research in progress".to_string()),
            metadata_patch: None,
        })
        .await
        .expect("goal update should succeed");

        let updated = goal_store
            .get(&goal.id)
            .await
            .expect("get should succeed")
            .expect("goal should exist");
        assert_eq!(updated.notes.as_deref(), Some("Research in progress"));
    }

    #[tokio::test]
    async fn goal_update_rejects_invalid_transition() {
        let goal_store = Arc::new(setup_test_store().await);
        let goal = create_goal(&goal_store, "paused goal").await;
        let tool = GoalUpdateTool::new(goal_store);

        tool.call(GoalUpdateArgs {
            id: goal.id.clone(),
            status: Some("paused".to_string()),
            priority: None,
            due_date: None,
            notes: None,
            metadata_patch: None,
        })
        .await
        .expect("pause should succeed");

        let error = tool
            .call(GoalUpdateArgs {
                id: goal.id,
                status: Some("completed".to_string()),
                priority: None,
                due_date: None,
                notes: None,
                metadata_patch: None,
            })
            .await
            .expect_err("paused -> completed must fail");

        assert!(error.to_string().contains("invalid goal status transition"));
    }

    #[tokio::test]
    async fn goal_update_reports_missing_goal() {
        let goal_store = Arc::new(setup_test_store().await);
        let tool = GoalUpdateTool::new(goal_store);

        let error = tool
            .call(GoalUpdateArgs {
                id: "missing".to_string(),
                status: None,
                priority: None,
                due_date: None,
                notes: Some("anything".to_string()),
                metadata_patch: None,
            })
            .await
            .expect_err("missing goal must fail");

        assert!(error.to_string().contains("goal missing not found"));
    }
}
