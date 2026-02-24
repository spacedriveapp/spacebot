//! Branch tool for forking context and thinking (channel only).

use crate::BranchId;
use crate::agent::channel::{ChannelState, spawn_branch_from_state};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for spawning branches.
#[derive(Debug, Clone)]
pub struct BranchTool {
    state: ChannelState,
}

impl BranchTool {
    /// Create a new branch tool with access to channel state.
    pub fn new(state: ChannelState) -> Self {
        Self { state }
    }
}

/// Error type for branch tool.
#[derive(Debug, thiserror::Error)]
#[error("Branch creation failed: {0}")]
pub struct BranchError(String);

/// Arguments for branch tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BranchArgs {
    /// Description of what the branch should think about or investigate.
    pub description: String,
}

/// Output from branch tool.
#[derive(Debug, Serialize)]
pub struct BranchOutput {
    /// The ID of the created branch.
    pub branch_id: BranchId,
    /// Whether the branch was spawned successfully.
    pub spawned: bool,
    /// Message about the branch status.
    pub message: String,
}

impl Tool for BranchTool {
    const NAME: &'static str = "branch";

    type Error = BranchError;
    type Args = BranchArgs;
    type Output = BranchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/branch").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "What the branch should investigate or think about. Be specific about what conclusion you want."
                    }
                },
                "required": ["description"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let readiness = self.state.deps.runtime_config.work_readiness();
        let branch_id = spawn_branch_from_state(&self.state, &args.description)
            .await
            .map_err(|e| BranchError(format!("{e}")))?;

        let readiness_note = if readiness.ready {
            String::new()
        } else {
            let reason = readiness
                .reason
                .map(|value| value.as_str())
                .unwrap_or("unknown");
            format!(
                " Readiness note: warmup is not fully ready ({reason}, state: {:?}); a warmup pass may already be running or was queued in the background.",
                readiness.warmup_state
            )
        };

        Ok(BranchOutput {
            branch_id,
            spawned: true,
            message: format!(
                "Branch {branch_id} spawned. It will investigate: {}.{}",
                args.description, readiness_note
            ),
        })
    }
}
