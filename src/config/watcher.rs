use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    Binding, Config, DiscordPermissions, RuntimeConfig, SlackPermissions, TelegramPermissions,
    TwitchPermissions, binding_runtime_adapter_key,
};
use sha2::{Digest, Sha256};

/// Per-agent context needed by the file watcher: (id, prompt_dir, identity_dir,
/// runtime_config, mcp_manager).
type WatchedAgent = (
    String,
    PathBuf,
    PathBuf,
    Arc<RuntimeConfig>,
    Arc<crate::mcp::McpManager>,
);

/// Watches config, prompt, identity, and skill files for changes and triggers
/// hot reload on the corresponding RuntimeConfig.
///
/// Returns a JoinHandle that runs until dropped. File events are debounced
/// to 2 seconds so rapid edits (e.g. :w in vim hitting multiple writes) are
/// collapsed into a single reload.
#[allow(clippy::too_many_arguments)]
pub fn spawn_file_watcher(
    config_path: PathBuf,
    instance_dir: PathBuf,
    agents: Vec<WatchedAgent>,
    discord_permissions: Option<Arc<arc_swap::ArcSwap<DiscordPermissions>>>,
    slack_permissions: Option<Arc<arc_swap::ArcSwap<SlackPermissions>>>,
    telegram_permissions: Option<Arc<arc_swap::ArcSwap<TelegramPermissions>>>,
    twitch_permissions: Option<Arc<arc_swap::ArcSwap<TwitchPermissions>>>,
    bindings: Arc<arc_swap::ArcSwap<Vec<Binding>>>,
    messaging_manager: Option<Arc<crate::messaging::MessagingManager>>,
    llm_manager: Arc<crate::llm::LlmManager>,
    agent_links: Arc<arc_swap::ArcSwap<Vec<crate::links::AgentLink>>>,
    agent_humans: Arc<arc_swap::ArcSwap<Vec<crate::config::HumanDef>>>,
) -> tokio::task::JoinHandle<()> {
    use notify::{Event, RecursiveMode, Watcher};
    use std::time::Duration;

    tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();

        let mut watcher = match notify::recommended_watcher(
            move |result: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = result {
                    // Only forward data modification events, not metadata/access changes
                    use notify::EventKind;
                    match &event.kind {
                        EventKind::Create(_)
                        | EventKind::Modify(notify::event::ModifyKind::Data(_))
                        | EventKind::Remove(_) => {
                            let _ = tx.send(event);
                        }
                        // Also forward Any/Other modify events (some backends don't distinguish)
                        EventKind::Modify(notify::event::ModifyKind::Any) => {
                            let _ = tx.send(event);
                        }
                        _ => {}
                    }
                }
            },
        ) {
            Ok(w) => w,
            Err(error) => {
                tracing::error!(%error, "failed to create file watcher");
                return;
            }
        };

        // Watch config.toml
        if let Err(error) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
            tracing::warn!(%error, path = %config_path.display(), "failed to watch config file");
        }

        // Watch instance-level skills directory
        let instance_skills_dir = instance_dir.join("skills");
        if instance_skills_dir.is_dir()
            && let Err(error) = watcher.watch(&instance_skills_dir, RecursiveMode::Recursive)
        {
            tracing::warn!(%error, path = %instance_skills_dir.display(), "failed to watch instance skills dir");
        }

        // Watch per-agent directories
        for (_, workspace, identity_dir, _, _) in &agents {
            // Watch workspace/skills for skill file changes
            {
                let path = workspace.join("skills");
                if path.is_dir()
                    && let Err(error) = watcher.watch(&path, RecursiveMode::Recursive)
                {
                    tracing::warn!(%error, path = %path.display(), "failed to watch agent skills dir");
                }
            }
            // Watch the agent root (identity_dir) for SOUL.md/IDENTITY.md/ROLE.md changes.
            // Identity files live outside the workspace, in the agent root directory.
            if let Err(error) = watcher.watch(identity_dir, RecursiveMode::NonRecursive) {
                tracing::warn!(%error, path = %identity_dir.display(), "failed to watch identity dir");
            }
        }

        tracing::info!("file watcher started");

        // Track config.toml content hash to skip no-op reloads
        let mut last_config_hash: u64 = std::fs::read(&config_path)
            .map(|bytes| {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut hasher);
                hasher.finish()
            })
            .unwrap_or(0);

        // Debounce loop: collect events for 2 seconds, then reload
        let debounce = Duration::from_secs(2);

        while let Ok(first) = rx.recv() {
            // Drain any additional events within the debounce window
            let mut changed_paths: Vec<PathBuf> = first.paths;
            while let Ok(event) = rx.recv_timeout(debounce) {
                changed_paths.extend(event.paths);
            }

            // Categorize what changed
            let mut config_changed = changed_paths.iter().any(|p| p.ends_with("config.toml"));
            let identity_changed = changed_paths.iter().any(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                matches!(name, "SOUL.md" | "IDENTITY.md" | "ROLE.md")
            });
            let skills_changed = changed_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("skills"));

            // Skip entirely if nothing relevant changed
            if !config_changed && !identity_changed && !skills_changed {
                continue;
            }

            // Skip config reload if file content hasn't actually changed
            if config_changed {
                let current_hash: u64 = std::fs::read(&config_path)
                    .map(|bytes| {
                        use std::hash::{Hash, Hasher};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        bytes.hash(&mut hasher);
                        hasher.finish()
                    })
                    .unwrap_or(0);
                if current_hash == last_config_hash {
                    config_changed = false;
                    // If config was the only thing that "changed", skip entirely
                    if !identity_changed && !skills_changed {
                        continue;
                    }
                } else {
                    last_config_hash = current_hash;
                }
            }

            let changed_summary: Vec<&str> = [
                config_changed.then_some("config"),
                identity_changed.then_some("identity"),
                skills_changed.then_some("skills"),
            ]
            .into_iter()
            .flatten()
            .collect();

            tracing::info!(
                changed = %changed_summary.join(", "),
                "file change detected, reloading"
            );

            // Reload config.toml if it changed
            let new_config = if config_changed {
                match Config::load_from_path(&config_path) {
                    Ok(config) => Some(config),
                    Err(error) => {
                        tracing::error!(%error, "failed to reload config.toml, keeping previous values");
                        None
                    }
                }
            } else {
                None
            };

            // Reload instance-level bindings, provider keys, and permissions
            if let Some(config) = &new_config {
                llm_manager.reload_config(config.llm.clone());

                bindings.store(Arc::new(config.bindings.clone()));
                tracing::info!("bindings reloaded ({} entries)", config.bindings.len());

                match crate::links::AgentLink::from_config(&config.links) {
                    Ok(links) => {
                        agent_links.store(Arc::new(links));
                        tracing::info!("agent links reloaded ({} entries)", config.links.len());
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to parse links from reloaded config");
                    }
                }

                agent_humans.store(Arc::new(config.humans.clone()));
                tracing::info!("agent humans reloaded ({} entries)", config.humans.len());

                if let Some(ref perms) = discord_permissions
                    && let Some(discord_config) = &config.messaging.discord
                {
                    let new_perms =
                        DiscordPermissions::from_config(discord_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("discord permissions reloaded");
                }

                if let Some(ref perms) = slack_permissions
                    && let Some(slack_config) = &config.messaging.slack
                {
                    let new_perms = SlackPermissions::from_config(slack_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("slack permissions reloaded");
                }

                if let Some(ref perms) = telegram_permissions
                    && let Some(telegram_config) = &config.messaging.telegram
                {
                    let new_perms =
                        TelegramPermissions::from_config(telegram_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("telegram permissions reloaded");
                }

                if let Some(ref perms) = twitch_permissions
                    && let Some(twitch_config) = &config.messaging.twitch
                {
                    let new_perms = TwitchPermissions::from_config(twitch_config, &config.bindings);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("twitch permissions reloaded");
                }

                if let Some(ref manager) = messaging_manager {
                    let rt = tokio::runtime::Handle::current();
                    let manager = manager.clone();
                    let config = config.clone();
                    let discord_permissions = discord_permissions.clone();
                    let slack_permissions = slack_permissions.clone();
                    let telegram_permissions = telegram_permissions.clone();
                    let twitch_permissions = twitch_permissions.clone();
                    let instance_dir = instance_dir.clone();

                    rt.spawn(async move {
                        match build_desired_configured_adapters(
                            &config,
                            &instance_dir,
                            discord_permissions,
                            slack_permissions,
                            telegram_permissions,
                            twitch_permissions,
                        ) {
                            Ok(desired) => {
                                if let Err(error) = manager.reconcile_configured(desired).await {
                                    tracing::warn!(%error, "messaging adapter reconciliation encountered errors");
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "failed to build desired messaging adapters from config change");
                            }
                        }
                    });
                }
            }

            // Apply reloads to each agent's RuntimeConfig
            for (agent_id, workspace, identity_dir, runtime_config, mcp_manager) in &agents {
                if let Some(config) = &new_config {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(runtime_config.reload_config(config, agent_id, mcp_manager));
                }

                if identity_changed {
                    let rt = tokio::runtime::Handle::current();
                    let identity = rt.block_on(crate::identity::Identity::load(identity_dir));
                    runtime_config.reload_identity(identity);
                }

                if skills_changed {
                    let rt = tokio::runtime::Handle::current();
                    let skills = rt.block_on(crate::skills::SkillSet::load(
                        &instance_dir.join("skills"),
                        &workspace.join("skills"),
                    ));
                    runtime_config.reload_skills(skills);
                }
            }
        }

        tracing::info!("file watcher stopped");
    })
}

