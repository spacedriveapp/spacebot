//! Send message tool for cross-channel messaging and DMs.

use crate::conversation::ChannelStore;
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
/// `MessagingManager::broadcast()`.
#[derive(Clone)]
pub struct SendMessageTool {
    messaging_manager: Arc<MessagingManager>,
    channel_store: ChannelStore,
}

impl std::fmt::Debug for SendMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendMessageTool").finish_non_exhaustive()
    }
}

impl SendMessageTool {
    pub fn new(messaging_manager: Arc<MessagingManager>, channel_store: ChannelStore) -> Self {
        Self {
            messaging_manager,
            channel_store,
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
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/send_message_to_another_channel").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "The target channel name, channel ID, or user identifier. Use a channel name like 'general' or a full channel ID from the available channels list."
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

        let channel = self
            .channel_store
            .find_by_name(&args.target)
            .await
            .map_err(|error| SendMessageError(format!("failed to search channels: {error}")))?
            .ok_or_else(|| {
                SendMessageError(format!(
                    "no channel found matching '{}'. Use a channel name or ID from the available channels list.",
                    args.target
                ))
            })?;

        let (adapter_name, broadcast_target) = resolve_broadcast_target(&channel)
            .ok_or_else(|| {
                SendMessageError(format!(
                    "could not resolve platform target for channel '{}' (platform: {})",
                    channel.display_name.as_deref().unwrap_or(&channel.id),
                    channel.platform,
                ))
            })?;

        self.messaging_manager
            .broadcast(
                &adapter_name,
                &broadcast_target,
                crate::OutboundResponse::Text(args.message),
            )
            .await
            .map_err(|error| SendMessageError(format!("failed to send message: {error}")))?;

        tracing::info!(
            adapter = %adapter_name,
            broadcast_target = %broadcast_target,
            channel_name = channel.display_name.as_deref().unwrap_or("unknown"),
            "message sent to channel"
        );

        Ok(SendMessageOutput {
            success: true,
            target: channel
                .display_name
                .unwrap_or_else(|| channel.id.clone()),
            platform: adapter_name,
        })
    }
}

/// Extract the adapter name and raw platform target ID from a ChannelInfo.
///
/// For Discord: adapter="discord", target=discord_channel_id (u64 as string)
/// For Slack: adapter="slack", target=slack_channel_id (string)
/// For Telegram: adapter="telegram", target=chat_id (parsed from channel ID)
fn resolve_broadcast_target(
    channel: &crate::conversation::channels::ChannelInfo,
) -> Option<(String, String)> {
    match channel.platform.as_str() {
        "discord" => {
            let parts: Vec<&str> = channel.id.split(':').collect();

            // DM channels: "discord:dm:{user_id}" -> broadcast target "dm:{user_id}"
            if let ["discord", "dm", user_id] = parts.as_slice() {
                return Some(("discord".to_string(), format!("dm:{user_id}")));
            }

            // Try platform_meta first for the raw discord channel ID
            if let Some(meta) = &channel.platform_meta {
                if let Some(channel_id) = meta.get("discord_channel_id") {
                    let id_str = if let Some(num) = channel_id.as_u64() {
                        num.to_string()
                    } else if let Some(s) = channel_id.as_str() {
                        s.to_string()
                    } else {
                        return None;
                    };
                    return Some(("discord".to_string(), id_str));
                }
            }

            // Fallback: parse from channel ID format "discord:{guild_id}:{channel_id}"
            match parts.as_slice() {
                ["discord", _, channel_id] => {
                    Some(("discord".to_string(), channel_id.to_string()))
                }
                _ => None,
            }
        }
        "slack" => {
            if let Some(meta) = &channel.platform_meta {
                if let Some(channel_id) = meta.get("slack_channel_id") {
                    if let Some(s) = channel_id.as_str() {
                        return Some(("slack".to_string(), s.to_string()));
                    }
                }
            }
            // Fallback: parse from "slack:{team_id}:{channel_id}" or "slack:{team_id}:{channel_id}:{thread_ts}"
            let parts: Vec<&str> = channel.id.split(':').collect();
            if parts.len() >= 3 {
                Some(("slack".to_string(), parts[2].to_string()))
            } else {
                None
            }
        }
        "telegram" => {
            // Telegram channel IDs are "telegram:{chat_id}"
            let parts: Vec<&str> = channel.id.split(':').collect();
            if parts.len() >= 2 {
                Some(("telegram".to_string(), parts[1].to_string()))
            } else {
                None
            }
        }
        _ => None,
    }
}
