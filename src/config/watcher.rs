use std::path::PathBuf;
use std::sync::Arc;

use super::{
    Binding, Config, DiscordPermissions, RuntimeConfig, SignalPermissions, SlackPermissions,
    TelegramPermissions, TwitchPermissions, binding_runtime_adapter_key,
};

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
    signal_permissions: Option<Arc<arc_swap::ArcSwap<SignalPermissions>>>,
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

                if let Some(ref perms) = signal_permissions
                    && let Some(signal_config) = &config.messaging.signal
                {
                    let new_perms = SignalPermissions::from_config(signal_config);
                    perms.store(Arc::new(new_perms));
                    tracing::info!("signal permissions reloaded");
                }

                // Hot-start adapters that are newly enabled in the config
                if let Some(ref manager) = messaging_manager {
                    let rt = tokio::runtime::Handle::current();
                    let manager = manager.clone();
                    let config = config.clone();
                    let discord_permissions = discord_permissions.clone();
                    let slack_permissions = slack_permissions.clone();
                    let telegram_permissions = telegram_permissions.clone();
                    let twitch_permissions = twitch_permissions.clone();
                    let signal_permissions = signal_permissions.clone();
                    let instance_dir = instance_dir.clone();

                    rt.spawn(async move {
                        // Discord: start default + named instances that are enabled and not already running.
                        if let Some(discord_config) = &config.messaging.discord
                            && discord_config.enabled {
                                if !discord_config.token.is_empty() && !manager.has_adapter("discord").await {
                                    let permissions = match discord_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = DiscordPermissions::from_config(discord_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let adapter = crate::messaging::discord::DiscordAdapter::new(
                                        "discord",
                                        &discord_config.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start discord adapter from config change");
                                    }
                                }

                                for instance in discord_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "discord",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions are not hot-updated on
                                        // config reload because each instance owns its own
                                        // Arc<ArcSwap> with no external handle. Fixing this
                                        // requires either a permission-update method on the
                                        // Messaging trait or a shared handle registry. Permissions
                                        // will be correct after a full restart.
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        DiscordPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    let adapter = crate::messaging::discord::DiscordAdapter::new(
                                        runtime_key,
                                        &instance.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named discord adapter from config change");
                                    }
                                }
                            }

                        // Slack: start default + named instances that are enabled and not already running.
                        if let Some(slack_config) = &config.messaging.slack
                            && slack_config.enabled {
                                if !slack_config.bot_token.is_empty()
                                    && !slack_config.app_token.is_empty()
                                    && !manager.has_adapter("slack").await
                                {
                                    let permissions = match slack_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = SlackPermissions::from_config(slack_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    match crate::messaging::slack::SlackAdapter::new(
                                        "slack",
                                        &slack_config.bot_token,
                                        &slack_config.app_token,
                                        permissions,
                                        slack_config.commands.clone(),
                                    ) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, "failed to hot-start slack adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, "failed to build slack adapter from config change");
                                        }
                                    }
                                }

                                for instance in slack_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "slack",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        SlackPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    match crate::messaging::slack::SlackAdapter::new(
                                        runtime_key,
                                        &instance.bot_token,
                                        &instance.app_token,
                                        permissions,
                                        instance.commands.clone(),
                                    ) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, adapter = %instance.name, "failed to hot-start named slack adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, adapter = %instance.name, "failed to build named slack adapter from config change");
                                        }
                                    }
                                }
                            }

                        // Telegram: start default + named instances that are enabled and not already running.
                        if let Some(telegram_config) = &config.messaging.telegram
                            && telegram_config.enabled {
                                if !telegram_config.token.is_empty()
                                    && !manager.has_adapter("telegram").await
                                {
                                    let permissions = match telegram_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = TelegramPermissions::from_config(telegram_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                        "telegram",
                                        &telegram_config.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start telegram adapter from config change");
                                    }
                                }

                                for instance in telegram_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "telegram",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        TelegramPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                        runtime_key,
                                        &instance.token,
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named telegram adapter from config change");
                                    }
                                }
                            }

                        // Email: start default + named instances that are enabled and not already running.
                        if let Some(email_config) = &config.messaging.email
                            && email_config.enabled {
                                if !email_config.imap_host.is_empty() && !manager.has_adapter("email").await {
                                    match crate::messaging::email::EmailAdapter::from_config(email_config) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, "failed to hot-start email adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, "failed to build email adapter from config change");
                                        }
                                    }
                                }

                                for instance in email_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "email",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        continue;
                                    }

                                    match crate::messaging::email::EmailAdapter::from_instance_config(
                                        runtime_key.as_str(),
                                        instance,
                                    ) {
                                        Ok(adapter) => {
                                            if let Err(error) = manager.register_and_start(adapter).await {
                                                tracing::error!(%error, adapter = %instance.name, "failed to hot-start named email adapter from config change");
                                            }
                                        }
                                        Err(error) => {
                                            tracing::error!(%error, adapter = %instance.name, "failed to build named email adapter from config change");
                                        }
                                    }
                                }
                            }

                        // Twitch: start default + named instances that are enabled and not already running.
                        if let Some(twitch_config) = &config.messaging.twitch
                            && twitch_config.enabled {
                                if !twitch_config.username.is_empty()
                                    && !twitch_config.oauth_token.is_empty()
                                    && !manager.has_adapter("twitch").await
                                {
                                    let permissions = match twitch_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = TwitchPermissions::from_config(twitch_config, &config.bindings);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let token_path = instance_dir.join("twitch_token.json");
                                    let adapter = crate::messaging::twitch::TwitchAdapter::new(
                                        "twitch",
                                        &twitch_config.username,
                                        &twitch_config.oauth_token,
                                        twitch_config.client_id.clone(),
                                        twitch_config.client_secret.clone(),
                                        twitch_config.refresh_token.clone(),
                                        Some(token_path),
                                        twitch_config.channels.clone(),
                                        twitch_config.trigger_prefix.clone(),
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start twitch adapter from config change");
                                    }
                                }

                                for instance in twitch_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "twitch",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let token_file_name = {
                                        use std::hash::{Hash, Hasher};
                                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                                        instance.name.hash(&mut hasher);
                                        let name_hash = hasher.finish();
                                        format!(
                                            "twitch_token_{}_{name_hash:016x}.json",
                                            instance
                                                .name
                                                .chars()
                                                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                                                .collect::<String>()
                                        )
                                    };
                                    let token_path = instance_dir.join(token_file_name);
                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        TwitchPermissions::from_instance_config(instance, &config.bindings),
                                    ));
                                    let adapter = crate::messaging::twitch::TwitchAdapter::new(
                                        runtime_key,
                                        &instance.username,
                                        &instance.oauth_token,
                                        instance.client_id.clone(),
                                        instance.client_secret.clone(),
                                        instance.refresh_token.clone(),
                                        Some(token_path),
                                        instance.channels.clone(),
                                        instance.trigger_prefix.clone(),
                                        permissions,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named twitch adapter from config change");
                                    }
                                }
                            }

                        // Signal: start default + named instances that are enabled and not already running.
                        if let Some(signal_config) = &config.messaging.signal
                            && signal_config.enabled {
                                if !signal_config.http_url.is_empty()
                                    && !signal_config.account.is_empty()
                                    && !manager.has_adapter("signal").await
                                {
                                    let permissions = match signal_permissions {
                                        Some(ref existing) => existing.clone(),
                                        None => {
                                            let permissions = SignalPermissions::from_config(signal_config);
                                            Arc::new(arc_swap::ArcSwap::from_pointee(permissions))
                                        }
                                    };
                                    let tmp_dir = instance_dir.join("tmp");
                                    let adapter = crate::messaging::signal::SignalAdapter::new(
                                        "signal",
                                        &signal_config.http_url,
                                        &signal_config.account,
                                        signal_config.ignore_stories,
                                        permissions,
                                        tmp_dir,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to hot-start signal adapter from config change");
                                    }
                                }

                                for instance in signal_config.instances.iter().filter(|instance| instance.enabled) {
                                    let runtime_key = binding_runtime_adapter_key(
                                        "signal",
                                        Some(instance.name.as_str()),
                                    );
                                    if manager.has_adapter(runtime_key.as_str()).await {
                                        // TODO: named instance permissions not hot-updated (see discord block comment)
                                        continue;
                                    }

                                    let permissions = Arc::new(arc_swap::ArcSwap::from_pointee(
                                        SignalPermissions::from_instance_config(instance),
                                    ));
                                    let tmp_dir = instance_dir.join("tmp");
                                    let adapter = crate::messaging::signal::SignalAdapter::new(
                                        runtime_key,
                                        &instance.http_url,
                                        &instance.account,
                                        instance.ignore_stories,
                                        permissions,
                                        tmp_dir,
                                    );
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to hot-start named signal adapter from config change");
                                    }
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
