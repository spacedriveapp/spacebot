//! Cancel tool for stopping workers or branches.

use crate::agent::channel::ChannelState;
use crate::{BranchId, WorkerId};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for canceling workers or branches.
#[derive(Debug, Clone)]
pub struct CancelTool {
    state: ChannelState,
}

impl CancelTool {
    /// Create a new cancel tool with access to channel state.
    pub fn new(state: ChannelState) -> Self {
        Self { state }
    }
}

/// Error type for cancel tool.
#[derive(Debug, thiserror::Error)]
#[error("Cancel failed: {0}")]
pub struct CancelError(String);

/// Arguments for cancel tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CancelArgs {
    /// The type of process to cancel.
    pub process_type: String,
    /// The ID of the process to cancel.
    pub process_id: String,
    /// Optional reason for cancellation.
    pub reason: Option<String>,
}

/// Output from cancel tool.
#[derive(Debug, Serialize)]
pub struct CancelOutput {
    /// Whether the cancellation was successful.
    pub cancelled: bool,
    /// The type of process that was cancelled.
    pub process_type: String,
    /// The process ID.
    pub process_id: String,
    /// Status message.
    pub message: String,
}

impl Tool for CancelTool {
    const NAME: &'static str = "cancel";

    type Error = CancelError;
    type Args = CancelArgs;
    type Output = CancelOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/cancel").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "process_type": {
                        "type": "string",
                        "enum": ["worker", "branch"],
                        "description": "Type of process to cancel"
                    },
                    "process_id": {
                        "type": "string",
                        "description": "The ID of the worker or branch to cancel (UUID format)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional reason for cancellation"
                    }
                },
                "required": ["process_type", "process_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.process_type.as_str() {
            "branch" => {
                let branch_id = args
                    .process_id
                    .parse::<BranchId>()
                    .map_err(|e| CancelError(format!("Invalid branch ID: {e}")))?;
                self.state
                    .cancel_branch(branch_id)
                    .await
                    .map_err(CancelError)?;
            }
            "worker" => {
                let worker_id = args
                    .process_id
                    .parse::<WorkerId>()
                    .map_err(|e| CancelError(format!("Invalid worker ID: {e}")))?;
                self.state
                    .cancel_worker(worker_id)
                    .await
                    .map_err(CancelError)?;
            }
            other => return Err(CancelError(format!("Unknown process type: {other}"))),
        }

        let message = if let Some(reason) = &args.reason {
            format!(
                "{} {} cancelled: {reason}",
                args.process_type, args.process_id
            )
        } else {
            format!("{} {} cancelled.", args.process_type, args.process_id)
        };

        Ok(CancelOutput {
            cancelled: true,
            process_type: args.process_type,
            process_id: args.process_id,
            message,
        })
    }
}
