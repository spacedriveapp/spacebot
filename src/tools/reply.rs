//! Reply tool for sending messages to users (channel only).

use crate::conversation::ConversationLogger;
use crate::tools::SkipFlag;
use crate::{ChannelId, OutboundResponse};
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
    /// Optional: formatted cards (e.g. Discord embeds) to attach to the message.
    /// Great for structured reports, summaries, or visually distinct content.
    #[serde(default)]
    pub cards: Option<Vec<crate::Card>>,
    /// Optional: interactive elements (e.g. buttons, select menus) to attach.
    /// Button clicks will be sent back to you as an inbound InteractionEvent
    /// with the corresponding custom_id.
    #[serde(default)]
    pub interactive_elements: Option<Vec<crate::InteractiveElements>>,
    /// Optional: a poll to attach to the message.
    #[serde(default)]
    pub poll: Option<crate::Poll>,
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
            // Parse metadata JSON to get clean display name
            if let Ok(meta) = serde_json::from_str::<HashMap<String, serde_json::Value>>(meta_str) {
                if let Some(display_name) = meta.get("sender_display_name").and_then(|v| v.as_str())
                {
                    // Older rows may include mention syntax "Name (<@ID>)"; strip it.
                    let clean_name = display_name.split(" (<@").next().unwrap_or(display_name);
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
        if let Some(user_id) = name_to_id.get(&name) {
            let mention_pattern = format!("@{}", name);
            let replacement = match source {
                "discord" | "slack" => format!("<@{}>", user_id),
                "telegram" => format!("@{}", name), // Telegram uses @username (already correct)
                _ => mention_pattern.clone(),       // Unknown platform, leave as-is
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
        let parameters = serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The content to send to the user. Can be markdown formatted."
                },
                "thread_name": {
                    "type": "string",
                    "description": "If provided, creates a new public thread with this name and posts the reply inside it. Max 100 characters."
                },
                "cards": {
                    "type": "array",
                    "description": "Optional: formatted cards (e.g. Discord embeds) to attach. Great for structured reports, summaries, or visually distinct content. Max 10 cards.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "description": { "type": "string" },
                            "color": { "type": "integer", "description": "Decimal color code" },
                            "url": { "type": "string" },
                            "fields": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "name": { "type": "string" },
                                        "value": { "type": "string" },
                                        "inline": { "type": "boolean" }
                                    },
                                    "required": ["name", "value"]
                                }
                            },
                            "footer": { "type": "string" }
                        }
                    }
                },
                "interactive_elements": {
                    "type": "array",
                    "description": "Optional: interactive components to attach. Button clicks will be sent back to you as an inbound InteractionEvent with the corresponding custom_id. Max 5 elements (rows).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["buttons", "select"] },
                            "buttons": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "custom_id": { "type": "string", "description": "ID sent back to you when clicked" },
                                        "style": { "type": "string", "enum": ["primary", "secondary", "success", "danger", "link"] },
                                        "url": { "type": "string", "description": "Required if style is link" }
                                    },
                                    "required": ["label", "style"]
                                }
                            },
                            "select": {
                                "type": "object",
                                "properties": {
                                    "custom_id": { "type": "string" },
                                    "options": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": { "type": "string" },
                                                "value": { "type": "string" },
                                                "description": { "type": "string" },
                                                "emoji": { "type": "string" }
                                            },
                                            "required": ["label", "value"]
                                        }
                                    },
                                    "placeholder": { "type": "string" }
                                },
                                "required": ["custom_id", "options"]
                            }
                        }
                    }
                },
                "poll": {
                    "type": "object",
                    "description": "Optional: a poll to attach to the message.",
                    "properties": {
                        "question": { "type": "string" },
                        "answers": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "allow_multiselect": { "type": "boolean" },
                        "duration_hours": { "type": "integer", "description": "Defaults to 24 if omitted" }
                    },
                    "required": ["question", "answers"]
                }
            },
            "required": ["content"]
        });

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/reply").to_string(),
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            conversation_id = %self.conversation_id,
            content_len = args.content.len(),
            thread_name = args.thread_name.as_deref(),
            "reply tool called"
        );

        // Extract source from conversation_id (format: "platform:id")
        let source = self.conversation_id.split(':').next().unwrap_or("unknown");

        // Auto-convert @mentions to platform-specific syntax
        let converted_content = convert_mentions(
            &args.content,
            &self.channel_id,
            &self.conversation_logger,
            source,
        )
        .await;

        self.conversation_logger
            .log_bot_message(&self.channel_id, &converted_content);

        let response = if let Some(ref name) = args.thread_name {
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
        } else if args.cards.is_some() || args.interactive_elements.is_some() || args.poll.is_some()
        {
            OutboundResponse::RichMessage {
                text: converted_content.clone(),
                blocks: vec![], // No block generation for now; Slack adapters will fall back to text
                cards: args.cards.unwrap_or_default(),
                interactive_elements: args.interactive_elements.unwrap_or_default(),
                poll: args.poll,
            }
        } else {
            OutboundResponse::Text(converted_content.clone())
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
