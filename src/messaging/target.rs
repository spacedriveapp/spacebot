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
        "signal" => {
            // Signal channels store target in signal_target metadata
            if let Some(signal_target) = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("signal_target"))
                .and_then(json_value_to_string)
            {
                // Parse from signal_target which already includes the normalized format
                // e.g., "uuid:xxxx" or "group:xxxx" or "+1234567890"
                return parse_delivery_target(&format!("signal:{signal_target}"));
            }

            // Fallback: parse from conversation ID
            // Format: signal:{target} or signal:{instance}:{target}
            // where {target} is uuid:xxx, group:xxx, or +xxx
            let parts: Vec<&str> = channel.id.split(':').collect();
            // Skip "signal" prefix and use shared parser for the rest
            return parse_signal_target_parts(parts.get(1..).unwrap_or(&[]));
        }
        "email" => {
            let reply_to = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("email_reply_to"))
                .and_then(json_value_to_string);
            let from = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("email_from"))
                .and_then(json_value_to_string);

            reply_to
                .as_deref()
                .and_then(normalize_email_target)
                .or_else(|| from.as_deref().and_then(normalize_email_target))?
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
        "email" => normalize_email_target(trimmed),
        "signal" => normalize_signal_target(trimmed),
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

fn normalize_email_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "email").trim();
    if target.is_empty() {
        return None;
    }

    if let Some((_, address)) = target.rsplit_once('<') {
        let address = address.trim_end_matches('>').trim();
        if address.contains('@') && !address.contains(char::is_whitespace) {
            return Some(address.to_string());
        }
    }

    if target.contains('@') && !target.contains(char::is_whitespace) {
        return Some(target.to_string());
    }

    None
}

fn normalize_signal_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "signal");

    // Handle uuid:xxxx-xxxx format
    if let Some(uuid) = target.strip_prefix("uuid:") {
        if !uuid.is_empty() {
            return Some(format!("uuid:{uuid}"));
        }
        return None;
    }

    // Handle group:grp123 format
    if let Some(group_id) = target.strip_prefix("group:") {
        if !group_id.is_empty() {
            return Some(format!("group:{group_id}"));
        }
        return None;
    }

    // Handle e164:+123 or bare +123 format
    if let Some(phone) = target.strip_prefix("e164:") {
        let phone = phone.trim_start_matches('+');
        if !phone.is_empty() && phone.len() >= 7 && phone.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("+{phone}"));
        }
        return None;
    }

    // Bare +123 format
    if let Some(phone) = target.strip_prefix('+') {
        if !phone.is_empty() && phone.len() >= 7 && phone.chars().all(|c| c.is_ascii_digit()) {
            return Some(target.to_string());
        }
        return None;
    }

    // Check if it's a valid UUID (contains dashes and alphanumeric)
    if target.contains('-') && target.len() > 8 && target.chars().any(|c| c.is_ascii_digit()) {
        return Some(format!("uuid:{target}"));
    }

    // Check if it's a bare phone number (7+ digits required for E.164)
    if target.chars().all(|c| c.is_ascii_digit()) && target.len() >= 7 {
        return Some(format!("+{target}"));
    }

    None
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