fn build_desired_configured_adapters(
    config: &Config,
    instance_dir: &Path,
    discord_permissions: Option<Arc<arc_swap::ArcSwap<DiscordPermissions>>>,
    slack_permissions: Option<Arc<arc_swap::ArcSwap<SlackPermissions>>>,
    telegram_permissions: Option<Arc<arc_swap::ArcSwap<TelegramPermissions>>>,
    twitch_permissions: Option<Arc<arc_swap::ArcSwap<TwitchPermissions>>>,
) -> anyhow::Result<Vec<crate::messaging::ConfiguredAdapter>> {
    let mut desired = Vec::new();

    if let Some(discord_config) = &config.messaging.discord
        && discord_config.enabled
    {
        if !discord_config.token.is_empty() {
            let permissions_snapshot =
                DiscordPermissions::from_config(discord_config, &config.bindings);
            let permissions = discord_permissions.unwrap_or_else(|| {
                Arc::new(arc_swap::ArcSwap::from_pointee(
                    permissions_snapshot.clone(),
                ))
            });
            let fingerprint = format!(
                "token={}|dm={:?}|allow_bot_messages={}|permissions={}",
                secret_fingerprint(&discord_config.token),
                sorted_strings(discord_config.dm_allowed_users.clone()),
                discord_config.allow_bot_messages,
                discord_permissions_fingerprint(&permissions_snapshot)
            );
            desired.push(crate::messaging::ConfiguredAdapter::new(
                crate::messaging::discord::DiscordAdapter::new(
                    "discord",
                    &discord_config.token,
                    permissions,
                ),
                fingerprint,
            ));
        }

        for instance in discord_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.token.is_empty() {
                continue;
            }
            let permissions_snapshot =
                DiscordPermissions::from_instance_config(instance, &config.bindings);
            let fingerprint = format!(
                "token={}|dm={:?}|allow_bot_messages={}|permissions={}",
                secret_fingerprint(&instance.token),
                sorted_strings(instance.dm_allowed_users.clone()),
                instance.allow_bot_messages,
                discord_permissions_fingerprint(&permissions_snapshot)
            );
            desired.push(crate::messaging::ConfiguredAdapter::new(
                crate::messaging::discord::DiscordAdapter::new(
                    binding_runtime_adapter_key("discord", Some(instance.name.as_str())),
                    &instance.token,
                    Arc::new(arc_swap::ArcSwap::from_pointee(permissions_snapshot)),
                ),
                fingerprint,
            ));
        }
    }

    if let Some(slack_config) = &config.messaging.slack
        && slack_config.enabled
    {
        if !slack_config.bot_token.is_empty() && !slack_config.app_token.is_empty() {
            let permissions_snapshot =
                SlackPermissions::from_config(slack_config, &config.bindings);
            let permissions = slack_permissions.unwrap_or_else(|| {
                Arc::new(arc_swap::ArcSwap::from_pointee(
                    permissions_snapshot.clone(),
                ))
            });
            let fingerprint = format!(
                "bot_token={}|app_token={}|dm={:?}|commands={:?}|permissions={}",
                secret_fingerprint(&slack_config.bot_token),
                secret_fingerprint(&slack_config.app_token),
                sorted_strings(slack_config.dm_allowed_users.clone()),
                sorted_slack_commands(slack_config.commands.clone()),
                slack_permissions_fingerprint(&permissions_snapshot)
            );
            let adapter = crate::messaging::slack::SlackAdapter::new(
                "slack",
                &slack_config.bot_token,
                &slack_config.app_token,
                permissions,
                slack_config.commands.clone(),
            )?;
            desired.push(crate::messaging::ConfiguredAdapter::new(
                adapter,
                fingerprint,
            ));
        }

        for instance in slack_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.bot_token.is_empty() || instance.app_token.is_empty() {
                continue;
            }
            let permissions_snapshot =
                SlackPermissions::from_instance_config(instance, &config.bindings);
            let fingerprint = format!(
                "bot_token={}|app_token={}|dm={:?}|commands={:?}|permissions={}",
                secret_fingerprint(&instance.bot_token),
                secret_fingerprint(&instance.app_token),
                sorted_strings(instance.dm_allowed_users.clone()),
                sorted_slack_commands(instance.commands.clone()),
                slack_permissions_fingerprint(&permissions_snapshot)
            );
            let adapter = crate::messaging::slack::SlackAdapter::new(
                binding_runtime_adapter_key("slack", Some(instance.name.as_str())),
                &instance.bot_token,
                &instance.app_token,
                Arc::new(arc_swap::ArcSwap::from_pointee(permissions_snapshot)),
                instance.commands.clone(),
            )?;
            desired.push(crate::messaging::ConfiguredAdapter::new(
                adapter,
                fingerprint,
            ));
        }
    }

    if let Some(telegram_config) = &config.messaging.telegram
        && telegram_config.enabled
    {
        if !telegram_config.token.is_empty() {
            let permissions_snapshot =
                TelegramPermissions::from_config(telegram_config, &config.bindings);
            let permissions = telegram_permissions.unwrap_or_else(|| {
                Arc::new(arc_swap::ArcSwap::from_pointee(
                    permissions_snapshot.clone(),
                ))
            });
            let fingerprint = format!(
                "token={}|dm={:?}|permissions={}",
                secret_fingerprint(&telegram_config.token),
                sorted_strings(telegram_config.dm_allowed_users.clone()),
                telegram_permissions_fingerprint(&permissions_snapshot)
            );
            desired.push(crate::messaging::ConfiguredAdapter::new(
                crate::messaging::telegram::TelegramAdapter::new(
                    "telegram",
                    &telegram_config.token,
                    permissions,
                ),
                fingerprint,
            ));
        }

        for instance in telegram_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.token.is_empty() {
                continue;
            }
            let permissions_snapshot =
                TelegramPermissions::from_instance_config(instance, &config.bindings);
            let fingerprint = format!(
                "token={}|dm={:?}|permissions={}",
                secret_fingerprint(&instance.token),
                sorted_strings(instance.dm_allowed_users.clone()),
                telegram_permissions_fingerprint(&permissions_snapshot)
            );
            desired.push(crate::messaging::ConfiguredAdapter::new(
                crate::messaging::telegram::TelegramAdapter::new(
                    binding_runtime_adapter_key("telegram", Some(instance.name.as_str())),
                    &instance.token,
                    Arc::new(arc_swap::ArcSwap::from_pointee(permissions_snapshot)),
                ),
                fingerprint,
            ));
        }
    }

    if let Some(email_config) = &config.messaging.email
        && email_config.enabled
    {
        if !email_config.imap_host.is_empty() {
            let fingerprint = format!(
                "imap_host={};imap_port={};imap_username={};imap_password={};imap_use_tls={};smtp_host={};smtp_port={};smtp_username={};smtp_password={};smtp_use_starttls={};from_address={};from_name={:?};poll_interval_secs={};folders={:?};allowed_senders={:?};max_body_bytes={};max_attachment_bytes={}",
                email_config.imap_host,
                email_config.imap_port,
                secret_fingerprint(&email_config.imap_username),
                secret_fingerprint(&email_config.imap_password),
                email_config.imap_use_tls,
                email_config.smtp_host,
                email_config.smtp_port,
                secret_fingerprint(&email_config.smtp_username),
                secret_fingerprint(&email_config.smtp_password),
                email_config.smtp_use_starttls,
                email_config.from_address,
                email_config.from_name,
                email_config.poll_interval_secs,
                sorted_strings(email_config.folders.clone()),
                sorted_strings(email_config.allowed_senders.clone()),
                email_config.max_body_bytes,
                email_config.max_attachment_bytes
            );
            let adapter = crate::messaging::email::EmailAdapter::from_config(email_config)?;
            desired.push(crate::messaging::ConfiguredAdapter::new(
                adapter,
                fingerprint,
            ));
        }

        for instance in email_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.imap_host.is_empty() {
                continue;
            }
            let fingerprint = format!(
                "name={};imap_host={};imap_port={};imap_username={};imap_password={};imap_use_tls={};smtp_host={};smtp_port={};smtp_username={};smtp_password={};smtp_use_starttls={};from_address={};from_name={:?};poll_interval_secs={};folders={:?};allowed_senders={:?};max_body_bytes={};max_attachment_bytes={}",
                instance.name,
                instance.imap_host,
                instance.imap_port,
                secret_fingerprint(&instance.imap_username),
                secret_fingerprint(&instance.imap_password),
                instance.imap_use_tls,
                instance.smtp_host,
                instance.smtp_port,
                secret_fingerprint(&instance.smtp_username),
                secret_fingerprint(&instance.smtp_password),
                instance.smtp_use_starttls,
                instance.from_address,
                instance.from_name,
                instance.poll_interval_secs,
                sorted_strings(instance.folders.clone()),
                sorted_strings(instance.allowed_senders.clone()),
                instance.max_body_bytes,
                instance.max_attachment_bytes
            );
            let adapter = crate::messaging::email::EmailAdapter::from_instance_config(
                binding_runtime_adapter_key("email", Some(instance.name.as_str())),
                instance,
            )?;
            desired.push(crate::messaging::ConfiguredAdapter::new(
                adapter,
                fingerprint,
            ));
        }
    }

    if let Some(webhook_config) = &config.messaging.webhook
        && webhook_config.enabled
    {
        let fingerprint = format!(
            "port={};bind={};auth_token={:?}",
            webhook_config.port,
            webhook_config.bind,
            webhook_config.auth_token.as_deref().map(secret_fingerprint)
        );
        desired.push(crate::messaging::ConfiguredAdapter::new(
            crate::messaging::webhook::WebhookAdapter::new(
                webhook_config.port,
                &webhook_config.bind,
                webhook_config.auth_token.clone(),
            ),
            fingerprint,
        ));
    }

    if let Some(twitch_config) = &config.messaging.twitch
        && twitch_config.enabled
    {
        if !twitch_config.username.is_empty() && !twitch_config.oauth_token.is_empty() {
            let permissions_snapshot =
                TwitchPermissions::from_config(twitch_config, &config.bindings);
            let permissions = twitch_permissions.unwrap_or_else(|| {
                Arc::new(arc_swap::ArcSwap::from_pointee(
                    permissions_snapshot.clone(),
                ))
            });
            let fingerprint = format!(
                "username={};oauth_token={};client_id={:?};client_secret={:?};refresh_token={:?};channels={:?};trigger_prefix={:?};permissions={}",
                secret_fingerprint(&twitch_config.username),
                secret_fingerprint(&twitch_config.oauth_token),
                twitch_config.client_id.as_deref().map(secret_fingerprint),
                twitch_config
                    .client_secret
                    .as_deref()
                    .map(secret_fingerprint),
                twitch_config
                    .refresh_token
                    .as_deref()
                    .map(secret_fingerprint),
                sorted_strings(twitch_config.channels.clone()),
                twitch_config.trigger_prefix,
                twitch_permissions_fingerprint(&permissions_snapshot)
            );
            let adapter = crate::messaging::twitch::TwitchAdapter::new(
                "twitch",
                &twitch_config.username,
                &twitch_config.oauth_token,
                twitch_config.client_id.clone(),
                twitch_config.client_secret.clone(),
                twitch_config.refresh_token.clone(),
                Some(instance_dir.join("twitch_token.json")),
                twitch_config.channels.clone(),
                twitch_config.trigger_prefix.clone(),
                permissions,
            );
            desired.push(crate::messaging::ConfiguredAdapter::new(
                adapter,
                fingerprint,
            ));
        }

        for instance in twitch_config
            .instances
            .iter()
            .filter(|instance| instance.enabled)
        {
            if instance.username.is_empty() || instance.oauth_token.is_empty() {
                continue;
            }
            let permissions_snapshot =
                TwitchPermissions::from_instance_config(instance, &config.bindings);
            let fingerprint = format!(
                "username={};oauth_token={};client_id={:?};client_secret={:?};refresh_token={:?};channels={:?};trigger_prefix={:?};permissions={}",
                secret_fingerprint(&instance.username),
                secret_fingerprint(&instance.oauth_token),
                instance.client_id.as_deref().map(secret_fingerprint),
                instance.client_secret.as_deref().map(secret_fingerprint),
                instance.refresh_token.as_deref().map(secret_fingerprint),
                sorted_strings(instance.channels.clone()),
                instance.trigger_prefix,
                twitch_permissions_fingerprint(&permissions_snapshot)
            );
            let token_path =
                instance_dir.join(crate::config::named_twitch_token_file_name(&instance.name));
            let adapter = crate::messaging::twitch::TwitchAdapter::new(
                binding_runtime_adapter_key("twitch", Some(instance.name.as_str())),
                &instance.username,
                &instance.oauth_token,
                instance.client_id.clone(),
                instance.client_secret.clone(),
                instance.refresh_token.clone(),
                Some(token_path),
                instance.channels.clone(),
                instance.trigger_prefix.clone(),
                Arc::new(arc_swap::ArcSwap::from_pointee(permissions_snapshot)),
            );
            desired.push(crate::messaging::ConfiguredAdapter::new(
                adapter,
                fingerprint,
            ));
        }
    }

    Ok(desired)
}

