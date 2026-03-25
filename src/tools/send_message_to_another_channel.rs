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
        // Check if any Signal adapter is registered (works in any context, including cron)
        let signal_adapter_available = self.messaging_manager.has_platform_adapters("signal").await;

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
            // Get actual Signal adapter names to determine if default or named-only
            let adapter_names = self.messaging_manager.adapter_names().await;
            let signal_adapters: Vec<String> = adapter_names
                .into_iter()
                .filter(|name| name == "signal" || name.starts_with("signal:"))
                .collect();
            let has_default_signal = signal_adapters.iter().any(|name| name == "signal");
            let named_adapters: Vec<String> = signal_adapters
                .into_iter()
                .filter(|name| name.starts_with("signal:"))
                .collect();

            if has_default_signal {
                // Default adapter exists - show generic syntax
                description.push_str(
                    " Signal messaging is enabled: you can target `signal:uuid:{uuid}`, `signal:group:{group_id}`, or `signal:+{phone}`.",
                );
                target_description.push_str(
                    " With Signal enabled, explicit targets are also allowed: `signal:uuid:{uuid}`, `signal:group:{group_id}`, `signal:+{phone}`",
                );

                // Also mention named instances if they exist
                if !named_adapters.is_empty() {
                    let named_examples: Vec<String> = named_adapters
                        .iter()
                        .take(2)
                        .map(|adapter| format!("`{}:+{{phone}}`", adapter))
                        .collect();
                    description.push_str(&format!(
                        " Named instances are also available: {}.",
                        named_examples.join(", ")
                    ));
                    target_description
                        .push_str(&format!(" Named instances: {}", named_examples.join(", ")));
                }
            } else {
                // Only named adapters - show specific instance names
                let instance_examples: Vec<String> = named_adapters
                    .iter()
                    .take(3)
                    .map(|adapter| format!("`{}:+{{phone}}`", adapter))
                    .collect();

                if !instance_examples.is_empty() {
                    description.push_str(&format!(
                        " Signal messaging is enabled with named instances: target using `{}:{{instance_name}}:{{target}}` format (e.g., {}).",
                        "signal",
                        instance_examples.join(", ")
                    ));
                    target_description.push_str(&format!(
                        " With Signal enabled, use named instance format: `{}:{{instance_name}}:{{target}}` (e.g., {})",
                        "signal",
                        instance_examples.join(", ")
                    ));
                }
            }
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
            // If explicit prefix returned default "signal" adapter, try to resolve
            // to a specific named instance for correct routing.
            if target.adapter == "signal" {
                target.adapter = resolve_signal_adapter(
                    &self.messaging_manager,
                    self.current_adapter.as_deref(),
                )
                .await
                .map_err(SendMessageError)?;
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
            .filter(|adapter| *adapter == "signal" || adapter.starts_with("signal:"))
        {
            // Verify the cached adapter is still registered before using it.
            // The channel's current_adapter is stale if the named adapter was removed.
            let live_signal_adapters: Vec<String> = self
                .messaging_manager
                .adapter_names()
                .await
                .into_iter()
                .filter(|name| name == "signal" || name.starts_with("signal:"))
                .collect();

            if !live_signal_adapters.contains(current_adapter) {
                // Adapter was removed — fall through to channel-name lookup instead
                // of routing to a dead adapter.
                tracing::warn!(
                    adapter = %current_adapter,
                    "current_adapter references a removed Signal adapter; skipping implicit shorthand"
                );
            } else {
                match parse_implicit_signal_shorthand(&args.target, current_adapter) {
                    Ok(Some(target)) => {
                        self.messaging_manager
                            .broadcast(
                                &target.adapter,
                                &target.target,
                                crate::OutboundResponse::Text(args.message),
                            )
                            .await
                            .map_err(|error| {
                                SendMessageError(format!("failed to send message: {error}"))
                            })?;

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
                    Err(validation_error) => {
                        return Err(SendMessageError(validation_error));
                    }
                    Ok(None) => {
                        // Not a Signal shorthand — fall through to channel-name lookup.
                    }
                }
            }
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

/// Resolve the Signal adapter to use for an explicit "signal:" target.
///
/// Looks up registered Signal adapters and returns:
/// - "signal" if a default adapter exists
/// - The named instance name if exactly one named adapter exists and no default
/// - An error if multiple named adapters exist and no default (ambiguous)
///
/// When `current_adapter` is a named Signal instance (e.g., "signal:work") and
/// no default adapter exists, it will be used as the target adapter.
pub async fn resolve_signal_adapter(
    messaging_manager: &crate::messaging::MessagingManager,
    current_adapter: Option<&str>,
) -> Result<String, String> {
    let all_signal_adapters: Vec<String> = messaging_manager
        .adapter_names()
        .await
        .into_iter()
        .filter(|name| name == "signal" || name.starts_with("signal:"))
        .collect();

    let has_default_signal = all_signal_adapters.iter().any(|name| name == "signal");
    let named_adapters: Vec<&str> = all_signal_adapters
        .iter()
        .filter(|name| name.starts_with("signal:"))
        .map(|s| s.as_str())
        .collect();

    if let Some(adapter) = current_adapter.filter(|adapter| {
        adapter.starts_with("signal:") && all_signal_adapters.iter().any(|a| a == adapter)
    }) {
        if !has_default_signal {
            return Ok(adapter.to_string());
        }
        // has_default_signal is true — fall through to return "signal"
    } else if !has_default_signal && named_adapters.len() == 1 {
        // No default, but exactly one named adapter - use it
        return Ok(named_adapters[0].to_string());
    } else if !has_default_signal && named_adapters.len() > 1 {
        // Multiple named adapters and no default - ambiguity error
        return Err(format!(
            "Multiple Signal adapters are configured ({}). Please specify which instance to use by targeting 'signal:<instance_name>:<target>' instead of 'signal:<target>'.",
            named_adapters.join(", ")
        ));
    }
    // has_default_signal is true OR no adapters at all — return default "signal"
    Ok("signal".to_string())
}

/// Parse implicit Signal shorthands - only in Signal conversations.
/// Handles bare UUIDs, group:xxx, and +phone without explicit signal: prefix.
/// Returns `Ok(Some(...))` on valid shorthand, `Ok(None)` when the input is
/// clearly not a Signal shorthand, and `Err(...)` when it *looks like* a
/// shorthand but is malformed (so the caller can surface a targeted error to
/// the LLM instead of falling through to a generic "no channel found").
fn parse_implicit_signal_shorthand(
    raw: &str,
    current_adapter: &str,
) -> Result<Option<crate::messaging::target::BroadcastTarget>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    use crate::messaging::target::BroadcastTarget;

    // Check for bare UUID format (strict validation)
    if is_valid_uuid(trimmed) {
        return Ok(Some(BroadcastTarget {
            adapter: current_adapter.to_string(),
            target: format!("uuid:{trimmed}"),
        }));
    }

    // Looks like a UUID but doesn't parse — give a targeted error.
    if trimmed.len() == 36 && trimmed.chars().filter(|c| *c == '-').count() == 4 {
        return Err(format!(
            "'{trimmed}' looks like a UUID but is malformed. Use the format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
        ));
    }

    // Phone number format: use strict E.164 validation
    if trimmed.starts_with('+') {
        if crate::messaging::target::is_valid_e164(trimmed) {
            return Ok(Some(BroadcastTarget {
                adapter: current_adapter.to_string(),
                target: trimmed.to_string(),
            }));
        }
        // Invalid phone number - provide specific error
        if let Some(digits) = trimmed.strip_prefix('+') {
            if digits.is_empty() {
                return Err(
                    "Phone number cannot be empty after + prefix. Use format: +1234567890"
                        .to_string(),
                );
            }
            if !digits.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!(
                    "'{trimmed}' contains non-digit characters. Use format: +1234567890"
                ));
            }
            if digits.len() < 6 {
                return Err(format!(
                    "'{trimmed}' is too short. Phone numbers need 6-15 digits after the + prefix (7-16 total)."
                ));
            }
            if digits.len() > 15 {
                return Err(format!(
                    "'{trimmed}' is too long. Phone numbers need 6-15 digits after the + prefix (7-16 total)."
                ));
            }
            if digits.starts_with('0') {
                return Err(format!(
                    "'{trimmed}' has invalid country code. Country codes cannot start with 0."
                ));
            }
            // Catch-all for any other '+' prefixed input that failed validation
            return Err(format!("'{trimmed}' is not a valid E.164 phone number"));
        }
    }

    // Group ID format: group:xxx
    if let Some(group_id) = trimmed.strip_prefix("group:") {
        if group_id.is_empty() {
            return Err("group: prefix requires an ID. Use format: group:<group_id>".to_string());
        }
        return Ok(Some(BroadcastTarget {
            adapter: current_adapter.to_string(),
            target: trimmed.to_string(),
        }));
    }

    Ok(None)
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
        .expect("no error")
        .expect("signal target");
        assert_eq!(target.adapter, "signal:gvoice1");
        assert_eq!(target.target, "uuid:123e4567-e89b-12d3-a456-426655440000");
    }

    #[test]
    fn parses_implicit_signal_group_shorthand() {
        let target = parse_implicit_signal_shorthand("group:grp123", "signal:default")
            .expect("no error")
            .expect("signal target");
        assert_eq!(target.adapter, "signal:default");
        assert_eq!(target.target, "group:grp123");
    }

    #[test]
    fn parses_implicit_signal_bare_phone_plus() {
        let target = parse_implicit_signal_shorthand("+1234567890", "signal:primary")
            .expect("no error")
            .expect("signal target");
        assert_eq!(target.adapter, "signal:primary");
        assert_eq!(target.target, "+1234567890");
    }

    #[test]
    fn ignores_bare_phone_digits_for_implicit() {
        // Bare phone numbers without + prefix are rejected
        // to avoid confusing numeric channel IDs with Signal numbers
        assert!(
            parse_implicit_signal_shorthand("1234567890", "signal:default")
                .expect("no error")
                .is_none()
        );
    }

    #[test]
    fn ignores_explicit_signal_prefix_for_implicit() {
        // parse_implicit_signal_shorthand does NOT handle signal: prefix
        // that's handled by parse_explicit_signal_prefix
        assert!(
            parse_implicit_signal_shorthand("signal:+1234567890", "signal:default")
                .expect("no error")
                .is_none()
        );
        assert!(
            parse_implicit_signal_shorthand(
                "signal:uuid:123e4567-e89b-12d3-a456-426655440000",
                "signal:default"
            )
            .expect("no error")
            .is_none()
        );
    }

    #[test]
    fn ignores_non_signal_targets_for_implicit() {
        assert!(
            parse_implicit_signal_shorthand("discord:123", "signal:default")
                .expect("no error")
                .is_none()
        );
        assert!(
            parse_implicit_signal_shorthand("general", "signal:default")
                .expect("no error")
                .is_none()
        );
        assert!(
            parse_implicit_signal_shorthand("", "signal:default")
                .expect("no error")
                .is_none()
        );
    }

    #[test]
    fn rejects_malformed_uuid_shorthand() {
        let error = parse_implicit_signal_shorthand(
            "123e4567-e89b-12d3-a456-42665544ZZZZ",
            "signal:default",
        )
        .expect_err("should be validation error");
        assert!(
            error.contains("looks like a UUID but is malformed"),
            "{error}"
        );
    }

    #[test]
    fn rejects_short_phone_number() {
        let error = parse_implicit_signal_shorthand("+123", "signal:default")
            .expect_err("should be validation error");
        assert!(error.contains("too short"), "{error}");
    }

    #[test]
    fn rejects_phone_with_non_digits() {
        let error = parse_implicit_signal_shorthand("+123456abc0", "signal:default")
            .expect_err("should be validation error");
        assert!(error.contains("non-digit"), "{error}");
    }

    #[test]
    fn rejects_empty_group_id() {
        let error = parse_implicit_signal_shorthand("group:", "signal:default")
            .expect_err("should be validation error");
        assert!(error.contains("requires an ID"), "{error}");
    }

    // Tests for resolve_signal_adapter
    // Note: These tests use a mock messaging manager to test the resolution logic

    #[tokio::test]
    async fn resolve_signal_adapter_returns_signal_when_no_adapters() {
        let manager = crate::messaging::MessagingManager::new();
        // No adapters registered - should return "signal" as default
        // This lets broadcast() fail with appropriate error if no adapters exist
        let result = super::resolve_signal_adapter(&manager, None).await;
        assert_eq!(result.unwrap(), "signal");
    }

    #[tokio::test]
    async fn resolve_signal_adapter_ignores_current_if_not_registered() {
        let manager = crate::messaging::MessagingManager::new();
        // When current_adapter is provided but not registered in manager,
        // it falls through to "signal" since we can't verify the adapter exists
        let result = super::resolve_signal_adapter(&manager, Some("signal:work")).await;
        assert_eq!(result.unwrap(), "signal");
    }

    // TODO: Add test for resolve_signal_adapter ambiguity case once MessagingManager
    // supports registering mock adapters or a test helper is available.
}
