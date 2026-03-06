//! Set status tool for workers.

use crate::{AgentId, ChannelId, ProcessEvent, WorkerId};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Tool for setting worker status.
#[derive(Debug, Clone)]
pub struct SetStatusTool {
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
    /// Tool secret pairs for scrubbing status text before it reaches the channel.
    tool_secret_pairs: Vec<(String, String)>,
}

impl SetStatusTool {
    /// Create a new set status tool.
    pub fn new(
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> Self {
        Self {
            agent_id,
            worker_id,
            channel_id,
            event_tx,
            tool_secret_pairs: Vec::new(),
        }
    }

    /// Set tool secret pairs for output scrubbing.
    pub fn with_tool_secrets(mut self, pairs: Vec<(String, String)>) -> Self {
        self.tool_secret_pairs = pairs;
        self
    }
}

/// Error type for set status tool.
#[derive(Debug, thiserror::Error)]
#[error("Failed to set status: {0}")]
pub struct SetStatusError(String);

/// The kind of status update.
///
/// `progress` (default) reports intermediate progress. `outcome` signals that
/// the worker has reached a terminal result — the task is done (or failed in a
/// way the worker can describe). Workers **must** emit an `outcome` status
/// before finishing; the system will nudge them back to work if they try to
/// stop without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    /// Intermediate progress update (default).
    #[default]
    Progress,
    /// Terminal outcome — the task is complete or has a definitive result.
    Outcome,
}

/// Arguments for set status tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetStatusArgs {
    /// The status message to report.
    pub status: String,
    /// The kind of status update: "progress" (default) for intermediate
    /// updates, "outcome" when the task has reached a terminal result.
    #[serde(default)]
    pub kind: StatusKind,
}

/// Output from set status tool.
#[derive(Debug, Serialize)]
pub struct SetStatusOutput {
    /// Whether the status was set successfully.
    pub success: bool,
    /// The worker ID.
    pub worker_id: WorkerId,
    /// The status that was set.
    pub status: String,
    /// The kind of status that was set.
    pub kind: StatusKind,
}

impl Tool for SetStatusTool {
    const NAME: &'static str = "set_status";

    type Error = SetStatusError;
    type Args = SetStatusArgs;
    type Output = SetStatusOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/set_status").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "description": "A concise status message describing your current progress or final result (1-2 sentences)"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["progress", "outcome"],
                        "default": "progress",
                        "description": "Use \"progress\" for intermediate updates. Use \"outcome\" when the task has reached a terminal result (success or failure) and you are ready to finish."
                    }
                },
                "required": ["status"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Cap status length to prevent context bloat in the status block.
        // Status is rendered into every channel turn so it should stay short.
        let status = if args.status.len() > 256 {
            let end = args.status.floor_char_boundary(256);
            let boundary = args.status[..end].rfind(char::is_whitespace).unwrap_or(end);
            format!("{}...", &args.status[..boundary])
        } else {
            args.status
        };

        // Scrub tool secret values before the status reaches the channel.
        // Layer 1: exact-match redaction of known secrets from the store.
        // Layer 2: regex-based redaction of unknown secret patterns.
        let status = crate::secrets::scrub::scrub_secrets(&status, &self.tool_secret_pairs);
        let status = crate::secrets::scrub::scrub_leaks(&status);

        let event = ProcessEvent::WorkerStatus {
            agent_id: self.agent_id.clone(),
            worker_id: self.worker_id,
            channel_id: self.channel_id.clone(),
            status: status.clone(),
        };

        let _ = self.event_tx.send(event);

        Ok(SetStatusOutput {
            success: true,
            worker_id: self.worker_id,
            status,
            kind: args.kind,
        })
    }
}

/// Legacy function for setting worker status.
pub fn set_status(
    agent_id: AgentId,
    worker_id: WorkerId,
    status: impl Into<String>,
    event_tx: &broadcast::Sender<ProcessEvent>,
) {
    let event = ProcessEvent::WorkerStatus {
        agent_id,
        worker_id,
        channel_id: None,
        status: status.into(),
    };

    let _ = event_tx.send(event);
}
