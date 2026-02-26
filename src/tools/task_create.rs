//! Task creation tool for branch processes.

use crate::tasks::{CreateTaskInput, TaskPriority, TaskStatus, TaskStore, TaskSubtask};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TaskCreateTool {
    task_store: Arc<TaskStore>,
    agent_id: String,
    created_by: String,
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
        }
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
    #[serde(default)]
    pub status: Option<String>,
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
                    },
                    "status": {
                        "type": "string",
                        "enum": crate::tasks::TaskStatus::ALL.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                        "description": "Optional initial status"
                    }
                },
                "required": ["title"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let priority = TaskPriority::parse(&args.priority)
            .ok_or_else(|| TaskCreateError(format!("invalid priority: {}", args.priority)))?;
        let status = match args.status.as_deref() {
            None => TaskStatus::Backlog,
            Some(value) => TaskStatus::parse(value)
                .ok_or_else(|| TaskCreateError(format!("invalid status: {value}")))?,
        };

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
                agent_id: self.agent_id.clone(),
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

        Ok(TaskCreateOutput {
            success: true,
            task_number: task.task_number,
            status: task.status.to_string(),
            message: format!("Created task #{}: {}", task.task_number, task.title),
        })
    }
}
