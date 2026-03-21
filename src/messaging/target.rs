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
                // signal_target is already normalized (e.g., "uuid:xxxx", "group:xxxx", "+123...")
                // Determine adapter from channel.id: named if format is "signal:{name}:..."
                let adapter = extract_signal_adapter_from_channel_id(&channel.id);
                let target = normalize_signal_target(&signal_target)?;
                return Some(BroadcastTarget { adapter, target });
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
        "mattermost" => {
            let adapter = extract_mattermost_adapter_from_channel_id(&channel.id);
            let raw_target = if let Some(channel_id) = channel
                .platform_meta
                .as_ref()
                .and_then(|meta| meta.get("mattermost_channel_id"))
                .and_then(json_value_to_string)
            {
                channel_id
            } else {
                // conversation id: mattermost:{team_id}:{channel_id}
                // or mattermost:{team_id}:dm:{user_id}
                // Named instance: mattermost:{instance}:{team_id}:{channel_id}
                // Named DM:       mattermost:{instance}:{team_id}:dm:{user_id}
                let parts: Vec<&str> = channel.id.split(':').collect();
                match parts.as_slice() {
                    [_, _team_id, "dm", user_id] => format!("dm:{user_id}"),
                    [_, _team_id, channel_id] => (*channel_id).to_string(),
                    [_, _instance, _team_id, "dm", user_id] => format!("dm:{user_id}"),
                    [_, _instance, _team_id, channel_id] => (*channel_id).to_string(),
                    _ => return None,
                }
            };
            let target = normalize_mattermost_target(&raw_target)?;
            return Some(BroadcastTarget { adapter, target });
        }
        _ => return None,
    };

    let target = normalize_target(adapter, &raw_target)?;

    Some(BroadcastTarget {
        adapter: adapter.to_string(),
        target,
    })
}

