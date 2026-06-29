use super::{
    Binding, DiscordConfig, DiscordInstanceConfig, MattermostConfig, MattermostInstanceConfig,
    SignalConfig, SignalInstanceConfig, SlackConfig, SlackInstanceConfig, TeamsConfig,
    TeamsInstanceConfig, TelegramConfig, TelegramInstanceConfig, TwitchConfig,
    TwitchInstanceConfig,
};
use std::collections::HashMap;

/// Hot-reloadable Discord permission filters.
///
/// Derived from bindings + discord config. Shared with the Discord adapter
/// via `Arc<ArcSwap<..>>` so the file watcher can swap in new values without
/// restarting the gateway connection.
#[derive(Debug, Clone, Default)]
pub struct DiscordPermissions {
    pub guild_filter: Option<Vec<u64>>,
    pub channel_filter: HashMap<u64, Vec<u64>>,
    pub dm_allowed_users: Vec<u64>,
    pub allow_bot_messages: bool,
}

impl DiscordPermissions {
    /// Build from the current config's discord settings and bindings.
    pub fn from_config(discord: &DiscordConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            discord.dm_allowed_users.clone(),
            discord.allow_bot_messages,
            bindings,
            None,
        )
    }

    /// Build permissions for a named Discord adapter instance.
    pub fn from_instance_config(instance: &DiscordInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            instance.allow_bot_messages,
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        allow_bot_messages: bool,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let discord_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "discord"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

        let guild_filter = {
            let guild_ids: Vec<u64> = discord_bindings
                .iter()
                .filter_map(|b| b.guild_id.as_ref()?.parse::<u64>().ok())
                .collect();
            if guild_ids.is_empty() {
                None
            } else {
                Some(guild_ids)
            }
        };

        let channel_filter = {
            let mut filter: HashMap<u64, Vec<u64>> = HashMap::new();
            for binding in &discord_bindings {
                if let Some(guild_id) = binding
                    .guild_id
                    .as_ref()
                    .and_then(|g| g.parse::<u64>().ok())
                    && !binding.channel_ids.is_empty()
                {
                    let channel_ids: Vec<u64> = binding
                        .channel_ids
                        .iter()
                        .filter_map(|id| id.parse::<u64>().ok())
                        .collect();
                    filter.entry(guild_id).or_default().extend(channel_ids);
                }
            }
            filter
        };

        let mut dm_allowed_users: Vec<u64> = seed_dm_allowed_users
            .iter()
            .filter_map(|id| id.parse::<u64>().ok())
            .collect();

        // Also collect dm_allowed_users from bindings
        for binding in &discord_bindings {
            for id in &binding.dm_allowed_users {
                if let Ok(uid) = id.parse::<u64>()
                    && !dm_allowed_users.contains(&uid)
                {
                    dm_allowed_users.push(uid);
                }
            }
        }

        Self {
            guild_filter,
            channel_filter,
            dm_allowed_users,
            allow_bot_messages,
        }
    }
}

/// Hot-reloadable Slack permission filters.
///
/// Shared with the Slack adapter via `Arc<ArcSwap<..>>` for hot-reloading.
#[derive(Debug, Clone, Default)]
pub struct SlackPermissions {
    pub workspace_filter: Option<Vec<String>>, // team IDs
    pub channel_filter: HashMap<String, Vec<String>>, // team_id -> allowed channel_ids
    pub dm_allowed_users: Vec<String>,         // user IDs
}

impl SlackPermissions {
    /// Build from the current config's slack settings and bindings.
    pub fn from_config(slack: &SlackConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(slack.dm_allowed_users.clone(), bindings, None)
    }

