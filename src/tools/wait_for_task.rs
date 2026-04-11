//! Wait for task tool — blocks until a delegated task reaches a terminal state.
//!
//! Instead of polling `task_get` or `task_list` repeatedly, agents call this
//! tool once and it blocks internally until the task completes, fails, or
//! times out. This eliminates the 15+ polling calls observed in practice.

use crate::tasks::TaskStore;
use crate::AgentId;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Default timeout in seconds (10 minutes).
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Poll interval in seconds.
const POLL_INTERVAL_SECS: u64 = 5;

/// Tool that waits for a delegated task to reach a terminal state.
#[derive(Debug, Clone)]
pub struct WaitForTaskTool {
    task_store: Arc<TaskStore>,
    agent_id: AgentId,
}

impl WaitForTaskTool {
    pub fn new(task_store: Arc<TaskStore>, agent_id: AgentId) -> Self {
        Self {
            task_store,
            agent_id,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wait_for_task failed: {0}")]
pub struct WaitForTaskError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitForTaskArgs {
    /// Task number to wait for.
    pub task_number: i32,
    /// Maximum seconds to wait before returning. Defaults to 600 (10 minutes).
    pub timeout_secs: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct WaitForTaskOutput {
    pub success: bool,
    pub task: Option<crate::tasks::Task>,
    pub message: String,
}

impl Tool for WaitForTaskTool {
    const NAME: &'static str = "wait_for_task";

    type Error = WaitForTaskError;
    type Args = WaitForTaskArgs;
    type Output = WaitForTaskOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Wait for a delegated task to complete, fail, or timeout. Blocks internally until the task reaches a terminal state (done/failed/backlog) or the timeout is reached. Use this instead of repeatedly polling task_get or task_list. Default timeout is 600 seconds (10 minutes).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_number": { "type": "integer", "description": "Task number to wait for (#N)" },
                    "timeout_secs": { "type": "integer", "description": "Maximum seconds to wait (default: 600)" }
                },
                "required": ["task_number"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let task_number = i64::from(args.task_number);
        let timeout_secs = args
            .timeout_secs
            .map(|s| s.max(10) as u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let agent_id_str = self.agent_id.to_string();
        let mut elapsed: u64 = 0;

        loop {
            let task = self
                .task_store
                .get_by_number(task_number)
                .await
                .map_err(|e| WaitForTaskError(format!("{e}")))?;

            let Some(task) = task else {
                return Err(WaitForTaskError(format!(
                    "task #{} not found",
                    task_number
                )));
            };

            // Access control: same as task_get
            let is_owner = task.owner_agent_id == agent_id_str;
            let is_creator = task.created_by == agent_id_str
                || task.created_by == format!("agent:{agent_id_str}");
            let is_assignee = task.assigned_agent_id == agent_id_str;

            if !is_owner && !is_creator && !is_assignee {
                return Err(WaitForTaskError(format!(
                    "access denied: task #{} not accessible by agent '{agent_id_str}'",
                    task_number
                )));
            }

            // Terminal states: done, failed, backlog
            if matches!(
                task.status,
                crate::tasks::TaskStatus::Done | crate::tasks::TaskStatus::Backlog
            ) {
                return Ok(WaitForTaskOutput {
                    success: true,
                    task: Some(task),
                    message: format!("Task #{} completed with status: {}", task_number, task.status),
                });
            }

            // Check timeout
            if elapsed >= timeout_secs {
                return Ok(WaitForTaskOutput {
                    success: false,
                    task: Some(task),
                    message: format!(
                        "Task #{} still {} after {}s. Call wait_for_task again to continue waiting, or check manually with task_get.",
                        task_number, task.status, elapsed
                    ),
                });
            }

            // Wait before next poll
            tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
            elapsed += POLL_INTERVAL_SECS;
        }
    }
}
