//! Reply tool for sending messages to users (channel only).

use crate::api::ApiState;
use crate::conversation::ConversationLogger;
use crate::{ChannelId, OutboundResponse, RoutedSender};
use regex::Regex;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

static BROKEN_DISCORD_MENTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<{2,}@(!?)>\s*(\d{15,22})>").expect("hardcoded broken mention regex")
});

static DISCORD_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{15,22}").expect("hardcoded discord id regex"));

/// Shared flag between the ReplyTool and the channel event loop.
///
/// When the tool is called, this is set to `true`. The channel checks it
/// after the LLM turn to decide whether to suppress fallback text output.
pub type RepliedFlag = Arc<AtomicBool>;

/// Create a new replied flag (defaults to false).
pub fn new_replied_flag() -> RepliedFlag {
    Arc::new(AtomicBool::new(false))
}

/// Target for reply delivery.
#[derive(Debug, Clone)]
pub enum ReplyTarget {
    /// Live delivery via the messaging adapter.
    Live(Box<RoutedSender>),
}

/// Tool for replying to users.
///
/// Holds a reply target which is either a live sender channel or a buffered
/// accumulator for cron jobs. The channel process creates a response sender per
/// conversation turn and the tool routes replies through it. This is compatible
/// with Rig's ToolServer which registers tools once and shares them across calls.
#[derive(Clone)]
pub struct ReplyTool {
    target: ReplyTarget,
    conversation_id: String,
    conversation_logger: ConversationLogger,
    channel_id: ChannelId,
    replied_flag: RepliedFlag,
    agent_display_name: String,
    api_state: Option<Arc<ApiState>>,
}

impl std::fmt::Debug for ReplyTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyTool")
            .field("conversation_id", &self.conversation_id)
            .field("channel_id", &self.channel_id)
            .field("agent_display_name", &self.agent_display_name)
            .finish()
    }
}

impl ReplyTool {
    /// Create a new reply tool bound to a conversation's response channel.
    pub fn new(
        target: ReplyTarget,
        conversation_id: impl Into<String>,
        conversation_logger: ConversationLogger,
        channel_id: ChannelId,
        replied_flag: RepliedFlag,
        agent_display_name: impl Into<String>,
        api_state: Option<Arc<ApiState>>,
    ) -> Self {
        Self {
            target,
            conversation_id: conversation_id.into(),
            conversation_logger,
            channel_id,
            replied_flag,
            agent_display_name: agent_display_name.into(),
            api_state,
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
    let mut result = normalize_discord_mention_tokens(content, source);

    // Load recent conversation to extract user mappings
    let messages = match conversation_logger.load_recent(channel_id, 50).await {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load conversation for mention conversion");
            return result;
        }
    };

    // Build display_name → user_id mapping from metadata
    let mut name_to_id: HashMap<String, String> = HashMap::new();
    for msg in messages {
        if let (Some(name), Some(id), Some(meta_str)) =
            (&msg.sender_name, &msg.sender_id, &msg.metadata)
        {
            // Parse metadata JSON to get clean display name
            if let Ok(meta) = serde_json::from_str::<HashMap<String, serde_json::Value>>(meta_str)
                && let Some(display_name) = meta.get("sender_display_name").and_then(|v| v.as_str())
            {
                // Older rows may include mention syntax "Name (<@ID>)"; strip it.
                let clean_name = display_name.split(" (<@").next().unwrap_or(display_name);
                name_to_id.insert(clean_name.to_string(), id.clone());
            }
            // Fallback: use sender_name from DB directly
            name_to_id.insert(name.clone(), id.clone());
        }
    }

    if name_to_id.is_empty() {
        return result;
    }

    // Convert @Name patterns to platform-specific mentions
    // Sort by name length (longest first) to avoid partial replacements
    // e.g., "Alice Smith" before "Alice"
    let mut names: Vec<_> = name_to_id.keys().cloned().collect();
    names.sort_by_key(|a| std::cmp::Reverse(a.len()));

    for name in names {
        let name = name.trim();
        if name.is_empty() || name.contains('<') || name.contains('>') || name.contains('@') {
            continue;
        }

        if let Some(user_id) = name_to_id.get(name) {
            let mention_pattern = format!("@{}", name);
            let replacement = match source {
                "discord" => {
                    let Some(discord_id) = sanitize_discord_user_id(user_id) else {
                        continue;
                    };
                    format!("<@{}>", discord_id)
                }
                "slack" => format!("<@{}>", user_id),
                "telegram" => format!("@{}", name), // Telegram uses @username (already correct)
                _ => mention_pattern.clone(),       // Unknown platform, leave as-is
            };

            result = result.replace(&mention_pattern, &replacement);
        }
    }

    result
}

fn sanitize_discord_user_id(user_id: &str) -> Option<String> {
    let trimmed = user_id.trim();
    if trimmed.len() >= 15 && trimmed.len() <= 22 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }

