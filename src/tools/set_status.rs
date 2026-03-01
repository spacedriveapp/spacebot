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

/// Arguments for set status tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetStatusArgs {
    /// The status message to report.
    pub status: String,
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
                        "description": "A concise status message describing your current progress (1-2 sentences)"
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
        let status = crate::secrets::scrub::scrub_secrets(&status, &self.tool_secret_pairs);

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
