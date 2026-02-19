//! Reply tool for sending messages to users (channel only).

use crate::conversation::ConversationLogger;
use crate::{ChannelId, OutboundResponse};
use crate::tools::SkipFlag;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
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
    skip_flag: SkipFlag,
}

impl ReplyTool {
    /// Create a new reply tool bound to a conversation's response channel.
    pub fn new(
        response_tx: mpsc::Sender<OutboundResponse>,
        conversation_id: impl Into<String>,
        conversation_logger: ConversationLogger,
        channel_id: ChannelId,
        skip_flag: SkipFlag,
    ) -> Self {
        Self {
            response_tx,
            conversation_id: conversation_id.into(),
            conversation_logger,
            channel_id,
            skip_flag,
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

/// Convert @username mentions to platform-specific syntax using conversation metadata.
///
/// Scans recent conversation history to build a name→ID mapping, then replaces
/// @DisplayName with the platform's mention format (<@ID> for Discord/Slack,
/// @username for Telegram).
async fn convert_mentions(
    content: &str,
    channel_id: &ChannelId,
    conversation_logger: &ConversationLogger,
    source: &str,
) -> String {
    // Load recent conversation to extract user mappings
    let messages = match conversation_logger.load_recent(channel_id, 50).await {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load conversation for mention conversion");
            return content.to_string();
        }
    };

    // Build display_name → user_id mapping from metadata
    let mut name_to_id: HashMap<String, String> = HashMap::new();
    for msg in messages {
        if let (Some(name), Some(id), Some(meta_str)) = 
            (&msg.sender_name, &msg.sender_id, &msg.metadata) 
        {
            // Parse metadata JSON to get clean display name (without mention syntax)
            if let Ok(meta) = serde_json::from_str::<HashMap<String, serde_json::Value>>(meta_str) {
                if let Some(display_name) = meta.get("sender_display_name").and_then(|v| v.as_str()) {
                    // For Slack (from PR #43), sender_display_name includes mention: "Name (<@ID>)"
                    // Extract just the name part
                    let clean_name = display_name
                        .split(" (<@")
                        .next()
                        .unwrap_or(display_name);
                    name_to_id.insert(clean_name.to_string(), id.clone());
                }
            }
            // Fallback: use sender_name from DB directly
            name_to_id.insert(name.clone(), id.clone());
        }
    }

    if name_to_id.is_empty() {
        return content.to_string();
    }

    // Convert @Name patterns to platform-specific mentions
    let mut result = content.to_string();
    
    // Sort by name length (longest first) to avoid partial replacements
    // e.g., "Alice Smith" before "Alice"
    let mut names: Vec<_> = name_to_id.keys().cloned().collect();
    names.sort_by(|a, b| b.len().cmp(&a.len()));

    for name in names {
        // Skip empty names/IDs — they produce "@" → "<@>" which corrupts any
        // text that contains an @ sign.
        if name.is_empty() {
            continue;
        }
        if let Some(user_id) = name_to_id.get(&name) {
            if user_id.is_empty() {
                continue;
            }
            let mention_pattern = format!("@{}", name);
            let replacement = match source {
                "discord" | "slack" => format!("<@{}>", user_id),
                "telegram" => format!("@{}", name), // Telegram uses @username (already correct)
                _ => mention_pattern.clone(), // Unknown platform, leave as-is
            };

            // Only replace if not already in correct format
            // Avoid double-converting "<@123>" patterns
            if !result.contains(&format!("<@{}>", user_id)) {
                result = result.replace(&mention_pattern, &replacement);
            }
        }
    }

    result
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
        // If a reply was already sent this turn, drop duplicate calls silently.
        // Some models (e.g. Grok) call reply multiple times per turn, causing
        // message spam. Returning Ok keeps the LLM from getting confused by errors.
        if self.skip_flag.load(Ordering::Relaxed) {
            tracing::warn!(
                conversation_id = %self.conversation_id,
                "reply already sent this turn; dropping duplicate call"
            );
            return Ok(ReplyOutput {
                success: true,
                conversation_id: self.conversation_id.clone(),
                content: String::new(),
            });
        }

        tracing::info!(
            conversation_id = %self.conversation_id,
            content_len = args.content.len(),
            thread_name = args.thread_name.as_deref(),
            "reply tool called"
        );

        // Extract source from conversation_id (format: "platform:id")
        let source = self.conversation_id
            .split(':')
            .next()
            .unwrap_or("unknown");

        // Auto-convert @mentions to platform-specific syntax
        let converted_content = convert_mentions(
            &args.content,
            &self.channel_id,
            &self.conversation_logger,
            source,
        ).await;

        self.conversation_logger.log_bot_message(&self.channel_id, &converted_content);

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
                    text: converted_content.clone(),
                }
            }
            None => OutboundResponse::Text(converted_content.clone()),
        };

        self.response_tx
            .send(response)
            .await
            .map_err(|e| ReplyError(format!("failed to send reply: {e}")))?;

        // Mark the turn as handled so handle_agent_result skips the fallback send.
        self.skip_flag.store(true, Ordering::Relaxed);

        tracing::debug!(conversation_id = %self.conversation_id, "reply sent to outbound channel");

        Ok(ReplyOutput {
            success: true,
            conversation_id: self.conversation_id.clone(),
            content: converted_content,
        })
    }
}
