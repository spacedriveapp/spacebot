//! Send message tool for cross-channel messaging and DMs.

use crate::ChannelId;
use crate::conversation::ChannelStore;
use crate::conversation::history::ConversationLogger;
use crate::messaging::MessagingManager;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for sending messages to other channels or DMs.
///
/// Resolves targets by name or ID via the channel store, extracts the
/// platform-specific target from channel metadata, and delivers via
/// `MessagingManager::broadcast()`. Logs the sent message to the destination
/// channel's conversation history so it appears in future transcripts.
#[derive(Clone)]
pub struct SendMessageTool {
    messaging_manager: Arc<MessagingManager>,
    channel_store: ChannelStore,
    conversation_logger: ConversationLogger,
    agent_display_name: String,
}

impl std::fmt::Debug for SendMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendMessageTool").finish_non_exhaustive()
    }
}

impl SendMessageTool {
    pub fn new(
        messaging_manager: Arc<MessagingManager>,
        channel_store: ChannelStore,
        conversation_logger: ConversationLogger,
        agent_display_name: String,
    ) -> Self {
        Self {
            messaging_manager,
            channel_store,
            conversation_logger,
            agent_display_name,
        }
    }
}

/// Error type for send_message tool.
#[derive(Debug, thiserror::Error)]
#[error("SendMessage failed: {0}")]
pub struct SendMessageError(String);

/// Arguments for send_message tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessageArgs {
    /// The target channel name, channel ID, or user identifier.
    /// Use a channel name like "general" or a full channel ID.
    pub target: String,
    /// The message content to send.
    pub message: String,
}

/// Output from send_message tool.
#[derive(Debug, Serialize)]
pub struct SendMessageOutput {
    pub success: bool,
    pub target: String,
    pub platform: String,
}

impl Tool for SendMessageTool {
    const NAME: &'static str = "send_message_to_another_channel";

    type Error = SendMessageError;
    type Args = SendMessageArgs;
    type Output = SendMessageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let email_adapter_available = self.messaging_manager.has_adapter("email").await;

        let mut description =
            crate::prompts::text::get("tools/send_message_to_another_channel").to_string();
        let mut target_description = "The target channel name, channel ID, or user identifier. Use a channel name like 'general' or a full channel ID from the available channels list.".to_string();

        if email_adapter_available {
            description.push_str(
                " Email delivery is enabled: for intentional outbound email you may target `email:alice@example.com` (or bare `alice@example.com`).",
            );
            target_description.push_str(
                " With email enabled, explicit email targets are also allowed: `email:alice@example.com` or `alice@example.com`.",
            );
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": target_description
                    },
                    "message": {
                        "type": "string",
                        "description": "The message content to send."
                    }
                },
                "required": ["target", "message"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            target = %args.target,
            message_len = args.message.len(),
            "send_message_to_another_channel tool called"
        );

        if let Some(explicit_target) = parse_explicit_email_target(&args.target) {
            self.messaging_manager
                .broadcast(
                    &explicit_target.adapter,
                    &explicit_target.target,
                    crate::OutboundResponse::Text(args.message),
                )
                .await
                .map_err(|error| SendMessageError(format!("failed to send message: {error}")))?;

            tracing::info!(
                adapter = %explicit_target.adapter,
                broadcast_target = %explicit_target.target,
                "message sent via explicit target"
            );

            // Email targets don't have a channel to log to.
            return Ok(SendMessageOutput {
                success: true,
                target: explicit_target.target,
                platform: explicit_target.adapter,
            });
        }

        let channel = self
            .channel_store
            .find_by_name(&args.target)
            .await
            .map_err(|error| SendMessageError(format!("failed to search channels: {error}")))?
            .ok_or_else(|| {
                SendMessageError(format!(
                    "no channel found matching '{}'. Use a channel name/ID from the available channels list or an explicit email target like email:alice@example.com.",
                    args.target
                ))
            })?;

        let broadcast_target = crate::messaging::target::resolve_broadcast_target(&channel)
            .ok_or_else(|| {
                SendMessageError(format!(
                    "could not resolve platform target for channel '{}' (platform: {})",
                    channel.display_name.as_deref().unwrap_or(&channel.id),
                    channel.platform,
                ))
            })?;

        self.messaging_manager
            .broadcast(
                &broadcast_target.adapter,
                &broadcast_target.target,
                crate::OutboundResponse::Text(args.message.clone()),
            )
            .await
            .map_err(|error| SendMessageError(format!("failed to send message: {error}")))?;

        // Log the sent message to the destination channel's conversation history
        // so it appears in future transcripts and channel recall.
        let destination_channel_id: ChannelId = Arc::from(channel.id.as_str());
        self.conversation_logger.log_bot_message_with_name(
            &destination_channel_id,
            &args.message,
            Some(&self.agent_display_name),
        );

        tracing::info!(
            adapter = %broadcast_target.adapter,
            broadcast_target = %broadcast_target.target,
            channel_name = channel.display_name.as_deref().unwrap_or("unknown"),
            destination_channel_id = %channel.id,
            "message sent to channel and logged to destination history"
        );

        Ok(SendMessageOutput {
            success: true,
            target: channel.display_name.unwrap_or_else(|| channel.id.clone()),
            platform: broadcast_target.adapter,
        })
    }
}

fn parse_explicit_email_target(raw: &str) -> Option<crate::messaging::target::BroadcastTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(parsed) = crate::messaging::target::parse_delivery_target(trimmed) {
        return (parsed.adapter == "email").then_some(parsed);
    }

    if !trimmed.contains('@') {
        return None;
    }

    crate::messaging::target::parse_delivery_target(&format!("email:{trimmed}"))
}

#[cfg(test)]
mod tests {
    use super::parse_explicit_email_target;

    #[test]
    fn parses_prefixed_email_target() {
        let target = parse_explicit_email_target("email:alice@example.com").expect("email target");
        assert_eq!(target.adapter, "email");
        assert_eq!(target.target, "alice@example.com");
    }

    #[test]
    fn parses_bare_email_target() {
        let target = parse_explicit_email_target("alice@example.com").expect("email target");
        assert_eq!(target.adapter, "email");
        assert_eq!(target.target, "alice@example.com");
    }

    #[test]
    fn parses_display_name_email_target() {
        let target = parse_explicit_email_target("Alice <alice@example.com>").expect("email");
        assert_eq!(target.adapter, "email");
        assert_eq!(target.target, "alice@example.com");
    }

    #[test]
    fn ignores_non_email_prefixed_target() {
        assert!(parse_explicit_email_target("discord:123").is_none());
    }

    #[test]
    fn ignores_channel_name_target() {
        assert!(parse_explicit_email_target("general").is_none());
    }
}
