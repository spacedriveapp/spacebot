//! Task update tool for branch and worker processes.

use crate::tasks::{
    Task, TaskPriority, TaskStatus, TaskStore, TaskSubtask, UpdateTaskInput, WorkerTaskUpdateResult,
};
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
    // Retained for future authorization checks on global task updates.
    #[allow(dead_code)]
    agent_id: AgentId,
    scope: TaskUpdateScope,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
}

impl TaskUpdateTool {
    pub fn for_branch(task_store: Arc<TaskStore>, agent_id: AgentId) -> Self {
        Self {
            task_store,
            agent_id,
            scope: TaskUpdateScope::Branch,
            working_memory: None,
        }
    }

    pub fn for_worker(task_store: Arc<TaskStore>, agent_id: AgentId, worker_id: WorkerId) -> Self {
        Self {
            task_store,
            agent_id,
            scope: TaskUpdateScope::Worker(worker_id),
            working_memory: None,
        }
    }

    pub fn with_working_memory(mut self, store: Arc<crate::memory::WorkingMemoryStore>) -> Self {
        self.working_memory = Some(store);
        self
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
                    "metadata": { "type": "object", "description": "Metadata object deep-merged with current metadata" },
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
                    "metadata": { "type": "object", "description": "Metadata object deep-merged with current metadata" },
                    "complete_subtask": { "type": "integer", "description": "Subtask index to mark complete" },
                    "worker_id": { "type": "string", "description": "Optional worker ID to bind to this task" },
                    "approved_by": { "type": "string", "description": "Optional approver identifier" }
                },
                "required": ["task_number"]
            })
        };

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/task_update").to_string(),
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let task_number = i64::from(args.task_number);
        if matches!(self.scope, TaskUpdateScope::Worker(_))
            && (args.title.is_some()
                || args.description.is_some()
                || args.status.is_some()
                || args.priority.is_some()
                || args.worker_id.is_some()
                || args.approved_by.is_some())
        {
            return Err(TaskUpdateError(
                "workers can only update subtasks and metadata".to_string(),
            ));
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

        let input = UpdateTaskInput {
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
            ..Default::default()
        };

        let update_result = match &self.scope {
            TaskUpdateScope::Branch => self
                .task_store
                .update_with_status_transition(task_number, input)
                .await
                .map_err(|error| TaskUpdateError(format!("{error}")))?
                .ok_or_else(|| TaskUpdateError(format!("task #{} not found", task_number)))?,
            TaskUpdateScope::Worker(worker_id) => match self
                .task_store
                .update_worker_task(&worker_id.to_string(), task_number, input)
                .await
                .map_err(|error| TaskUpdateError(format!("{error}")))?
            {
                WorkerTaskUpdateResult::Updated(result) => *result,
                WorkerTaskUpdateResult::NotAssigned => {
                    return Err(TaskUpdateError(
                        "worker is not assigned to a task".to_string(),
                    ));
                }
                WorkerTaskUpdateResult::WrongTask {
                    assigned_task_number,
                } => {
                    return Err(TaskUpdateError(format!(
                        "worker {} can only update task #{}",
                        worker_id, assigned_task_number
                    )));
                }
            },
        };
        let previous_task = update_result.previous_task;
        let previous_status = update_result.previous_status;
        let updated = update_result.task;

        if let Some(working_memory) = &self.working_memory {
            let transitioned_to_done =
                previous_status != TaskStatus::Done && updated.status == TaskStatus::Done;
            let (event_type, summary, importance) = if transitioned_to_done {
                (
                    crate::memory::WorkingMemoryEventType::Outcome,
                    format!("Task #{} completed", updated.task_number),
                    0.7,
                )
            } else {
                (
                    crate::memory::WorkingMemoryEventType::TaskUpdate,
                    task_update_memory_summary(&previous_task, &updated),
                    0.4,
                )
            };
            working_memory
                .emit(event_type, summary)
                .importance(importance)
                .record();
        }

        Ok(TaskUpdateOutput {
            success: true,
            task_number: updated.task_number,
            status: updated.status.to_string(),
            message: format!("Updated task #{}", updated.task_number),
        })
    }
}

