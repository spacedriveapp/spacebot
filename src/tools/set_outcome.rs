//! SetOutcome tool: stores the final delivery outcome for a cron job.
//!
//! The cron channel works like a normal channel — `reply()` posts messages
//! visibly in the channel UI. When the LLM is ready to deliver the final
//! result to the configured target (e.g. Telegram), it calls `set_outcome()`
//! with the polished content. The scheduler reads this after channel exit.

use crate::cron::CronOutcome;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for setting the cron job's delivery outcome.
#[derive(Debug, Clone)]
pub struct SetOutcomeTool {
    outcome: CronOutcome,
    conversation_id: String,
}

impl SetOutcomeTool {
    pub fn new(outcome: CronOutcome, conversation_id: impl Into<String>) -> Self {
        Self {
            outcome,
            conversation_id: conversation_id.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("SetOutcome failed: {0}")]
pub struct SetOutcomeError(String);

/// Arguments for the set_outcome tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetOutcomeArgs {
    /// The final synthesized content to deliver to the cron job's target.
    pub content: String,
}

/// Output from the set_outcome tool.
#[derive(Debug, Serialize)]
pub struct SetOutcomeOutput {
    pub success: bool,
}

impl Tool for SetOutcomeTool {
    const NAME: &'static str = "set_outcome";

    type Error = SetOutcomeError;
    type Args = SetOutcomeArgs;
    type Output = SetOutcomeOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/set_outcome").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The final synthesized content to deliver to the cron job's target channel when the run completes."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let content = args.content.trim().to_string();

        if content.is_empty() {
            return Err(SetOutcomeError("content cannot be empty".into()));
        }

        if let Some(leak) = crate::secrets::scrub::scan_for_leaks(&content) {
            tracing::error!(
                conversation_id = %self.conversation_id,
                leak_prefix = %&leak[..leak.len().min(8)],
                "set_outcome blocked content matching secret pattern"
            );
            return Err(SetOutcomeError("blocked: potential secret detected".into()));
        }

        self.outcome.set(content);

        tracing::info!(
            conversation_id = %self.conversation_id,
            "cron outcome set, will be delivered when run completes"
        );

        Ok(SetOutcomeOutput { success: true })
    }
}