fn sorted_strings(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

fn sorted_u64s(mut values: Vec<u64>) -> Vec<u64> {
    values.sort_unstable();
    values
}

fn sorted_i64s(mut values: Vec<i64>) -> Vec<i64> {
    values.sort_unstable();
    values
}

fn format_u64_map(map: &std::collections::HashMap<u64, Vec<u64>>) -> String {
    let mut entries = map
        .iter()
        .map(|(key, values)| (*key, sorted_u64s(values.clone())))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| *key);
    format!("{entries:?}")
}

fn format_string_map(map: &std::collections::HashMap<String, Vec<String>>) -> String {
    let mut entries = map
        .iter()
        .map(|(key, values)| (key.clone(), sorted_strings(values.clone())))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    format!("{entries:?}")
}

fn discord_permissions_fingerprint(permissions: &DiscordPermissions) -> String {
    format!(
        "guild_filter={:?};channel_filter={};dm_allowed_users={:?};allow_bot_messages={}",
        permissions.guild_filter.clone().map(sorted_u64s),
        format_u64_map(&permissions.channel_filter),
        sorted_u64s(permissions.dm_allowed_users.clone()),
        permissions.allow_bot_messages
    )
}

fn slack_permissions_fingerprint(permissions: &SlackPermissions) -> String {
    format!(
        "workspace_filter={:?};channel_filter={};dm_allowed_users={:?}",
        permissions.workspace_filter.clone().map(sorted_strings),
        format_string_map(&permissions.channel_filter),
        sorted_strings(permissions.dm_allowed_users.clone())
    )
}

fn telegram_permissions_fingerprint(permissions: &TelegramPermissions) -> String {
    format!(
        "chat_filter={:?};dm_allowed_users={:?}",
        permissions.chat_filter.clone().map(sorted_i64s),
        sorted_i64s(permissions.dm_allowed_users.clone())
    )
}

fn twitch_permissions_fingerprint(permissions: &TwitchPermissions) -> String {
    format!(
        "channel_filter={:?};allowed_users={:?}",
        permissions.channel_filter.clone().map(sorted_strings),
        sorted_strings(permissions.allowed_users.clone())
    )
}

fn secret_fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(&digest[..8])
}

fn sorted_slack_commands(
    mut commands: Vec<crate::config::SlackCommandConfig>,
) -> Vec<(String, String, Option<String>)> {
    let mut normalized = commands
        .drain(..)
        .map(|command| (command.command, command.agent_id, command.description))
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    normalized
}
