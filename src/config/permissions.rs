use super::{
    Binding, DiscordConfig, DiscordInstanceConfig, SlackConfig, SlackInstanceConfig,
    TelegramConfig, TelegramInstanceConfig, TwitchConfig, TwitchInstanceConfig,
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

fn binding_adapter_selector_matches(binding: &Binding, adapter_selector: Option<&str>) -> bool {
    match (binding.adapter.as_deref(), adapter_selector) {
        (None, None) => true,
        (Some(binding_selector), Some(requested_selector)) => {
            binding_selector == requested_selector
        }
        _ => false,
    }
}
