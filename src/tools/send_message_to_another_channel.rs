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

/// Check if a string is a valid UUID format.
/// Accepts standard UUID format: 8-4-4-4-12 hexadecimal digits.
fn is_valid_uuid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok()
}

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
    current_adapter: Option<String>,
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
        current_adapter: Option<String>,
    ) -> Self {
        Self {
            messaging_manager,
            channel_store,
            conversation_logger,
            agent_display_name,
            current_adapter,
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
        // Check if current adapter is Signal (e.g., "signal:gvoice1" starts with "signal")
        let signal_adapter_available = self
            .current_adapter
            .as_ref()
            .map(|adapter| adapter.starts_with("signal"))
            .unwrap_or(false);

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

        if signal_adapter_available {
            description.push_str(
                " Signal messaging is enabled: you can target `signal:uuid:{uuid}`, `signal:group:{group_id}`, or `signal:+{phone}`.",
            );
            target_description.push_str(
                " With Signal enabled, explicit targets are also allowed: `signal:uuid:{uuid}`, `signal:group:{group_id}`, `signal:+{phone}`",
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

        // Check for explicit signal: prefix first - always honored regardless of current adapter.
        // This allows users to explicitly target Signal even when in Discord/Telegram/etc.
        if let Some(mut target) = parse_explicit_signal_prefix(&args.target) {
            // If explicit prefix returned default "signal" adapter but we're in a named
            // Signal adapter conversation (e.g., signal:gvoice1), use the current adapter
            // to ensure the message goes through the correct account.
            if target.adapter == "signal"
                && let Some(current_adapter) = self
                    .current_adapter
                    .as_ref()
                    .filter(|adapter| adapter.starts_with("signal:"))
            {
                target.adapter = current_adapter.clone();
            }

            self.messaging_manager
                .broadcast(
                    &target.adapter,
                    &target.target,
                    crate::OutboundResponse::Text(args.message),
                )
                .await
                .map_err(|error| SendMessageError(format!("failed to send message: {error}")))?;

            tracing::info!(
                adapter = %target.adapter,
                broadcast_target = %"[REDACTED]",
                "message sent via explicit signal: prefix"
            );

            return Ok(SendMessageOutput {
                success: true,
                target: target.target,
                platform: target.adapter,
            });
        }

        // Check for implicit Signal shorthands, but only when in a Signal conversation.
        // This prevents bare UUIDs, group:..., or +phone from being hijacked as Signal targets
        // when the user is actually in a Discord/Telegram/etc conversation.
        if let Some(current_adapter) = self
            .current_adapter
            .as_ref()
            .filter(|adapter| adapter.starts_with("signal"))
            && let Some(target) = parse_implicit_signal_shorthand(&args.target, current_adapter)
        {
            self.messaging_manager
                .broadcast(
                    &target.adapter,
                    &target.target,
                    crate::OutboundResponse::Text(args.message),
                )
                .await
                .map_err(|error| SendMessageError(format!("failed to send message: {error}")))?;

            tracing::info!(
                adapter = %target.adapter,
                broadcast_target = %"[REDACTED]",
                "message sent via implicit Signal shorthand"
            );

            return Ok(SendMessageOutput {
                success: true,
                target: target.target,
                platform: target.adapter,
            });
        }

        // Check for explicit email target
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

        // Try to find channel by name first
        let channel_result = self
            .channel_store
            .find_by_name(&args.target)
            .await
            .map_err(|error| SendMessageError(format!("failed to search channels: {error}")))?;

        // If channel not found, return error.
        // Signal targets are handled earlier (explicit signal: prefix always,
        // implicit shorthands only in Signal conversations).
        let channel = match channel_result {
            Some(ch) => ch,
            None => {
                return Err(SendMessageError(format!(
                    "no channel found matching '{}'. Use a channel name/ID from the available channels list, an explicit email target like email:alice@example.com, or signal: prefix for Signal targets.",
                    args.target
                )));
            }
        };

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

/// Parse explicit signal: prefix - always honored regardless of current adapter.
/// Returns BroadcastTarget with adapter="signal" for targets like:
/// - signal:uuid:xxx
/// - signal:group:xxx
/// - signal:+1234567890
/// - signal:e164:...
fn parse_explicit_signal_prefix(raw: &str) -> Option<crate::messaging::target::BroadcastTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Only handle explicit signal: prefix
    if let Some(rest) = trimmed.strip_prefix("signal:") {
        let parts: Vec<&str> = rest.split(':').collect();
        // Use shared parser for Signal target components
        if let Some(target) = crate::messaging::target::parse_signal_target_parts(&parts) {
            return Some(target);
        }
        // Fallback: try parsing the full signal:... string
        return crate::messaging::target::parse_delivery_target(trimmed);
    }

    None
}

/// Parse implicit Signal shorthands - only in Signal conversations.
/// Handles bare UUIDs, group:xxx, and +phone without explicit signal: prefix.
/// Returns BroadcastTarget directly instead of building strings to avoid
/// parse_delivery_target() issues with colons in named adapters.
fn parse_implicit_signal_shorthand(
    raw: &str,
    current_adapter: &str,
) -> Option<crate::messaging::target::BroadcastTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    use crate::messaging::target::BroadcastTarget;

    // Check for bare UUID format (strict validation)
    if is_valid_uuid(trimmed) {
        return Some(BroadcastTarget {
            adapter: current_adapter.to_string(),
            target: format!("uuid:{trimmed}"),
        });
    }

    // Phone number format: starts with + followed by 7+ digits
    if trimmed.starts_with('+')
        && trimmed[1..].len() >= 7
        && trimmed[1..].chars().all(|c| c.is_ascii_digit())
    {
        return Some(BroadcastTarget {
            adapter: current_adapter.to_string(),
            target: trimmed.to_string(),
        });
    }

    // Group ID format: group:xxx
    if trimmed.starts_with("group:") {
        return Some(BroadcastTarget {
            adapter: current_adapter.to_string(),
            target: trimmed.to_string(),
        });
    }

    None
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
    use super::{
        parse_explicit_email_target, parse_explicit_signal_prefix, parse_implicit_signal_shorthand,
    };

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

    // Signal tests - explicit signal: prefix (always honored regardless of adapter)
    #[test]
    fn parses_explicit_signal_uuid_prefixed() {
        let target =
            parse_explicit_signal_prefix("signal:uuid:123e4567-e89b-12d3-a456-426655440000")
                .expect("signal target");
        assert_eq!(target.adapter, "signal");
        assert_eq!(target.target, "uuid:123e4567-e89b-12d3-a456-426655440000");
    }

    #[test]
    fn parses_explicit_signal_group_prefixed() {
        let target = parse_explicit_signal_prefix("signal:group:grp123").expect("signal target");
        assert_eq!(target.adapter, "signal");
        assert_eq!(target.target, "group:grp123");
    }

    #[test]
    fn parses_explicit_signal_phone_plus_prefixed() {
        let target = parse_explicit_signal_prefix("signal:+1234567890").expect("signal target");
        assert_eq!(target.adapter, "signal");
        assert_eq!(target.target, "+1234567890");
    }

    #[test]
    fn parses_explicit_signal_phone_e164_prefixed() {
        let target =
            parse_explicit_signal_prefix("signal:e164:+1234567890").expect("signal target");
        assert_eq!(target.adapter, "signal");
        assert_eq!(target.target, "+1234567890");
    }

    #[test]
    fn ignores_non_signal_prefixed_for_explicit() {
        // parse_explicit_signal_prefix only handles signal: prefix
        assert!(
            parse_explicit_signal_prefix("uuid:123e4567-e89b-12d3-a456-426655440000").is_none()
        );
        assert!(parse_explicit_signal_prefix("+1234567890").is_none());
        assert!(parse_explicit_signal_prefix("group:grp123").is_none());
        assert!(parse_explicit_signal_prefix("discord:123").is_none());
        assert!(parse_explicit_signal_prefix("general").is_none());
        assert!(parse_explicit_signal_prefix("").is_none());
    }

    // Signal tests - implicit shorthands (only in Signal conversations)
    #[test]
    fn parses_implicit_signal_bare_uuid() {
        let target = parse_implicit_signal_shorthand(
            "123e4567-e89b-12d3-a456-426655440000",
            "signal:gvoice1",
        )
        .expect("signal target");
        assert_eq!(target.adapter, "signal:gvoice1");
        assert_eq!(target.target, "uuid:123e4567-e89b-12d3-a456-426655440000");
    }

    #[test]
    fn parses_implicit_signal_group_shorthand() {
        let target = parse_implicit_signal_shorthand("group:grp123", "signal:default")
            .expect("signal target");
        assert_eq!(target.adapter, "signal:default");
        assert_eq!(target.target, "group:grp123");
    }

    #[test]
    fn parses_implicit_signal_bare_phone_plus() {
        let target = parse_implicit_signal_shorthand("+1234567890", "signal:primary")
            .expect("signal target");
        assert_eq!(target.adapter, "signal:primary");
        assert_eq!(target.target, "+1234567890");
    }

    #[test]
    fn ignores_bare_phone_digits_for_implicit() {
        // Bare phone numbers without + prefix are rejected
        // to avoid confusing numeric channel IDs with Signal numbers
        assert!(parse_implicit_signal_shorthand("1234567890", "signal:default").is_none());
    }

    #[test]
    fn ignores_explicit_signal_prefix_for_implicit() {
        // parse_implicit_signal_shorthand does NOT handle signal: prefix
        // that's handled by parse_explicit_signal_prefix
        assert!(parse_implicit_signal_shorthand("signal:+1234567890", "signal:default").is_none());
        assert!(
            parse_implicit_signal_shorthand(
                "signal:uuid:123e4567-e89b-12d3-a456-426655440000",
                "signal:default"
            )
            .is_none()
        );
    }

    #[test]
    fn ignores_non_signal_targets_for_implicit() {
        assert!(parse_implicit_signal_shorthand("discord:123", "signal:default").is_none());
        assert!(parse_implicit_signal_shorthand("general", "signal:default").is_none());
        assert!(parse_implicit_signal_shorthand("", "signal:default").is_none());
    }
}