pub fn normalize_target(adapter: &str, raw_target: &str) -> Option<String> {
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
        "mattermost" => normalize_mattermost_target(trimmed),
        // Webchat targets are full conversation IDs (e.g. "portal:chat:main")
        "webchat" => Some(trimmed.to_string()),
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

/// Extract the runtime adapter key from a Mattermost conversation ID.
///
/// Mattermost conversation IDs encode whether a named instance was used:
/// - Default channel:  `mattermost:{team_id}:{channel_id}` (3 parts) → `"mattermost"`
/// - Default DM:       `mattermost:{team_id}:dm:{user_id}` (4 parts, 3rd = `"dm"`) → `"mattermost"`
/// - Named channel:    `mattermost:{instance}:{team_id}:{channel_id}` (4 parts, last ≠ `"dm"`) → `"mattermost:{instance}"`
/// - Named DM:         `mattermost:{instance}:{team_id}:dm:{user_id}` (5 parts) → `"mattermost:{instance}"`
fn extract_mattermost_adapter_from_channel_id(channel_id: &str) -> String {
    // Named instance conv IDs: "mattermost:{instance}:{team_id}:{channel_id}" (4 parts)
    //                      or: "mattermost:{instance}:{team_id}:dm:{user_id}" (5 parts)
    // Default conv IDs:        "mattermost:{team_id}:{channel_id}" (3 parts)
    //                      or: "mattermost:{team_id}:dm:{user_id}" (4 parts, 3rd part = "dm")
    let parts: Vec<&str> = channel_id.split(':').collect();
    match parts.as_slice() {
        // Default DM: mattermost:{team_id}:dm:{user_id} — must come before the named-channel arm
        ["mattermost", _, "dm", _] => "mattermost".to_string(),
        // Named DM: mattermost:{instance}:{team_id}:dm:{user_id}
        ["mattermost", instance, _, "dm", _] => format!("mattermost:{instance}"),
        // Named channel: mattermost:{instance}:{team_id}:{channel_id}
        ["mattermost", instance, _, _] => format!("mattermost:{instance}"),
        _ => "mattermost".to_string(),
    }
}

/// Normalize a raw Mattermost target string to a bare channel ID or `dm:{user_id}`.
///
/// Accepts any of the following forms (with or without a leading `mattermost:` prefix):
/// - `channel_id` → `channel_id`
/// - `dm:{user_id}` → `dm:{user_id}`
/// - `{team_id}:{channel_id}` → `channel_id`
/// - `{team_id}:dm:{user_id}` → `dm:{user_id}`
/// - `{instance}:{team_id}:{channel_id}` → `channel_id`
/// - `{instance}:{team_id}:dm:{user_id}` → `dm:{user_id}`
///
/// Returns `None` if the input is empty or does not match any recognised shape.
fn normalize_mattermost_target(raw_target: &str) -> Option<String> {
    let target = strip_repeated_prefix(raw_target, "mattermost");
    // Parse out just the channel_id or dm:{user_id}, discarding any team/instance prefix.
    match target.split(':').collect::<Vec<_>>().as_slice() {
        // Already bare: "channel_id" (but not the bare word "dm" without a user_id)
        [channel_id] if !channel_id.is_empty() && *channel_id != "dm" => {
            Some((*channel_id).to_string())
        }
        ["dm", user_id] if !user_id.is_empty() => Some(format!("dm:{user_id}")),
        // With team prefix: "team_id:channel_id" or "team_id:dm:user_id"
        [_team_id, channel_id] if !channel_id.is_empty() && *channel_id != "dm" => {
            Some((*channel_id).to_string())
        }
        [_team_id, "dm", user_id] if !user_id.is_empty() => Some(format!("dm:{user_id}")),
        // With instance+team prefix: "instance:team_id:channel_id" or "instance:team_id:dm:user_id"
        [_instance, _team_id, channel_id] if !channel_id.is_empty() && *channel_id != "dm" => {
            Some((*channel_id).to_string())
        }
        [_instance, _team_id, "dm", user_id] if !user_id.is_empty() => {
            Some(format!("dm:{user_id}"))
        }
        _ => None,
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

/// Extract the Signal adapter name from a channel ID.
///
/// Channel ID formats:
/// - "signal:{target}" -> default adapter "signal"
/// - "signal:{instance}:{target}" -> named adapter "signal:{instance}"
///
/// Where {target} is uuid:xxx, group:xxx, or +xxx (starts with valid target prefix)
fn extract_signal_adapter_from_channel_id(channel_id: &str) -> String {
    let parts: Vec<&str> = channel_id.split(':').collect();
    match parts.as_slice() {
        // Named adapter: signal:{instance}:uuid:{uuid}, signal:{instance}:group:{id}
        // or signal:{instance}:e164:+{phone}
        ["signal", instance, "uuid", ..]
        | ["signal", instance, "group", ..]
        | ["signal", instance, "e164", ..] => {
            format!("signal:{instance}")
        }
        // Named adapter: signal:{instance}:+{phone}
        ["signal", instance, phone, ..] if phone.starts_with('+') => {
            format!("signal:{instance}")
        }
        // Default adapter: signal:{target}
        _ => "signal".to_string(),
    }
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
        // Use normalize_signal_target for phone/e164 to ensure consistent parsing
        ["e164", phone] => {
            normalize_signal_target(&format!("e164:{phone}")).map(|target| BroadcastTarget {
                adapter: "signal".to_string(),
                target,
            })
        }
        [phone] if phone.starts_with('+') => {
            normalize_signal_target(phone).map(|target| BroadcastTarget {
                adapter: "signal".to_string(),
                target,
            })
        }
        // Single-part targets: delegate to normalize_signal_target for bare UUIDs/phones
        [single] => normalize_signal_target(single).map(|target| BroadcastTarget {
            adapter: "signal".to_string(),
            target,
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
        // Named adapter: signal:instance:e164:+xxx - use normalize_signal_target
        [instance, "e164", phone] => {
            normalize_signal_target(&format!("e164:{phone}")).map(|target| BroadcastTarget {
                adapter: format!("signal:{instance}"),
                target,
            })
        }
        // Named adapter: signal:instance:+xxx - use normalize_signal_target
        [instance, phone] if phone.starts_with('+') => {
            normalize_signal_target(phone).map(|target| BroadcastTarget {
                adapter: format!("signal:{instance}"),
                target,
            })
        }
        // Named adapter with single-part target: delegate to normalize_signal_target
        [instance, single] => normalize_signal_target(single).map(|target| BroadcastTarget {
            adapter: format!("signal:{instance}"),
            target,
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
