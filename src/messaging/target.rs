//! Shared delivery target parsing and channel target resolution.

use crate::conversation::channels::ChannelInfo;

/// Canonical target for `MessagingManager::broadcast`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastTarget {
    pub adapter: String,
    pub target: String,
}

impl std::fmt::Display for BroadcastTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.adapter, self.target)
    }
}

/// Parse and normalize a delivery target in `adapter:target` format.
pub fn parse_delivery_target(raw: &str) -> Option<BroadcastTarget> {
    let (adapter, raw_target) = raw.split_once(':')?;
    if adapter.is_empty() || raw_target.is_empty() {
        return None;
    }

    let target = normalize_target(adapter, raw_target)?;

    Some(BroadcastTarget {
        adapter: adapter.to_string(),
        target,
    })
}

/// Resolve adapter and broadcast target from a tracked channel.
pub fn resolve_broadcast_target(channel: &ChannelInfo) -> Option<BroadcastTarget> {
    let adapter = channel.platform.as_str();

    let raw_target = match adapter {
        "discord" => {
            if let Some(channel_id) = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("discord_channel_id"))
                .and_then(json_value_to_string)
            {
                channel_id
            } else {
                let parts: Vec<&str> = channel.id.split(':').collect();
                match parts.as_slice() {
                    ["discord", "dm", user_id] => format!("dm:{user_id}"),
                    ["discord", _, channel_id] => (*channel_id).to_string(),
                    _ => return None,
                }
            }
        }
        "slack" => {
            if let Some(channel_id) = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("slack_channel_id"))
                .and_then(json_value_to_string)
            {
                channel_id
            } else {
                let parts: Vec<&str> = channel.id.split(':').collect();
                match parts.as_slice() {
                    ["slack", _, channel_id] => (*channel_id).to_string(),
                    ["slack", _, channel_id, _] => (*channel_id).to_string(),
                    _ => return None,
                }
            }
        }
        "telegram" => {
            if let Some(chat_id) = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("telegram_chat_id"))
                .and_then(json_value_to_string)
            {
                chat_id
            } else {
                let parts: Vec<&str> = channel.id.split(':').collect();
                match parts.as_slice() {
                    ["telegram", chat_id] => (*chat_id).to_string(),
                    _ => return None,
                }
            }
        }
        "twitch" => {
            if let Some(channel_login) = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("twitch_channel"))
                .and_then(json_value_to_string)
            {
                channel_login
            } else {
                let parts: Vec<&str> = channel.id.split(':').collect();
                match parts.as_slice() {
                    ["twitch", channel_login] => (*channel_login).to_string(),
                    _ => return None,
                }
            }
        }
        _ => return None,
    };

    let target = normalize_target(adapter, &raw_target)?;

    Some(BroadcastTarget {
        adapter: adapter.to_string(),
        target,
    })
}

fn normalize_target(adapter: &str, raw_target: &str) -> Option<String> {
    let trimmed = raw_target.trim();
    if trimmed.is_empty() {
        return None;
    }

    match adapter {
        "discord" => normalize_discord_target(trimmed),
        "slack" => normalize_slack_target(trimmed),
        "telegram" => normalize_telegram_target(trimmed),
        "twitch" => normalize_twitch_target(trimmed),
        _ => Some(trimmed.to_string()),
    }
}

fn normalize_discord_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "discord");

    if let Some(user_id) = target.strip_prefix("dm:") {
        if !user_id.is_empty() && user_id.chars().all(|character| character.is_ascii_digit()) {
            return Some(format!("dm:{user_id}"));
        }
        return None;
    }

    if target.chars().all(|character| character.is_ascii_digit()) {
        return Some(target.to_string());
    }

    let (maybe_guild_id, channel_id) = target.split_once(':')?;
    if maybe_guild_id
        .chars()
        .all(|character| character.is_ascii_digit())
        && channel_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Some(channel_id.to_string());
    }

    None
}

fn normalize_slack_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "slack");

    if let Some(user_id) = target.strip_prefix("dm:") {
        if !user_id.is_empty() {
            return Some(format!("dm:{user_id}"));
        }
        return None;
    }

    if let Some((workspace_id, channel_id)) = target.split_once(':') {
        if !workspace_id.is_empty() && !channel_id.is_empty() {
            return Some(channel_id.to_string());
        }
        return None;
    }

    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}

fn normalize_telegram_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "telegram");
    let chat_id = target.parse::<i64>().ok()?;
    Some(chat_id.to_string())
}

fn normalize_twitch_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "twitch");
    let channel_login = target.strip_prefix('#').unwrap_or(target);
    if channel_login.is_empty() {
        None
    } else {
        Some(channel_login.to_string())
    }
}

fn strip_repeated_prefix<'a>(raw_target: &'a str, adapter: &str) -> &'a str {
    let mut target = raw_target;
    let prefix = format!("{adapter}:");
    while let Some(stripped) = target.strip_prefix(&prefix) {
        target = stripped;
    }
    target
}

fn json_value_to_string(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(number) = value.as_i64() {
        return Some(number.to_string());
    }
    if let Some(number) = value.as_u64() {
        return Some(number.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_delivery_target, resolve_broadcast_target};
    use crate::conversation::channels::ChannelInfo;

    fn test_channel_info(id: &str, platform: &str) -> ChannelInfo {
        ChannelInfo {
            id: id.to_string(),
            platform: platform.to_string(),
            display_name: None,
            platform_meta: None,
            is_active: true,
            created_at: chrono::Utc::now(),
            last_activity_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn parse_discord_legacy_target() {
        let parsed = parse_delivery_target("discord:123456789:987654321");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "discord".to_string(),
                target: "987654321".to_string(),
            })
        );
    }

    #[test]
    fn parse_slack_conversation_target() {
        let parsed = parse_delivery_target("slack:T012345:C012345");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "slack".to_string(),
                target: "C012345".to_string(),
            })
        );
    }

    #[test]
    fn parse_twitch_target_with_prefix() {
        let parsed = parse_delivery_target("twitch:twitch:jamiepinelive");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "twitch".to_string(),
                target: "jamiepinelive".to_string(),
            })
        );
    }

    #[test]
    fn resolve_twitch_target_from_channel_id() {
        let channel = test_channel_info("twitch:jamiepinelive", "twitch");
        let resolved = resolve_broadcast_target(&channel);

        assert_eq!(
            resolved,
            Some(super::BroadcastTarget {
                adapter: "twitch".to_string(),
                target: "jamiepinelive".to_string(),
            })
        );
    }
}