    /// Build permissions for a named Slack adapter instance.
    pub fn from_instance_config(instance: &SlackInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let slack_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "slack"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

        let workspace_filter = {
            let workspace_ids: Vec<String> = slack_bindings
                .iter()
                .filter_map(|b| b.workspace_id.clone())
                .collect();
            if workspace_ids.is_empty() {
                None
            } else {
                Some(workspace_ids)
            }
        };

        let channel_filter = {
            let mut filter: HashMap<String, Vec<String>> = HashMap::new();
            for binding in &slack_bindings {
                if let Some(workspace_id) = &binding.workspace_id
                    && !binding.channel_ids.is_empty()
                {
                    filter
                        .entry(workspace_id.clone())
                        .or_default()
                        .extend(binding.channel_ids.clone());
                }
            }
            filter
        };

        let mut dm_allowed_users = seed_dm_allowed_users;

        for binding in &slack_bindings {
            for id in &binding.dm_allowed_users {
                if !dm_allowed_users.contains(id) {
                    dm_allowed_users.push(id.clone());
                }
            }
        }

        Self {
            workspace_filter,
            channel_filter,
            dm_allowed_users,
        }
    }
}

/// Hot-reloadable Telegram permission filters.
///
/// Shared with the Telegram adapter via `Arc<ArcSwap<..>>` for hot-reloading.
#[derive(Debug, Clone, Default)]
pub struct TelegramPermissions {
    /// Allowed chat IDs (None = all chats accepted).
    pub chat_filter: Option<Vec<i64>>,
    /// User IDs allowed in private chats.
    pub dm_allowed_users: Vec<i64>,
}

impl TelegramPermissions {
    /// Build from the current config's telegram settings and bindings.
    pub fn from_config(telegram: &TelegramConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(telegram.dm_allowed_users.clone(), bindings, None)
    }

    /// Build permissions for a named Telegram adapter instance.
    pub fn from_instance_config(instance: &TelegramInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let telegram_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "telegram"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

        let chat_filter = {
            let chat_ids: Vec<i64> = telegram_bindings
                .iter()
                .filter_map(|b| b.chat_id.as_ref()?.parse::<i64>().ok())
                .collect();
            if chat_ids.is_empty() {
                None
            } else {
                Some(chat_ids)
            }
        };

        let mut dm_allowed_users: Vec<i64> = seed_dm_allowed_users
            .iter()
            .filter_map(|id| id.parse::<i64>().ok())
            .collect();

        for binding in &telegram_bindings {
            for id in &binding.dm_allowed_users {
                if let Ok(uid) = id.parse::<i64>()
                    && !dm_allowed_users.contains(&uid)
                {
                    dm_allowed_users.push(uid);
                }
            }
        }

        Self {
            chat_filter,
            dm_allowed_users,
        }
    }
}

/// Hot-reloadable Twitch permission filters.
///
/// Shared with the Twitch adapter via `Arc<ArcSwap<..>>` for hot-reloading.
#[derive(Debug, Clone, Default)]
pub struct TwitchPermissions {
    /// Allowed channel names (None = all joined channels accepted).
    pub channel_filter: Option<Vec<String>>,
    /// User login names allowed to interact with the bot. Empty = all users.
    pub allowed_users: Vec<String>,
}

impl TwitchPermissions {
    /// Build from the current config's twitch settings and bindings.
    pub fn from_config(_twitch: &TwitchConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(bindings, None)
    }

    /// Build permissions for a named Twitch adapter instance.
    pub fn from_instance_config(instance: &TwitchInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(bindings, Some(instance.name.as_str()))
    }

    fn from_bindings_for_adapter(bindings: &[Binding], adapter_selector: Option<&str>) -> Self {
        let twitch_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "twitch"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

        let channel_filter = {
            let channel_ids: Vec<String> = twitch_bindings
                .iter()
                .flat_map(|b| b.channel_ids.clone())
                .collect();
            if channel_ids.is_empty() {
                None
            } else {
                Some(channel_ids)
            }
        };

        let mut allowed_users: Vec<String> = Vec::new();
        for binding in &twitch_bindings {
            for id in &binding.dm_allowed_users {
                if !allowed_users.contains(id) {
                    allowed_users.push(id.clone());
                }
            }
        }

        Self {
            channel_filter,
            allowed_users,
        }
    }
}

