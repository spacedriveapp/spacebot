//! Task listing tool for branch processes.

use crate::tasks::{TaskPriority, TaskStatus, TaskStore};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TaskListTool {
    task_store: Arc<TaskStore>,
    agent_id: String,
}

impl TaskListTool {
    pub fn new(task_store: Arc<TaskStore>, agent_id: impl Into<String>) -> Self {
        Self {
            task_store,
            agent_id: agent_id.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task_list failed: {0}")]
pub struct TaskListError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskListArgs {
    pub status: Option<String>,
    pub priority: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i32,
}

fn default_limit() -> i32 {
    20
}

#[derive(Debug, Serialize)]
pub struct TaskListOutput {
    pub success: bool,
    pub count: usize,
    pub tasks: Vec<crate::tasks::Task>,
}

impl Tool for TaskListTool {
    const NAME: &'static str = "task_list";

    type Error = TaskListError;
    type Args = TaskListArgs;
    type Output = TaskListOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/task_list").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": crate::tasks::TaskStatus::ALL.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                        "description": "Optional status filter"
                    },
                    "priority": {
                        "type": "string",
                        "enum": crate::tasks::TaskPriority::ALL.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                        "description": "Optional priority filter"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of tasks to return"
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let status = match args.status.as_deref() {
            None => None,
            Some(value) => Some(
                TaskStatus::parse(value)
                    .ok_or_else(|| TaskListError(format!("invalid status filter: {value}")))?,
            ),
        };
        let priority = match args.priority.as_deref() {
            None => None,
            Some(value) => Some(
                TaskPriority::parse(value)
                    .ok_or_else(|| TaskListError(format!("invalid priority filter: {value}")))?,
            ),
        };
        let limit = i64::from(args.limit).clamp(1, 500);
        let tasks = self
            .task_store
            .list(&self.agent_id, status, priority, limit)
            .await
            .map_err(|error| TaskListError(format!("{error}")))?;

        Ok(TaskListOutput {
            success: true,
            count: tasks.len(),
            tasks,
        })
    }
}
