//! Conclude link tool: gracefully ends a link conversation.
//!
//! When an agent determines the conversation objective is met, it calls this
//! tool with a summary. The channel checks the flag after the LLM turn and
//! routes the summary back to the originating channel as a system message.

use crate::OutboundResponse;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{RwLock, mpsc};

/// Shared flag between the ConcludeLinkTool and the channel event loop.
pub type ConcludeLinkFlag = Arc<AtomicBool>;

/// Shared storage for the conclusion summary.
pub type ConcludeLinkSummary = Arc<RwLock<Option<String>>>;

/// Create a new conclude flag + summary pair.
pub fn new_conclude_link() -> (ConcludeLinkFlag, ConcludeLinkSummary) {
    (
        Arc::new(AtomicBool::new(false)),
        Arc::new(RwLock::new(None)),
    )
}

/// Tool that signals the link conversation should end.
#[derive(Debug, Clone)]
pub struct ConcludeLinkTool {
    flag: ConcludeLinkFlag,
    summary: ConcludeLinkSummary,
    response_tx: mpsc::Sender<OutboundResponse>,
}

impl ConcludeLinkTool {
    pub fn new(
        flag: ConcludeLinkFlag,
        summary: ConcludeLinkSummary,
        response_tx: mpsc::Sender<OutboundResponse>,
    ) -> Self {
        Self {
            flag,
            summary,
            response_tx,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Conclude link failed: {0}")]
pub struct ConcludeLinkError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConcludeLinkArgs {
    /// Summary of the conversation outcomes, decisions, and any action items.
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct ConcludeLinkOutput {
    pub concluded: bool,
}

impl Tool for ConcludeLinkTool {
    const NAME: &'static str = "conclude_link";

    type Error = ConcludeLinkError;
    type Args = ConcludeLinkArgs;
    type Output = ConcludeLinkOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/conclude_link").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Summary of the conversation outcomes, decisions made, and any action items."
                    }
                },
                "required": ["summary"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.flag.store(true, Ordering::Relaxed);

        let summary_len = args.summary.len();
        *self.summary.write().await = Some(args.summary);

        let _ = self
            .response_tx
            .send(OutboundResponse::Status(crate::StatusUpdate::StopTyping))
            .await;

        tracing::info!(
            summary_len,
            "conclude_link tool called, ending link conversation"
        );

        Ok(ConcludeLinkOutput { concluded: true })
    }
}