fn task_update_memory_summary(previous: &Task, updated: &Task) -> String {
    let mut changes = Vec::new();

    if previous.status != updated.status {
        changes.push(format!("status {} -> {}", previous.status, updated.status));
    }
    if previous.priority != updated.priority {
        changes.push(format!(
            "priority {} -> {}",
            previous.priority, updated.priority
        ));
    }
    if previous.title != updated.title {
        changes.push("title".to_string());
    }
    if previous.description != updated.description {
        changes.push("description".to_string());
    }
    if previous.subtasks != updated.subtasks {
        changes.push("subtasks".to_string());
    }
    if previous.metadata != updated.metadata {
        changes.push("metadata".to_string());
    }
    if previous.worker_id != updated.worker_id {
        changes.push(match (&previous.worker_id, &updated.worker_id) {
            (None, Some(_)) => "worker assigned".to_string(),
            (Some(_), None) => "worker unassigned".to_string(),
            _ => "worker binding".to_string(),
        });
    }
    if previous.approved_by != updated.approved_by {
        changes.push("approval".to_string());
    }
    if previous.assigned_agent_id != updated.assigned_agent_id {
        changes.push("assignment".to_string());
    }

    if changes.is_empty() {
        return format!("Task #{} updated", updated.task_number);
    }

    if changes.len() == 1 && previous.status != updated.status {
        return format!(
            "Task #{} updated to {}",
            updated.task_number, updated.status
        );
    }

    format!(
        "Task #{} updated: {}",
        updated.task_number,
        changes.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::memory::working::WorkingMemoryEvent;
    use crate::memory::{WorkingMemoryEventType, WorkingMemoryStore};
    use crate::tasks::store::setup_test_store;
    use crate::tasks::{Task, TaskPriority, TaskStatus, TaskSubtask};
    use chrono_tz::Tz;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::time::Duration;

    async fn setup_working_memory() -> Arc<WorkingMemoryStore> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite connect");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        WorkingMemoryStore::new(pool, Tz::UTC)
    }

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

    fn task_fixture() -> Task {
        Task {
            id: "task-id".to_string(),
            task_number: 7,
            title: "Original title".to_string(),
            description: Some("Original description".to_string()),
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            owner_agent_id: "agent".to_string(),
            assigned_agent_id: "agent".to_string(),
            subtasks: Vec::new(),
            metadata: serde_json::json!({}),
            source_memory_id: None,
            worker_id: None,
            created_by: "branch".to_string(),
            approved_at: None,
            approved_by: None,
            created_at: "2026-04-19T00:00:00Z".to_string(),
            updated_at: "2026-04-19T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    #[test]
    fn task_update_memory_summary_preserves_status_update_wording() {
        let previous = task_fixture();
        let mut updated = previous.clone();
        updated.status = TaskStatus::Ready;

        assert_eq!(
            task_update_memory_summary(&previous, &updated),
            "Task #7 updated to ready"
        );
    }

    #[test]
    fn task_update_memory_summary_names_non_status_changes() {
        let previous = task_fixture();
        let mut updated = previous.clone();
        updated.title = "New title".to_string();
        updated.description = Some("New description".to_string());
        updated.priority = TaskPriority::High;
        updated.subtasks = vec![TaskSubtask {
            title: "Check output".to_string(),
            completed: true,
        }];
        updated.metadata = serde_json::json!({"source": "review"});
        updated.worker_id = Some("worker-1".to_string());
        updated.approved_by = Some("victor".to_string());

        assert_eq!(
            task_update_memory_summary(&previous, &updated),
            "Task #7 updated: priority medium -> high, title, description, subtasks, metadata, worker assigned, approval"
        );
    }

    #[test]
    fn task_update_memory_summary_handles_no_actual_delta() {
        let previous = task_fixture();

        assert_eq!(
            task_update_memory_summary(&previous, &previous),
            "Task #7 updated"
        );
    }

    #[tokio::test]
    async fn task_update_emits_outcome_for_done_status() {
        let task_store = Arc::new(setup_test_store().await);
        let working_memory = setup_working_memory().await;

        let created = task_store
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-test".to_string(),
                assigned_agent_id: "agent-test".to_string(),
                title: "Review PR 2".to_string(),
                description: None,
                status: TaskStatus::InProgress,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "branch".to_string(),
            })
            .await
            .expect("task should be created");

        let tool = TaskUpdateTool::for_branch(task_store, AgentId::from("agent-test"))
            .with_working_memory(working_memory.clone());

        let output = tool
            .call(TaskUpdateArgs {
                task_number: created.task_number as i32,
                title: None,
                description: None,
                status: Some("done".to_string()),
                priority: None,
                subtasks: None,
                metadata: None,
                complete_subtask: None,
                worker_id: None,
                approved_by: None,
            })
            .await
            .expect("task update should succeed");

        assert_eq!(output.status, "done");

        let event = wait_for_single_event(&working_memory).await;
        assert_eq!(event.event_type, WorkingMemoryEventType::Outcome);
        assert_eq!(
            event.summary,
            format!("Task #{} completed", created.task_number)
        );
    }

    #[tokio::test]
    async fn task_update_keeps_task_update_event_when_task_was_already_done() {
        let task_store = Arc::new(setup_test_store().await);
        let working_memory = setup_working_memory().await;

        let created = task_store
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-test".to_string(),
                assigned_agent_id: "agent-test".to_string(),
                title: "Review merged changes".to_string(),
                description: None,
                status: TaskStatus::Done,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "branch".to_string(),
            })
            .await
            .expect("task should be created");

        let tool = TaskUpdateTool::for_branch(task_store, AgentId::from("agent-test"))
            .with_working_memory(working_memory.clone());

        let output = tool
            .call(TaskUpdateArgs {
                task_number: created.task_number as i32,
                title: Some("Review merged changes carefully".to_string()),
                description: None,
                status: None,
                priority: None,
                subtasks: None,
                metadata: None,
                complete_subtask: None,
                worker_id: None,
                approved_by: None,
            })
            .await
            .expect("task update should succeed");

        assert_eq!(output.status, "done");

        let event = wait_for_single_event(&working_memory).await;
        assert_eq!(event.event_type, WorkingMemoryEventType::TaskUpdate);
        assert_eq!(
            event.summary,
            format!("Task #{} updated: title", created.task_number)
        );
    }

    #[tokio::test]
    async fn worker_scope_checks_assignment_before_global_task_lookup() {
        let task_store = Arc::new(setup_test_store().await);
        let assigned = task_store
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-test".to_string(),
                assigned_agent_id: "agent-test".to_string(),
                title: "Assigned task".to_string(),
                description: None,
                status: TaskStatus::InProgress,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "branch".to_string(),
            })
            .await
            .expect("assigned task should be created");
        let other = task_store
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: "agent-test".to_string(),
                assigned_agent_id: "agent-test".to_string(),
                title: "Other task".to_string(),
                description: None,
                status: TaskStatus::InProgress,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "branch".to_string(),
            })
            .await
            .expect("other task should be created");
        let worker_id = WorkerId::new_v4();
        task_store
            .update(
                assigned.task_number,
                crate::tasks::UpdateTaskInput {
                    worker_id: Some(worker_id.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("worker assignment should update");

        let tool = TaskUpdateTool::for_worker(task_store, AgentId::from("agent-test"), worker_id);

        let existing_foreign = tool
            .call(TaskUpdateArgs {
                task_number: other.task_number as i32,
                title: None,
                description: None,
                status: None,
                priority: None,
                subtasks: None,
                metadata: Some(serde_json::json!({"progress": "checked"})),
                complete_subtask: None,
                worker_id: None,
                approved_by: None,
            })
            .await
            .expect_err("foreign task should be rejected");
        let missing = tool
            .call(TaskUpdateArgs {
                task_number: 999,
                title: None,
                description: None,
                status: None,
                priority: None,
                subtasks: None,
                metadata: Some(serde_json::json!({"progress": "checked"})),
                complete_subtask: None,
                worker_id: None,
                approved_by: None,
            })
            .await
            .expect_err("missing task should be rejected the same way");

        assert_eq!(existing_foreign.to_string(), missing.to_string());
        assert_eq!(
            existing_foreign.to_string(),
            format!(
                "task_update failed: worker {} can only update task #{}",
                worker_id, assigned.task_number
            )
        );
    }
}
