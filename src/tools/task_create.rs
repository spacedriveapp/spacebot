//! Task creation tool for branch processes.

use crate::notifications::{NewNotification, NotificationKind, NotificationSeverity};
use crate::tasks::{CreateTaskInput, TaskPriority, TaskStatus, TaskStore, TaskSubtask};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct TaskCreateTool {
    task_store: Arc<TaskStore>,
    agent_id: String,
    created_by: String,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
    api_state: Option<Arc<crate::api::ApiState>>,
}

impl std::fmt::Debug for TaskCreateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCreateTool")
            .field("agent_id", &self.agent_id)
            .field("created_by", &self.created_by)
            .finish()
    }
}

impl TaskCreateTool {
    pub fn new(
        task_store: Arc<TaskStore>,
        agent_id: impl Into<String>,
        created_by: impl Into<String>,
    ) -> Self {
        Self {
            task_store,
            agent_id: agent_id.into(),
            created_by: created_by.into(),
            working_memory: None,
            api_state: None,
        }
    }

    pub fn with_working_memory(mut self, store: Arc<crate::memory::WorkingMemoryStore>) -> Self {
        self.working_memory = Some(store);
        self
    }

    pub fn with_api_state(mut self, state: Arc<crate::api::ApiState>) -> Self {
        self.api_state = Some(state);
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task_create failed: {0}")]
pub struct TaskCreateError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskCreateArgs {
    pub title: String,
    pub description: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default)]
    pub subtasks: Vec<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

fn default_priority() -> String {
    "medium".to_string()
}

#[derive(Debug, Serialize)]
pub struct TaskCreateOutput {
    pub success: bool,
    pub task_number: i64,
    pub status: String,
    pub message: String,
}

impl Tool for TaskCreateTool {
    const NAME: &'static str = "task_create";

    type Error = TaskCreateError;
    type Args = TaskCreateArgs;
    type Output = TaskCreateOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/task_create").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short task title" },
                    "description": { "type": "string", "description": "Optional detailed description" },
                    "priority": {
                        "type": "string",
                        "enum": crate::tasks::TaskPriority::ALL.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                        "description": "Task priority"
                    },
                    "subtasks": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional checklist items"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Optional metadata object"
                    }
                },
                "required": ["title"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let priority = TaskPriority::parse(&args.priority)
            .ok_or_else(|| TaskCreateError(format!("invalid priority: {}", args.priority)))?;
        let status = TaskStatus::PendingApproval;

        let subtasks = args
            .subtasks
            .into_iter()
            .map(|title| TaskSubtask {
                title,
                completed: false,
            })
            .collect::<Vec<_>>();

        let task = self
            .task_store
            .create(CreateTaskInput {
                owner_agent_id: self.agent_id.clone(),
                assigned_agent_id: self.agent_id.clone(),
                title: args.title,
                description: args.description,
                status,
                priority,
                subtasks,
                metadata: args.metadata.unwrap_or_else(|| serde_json::json!({})),
                source_memory_id: None,
                created_by: self.created_by.clone(),
            })
            .await
            .map_err(|error| TaskCreateError(format!("{error}")))?;

        // Emit SSE event + notification so the dashboard updates in real time.
        if let Some(api_state) = &self.api_state {
            api_state
                .event_tx
                .send(crate::api::ApiEvent::TaskUpdated {
                    agent_id: task.assigned_agent_id.clone(),
                    task_number: task.task_number,
                    status: task.status.to_string(),
                    action: "created".to_string(),
                })
                .ok();
            if task.status == TaskStatus::PendingApproval {
                api_state.emit_notification(NewNotification {
                    kind: NotificationKind::TaskApproval,
                    severity: NotificationSeverity::Info,
                    title: task.title.clone(),
                    body: task.description.clone(),
                    agent_id: Some(task.assigned_agent_id.clone()),
                    related_entity_type: Some("task".to_string()),
                    related_entity_id: Some(task.task_number.to_string()),
                    action_url: Some(format!("/tasks/{}", task.task_number)),
                    metadata: None,
                });
            }
        }

        if let Some(working_memory) = &self.working_memory {
            let (event_type, summary, importance) = if task.status == TaskStatus::Done {
                (
                    crate::memory::WorkingMemoryEventType::Outcome,
                    format!("Task #{} completed: {}", task.task_number, task.title),
                    0.7,
                )
            } else {
                (
                    crate::memory::WorkingMemoryEventType::TaskUpdate,
                    format!(
                        "Task created #{}: {} (status: {})",
                        task.task_number, task.title, task.status
                    ),
                    0.5,
                )
            };
            working_memory
                .emit(event_type, summary)
                .importance(importance)
                .record();
        }

        Ok(TaskCreateOutput {
            success: true,
            task_number: task.task_number,
            status: task.status.to_string(),
            message: format!("Created task #{}: {}", task.task_number, task.title),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::memory::working::WorkingMemoryEvent;
    use crate::memory::{WorkingMemoryEventType, WorkingMemoryStore};
    use crate::tasks::store::setup_test_store;
    use chrono_tz::Tz;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::time::Duration;

    async fn wait_for_single_event(store: &WorkingMemoryStore) -> WorkingMemoryEvent {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = store
                    .get_recent_events(10, 0.0)
                    .await
                    .expect("working memory query");
                if let Some(event) = events.into_iter().next() {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for working memory event")
    }

    #[tokio::test]
    async fn task_create_emits_outcome_for_done_tasks() {
        let task_store = Arc::new(setup_test_store().await);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite connect");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        let working_memory = WorkingMemoryStore::new(pool, Tz::UTC);

        let tool = TaskCreateTool::new(task_store, "agent-test", "branch")
            .with_working_memory(working_memory.clone());

        let output = tool
            .call(TaskCreateArgs {
                title: "Ship observation MVP".to_string(),
                description: Some("land the first packet".to_string()),
                priority: "medium".to_string(),
                subtasks: Vec::new(),
                metadata: None,
                status: Some("done".to_string()),
            })
            .await
            .expect("task create should succeed");

        assert_eq!(output.status, "done");

        let event = wait_for_single_event(&working_memory).await;
        assert_eq!(event.event_type, WorkingMemoryEventType::Outcome);
        assert_eq!(event.summary, "Task #1 completed: Ship observation MVP");
    }
}
