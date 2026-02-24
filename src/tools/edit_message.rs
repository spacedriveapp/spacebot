//! Tool for editing a previously sent Telegram message.

use crate::conversation::ConversationLogger;
use crate::{ChannelId, OutboundResponse};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// Shared flag between EditMessageTool and the channel event loop.
pub type EditedFlag = Arc<AtomicBool>;

/// Create a new edited flag (defaults to false).
pub fn new_edited_flag() -> EditedFlag {
    Arc::new(AtomicBool::new(false))
}

/// Tool for editing a previously sent Telegram message.
///
/// This tool allows the agent to edit a message it previously sent by specifying
/// the message ID. Currently only supported on Telegram (uses editMessageText API).
/// On other platforms, calls are logged but have no effect.
#[derive(Debug, Clone)]
pub struct EditMessageTool {
    response_tx: mpsc::Sender<OutboundResponse>,
    conversation_id: String,
    conversation_logger: ConversationLogger,
    channel_id: ChannelId,
    edited_flag: EditedFlag,
}

impl EditMessageTool {
    pub fn new(
        response_tx: mpsc::Sender<OutboundResponse>,
        conversation_id: impl Into<String>,
        conversation_logger: ConversationLogger,
        channel_id: ChannelId,
        edited_flag: EditedFlag,
    ) -> Self {
        Self {
            response_tx,
            conversation_id: conversation_id.into(),
            conversation_logger,
            channel_id,
            edited_flag,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Edit message failed: {0}")]
pub struct EditMessageError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditMessageArgs {
    /// The message ID to edit. This is the Telegram message_id that was returned
    /// when the original message was sent. You can find this in conversation logs
    /// or by referencing a previous message.
    pub message_id: String,
    /// The new text content to replace the original message with.
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct EditMessageOutput {
    pub success: bool,
    pub conversation_id: String,
    pub message_id: String,
    pub content: String,
}

impl Tool for EditMessageTool {
    const NAME: &'static str = "edit_message";

    type Error = EditMessageError;
    type Args = EditMessageArgs;
    type Output = EditMessageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let parameters = serde_json::json!({
            "type": "object",
            "properties": {
                "message_id": {
                    "type": "string",
                    "description": "The Telegram message ID to edit. This is the numeric message_id from Telegram."
                },
                "content": {
                    "type": "string",
                    "description": "The new text content to replace the original message with. Supports markdown formatting."
                }
            },
            "required": ["message_id", "content"]
        });

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Edit a previously sent Telegram message by its message ID. Only works on Telegram; other platforms will log but not execute the edit.".to_string(),
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            conversation_id = %self.conversation_id,
            message_id = %args.message_id,
            content_len = args.content.len(),
            "edit_message tool called"
        );

        let response = OutboundResponse::EditMessage {
            message_id: args.message_id.clone(),
            text: args.content.clone(),
        };

        self.response_tx
            .send(response)
            .await
            .map_err(|e| EditMessageError(format!("failed to send edit: {e}")))?;

        // Mark the turn as handled so handle_agent_result skips any fallback
        self.edited_flag.store(true, Ordering::Relaxed);

        tracing::debug!(
            conversation_id = %self.conversation_id,
            message_id = %args.message_id,
            "edit sent to outbound channel"
        );

        Ok(EditMessageOutput {
            success: true,
            conversation_id: self.conversation_id.clone(),
            message_id: args.message_id,
            content: args.content,
        })
    }
}
