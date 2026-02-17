//! Skip tool: allows the channel to produce no response.
//!
//! When the LLM decides the conversation doesn't warrant a reply (lurking in
//! a group chat, message wasn't directed at the bot, etc.), it calls this tool
//! instead of `reply`. The channel checks the skip flag after the LLM turn and
//! suppresses any fallback text output.

use crate::OutboundResponse;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// Shared flag between the SkipTool and the channel event loop.
///
/// When the tool is called, this is set to `true`. The channel checks it
/// after the LLM turn to decide whether to suppress fallback text output.
pub type SkipFlag = Arc<AtomicBool>;

/// Create a new skip flag (defaults to false).
pub fn new_skip_flag() -> SkipFlag {
    Arc::new(AtomicBool::new(false))
}

/// Tool that signals the channel should produce no response.
#[derive(Debug, Clone)]
pub struct SkipTool {
    flag: SkipFlag,
    response_tx: mpsc::Sender<OutboundResponse>,
}

impl SkipTool {
    pub fn new(flag: SkipFlag, response_tx: mpsc::Sender<OutboundResponse>) -> Self {
        Self { flag, response_tx }
    }
}

/// Error type for skip tool.
#[derive(Debug, thiserror::Error)]
#[error("Skip failed: {0}")]
pub struct SkipError(String);

/// Arguments for skip tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SkipArgs {
    /// Brief reason for not responding (logged but not sent to the user).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Output from skip tool.
#[derive(Debug, Serialize)]
pub struct SkipOutput {
    pub skipped: bool,
}

impl Tool for SkipTool {
    const NAME: &'static str = "skip";

    type Error = SkipError;
    type Args = SkipArgs;
    type Output = SkipOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/skip").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Brief internal reason for skipping (logged, not sent to the user)."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.flag.store(true, Ordering::Relaxed);

        // Cancel the typing indicator so it doesn't linger
        let _ = self
            .response_tx
            .send(OutboundResponse::Status(crate::StatusUpdate::StopTyping))
            .await;

        let reason = args.reason.as_deref().unwrap_or("no reason given");
        tracing::info!(reason, "skip tool called, suppressing response");

        Ok(SkipOutput { skipped: true })
    }
}
