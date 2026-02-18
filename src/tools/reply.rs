//! Reply tool for sending messages to users (channel only).

use crate::conversation::ConversationLogger;
use crate::{ChannelId, OutboundResponse};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Tool for replying to users.
///
/// Holds a sender channel rather than a specific InboundMessage. The channel
/// process creates a response sender per conversation turn and the tool routes
/// replies through it. This is compatible with Rig's ToolServer which registers
/// tools once and shares them across calls.
#[derive(Debug, Clone)]
pub struct ReplyTool {
    response_tx: mpsc::Sender<OutboundResponse>,
    conversation_id: String,
    conversation_logger: ConversationLogger,
    channel_id: ChannelId,
}

impl ReplyTool {
    /// Create a new reply tool bound to a conversation's response channel.
    pub fn new(
        response_tx: mpsc::Sender<OutboundResponse>,
        conversation_id: impl Into<String>,
        conversation_logger: ConversationLogger,
        channel_id: ChannelId,
    ) -> Self {
        Self {
            response_tx,
            conversation_id: conversation_id.into(),
            conversation_logger,
            channel_id,
        }
    }
}

/// Error type for reply tool.
#[derive(Debug, thiserror::Error)]
#[error("Reply failed: {0}")]
pub struct ReplyError(String);

/// Arguments for reply tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplyArgs {
    /// The message content to send to the user.
    pub content: String,
    /// Optional: create a new thread with this name and reply inside it.
    /// When set, a public thread is created in the current channel and the
    /// reply is posted there. Thread names are capped at 100 characters.
    #[serde(default)]
    pub thread_name: Option<String>,
}

/// Output from reply tool.
#[derive(Debug, Serialize)]
pub struct ReplyOutput {
    pub success: bool,
    pub conversation_id: String,
    pub content: String,
}

impl Tool for ReplyTool {
    const NAME: &'static str = "reply";

    type Error = ReplyError;
    type Args = ReplyArgs;
    type Output = ReplyOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/reply").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The content to send to the user. Can be markdown formatted."
                    },
                    "thread_name": {
                        "type": "string",
                        "description": "If provided, creates a new public thread with this name and posts the reply inside it. Max 100 characters."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            conversation_id = %self.conversation_id,
            content_len = args.content.len(),
            thread_name = args.thread_name.as_deref(),
            "reply tool called"
        );

        self.conversation_logger
            .log_bot_message(&self.channel_id, &args.content);

        let response = match args.thread_name {
            Some(ref name) => {
                // Cap thread names at 100 characters (Discord limit)
                let thread_name = if name.len() > 100 {
                    name[..name.floor_char_boundary(100)].to_string()
                } else {
                    name.clone()
                };
                OutboundResponse::ThreadReply {
                    thread_name,
                    text: args.content.clone(),
                }
            }
            None => OutboundResponse::Text(args.content.clone()),
        };

        self.response_tx
            .send(response)
            .await
            .map_err(|e| ReplyError(format!("failed to send reply: {e}")))?;

        tracing::debug!(conversation_id = %self.conversation_id, "reply sent to outbound channel");

        Ok(ReplyOutput {
            success: true,
            conversation_id: self.conversation_id.clone(),
            content: args.content,
        })
    }
}
