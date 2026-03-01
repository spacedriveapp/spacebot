//! React tool for adding emoji reactions to messages (channel only).

use crate::conversation::ConversationLogger;
use crate::{ChannelId, OutboundResponse};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Tool for reacting to messages with emoji.
#[derive(Debug, Clone)]
pub struct ReactTool {
    response_tx: mpsc::Sender<OutboundResponse>,
    conversation_logger: ConversationLogger,
    channel_id: ChannelId,
}

impl ReactTool {
    pub fn new(
        response_tx: mpsc::Sender<OutboundResponse>,
        conversation_logger: ConversationLogger,
        channel_id: ChannelId,
    ) -> Self {
        Self {
            response_tx,
            conversation_logger,
            channel_id,
        }
    }
}

/// Error type for react tool.
#[derive(Debug, thiserror::Error)]
#[error("React failed: {0}")]
pub struct ReactError(String);

/// Arguments for react tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReactArgs {
    /// The emoji to react with. Use a unicode emoji character (e.g. "👍", "😂", "🔥").
    pub emoji: String,
}

/// Output from react tool.
#[derive(Debug, Serialize)]
pub struct ReactOutput {
    pub success: bool,
    pub emoji: String,
}

impl Tool for ReactTool {
    const NAME: &'static str = "react";

    type Error = ReactError;
    type Args = ReactArgs;
    type Output = ReactOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/react").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "emoji": {
                        "type": "string",
                        "description": "A single unicode emoji character (e.g. \"👍\", \"😂\", \"🔥\", \"👀\")."
                    }
                },
                "required": ["emoji"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(emoji = %args.emoji, "react tool called");

        self.conversation_logger
            .log_reaction(&self.channel_id, &args.emoji);

        self.response_tx
            .send(OutboundResponse::Reaction(args.emoji.clone()))
            .await
            .map_err(|error| ReactError(format!("failed to send reaction: {error}")))?;

        Ok(ReactOutput {
            success: true,
            emoji: args.emoji,
        })
    }
}