/// Parse Signal target components into BroadcastTarget.
///
/// Handles formats:
/// - Default adapter: ["uuid", xxx], ["group", xxx], ["e164", +xxx], ["+xxx"]
/// - Named adapter: [instance, "uuid", xxx], [instance, "group", xxx], [instance, "e164", +xxx], [instance, "+xxx"]
///
/// Returns None for invalid formats.
pub fn parse_signal_target_parts(parts: &[&str]) -> Option<BroadcastTarget> {
    match parts {
        // Default adapter: signal:uuid:xxx, signal:group:xxx, signal:e164:+xxx, signal:+xxx
        ["uuid", uuid] => Some(BroadcastTarget {
            adapter: "signal".to_string(),
            target: format!("uuid:{uuid}"),
        }),
        ["group", group_id] => Some(BroadcastTarget {
            adapter: "signal".to_string(),
            target: format!("group:{group_id}"),
        }),
        ["e164", phone] => Some(BroadcastTarget {
            adapter: "signal".to_string(),
            target: phone.to_string(),
        }),
        [phone] if phone.starts_with('+') => Some(BroadcastTarget {
            adapter: "signal".to_string(),
            target: phone.to_string(),
        }),
        // Named adapter: signal:instance:uuid:xxx, signal:instance:group:xxx
        [instance, "uuid", uuid] => Some(BroadcastTarget {
            adapter: format!("signal:{instance}"),
            target: format!("uuid:{uuid}"),
        }),
        [instance, "group", group_id] => Some(BroadcastTarget {
            adapter: format!("signal:{instance}"),
            target: format!("group:{group_id}"),
        }),
        // Named adapter: signal:instance:e164:+xxx
        [instance, "e164", phone] => Some(BroadcastTarget {
            adapter: format!("signal:{instance}"),
            target: phone.to_string(),
        }),
        // Named adapter: signal:instance:+xxx
        [instance, phone] if phone.starts_with('+') => Some(BroadcastTarget {
            adapter: format!("signal:{instance}"),
            target: phone.to_string(),
        }),
        _ => None,
    }
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

    #[test]
    fn parse_email_target_with_prefix() {
        let parsed = parse_delivery_target("email:alice@example.com");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "email".to_string(),
                target: "alice@example.com".to_string(),
            })
        );
    }

    #[test]
    fn parse_email_target_with_display_name() {
        let parsed = parse_delivery_target("email:Alice <alice@example.com>");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "email".to_string(),
                target: "alice@example.com".to_string(),
            })
        );
    }

    #[test]
    fn resolve_email_target_falls_back_when_reply_to_invalid() {
        let mut channel = test_channel_info("email:acct:thread", "email");
        channel.platform_meta = Some(serde_json::json!({
            "email_reply_to": "not-an-email",
            "email_from": "valid@example.com"
        }));

        let resolved = resolve_broadcast_target(&channel);

        assert_eq!(
            resolved,
            Some(super::BroadcastTarget {
                adapter: "email".to_string(),
                target: "valid@example.com".to_string(),
            })
        );
    }

    // Signal tests
    #[test]
    fn parse_signal_uuid_with_prefix() {
        let parsed = parse_delivery_target("signal:uuid:abc-123-def");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "uuid:abc-123-def".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_group_with_prefix() {
        let parsed = parse_delivery_target("signal:group:grp123");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "group:grp123".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_phone_with_prefix() {
        let parsed = parse_delivery_target("signal:+1234567890");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_phone_e164_format() {
        let parsed = parse_delivery_target("signal:e164:+1234567890");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_phone_e164_no_plus() {
        let parsed = parse_delivery_target("signal:e164:1234567890");
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    // Tests for parse_signal_target_parts
    #[test]
    fn parse_signal_target_parts_uuid_default() {
        let parsed =
            super::parse_signal_target_parts(&["uuid", "550e8400-e29b-41d4-a716-446655440000"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "uuid:550e8400-e29b-41d4-a716-446655440000".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_group_default() {
        let parsed = super::parse_signal_target_parts(&["group", "grp123"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "group:grp123".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_phone_default() {
        let parsed = super::parse_signal_target_parts(&["+1234567890"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_e164_default() {
        let parsed = super::parse_signal_target_parts(&["e164", "+1234567890"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_uuid_named() {
        let parsed = super::parse_signal_target_parts(&[
            "gvoice1",
            "uuid",
            "550e8400-e29b-41d4-a716-446655440000",
        ]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal:gvoice1".to_string(),
                target: "uuid:550e8400-e29b-41d4-a716-446655440000".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_group_named() {
        let parsed = super::parse_signal_target_parts(&["gvoice1", "group", "grp123"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal:gvoice1".to_string(),
                target: "group:grp123".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_phone_named() {
        let parsed = super::parse_signal_target_parts(&["gvoice1", "+1234567890"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal:gvoice1".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_e164_named() {
        let parsed = super::parse_signal_target_parts(&["gvoice1", "e164", "+1234567890"]);
        assert_eq!(
            parsed,
            Some(super::BroadcastTarget {
                adapter: "signal:gvoice1".to_string(),
                target: "+1234567890".to_string(),
            })
        );
    }

    #[test]
    fn parse_signal_target_parts_invalid() {
        assert!(super::parse_signal_target_parts(&[]).is_none());
        assert!(super::parse_signal_target_parts(&["unknown"]).is_none());
        assert!(super::parse_signal_target_parts(&["uuid"]).is_none()); // missing UUID value
        assert!(super::parse_signal_target_parts(&["gvoice1", "unknown"]).is_none());
    }
}
