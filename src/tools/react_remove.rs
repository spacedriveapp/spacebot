//! React-remove tool for removing emoji reactions from messages (channel only).

use crate::{OutboundResponse, RoutedSender};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for removing a reaction from the triggering message.
#[derive(Debug, Clone)]
pub struct ReactRemoveTool {
    response_tx: RoutedSender,
}

impl ReactRemoveTool {
    pub fn new(response_tx: RoutedSender) -> Self {
        Self { response_tx }
    }
}

/// Error type for react_remove tool.
#[derive(Debug, thiserror::Error)]
#[error("React remove failed: {0}")]
pub struct ReactRemoveError(String);

/// Arguments for react_remove tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReactRemoveArgs {
    /// The emoji to remove. Use the same unicode emoji character that was originally reacted (e.g. "👍", "👀").
    pub emoji: String,
}

/// Output from react_remove tool.
#[derive(Debug, Serialize)]
pub struct ReactRemoveOutput {
    pub success: bool,
    pub emoji: String,
}

impl Tool for ReactRemoveTool {
    const NAME: &'static str = "react_remove";

    type Error = ReactRemoveError;
    type Args = ReactRemoveArgs;
    type Output = ReactRemoveOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/react_remove").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "emoji": {
                        "type": "string",
                        "description": "The unicode emoji character to remove (e.g. \"👍\", \"👀\")."
                    }
                },
                "required": ["emoji"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(emoji = %args.emoji, "react_remove tool called");

        self.response_tx
            .send(OutboundResponse::RemoveReaction(args.emoji.clone()))
            .await
            .map_err(|error| {
                ReactRemoveError(format!("failed to send remove reaction: {error}"))
            })?;

        Ok(ReactRemoveOutput {
            success: true,
            emoji: args.emoji,
        })
    }
}
