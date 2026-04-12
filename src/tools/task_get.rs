//! Task retrieval tool for reading full task details.
//!
//! Allows agents to read the complete details of a specific task by number,
//! including description, metadata, status, and any output/findings. Access
//! is restricted to tasks owned by or created by the calling agent.

use crate::AgentId;
use crate::tasks::TaskStore;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool that retrieves full task details by task number.
///
/// Access is restricted to tasks where the caller's agent is the owner
/// or the task was created by the caller.
#[derive(Debug, Clone)]
pub struct TaskGetTool {
    task_store: Arc<TaskStore>,
    agent_id: AgentId,
}

impl TaskGetTool {
    /// Create a new `TaskGetTool` scoped to the given agent.
    pub fn new(task_store: Arc<TaskStore>, agent_id: AgentId) -> Self {
        Self {
            task_store,
            agent_id,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task_get failed: {0}")]
pub struct TaskGetError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskGetArgs {
    /// Task number to retrieve.
    pub task_number: i32,
}

#[derive(Debug, Serialize)]
pub struct TaskGetOutput {
    pub success: bool,
    pub task: Option<crate::tasks::Task>,
    pub message: String,
}

impl Tool for TaskGetTool {
    const NAME: &'static str = "task_get";

    type Error = TaskGetError;
    type Args = TaskGetArgs;
    type Output = TaskGetOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Read the full details of a specific task by task number. Use this to read the results/findings of delegated tasks after they complete. You can only read tasks that you created or that are assigned to your agent.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_number": { "type": "integer", "description": "Task number to retrieve (#N)" }
                },
                "required": ["task_number"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let task_number = i64::from(args.task_number);

        let task = self
            .task_store
            .get_by_number(task_number)
            .await
            .map_err(|error| TaskGetError(format!("{error}")))?;

        let Some(task) = task else {
            return Err(TaskGetError(format!("task #{} not found", task_number)));
        };

        // Access control: allow if the task is owned by this agent, was
        // created by a process belonging to this agent (branch, worker, etc.),
        // or is assigned to this agent.
        let agent_id_str = self.agent_id.to_string();
        let is_owner = task.owner_agent_id == agent_id_str;
        let is_creator =
            task.created_by == agent_id_str || task.created_by == format!("agent:{agent_id_str}");
        let is_assignee = task.assigned_agent_id == agent_id_str;

        if !is_owner && !is_creator && !is_assignee {
            return Err(TaskGetError(format!(
                "access denied: task #{} was created by '{}' and owned by '{}', not accessible by agent '{agent_id_str}'",
                task_number, task.created_by, task.owner_agent_id
            )));
        }

        Ok(TaskGetOutput {
            success: true,
            task: Some(task),
            message: format!("Retrieved task #{}", task_number),
        })
    }
}
