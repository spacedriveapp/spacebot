use crate::InboundMessage;
use crate::config::{HumanDef, ParticipantContextConfig};

use chrono::{DateTime, Utc};

use std::collections::HashMap;

const MAX_DISPLAY_NAME_CHARS: usize = 80;

/// Active participant state tracked for a live channel session.
///
/// This is the bridge between today's lightweight config-backed participant
/// context and the future DB-backed participant-awareness pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveParticipant {
    /// Canonical participant key for the current implementation.
    ///
    /// Uses the configured human ID when available. Otherwise falls back to a
    /// platform-scoped sender key so the future humans/user-identifiers store
    /// can replace the backing source without changing channel state shape.
    pub participant_key: String,
    /// Raw platform source ("discord", "slack", etc).
    pub platform: String,
    /// Raw sender ID from the inbound message.
    pub sender_id: String,
    /// Best display name available for prompt rendering.
    pub display_name: String,
    /// Optional org/config role.
    pub role: Option<String>,
    /// Optional lightweight profile summary from configured human metadata.
    pub profile_summary: Option<String>,
    /// Most recent time this participant spoke in the current channel session.
    pub last_message_at: DateTime<Utc>,
}

/// Resolve the working-memory identity key for a sender.
///
/// Uses the configured human ID when available; otherwise falls back to a
/// platform-scoped sender key to avoid cross-adapter collisions.
pub fn participant_memory_key(
    humans: &[HumanDef],
    base_platform: &str,
    adapter: Option<&str>,
    sender_id: &str,
) -> String {
    find_human_for_sender(humans, base_platform, sender_id)
        .map(|entry| entry.id.clone())
        .unwrap_or_else(|| {
            let adapter_or_base = adapter.unwrap_or(base_platform);
            format!("{adapter_or_base}:{sender_id}")
        })
}

/// Resolve and record a participant seen in an inbound message.
pub fn track_active_participant(
    participants: &mut HashMap<String, ActiveParticipant>,
    humans: &[HumanDef],
    message: &InboundMessage,
) {
    if message.source == "system" || message.sender_id.trim().is_empty() {
        return;
    }

    let platform = message.source.trim();
    if platform.is_empty() {
        return;
    }

    let adapter = message.adapter.as_deref();
    let human = find_human_for_sender(humans, platform, &message.sender_id);
    let display_name = human
        .and_then(|entry| entry.display_name.as_deref())
        .and_then(sanitize_display_name)
        .unwrap_or_else(|| participant_display_name(message));
    let participant_key = participant_memory_key(humans, platform, adapter, &message.sender_id);
    let fallback_key = format!("{}:{}", adapter.unwrap_or(platform), message.sender_id);

    participants.retain(|existing_key, participant| {
        (existing_key == &participant_key)
            || participant.platform != adapter.unwrap_or(platform)
            || participant.sender_id != message.sender_id
    });
    if participant_key != fallback_key {
        participants.remove(&fallback_key);
    }

    participants.insert(
        participant_key.clone(),
        ActiveParticipant {
            participant_key,
            platform: adapter.unwrap_or(platform).to_string(),
            sender_id: message.sender_id.clone(),
            display_name,
            role: human.and_then(|entry| entry.role.clone()),
            profile_summary: human_profile_summary(human),
            last_message_at: message.timestamp,
        },
    );
}

/// Return participants in prompt-render order.
pub fn renderable_participants(
    participants: &HashMap<String, ActiveParticipant>,
    config: &ParticipantContextConfig,
) -> Vec<ActiveParticipant> {
    if !config.enabled || participants.len() < config.min_participants {
        return Vec::new();
    }

    let mut entries: Vec<ActiveParticipant> = participants.values().cloned().collect();
    entries.sort_by(|left, right| {
        right
            .last_message_at
            .cmp(&left.last_message_at)
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    entries.truncate(config.max_participants);
    entries
}

/// Best-effort display name for an inbound message sender.
pub fn participant_display_name(message: &InboundMessage) -> String {
    message
        .formatted_author
        .as_deref()
        .and_then(sanitize_display_name)
        .or_else(|| {
            message
                .metadata
                .get("sender_display_name")
                .and_then(|value| value.as_str())
                .and_then(sanitize_display_name)
        })
        .or_else(|| sanitize_display_name(&message.sender_id))
        .unwrap_or_else(|| message.sender_id.clone())
}

fn sanitize_display_name(value: &str) -> Option<String> {
    let collapsed = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let mut sanitized = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");
    if sanitized.is_empty() {
        return None;
    }

    if sanitized.len() > MAX_DISPLAY_NAME_CHARS {
        let boundary = sanitized.floor_char_boundary(MAX_DISPLAY_NAME_CHARS);
        sanitized.truncate(boundary);
        sanitized.push_str("...");
    }

    Some(sanitized)
}

fn find_human_for_sender<'a>(
    humans: &'a [HumanDef],
    platform: &str,
    sender_id: &str,
) -> Option<&'a HumanDef> {
    humans.iter().find(|human| {
        human.id == sender_id
            || human.email.as_deref() == Some(sender_id)
            || match platform {
                "discord" => human.discord_id.as_deref() == Some(sender_id),
                "telegram" => human.telegram_id.as_deref() == Some(sender_id),
                "slack" => human.slack_id.as_deref() == Some(sender_id),
                _ => false,
            }
    })
}