/// Hot-reloadable Signal permission filters.
///
/// Shared with the Signal adapter via `Arc<ArcSwap<..>>` for hot-reloading.
/// Uses string-based identifiers since Signal users are identified by phone
/// numbers (E.164) or UUIDs.
///
/// Wildcards:
/// - `"*"` in `dm_allowed_users` means allow all DM users
/// - `"*"` in `group_allowed_users` means allow all group users
/// - `"*"` in `group_filter` means allow all groups
/// - Empty array means block all (the `"*"` must be explicitly set to allow all)
#[derive(Debug, Clone, Default)]
pub struct SignalPermissions {
    /// Allowed group IDs. None = block all, Some(["*"]) = allow all, Some([...]) = specific list.
    pub group_filter: Option<Vec<String>>,
    /// Phone numbers or UUIDs allowed to DM the bot. ["*"] = allow all, [] = block all.
    /// Only applies to direct messages.
    pub dm_allowed_users: Vec<String>,
    /// Phone numbers or UUIDs allowed in group messages. ["*"] = allow all, [] = block all.
    /// For groups, both dm_allowed_users AND group_allowed_users are checked (merged).
    pub group_allowed_users: Vec<String>,
}

impl SignalPermissions {
    /// Build from the current config's signal settings.
    pub fn from_config(signal: &SignalConfig) -> Self {
        Self::build_from_seed(
            signal.dm_allowed_users.clone(),
            signal.group_ids.clone(),
            signal.group_allowed_users.clone(),
        )
    }

    /// Build permissions for a named Signal adapter instance.
    pub fn from_instance_config(instance: &SignalInstanceConfig) -> Self {
        Self::build_from_seed(
            instance.dm_allowed_users.clone(),
            instance.group_ids.clone(),
            instance.group_allowed_users.clone(),
        )
    }