    DISCORD_ID_REGEX
        .find(trimmed)
        .map(|m| m.as_str().to_string())
}

pub(crate) fn normalize_discord_mention_tokens(content: &str, source: &str) -> String {
    let _ = source;

    let mut normalized = content
        .replace("&lt;@!", "<@!")
        .replace("&lt;@", "<@")
        .replace("&gt;", ">")
        .replace("\\<@!", "<@!")
        .replace("\\<@", "<@")
        .replace("<<@!>", "<@!")
        .replace("<<@>", "<@");

    while normalized.contains("<<@") {
        normalized = normalized.replace("<<@", "<@");
    }

    normalized = normalized.replace("<@!>", "<@!").replace("<@>", "<@");

    normalized = BROKEN_DISCORD_MENTION_REGEX
        .replace_all(&normalized, "<@$1$2>")
        .into_owned();

    normalized
}

fn normalize_poll_payload(poll: crate::Poll) -> Option<crate::Poll> {
    let question = poll.question.trim().to_string();
    if question.is_empty() {
        return None;
    }

    let answers: Vec<String> = poll
        .answers
        .into_iter()
        .map(|answer| answer.trim().to_string())
        .filter(|answer| !answer.is_empty())
        .collect();

    if answers.len() < 2 {
        return None;
    }

    Some(crate::Poll {
        question,
        answers,
        allow_multiselect: poll.allow_multiselect,
        duration_hours: poll.duration_hours,
    })
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
                            "footer": {
                                "type": "object",
                                "properties": {
                                    "text": { "type": "string" },
                                    "icon_url": { "type": "string", "format": "uri" }
                                },
                                "required": ["text"]
                            },
                            "thumbnail": {
                                "type": "object",
                                "description": "Small image in the top-right corner of the embed.",
                                "properties": { "url": { "type": "string", "format": "uri" } },
                                "required": ["url"]
                            },
                            "image": {
                                "type": "object",
                                "description": "Large image at the bottom of the embed.",
                                "properties": { "url": { "type": "string", "format": "uri" } },
                                "required": ["url"]
                            },
                            "author": {
                                "type": "object",
                                "description": "Author bar at the top of the embed.",
                                "properties": {
                                    "name": { "type": "string" },
                                    "url": { "type": "string", "format": "uri" },
                                    "icon_url": { "type": "string", "format": "uri" }
                                },
                                "required": ["name"]
                            },
                            "timestamp": {
                                "type": "string",
                                "format": "date-time",
                                "description": "ISO 8601 timestamp (e.g. 2024-01-01T00:00:00Z) displayed in the footer area."
                            }
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

        let source = self.conversation_id.split(':').next().unwrap_or("unknown");
        let mut description = crate::prompts::text::get("tools/reply").to_string();
        if source == "email" {
            description.push_str(
                " In email conversations this sends an actual outbound email to the sender. Use only when an explicit reply is required; otherwise prefer branch + skip.",
            );
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description,
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

        if crate::tools::should_block_user_visible_text(&converted_content) {
            tracing::warn!(
                conversation_id = %self.conversation_id,
                "reply tool blocked structured or tool-like output"
            );
            return Err(ReplyError(
                "blocked reply content: looks like tool syntax or structured payload".into(),
            ));
        }

        let thread_name = args
            .thread_name
            .as_ref()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty());
        let poll = args.poll.and_then(normalize_poll_payload);

        if let Some(leak) = crate::secrets::scrub::scan_for_leaks(&converted_content) {
            tracing::error!(
                conversation_id = %self.conversation_id,
                leak_prefix = %&leak[..leak.len().min(8)],
                "reply tool blocked content matching secret pattern"
            );
            return Err(ReplyError(
                "blocked reply content: potential secret detected".into(),
            ));
        }

        let response = if let Some(name) = thread_name {
            // Cap thread names at 100 characters (Discord limit)
            let thread_name = if name.len() > 100 {
                name[..name.floor_char_boundary(100)].to_string()
            } else {
                name.to_string()
            };
            OutboundResponse::ThreadReply {
                thread_name,
                text: converted_content.clone(),
            }
        } else if args.cards.is_some() || args.interactive_elements.is_some() || poll.is_some() {
            OutboundResponse::RichMessage {
                text: converted_content.clone(),
                blocks: vec![],
                cards: args.cards.unwrap_or_default(),
                interactive_elements: args.interactive_elements.unwrap_or_default(),
                poll,
            }
        } else {
            OutboundResponse::Text(converted_content.clone())
        };

        // Branch on reply target: live delivery or buffered staging
        match &self.target {
            ReplyTarget::Live(sender) => {
                sender
                    .send(response)
                    .await
                    .map_err(|e| ReplyError(format!("failed to send reply: {e}")))?;

                // Drain accumulated channel tool calls and pack into message metadata.
                let tool_calls_json = if let Some(ref api_state) = self.api_state {
                    let calls = api_state.take_channel_tool_calls(&self.channel_id).await;
                    if calls.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&calls).ok()
                    }
                } else {
                    None
                };
                self.conversation_logger.log_bot_message_with_metadata(
                    &self.channel_id,
                    &converted_content,
                    Some(&self.agent_display_name),
                    tool_calls_json,
                );

                tracing::debug!(conversation_id = %self.conversation_id, "reply sent to outbound channel");
            }
        }

        // Mark the turn as handled so handle_agent_result skips the fallback send.
        self.replied_flag.store(true, Ordering::Relaxed);

        Ok(ReplyOutput {
            success: true,
            conversation_id: self.conversation_id.clone(),
            content: "Message sent.".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_discord_mention_tokens, normalize_poll_payload, sanitize_discord_user_id,
    };
    use crate::Poll;

    #[test]
    fn normalizes_broken_discord_mentions() {
        let input = "hello <<@>123> and <<@!>456>";
        let output = normalize_discord_mention_tokens(input, "discord");

        assert_eq!(output, "hello <@123> and <@!456>");
    }

    #[test]
    fn leaves_plain_text_unchanged() {
        let input = "hello team";
        let output = normalize_discord_mention_tokens(input, "slack");

        assert_eq!(output, input);
    }

    #[test]
    fn normalizes_repeated_prefix_and_html_encoded_tokens() {
        let input = "<<<@>190291964875374603> and &lt;@!234152400653385729&gt;";
        let output = normalize_discord_mention_tokens(input, "discord");

        assert_eq!(output, "<@190291964875374603> and <@!234152400653385729>");
    }

    #[test]
    fn sanitizes_discord_ids_with_prefix_noise() {
        let parsed = sanitize_discord_user_id(">234152400653385729").expect("should parse id");
        assert_eq!(parsed, "234152400653385729");
    }

    #[test]
    fn drops_poll_with_blank_question() {
        let poll = Poll {
            question: "   ".into(),
            answers: vec!["Yes".into(), "No".into()],
            allow_multiselect: false,
            duration_hours: 24,
        };

        assert!(normalize_poll_payload(poll).is_none());
    }

    #[test]
    fn drops_poll_with_fewer_than_two_non_empty_answers() {
        let poll = Poll {
            question: "Ship it?".into(),
            answers: vec!["Yes".into(), "   ".into()],
            allow_multiselect: false,
            duration_hours: 24,
        };

        assert!(normalize_poll_payload(poll).is_none());
    }

    #[test]
    fn trims_poll_question_and_answers() {
        let poll = Poll {
            question: "  Ship it?  ".into(),
            answers: vec!["  Yes ".into(), " No  ".into()],
            allow_multiselect: true,
            duration_hours: 12,
        };

        let normalized = normalize_poll_payload(poll).expect("poll should remain valid");

        assert_eq!(normalized.question, "Ship it?");
        assert_eq!(normalized.answers, vec!["Yes", "No"]);
        assert!(normalized.allow_multiselect);
        assert_eq!(normalized.duration_hours, 12);
    }
}