fn human_profile_summary(human: Option<&HumanDef>) -> Option<String> {
    human
        .and_then(|entry| entry.description.as_deref().or(entry.bio.as_deref()))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{InboundMessage, MessageContent};

    use chrono::TimeZone;

    fn test_message() -> InboundMessage {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "sender_display_name".to_string(),
            serde_json::Value::String("Victor".to_string()),
        );

        InboundMessage {
            id: "message-1".to_string(),
            source: "discord".to_string(),
            adapter: None,
            conversation_id: "discord:chan-1".to_string(),
            sender_id: "12345".to_string(),
            agent_id: None,
            content: MessageContent::Text("hello".to_string()),
            timestamp: Utc.with_ymd_and_hms(2026, 3, 31, 12, 0, 0).unwrap(),
            metadata,
            formatted_author: None,
        }
    }

    #[test]
    fn tracks_known_human_with_canonical_id() {
        let mut participants = HashMap::new();
        let message = test_message();
        let humans = vec![HumanDef {
            id: "victor".to_string(),
            display_name: Some("Victor".to_string()),
            role: Some("Maintainer".to_string()),
            bio: Some("Prefers direct replies.".to_string()),
            description: None,
            discord_id: Some("12345".to_string()),
            telegram_id: None,
            slack_id: None,
            email: None,
        }];

        track_active_participant(&mut participants, &humans, &message);

        let participant = participants.get("victor").expect("participant missing");
        assert_eq!(participant.display_name, "Victor");
        assert_eq!(participant.role.as_deref(), Some("Maintainer"));
        assert_eq!(
            participant.profile_summary.as_deref(),
            Some("Prefers direct replies.")
        );
    }

    #[test]
    fn falls_back_to_platform_scoped_key_for_unknown_sender() {
        let mut participants = HashMap::new();
        let message = test_message();

        track_active_participant(&mut participants, &[], &message);

        let participant = participants
            .get("discord:12345")
            .expect("participant missing");
        assert_eq!(participant.display_name, "Victor");
        assert!(participant.role.is_none());
        assert!(participant.profile_summary.is_none());
    }

    #[test]
    fn renderable_participants_respects_minimum_and_ordering() {
        let mut participants = HashMap::new();
        let message = test_message();
        let mut later = test_message();
        later.sender_id = "999".to_string();
        later.metadata.insert(
            "sender_display_name".to_string(),
            serde_json::Value::String("Alex".to_string()),
        );
        later.timestamp = Utc.with_ymd_and_hms(2026, 3, 31, 12, 5, 0).unwrap();

        track_active_participant(&mut participants, &[], &message);
        track_active_participant(&mut participants, &[], &later);

        let config = ParticipantContextConfig {
            min_participants: 2,
            max_participants: 1,
            ..ParticipantContextConfig::default()
        };

        let renderable = renderable_participants(&participants, &config);
        assert_eq!(renderable.len(), 1);
        assert_eq!(renderable[0].display_name, "Alex");
    }

    #[test]
    fn participant_memory_key_uses_platform_scope_for_unknown_sender() {
        assert_eq!(
            participant_memory_key(&[], "discord", None, "12345"),
            "discord:12345"
        );
    }

    #[test]
    fn participant_memory_key_uses_adapter_scope_for_unknown_sender() {
        assert_eq!(
            participant_memory_key(&[], "signal", Some("signal:work"), "12345"),
            "signal:work:12345"
        );
    }

    #[test]
    fn track_active_participant_uses_adapter_scope_for_fallback_key() {
        let mut participants = HashMap::new();
        let mut message = test_message();
        message.source = "signal".to_string();
        message.adapter = Some("signal:work".to_string());
        message.conversation_id = "signal:work:chan-1".to_string();

        track_active_participant(&mut participants, &[], &message);

        assert!(participants.contains_key("signal:work:12345"));
        assert!(!participants.contains_key("signal:12345"));
    }

    #[test]
    fn participant_display_name_prefers_formatted_author() {
        let mut message = test_message();
        message.formatted_author = Some("Victor Summit".to_string());

        assert_eq!(participant_display_name(&message), "Victor Summit");
    }

    #[test]
    fn participant_display_name_sanitizes_user_input() {
        let mut message = test_message();
        message.formatted_author = Some("  Victor\n\t<System>\u{0007} ".repeat(10));

        let display_name = participant_display_name(&message);
        assert!(display_name.starts_with("Victor <System>"));
        assert!(display_name.ends_with("..."));
        assert!(display_name.len() <= MAX_DISPLAY_NAME_CHARS + 3);
        assert!(!display_name.contains('\n'));
        assert!(!display_name.contains('\u{0007}'));
    }

    #[test]
    fn upgrades_fallback_participant_entry_without_duplicates() {
        let mut participants = HashMap::new();
        let message = test_message();

        track_active_participant(&mut participants, &[], &message);
        assert!(participants.contains_key("discord:12345"));

        let humans = vec![HumanDef {
            id: "victor".to_string(),
            display_name: Some("Victor".to_string()),
            role: Some("Maintainer".to_string()),
            bio: Some("Prefers direct replies.".to_string()),
            description: None,
            discord_id: Some("12345".to_string()),
            telegram_id: None,
            slack_id: None,
            email: None,
        }];

        track_active_participant(&mut participants, &humans, &message);

        assert_eq!(participants.len(), 1);
        assert!(!participants.contains_key("discord:12345"));
        assert!(participants.contains_key("victor"));
    }
}
