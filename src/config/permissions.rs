use super::{
    Binding, DiscordConfig, DiscordInstanceConfig, SignalConfig, SignalInstanceConfig, SlackConfig,
    SlackInstanceConfig, TelegramConfig, TelegramInstanceConfig, TwitchConfig,
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
    /// Build from the current config's signal settings and bindings.
    pub fn from_config(signal: &SignalConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            signal.dm_allowed_users.clone(),
            signal.group_ids.clone(),
            signal.group_allowed_users.clone(),
            bindings,
            None,
        )
    }

    /// Build permissions for a named Signal adapter instance.
    pub fn from_instance_config(instance: &SignalInstanceConfig, bindings: &[Binding]) -> Self {
        Self::from_bindings_for_adapter(
            instance.dm_allowed_users.clone(),
            instance.group_ids.clone(),
            instance.group_allowed_users.clone(),
            bindings,
            Some(instance.name.as_str()),
        )
    }

    fn from_bindings_for_adapter(
        seed_dm_allowed_users: Vec<String>,
        seed_group_ids: Vec<String>,
        seed_group_allowed_users: Vec<String>,
        bindings: &[Binding],
        adapter_selector: Option<&str>,
    ) -> Self {
        let signal_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|binding| {
                binding.channel == "signal"
                    && binding_adapter_selector_matches(binding, adapter_selector)
            })
            .collect();

        // Group filter: collect group_ids from signal config/instance + bindings.
        // - "*" means allow all groups
        // - Empty list means block all groups
        // - Specific IDs means only those groups are allowed
        let group_filter = {
            let mut all_group_ids = seed_group_ids.clone();

            // Check for wildcard in seed
            if all_group_ids.iter().any(|id| id.trim() == "*") {
                return Self {
                    group_filter: Some(vec!["*".to_string()]),
                    dm_allowed_users: vec!["*".to_string()],
                    group_allowed_users: vec!["*".to_string()],
                };
            }

            // Add group_ids from bindings (Vec<String>)
            for binding in &signal_bindings {
                for id in &binding.group_ids {
                    let id = id.trim().to_string();
                    if id.is_empty() {
                        continue;
                    }
                    if id == "*" {
                        // Wildcard in binding means allow all groups
                        return Self {
                            group_filter: Some(vec!["*".to_string()]),
                            dm_allowed_users: vec!["*".to_string()],
                            group_allowed_users: vec!["*".to_string()],
                        };
                    }
                    // Signal group IDs are base64-encoded; validate format.
                    if !is_valid_base64(&id) {
                        tracing::warn!(
                            group_id = %id,
                            "signal: group_id is not valid base64, dropping"
                        );
                        continue;
                    }
                    if !all_group_ids.contains(&id) {
                        all_group_ids.push(id);
                    }
                }
            }

            Some(all_group_ids)
        };

        // Build dm_allowed_users separately (for DMs only)
        // - "*" means allow all DM users
        // - Empty list means block all DMs
        // - Specific list means only those users are allowed for DMs
        let mut dm_users = seed_dm_allowed_users.clone();

        // Check for wildcard in seed dm_allowed_users
        if dm_users.iter().any(|id| id.trim() == "*") {
            return Self {
                group_filter,
                dm_allowed_users: vec!["*".to_string()],
                group_allowed_users: vec![],
            };
        }

        // Add dm_allowed_users from bindings
        for binding in &signal_bindings {
            for id in &binding.dm_allowed_users {
                let id = id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                if id == "*" {
                    return Self {
                        group_filter,
                        dm_allowed_users: vec!["*".to_string()],
                        group_allowed_users: vec![],
                    };
                }
                if !dm_users.contains(&id) {
                    dm_users.push(id);
                }
            }
        }

        // Build group_allowed_users separately (for groups only)
        // - "*" means allow all group users
        // - Empty list means block all group users
        // - Specific list means only those users are allowed in groups
        let mut group_users = seed_group_allowed_users.clone();

        // Check for wildcard in seed group_allowed_users
        if group_users.iter().any(|id| id.trim() == "*") {
            return Self {
                group_filter,
                dm_allowed_users: dm_users,
                group_allowed_users: vec!["*".to_string()],
            };
        }

        // Add group_allowed_users from bindings
        for binding in &signal_bindings {
            for id in &binding.group_allowed_users {
                let id = id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                if id == "*" {
                    return Self {
                        group_filter,
                        dm_allowed_users: dm_users,
                        group_allowed_users: vec!["*".to_string()],
                    };
                }
                if !group_users.contains(&id) {
                    group_users.push(id);
                }
            }
        }

        Self {
            group_filter,
            dm_allowed_users: dm_users,
            group_allowed_users: group_users,
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
        Engine, engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE_NO_PAD,
    };

    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }

    URL_SAFE_NO_PAD.decode(trimmed).is_ok() || STANDARD.decode(trimmed).is_ok()
}

#[cfg(test)]
mod base64_tests {
    use super::is_valid_base64;

    #[test]
    fn test_valid_url_safe_base64() {
        // URL-safe base64 without padding (common for Signal group IDs)
        assert!(is_valid_base64("abc123def456"));
        assert!(is_valid_base64("abc123_def_456"));
    }

    #[test]
    fn test_valid_standard_base64() {
        // Standard base64
        assert!(is_valid_base64("abc123DEF456+/="));
        assert!(is_valid_base64("SGVsbG8gV29ybGQ="));
    }

    #[test]
    fn test_invalid_base64() {
        // Invalid characters not in base64 alphabet
        assert!(!is_valid_base64("not@valid!base64"));
        assert!(!is_valid_base64(""));
        assert!(!is_valid_base64("   "));
    }
}
