//! Task update tool for branch and worker processes.

use crate::tasks::{TaskPriority, TaskStatus, TaskStore, TaskSubtask, UpdateTaskInput};
use crate::{AgentId, WorkerId};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum TaskUpdateScope {
    Branch,
    Worker(WorkerId),
}

#[derive(Debug, Clone)]
pub struct TaskUpdateTool {
    task_store: Arc<TaskStore>,
    agent_id: AgentId,
    scope: TaskUpdateScope,
    cortex_ctx: Option<crate::tools::spawn_worker::CortexChatContext>,
}

impl TaskUpdateTool {
    pub fn for_branch(task_store: Arc<TaskStore>, agent_id: AgentId) -> Self {
        Self {
            task_store,
            agent_id,
            scope: TaskUpdateScope::Branch,
            cortex_ctx: None,
        }
    }

    pub fn for_worker(task_store: Arc<TaskStore>, agent_id: AgentId, worker_id: WorkerId) -> Self {
        Self {
            task_store,
            agent_id,
            scope: TaskUpdateScope::Worker(worker_id),
            cortex_ctx: None,
        }
    }

    pub fn for_cortex(
        task_store: Arc<TaskStore>,
        agent_id: AgentId,
        cortex_ctx: crate::tools::spawn_worker::CortexChatContext,
    ) -> Self {
        Self {
            task_store,
            agent_id,
            scope: TaskUpdateScope::Branch,
            cortex_ctx: Some(cortex_ctx),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task_update failed: {0}")]
pub struct TaskUpdateError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskUpdateArgs {
    pub task_number: i32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub subtasks: Option<Vec<TaskSubtask>>,
    pub metadata: Option<serde_json::Value>,
    pub complete_subtask: Option<i32>,
    pub worker_id: Option<String>,
    pub approved_by: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TaskUpdateOutput {
    pub success: bool,
    pub task_number: i64,
    pub status: String,
    pub message: String,
}

impl Tool for TaskUpdateTool {
    const NAME: &'static str = "task_update";

    type Error = TaskUpdateError;
    type Args = TaskUpdateArgs;
    type Output = TaskUpdateOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let is_worker = matches!(self.scope, TaskUpdateScope::Worker(_));

        // Workers only see subtask/metadata fields; branches/cortex see everything.
        let parameters = if is_worker {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_number": { "type": "integer", "description": "Task number reference (#N)" },
                    "subtasks": {
                        "type": "array",
                        "description": "Optional full replacement of subtask list",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "completed": { "type": "boolean" }
                            },
                            "required": ["title", "completed"]
                        }
                    },
                    "metadata": { "type": "object", "description": "Metadata object merged with current metadata" },
                    "complete_subtask": { "type": "integer", "description": "Subtask index to mark complete" }
                },
                "required": ["task_number"]
            })
        } else {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_number": { "type": "integer", "description": "Task number reference (#N)" },
                    "title": { "type": "string", "description": "Optional new title" },
                    "description": { "type": "string", "description": "Optional new description" },
                    "status": {
                        "type": "string",
                        "enum": crate::tasks::TaskStatus::ALL.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                        "description": "Optional new status"
                    },
                    "priority": {
                        "type": "string",
                        "enum": crate::tasks::TaskPriority::ALL.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                        "description": "Optional new priority"
                    },
                    "subtasks": {
                        "type": "array",
                        "description": "Optional full replacement of subtask list",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "completed": { "type": "boolean" }
                            },
                            "required": ["title", "completed"]
                        }
                    },
                    "metadata": { "type": "object", "description": "Metadata object merged with current metadata" },
                    "complete_subtask": { "type": "integer", "description": "Subtask index to mark complete" },
                    "worker_id": { "type": "string", "description": "Optional worker ID to bind to this task" },
                    "approved_by": { "type": "string", "description": "Optional approver identifier" }
                },
                "required": ["task_number"]
            })
        };

        let mut description = crate::prompts::text::get("tools/task_update").to_string();
        if self.cortex_ctx.is_some() {
            description.push_str(
                " In CorPilot, do not rewrite the core spec of an `in_progress` task in place; prefer adding context, steering execution, or changing status first.",
            );
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description,
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let task_number = i64::from(args.task_number);

        if let Some(cortex_ctx) = &self.cortex_ctx
            && let Some(current_task_number) = *cortex_ctx.current_task_number.read().await
        {
            if current_task_number != task_number {
                return Err(TaskUpdateError(format!(
                    "CorPilot can only update the currently scoped task (#{current_task_number})."
                )));
            }

            let current_task = self
                .task_store
                .get_by_number(&self.agent_id, task_number)
                .await
                .map_err(|error| TaskUpdateError(format!("{error}")))?;

            if let Some(task) = current_task {
                let is_rewriting_core_spec =
                    args.title.is_some() || args.description.is_some() || args.subtasks.is_some();

                if task.status == TaskStatus::InProgress && is_rewriting_core_spec {
                    return Err(TaskUpdateError(
                        "CorPilot cannot rewrite the core spec of an in-progress task in place. Add context, inspect/steer execution, update status, or move it out of in_progress before rewriting the title/description/subtasks.".to_string(),
                    ));
                }
            }
        }

        if let TaskUpdateScope::Worker(ref worker_id) = self.scope {
            let current = self
                .task_store
                .get_by_worker_id(&worker_id.to_string())
                .await
                .map_err(|error| TaskUpdateError(format!("{error}")))?;

            let Some(task) = current else {
                return Err(TaskUpdateError(
                    "worker is not assigned to a task".to_string(),
                ));
            };

            if task.task_number != task_number {
                return Err(TaskUpdateError(format!(
                    "worker {} can only update task #{}",
                    worker_id, task.task_number
                )));
            }

            // Workers can only update subtasks and metadata — not status, priority,
            // title, description, worker binding, or approval.
            if args.title.is_some()
                || args.description.is_some()
                || args.status.is_some()
                || args.priority.is_some()
                || args.worker_id.is_some()
                || args.approved_by.is_some()
            {
                return Err(TaskUpdateError(
                    "workers can only update subtasks and metadata".to_string(),
                ));
            }
        }

        let status = match args.status.as_deref() {
            None => None,
            Some(value) => Some(
                TaskStatus::parse(value)
                    .ok_or_else(|| TaskUpdateError(format!("invalid status: {value}")))?,
            ),
        };
        let priority = match args.priority.as_deref() {
            None => None,
            Some(value) => Some(
                TaskPriority::parse(value)
                    .ok_or_else(|| TaskUpdateError(format!("invalid priority: {value}")))?,
            ),
        };
        let complete_subtask = match args.complete_subtask {
            None => None,
            Some(value) => Some(
                usize::try_from(value)
                    .map_err(|_| TaskUpdateError(format!("invalid subtask index: {value}")))?,
            ),
        };

        let updated = self
            .task_store
            .update(
                &self.agent_id,
                task_number,
                UpdateTaskInput {
                    title: args.title,
                    description: args.description,
                    status,
                    priority,
                    subtasks: args.subtasks,
                    metadata: args.metadata,
                    worker_id: args.worker_id,
                    clear_worker_id: false,
                    approved_by: args.approved_by,
                    complete_subtask,
                },
            )
            .await
            .map_err(|error| TaskUpdateError(format!("{error}")))?
            .ok_or_else(|| TaskUpdateError(format!("task #{} not found", task_number)))?;

        Ok(TaskUpdateOutput {
            success: true,
            task_number: updated.task_number,
            status: updated.status.to_string(),
            message: format!("Updated task #{}", updated.task_number),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::CreateTaskInput;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    async fn setup_store() -> TaskStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

        sqlx::query(
            r#"
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                task_number INTEGER NOT NULL,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL DEFAULT 'backlog',
                priority TEXT NOT NULL DEFAULT 'medium',
                subtasks TEXT,
                metadata TEXT,
                source_memory_id TEXT,
                worker_id TEXT,
                created_by TEXT NOT NULL,
                approved_at TIMESTAMP,
                approved_by TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP,
                UNIQUE(agent_id, task_number)
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("tasks schema should be created");

        TaskStore::new(pool)
    }

    fn corpilot_context(task_number: i64) -> crate::tools::spawn_worker::CortexChatContext {
        crate::tools::spawn_worker::CortexChatContext {
            current_thread_id: Arc::new(RwLock::new(Some("corpilot:test".to_string()))),
            current_channel_context: Arc::new(RwLock::new(None)),
            current_task_number: Arc::new(RwLock::new(Some(task_number))),
            tracked_workers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn corpilot_blocks_core_rewrite_for_in_progress_task() {
        let store = Arc::new(setup_store().await);
        let agent_id: AgentId = Arc::from("agent-test");
        let created = store
            .create(CreateTaskInput {
                agent_id: agent_id.to_string(),
                title: "in-progress task".to_string(),
                description: Some("original".to_string()),
                status: TaskStatus::InProgress,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "cortex".to_string(),
            })
            .await
            .expect("task should be created");

        let tool = TaskUpdateTool::for_cortex(
            store.clone(),
            agent_id.clone(),
            corpilot_context(created.task_number),
        );

        let error = tool
            .call(TaskUpdateArgs {
                task_number: created.task_number as i32,
                title: Some("rewritten".to_string()),
                description: Some("rewritten description".to_string()),
                status: None,
                priority: None,
                subtasks: Some(vec![TaskSubtask {
                    title: "new subtask".to_string(),
                    completed: false,
                }]),
                metadata: None,
                complete_subtask: None,
                worker_id: None,
                approved_by: None,
            })
            .await
            .expect_err("CorPilot should block core rewrites for in-progress tasks");

        assert!(
            error
                .to_string()
                .contains("cannot rewrite the core spec of an in-progress task"),
            "unexpected error: {error}"
        );

        let updated = store
            .get_by_number(agent_id.as_ref(), created.task_number)
            .await
            .expect("task fetch should succeed")
            .expect("task should exist");
        assert_eq!(updated.status, TaskStatus::InProgress);
        assert_eq!(updated.title, "in-progress task");
        assert_eq!(updated.description.as_deref(), Some("original"));
        assert!(updated.subtasks.is_empty());
    }

    #[tokio::test]
    async fn corpilot_allows_status_only_update_for_in_progress_task() {
        let store = Arc::new(setup_store().await);
        let agent_id: AgentId = Arc::from("agent-test");
        let created = store
            .create(CreateTaskInput {
                agent_id: agent_id.to_string(),
                title: "in-progress task".to_string(),
                description: Some("original".to_string()),
                status: TaskStatus::InProgress,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "cortex".to_string(),
            })
            .await
            .expect("task should be created");

        let tool = TaskUpdateTool::for_cortex(
            store.clone(),
            agent_id.clone(),
            corpilot_context(created.task_number),
        );

        let output = tool
            .call(TaskUpdateArgs {
                task_number: created.task_number as i32,
                title: None,
                description: None,
                status: Some("ready".to_string()),
                priority: None,
                subtasks: None,
                metadata: None,
                complete_subtask: None,
                worker_id: None,
                approved_by: None,
            })
            .await
            .expect("status-only update should succeed");

        assert_eq!(output.status, "ready");

        let updated = store
            .get_by_number(agent_id.as_ref(), created.task_number)
            .await
            .expect("task fetch should succeed")
            .expect("task should exist");
        assert_eq!(updated.status, TaskStatus::Ready);
        assert_eq!(updated.title, "in-progress task");
        assert_eq!(updated.description.as_deref(), Some("original"));
    }
}