    fn build_from_seed(
        seed_dm_allowed_users: Vec<String>,
        seed_group_ids: Vec<String>,
        seed_group_allowed_users: Vec<String>,
    ) -> Self {
        // Group filter: collect group_ids from signal config/instance.
        // - "*" means allow all groups
        // - Empty list means block all groups
        // - Specific IDs means only those groups are allowed
        let mut group_filter_wildcard = false;
        let group_filter = {
            let mut all_group_ids: Vec<String> = Vec::new();

            // Process seed_group_ids with validation
            for id in &seed_group_ids {
                let id = id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                if id == "*" {
                    group_filter_wildcard = true;
                    break;
                }
                // Signal group IDs are base64-encoded; validate format.
                if !is_valid_base64(&id) {
                    tracing::warn!(
                        group_id = %id,
                        "signal: seed group_id is not valid base64, dropping"
                    );
                    continue;
                }
                if !all_group_ids.contains(&id) {
                    all_group_ids.push(id);
                }
            }

            if group_filter_wildcard {
                Some(vec!["*".to_string()])
            } else if all_group_ids.is_empty() {
                None
            } else {
                Some(all_group_ids)
            }
        };

        // Build dm_allowed_users separately (for DMs only)
        // - "*" means allow all DM users
        // - Empty list means block all DMs
        // - Specific list means only those users are allowed for DMs
        let dm_users: Vec<String> = seed_dm_allowed_users
            .iter()
            .filter_map(|id| {
                let trimmed = id.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .collect();
        let dm_wildcard = dm_users.iter().any(|id| id == "*");

        // Build group_allowed_users separately (for groups only)
        // - "*" means allow all group users
        // - Empty list means block all group users
        // - Specific list means only those users are allowed in groups
        let group_users: Vec<String> = seed_group_allowed_users
            .iter()
            .filter_map(|id| {
                let trimmed = id.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .collect();
        let group_wildcard = group_users.iter().any(|id| id == "*");

        Self {
            group_filter,
            dm_allowed_users: if dm_wildcard {
                vec!["*".to_string()]
            } else {
                dm_users
            },
            group_allowed_users: if group_wildcard {
                vec!["*".to_string()]
            } else {
                group_users
            },
        }
    }
}

/// Per-adapter permissions for the Mattermost platform.
#[derive(Debug, Clone, Default)]
pub struct MattermostPermissions {
    pub team_filter: Option<Vec<String>>,
    pub channel_filter: HashMap<String, Vec<String>>,
    pub dm_allowed_users: Vec<String>,
}

impl MattermostPermissions {
    pub fn from_config(config: &MattermostConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(config.dm_allowed_users.clone(), bindings, None)
    }

    pub fn from_instance_config(instance: &MattermostInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let mm_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|b| {
                b.channel == "mattermost" && binding_adapter_selector_matches(b, adapter_selector)
            })
            .collect();

        let team_filter = {
            let team_ids: Vec<String> = mm_bindings
                .iter()
                .filter_map(|b| b.team_id.clone())
                .collect();
            if team_ids.is_empty() {
                None
            } else {
                Some(team_ids)
            }
        };

        let channel_filter = {
            let mut filter: HashMap<String, Vec<String>> = HashMap::new();
            for binding in &mm_bindings {
                if let Some(team_id) = &binding.team_id
                    && !binding.channel_ids.is_empty()
                {
                    filter
                        .entry(team_id.clone())
                        .or_default()
                        .extend(binding.channel_ids.clone());
                }
            }
            filter
        };

        let mut dm_allowed_users = seed_dm_allowed_users;
        for binding in &mm_bindings {
            for id in &binding.dm_allowed_users {
                if !dm_allowed_users.contains(id) {
                    dm_allowed_users.push(id.clone());
                }
            }
        }

        Self {
            team_filter,
            channel_filter,
            dm_allowed_users,
        }
    }
}

/// Hot-reloadable Microsoft Teams permission filters.
///
/// Derived from bindings + Teams config. Shared with the Teams adapter
/// via `Arc<ArcSwap<..>>` so the file watcher can swap in new values without
/// restarting the webhook listener.
///
/// Teams bindings do not carry a workspace/tenant grouping key (unlike Slack's
/// `workspace_id`), so `channel_filter` is a flat optional list rather than a
/// per-workspace map.
#[derive(Debug, Clone, Default)]
pub struct TeamsPermissions {
    /// Allowed Teams channel IDs (None = all channels accepted).
    pub channel_filter: Option<Vec<String>>,
    /// Teams user IDs allowed to DM the bot, matched against the inbound
    /// `activity.from.id` (the user's MRI, e.g. `29:...`). A `"*"` entry
    /// allows any DM sender. Empty = DMs blocked entirely.
    pub dm_allowed_users: Vec<String>,
}

impl TeamsPermissions {
    /// Build from the current config's Teams settings and bindings.
    pub fn from_config(teams: &TeamsConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(teams.dm_allowed_users.clone(), bindings, None)
    }

    /// Build permissions for a named Teams adapter instance.
    pub fn from_instance_config(instance: &TeamsInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let teams_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "teams"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

        // Teams bindings carry no workspace/tenant grouping key, so we collect
        // a flat list of allowed channel IDs across all matching bindings.
        let channel_filter = {
            let channel_ids: Vec<String> = teams_bindings
                .iter()
                .flat_map(|b| b.channel_ids.clone())
                .collect();
            if channel_ids.is_empty() {
                None
            } else {
                Some(channel_ids)
            }
        };

        let mut dm_allowed_users = seed_dm_allowed_users;

        for binding in &teams_bindings {
            for id in &binding.dm_allowed_users {
                if !dm_allowed_users.contains(id) {
                    dm_allowed_users.push(id.clone());
                }
            }
        }

        Self {
            channel_filter,
            dm_allowed_users,
        }
    }

    /// Decide whether an inbound Teams activity should be dispatched.
    ///
    /// # Arguments
    ///
    /// * `conversation_type` – the `conversationType` claim from the Activity
    ///   (`"personal"` for DMs, `"channel"` / `"groupChat"` for team channels).
    ///   `None` is treated as a channel message.
    /// * `sender_id` – the AAD object ID of the sender.
    /// * `channel_id` – the Teams channel ID (from `activity.conversation.id`).
    ///
    /// # Decision rules
    ///
    /// - **DM (`"personal"`):** allowed iff `dm_allowed_users` is non-empty and
    ///   contains `sender_id`. Empty list = all DMs blocked.
    /// - **Channel / other:** allowed iff `channel_filter` is `None` (open)
    ///   **or** `channel_filter` contains `channel_id`.
    pub fn is_allowed(
        &self,
        conversation_type: Option<&str>,
        sender_id: &str,
        channel_id: &str,
    ) -> bool {
        if conversation_type == Some("personal") {
            // DM path — fail-closed: block when list is empty or sender absent.
            // A `"*"` entry is an explicit allow-all-DMs wildcard (mirrors
            // Signal); an empty list still blocks every DM.
            if self.dm_allowed_users.is_empty() {
                return false;
            }
            self.dm_allowed_users
                .iter()
                .any(|u| u == "*" || u == sender_id)
        } else {
            // Channel / groupChat path.
            match &self.channel_filter {
                None => true,
                Some(allowed) => allowed.iter().any(|c| c == channel_id),
            }
        }
    }
}

fn binding_adapter_selector_matches(binding: &Binding, adapter_selector: Option<&str>) -> bool {
    match (binding.adapter.as_deref(), adapter_selector) {
        (None, None) => true,
        (Some(binding_selector), Some(requested_selector)) => {
            binding_selector == requested_selector
        }
        _ => false,
    }
}

/// Check if a string is valid base64 (URL-safe or standard).
/// Signal group IDs are base64-encoded.
fn is_valid_base64(s: &str) -> bool {
    use base64::{
        Engine, engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE,
        engine::general_purpose::URL_SAFE_NO_PAD,
    };

    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }

    URL_SAFE_NO_PAD.decode(trimmed).is_ok()
        || URL_SAFE.decode(trimmed).is_ok()
        || STANDARD.decode(trimmed).is_ok()
}

#[cfg(test)]
mod teams_permissions_tests {
    use super::*;
    use crate::config::types::{TeamsConfig, TeamsInstanceConfig};

    fn make_teams_config(dm_allowed_users: Vec<&str>) -> TeamsConfig {
        TeamsConfig {
            enabled: true,
            app_id: "app-id".into(),
            client_secret: "secret".into(),
            tenant_id: "common".into(),
            port: 3979,
            bind: "0.0.0.0".into(),
            instances: vec![],
            dm_allowed_users: dm_allowed_users.into_iter().map(String::from).collect(),
        }
    }

    fn make_teams_instance_config(name: &str, dm_allowed_users: Vec<&str>) -> TeamsInstanceConfig {
        TeamsInstanceConfig {
            name: name.into(),
            enabled: true,
            app_id: "app-id".into(),
            client_secret: "secret".into(),
            tenant_id: "common".into(),
            dm_allowed_users: dm_allowed_users.into_iter().map(String::from).collect(),
        }
    }

    /// A DM sender present in dm_allowed_users is allowed.
    #[test]
    fn dm_allowed_user_is_permitted() {
        let config = make_teams_config(vec!["user-aad-123"]);
        let perms = TeamsPermissions::from_config(&config, &[]);
        assert!(
            perms.dm_allowed_users.contains(&"user-aad-123".to_string()),
            "user-aad-123 should be in dm_allowed_users"
        );
    }

    /// A DM sender NOT in dm_allowed_users is denied.
    #[test]
    fn dm_unknown_user_is_denied() {
        let config = make_teams_config(vec!["user-aad-123"]);
        let perms = TeamsPermissions::from_config(&config, &[]);
        assert!(
            !perms.dm_allowed_users.contains(&"unknown-user".to_string()),
            "unknown-user should not be in dm_allowed_users"
        );
    }

    /// Empty dm_allowed_users means DMs are blocked (no users permitted).
    #[test]
    fn empty_dm_allowed_users_blocks_all() {
        let config = make_teams_config(vec![]);
        let perms = TeamsPermissions::from_config(&config, &[]);
        assert!(
            perms.dm_allowed_users.is_empty(),
            "empty dm_allowed_users should remain empty (block all DMs)"
        );
    }

    /// channel_filter is None when no bindings provide channel IDs.
    #[test]
    fn no_bindings_yields_no_channel_filter() {
        let config = make_teams_config(vec![]);
        let perms = TeamsPermissions::from_config(&config, &[]);
        assert!(
            perms.channel_filter.is_none(),
            "channel_filter should be None when no bindings specify channel IDs"
        );
    }

    /// A `"*"` DM wildcard allows any DM sender (mirrors Signal), while an
    /// empty list still blocks all DMs and a specific list stays exact-match.
    #[test]
    fn dm_wildcard_allows_any_sender() {
        let wildcard = TeamsPermissions {
            channel_filter: None,
            dm_allowed_users: vec!["*".to_string()],
        };
        assert!(
            wildcard.is_allowed(Some("personal"), "anyone-at-all", ""),
            "\"*\" wildcard must allow an arbitrary DM sender"
        );
        assert!(
            wildcard.is_allowed(Some("personal"), "29:another-random-mri", ""),
            "\"*\" wildcard must allow a second arbitrary DM sender"
        );

        let empty = TeamsPermissions {
            channel_filter: None,
            dm_allowed_users: vec![],
        };
        assert!(
            !empty.is_allowed(Some("personal"), "anyone", ""),
            "empty dm_allowed_users must still block all DMs (no implicit wildcard)"
        );

        let specific = TeamsPermissions {
            channel_filter: None,
            dm_allowed_users: vec!["user-aad-123".to_string()],
        };
        assert!(
            specific.is_allowed(Some("personal"), "user-aad-123", ""),
            "a specifically-listed user is still allowed"
        );
        assert!(
            !specific.is_allowed(Some("personal"), "someone-else", ""),
            "a non-listed user is still denied when there is no wildcard"
        );
    }

    /// from_instance_config wires dm_allowed_users from the instance.
    #[test]
    fn instance_config_dm_allowed_users() {
        let instance = make_teams_instance_config("prod", vec!["instance-user-456"]);
        let perms = TeamsPermissions::from_instance_config(&instance, &[]);
        assert!(
            perms
                .dm_allowed_users
                .contains(&"instance-user-456".to_string()),
            "instance-user-456 should be in dm_allowed_users from instance config"
        );
        assert!(
            !perms.dm_allowed_users.contains(&"other-user".to_string()),
            "other-user should not be allowed"
        );
    }
}

#[cfg(test)]
mod base64_tests {
    use super::is_valid_base64;

    #[test]
    fn test_valid_url_safe_base64() {
        // URL-safe base64 without padding (common for Signal group IDs)
        assert!(is_valid_base64("abc123def456"));
        assert!(is_valid_base64("abc123_def_456__")); // URL-safe with underscores
    }

    #[test]
    fn test_valid_standard_base64() {
        // Standard base64
        assert!(is_valid_base64("abc123DEF456"));
        assert!(is_valid_base64("SGVsbG8gV29ybGQ=")); // "Hello World"
    }

    #[test]
    fn test_invalid_base64() {
        // Invalid characters not in base64 alphabet
        assert!(!is_valid_base64("not@valid!base64"));
        assert!(!is_valid_base64(""));
        assert!(!is_valid_base64("   "));
    }
}
