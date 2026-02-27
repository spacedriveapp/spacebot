use super::state::ApiState;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Clone)]
pub(super) struct PlatformStatus {
    configured: bool,
    enabled: bool,
}

#[derive(Serialize, Clone)]
pub(super) struct AdapterInstanceStatus {
    platform: String,
    /// `None` means the default instance for the platform.
    name: Option<String>,
    runtime_key: String,
    configured: bool,
    enabled: bool,
    binding_count: usize,
}

#[derive(Serialize)]
pub(super) struct MessagingStatusResponse {
    discord: PlatformStatus,
    slack: PlatformStatus,
    telegram: PlatformStatus,
    email: PlatformStatus,
    webhook: PlatformStatus,
    twitch: PlatformStatus,
    instances: Vec<AdapterInstanceStatus>,
}

#[derive(Deserialize)]
pub(super) struct DisconnectPlatformRequest {
    platform: String,
    #[serde(default)]
    adapter: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TogglePlatformRequest {
    platform: String,
    #[serde(default)]
    adapter: Option<String>,
    enabled: bool,
}

#[derive(Deserialize, Default)]
pub(super) struct InstanceCredentials {
    #[serde(default)]
    discord_token: Option<String>,
    #[serde(default)]
    slack_bot_token: Option<String>,
    #[serde(default)]
    slack_app_token: Option<String>,
    #[serde(default)]
    telegram_token: Option<String>,
    #[serde(default)]
    twitch_username: Option<String>,
    #[serde(default)]
    twitch_oauth_token: Option<String>,
    #[serde(default)]
    twitch_client_id: Option<String>,
    #[serde(default)]
    twitch_client_secret: Option<String>,
    #[serde(default)]
    twitch_refresh_token: Option<String>,
    // Email credentials
    #[serde(default)]
    email_imap_host: Option<String>,
    #[serde(default)]
    email_imap_port: Option<u16>,
    #[serde(default)]
    email_imap_username: Option<String>,
    #[serde(default)]
    email_imap_password: Option<String>,
    #[serde(default)]
    email_smtp_host: Option<String>,
    #[serde(default)]
    email_smtp_port: Option<u16>,
    #[serde(default)]
    email_smtp_username: Option<String>,
    #[serde(default)]
    email_smtp_password: Option<String>,
    #[serde(default)]
    email_from_address: Option<String>,
    // Webhook credentials
    #[serde(default)]
    webhook_port: Option<u16>,
    #[serde(default)]
    webhook_bind: Option<String>,
    #[serde(default)]
    webhook_auth_token: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CreateMessagingInstanceRequest {
    platform: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    credentials: InstanceCredentials,
}

#[derive(Deserialize)]
pub(super) struct DeleteMessagingInstanceRequest {
    platform: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize)]
pub(super) struct MessagingInstanceActionResponse {
    success: bool,
    message: String,
}

fn normalize_adapter_selector(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_string)
}

fn binding_count_for(
    bindings: Option<&toml_edit::ArrayOfTables>,
    platform: &str,
    adapter: Option<&str>,
) -> usize {
    let Some(bindings) = bindings else {
        return 0;
    };

    bindings
        .iter()
        .filter(|table| {
            let Some(channel) = table.get("channel").and_then(|value| value.as_str()) else {
                return false;
            };
            if channel != platform {
                return false;
            }

            let binding_adapter =
                normalize_adapter_selector(table.get("adapter").and_then(|value| value.as_str()));

            match (binding_adapter.as_deref(), adapter) {
                (None, None) => true,
                (Some(binding_adapter), Some(adapter)) => binding_adapter == adapter,
                _ => false,
            }
        })
        .count()
}

fn push_instance_status(
    instances: &mut Vec<AdapterInstanceStatus>,
    bindings: Option<&toml_edit::ArrayOfTables>,
    platform: &str,
    name: Option<String>,
    configured: bool,
    enabled: bool,
) {
    let runtime_key = crate::config::binding_runtime_adapter_key(platform, name.as_deref());
    let binding_count = binding_count_for(bindings, platform, name.as_deref());

    instances.push(AdapterInstanceStatus {
        platform: platform.to_string(),
        name,
        runtime_key,
        configured,
        enabled,
        binding_count,
    });
}

/// Get which messaging platforms are configured and enabled.
pub(super) async fn messaging_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<MessagingStatusResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (discord, slack, telegram, email, webhook, twitch, instances) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to read config.toml for messaging status");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        let doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
            tracing::warn!(%error, "failed to parse config.toml for messaging status");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut instances: Vec<AdapterInstanceStatus> = Vec::new();
        let bindings = doc
            .get("bindings")
            .and_then(|value| value.as_array_of_tables());

        let discord_status = doc
            .get("messaging")
            .and_then(|m| m.get("discord"))
            .map(|d| {
                let has_token = d
                    .get("token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enabled = d.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

                if has_token {
                    push_instance_status(&mut instances, bindings, "discord", None, true, enabled);
                }

                if let Some(named_instances) = d
                    .get("instances")
                    .and_then(|value| value.as_array_of_tables())
                {
                    for instance in named_instances {
                        let instance_name = normalize_adapter_selector(
                            instance.get("name").and_then(|value| value.as_str()),
                        );
                        let instance_enabled = instance
                            .get("enabled")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(true)
                            && enabled;
                        let instance_configured = instance
                            .get("token")
                            .and_then(|value| value.as_str())
                            .is_some_and(|token| !token.is_empty());

                        if let Some(instance_name) = instance_name
                            && instance_configured
                        {
                            push_instance_status(
                                &mut instances,
                                bindings,
                                "discord",
                                Some(instance_name),
                                true,
                                instance_enabled,
                            );
                        }
                    }
                }

                PlatformStatus {
                    configured: has_token,
                    enabled: has_token && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let slack_status = doc
            .get("messaging")
            .and_then(|m| m.get("slack"))
            .map(|s| {
                let has_bot_token = s
                    .get("bot_token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| !t.is_empty());
                let has_app_token = s
                    .get("app_token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| !t.is_empty());
                let enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

                if has_bot_token && has_app_token {
                    push_instance_status(&mut instances, bindings, "slack", None, true, enabled);
                }

                if let Some(named_instances) = s
                    .get("instances")
                    .and_then(|value| value.as_array_of_tables())
                {
                    for instance in named_instances {
                        let instance_name = normalize_adapter_selector(
                            instance.get("name").and_then(|value| value.as_str()),
                        );
                        let has_instance_bot = instance
                            .get("bot_token")
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| !value.is_empty());
                        let has_instance_app = instance
                            .get("app_token")
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| !value.is_empty());
                        let instance_enabled = instance
                            .get("enabled")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(true)
                            && enabled;

                        if let Some(instance_name) = instance_name
                            && has_instance_bot
                            && has_instance_app
                        {
                            push_instance_status(
                                &mut instances,
                                bindings,
                                "slack",
                                Some(instance_name),
                                true,
                                instance_enabled,
                            );
                        }
                    }
                }

                PlatformStatus {
                    configured: has_bot_token && has_app_token,
                    enabled: has_bot_token && has_app_token && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let webhook_status = doc
            .get("messaging")
            .and_then(|m| m.get("webhook"))
            .map(|w| {
                let enabled = w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

                push_instance_status(&mut instances, bindings, "webhook", None, true, enabled);

                PlatformStatus {
                    configured: true,
                    enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let email_status = doc
            .get("messaging")
            .and_then(|m| m.get("email"))
            .map(|email| {
                let has_imap_host = email
                    .get("imap_host")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let has_imap_username = email
                    .get("imap_username")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let has_imap_password = email
                    .get("imap_password")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let has_smtp_host = email
                    .get("smtp_host")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());

                let configured =
                    has_imap_host && has_imap_username && has_imap_password && has_smtp_host;

                let enabled = email
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if configured {
                    push_instance_status(&mut instances, bindings, "email", None, true, enabled);
                }

                PlatformStatus {
                    configured,
                    enabled: configured && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let telegram_status = doc
            .get("messaging")
            .and_then(|m| m.get("telegram"))
            .map(|t| {
                let has_token = t
                    .get("token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enabled = t.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

                if has_token {
                    push_instance_status(&mut instances, bindings, "telegram", None, true, enabled);
                }

                if let Some(named_instances) = t
                    .get("instances")
                    .and_then(|value| value.as_array_of_tables())
                {
                    for instance in named_instances {
                        let instance_name = normalize_adapter_selector(
                            instance.get("name").and_then(|value| value.as_str()),
                        );
                        let instance_enabled = instance
                            .get("enabled")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(true)
                            && enabled;
                        let instance_configured = instance
                            .get("token")
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| !value.is_empty());

                        if let Some(instance_name) = instance_name
                            && instance_configured
                        {
                            push_instance_status(
                                &mut instances,
                                bindings,
                                "telegram",
                                Some(instance_name),
                                true,
                                instance_enabled,
                            );
                        }
                    }
                }

                PlatformStatus {
                    configured: has_token,
                    enabled: has_token && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let twitch_status = doc
            .get("messaging")
            .and_then(|m| m.get("twitch"))
            .map(|t| {
                let has_username = t
                    .get("username")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let has_token = t
                    .get("oauth_token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enabled = t.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

                if has_username && has_token {
                    push_instance_status(&mut instances, bindings, "twitch", None, true, enabled);
                }

                if let Some(named_instances) = t
                    .get("instances")
                    .and_then(|value| value.as_array_of_tables())
                {
                    for instance in named_instances {
                        let instance_name = normalize_adapter_selector(
                            instance.get("name").and_then(|value| value.as_str()),
                        );
                        let instance_enabled = instance
                            .get("enabled")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(true)
                            && enabled;
                        let has_instance_username = instance
                            .get("username")
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| !value.is_empty());
                        let has_instance_token = instance
                            .get("oauth_token")
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| !value.is_empty());

                        if let Some(instance_name) = instance_name
                            && has_instance_username
                            && has_instance_token
                        {
                            push_instance_status(
                                &mut instances,
                                bindings,
                                "twitch",
                                Some(instance_name),
                                true,
                                instance_enabled,
                            );
                        }
                    }
                }

                PlatformStatus {
                    configured: has_username && has_token,
                    enabled: has_username && has_token && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        (
            discord_status,
            slack_status,
            telegram_status,
            email_status,
            webhook_status,
            twitch_status,
            instances,
        )
    } else {
        let default = PlatformStatus {
            configured: false,
            enabled: false,
        };
        (
            default.clone(),
            default.clone(),
            default.clone(),
            default.clone(),
            default.clone(),
            default,
            Vec::new(),
        )
    };

    Ok(Json(MessagingStatusResponse {
        discord,
        slack,
        telegram,
        email,
        webhook,
        twitch,
        instances,
    }))
}

/// Disconnect a messaging platform: remove credentials from config, remove all
/// bindings for that platform, and shut down the adapter.
///
/// When `adapter` is set, only the specified named instance is disconnected.
/// When `adapter` is absent, the entire platform section (default + all named
/// instances) is removed.
pub(super) async fn disconnect_platform(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DisconnectPlatformRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let platform = &request.platform;
    let adapter_name = request
        .adapter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let config_path = state.config_path.read().await.clone();

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if let Some(adapter_name) = adapter_name {
        // Disconnect a specific named instance only
        if let Some(instances) = doc
            .get_mut("messaging")
            .and_then(|m| m.get_mut(platform.as_str()))
            .and_then(|p| p.get_mut("instances"))
            .and_then(|item| item.as_array_of_tables_mut())
        {
            let mut index = 0;
            while index < instances.len() {
                let matches = instances
                    .get(index)
                    .and_then(|t| t.get("name"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| name == adapter_name);
                if matches {
                    instances.remove(index);
                    break;
                }
                index += 1;
            }
        }

        // Remove bindings targeting this specific adapter
        if let Some(bindings) = doc
            .get_mut("bindings")
            .and_then(|b| b.as_array_of_tables_mut())
        {
            let mut index = 0;
            while index < bindings.len() {
                let binding_channel = bindings
                    .get(index)
                    .and_then(|t| t.get("channel"))
                    .and_then(|v| v.as_str());
                let binding_adapter = bindings
                    .get(index)
                    .and_then(|t| t.get("adapter"))
                    .and_then(|v| v.as_str());

                if binding_channel.is_some_and(|ch| ch == platform)
                    && binding_adapter.is_some_and(|ba| ba == adapter_name)
                {
                    bindings.remove(index);
                } else {
                    index += 1;
                }
            }
        }
    } else {
        // Disconnect entire platform — remove the whole section and all bindings
        if let Some(messaging) = doc.get_mut("messaging").and_then(|m| m.as_table_mut()) {
            messaging.remove(platform);
        }

        if let Some(bindings) = doc
            .get_mut("bindings")
            .and_then(|b| b.as_array_of_tables_mut())
        {
            let mut i = 0;
            while i < bindings.len() {
                let matches = bindings
                    .get(i)
                    .and_then(|t| t.get("channel"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|ch| ch == platform);
                if matches {
                    bindings.remove(i);
                } else {
                    i += 1;
                }
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
    }

    let manager_guard = state.messaging_manager.read().await;
    if let Some(manager) = manager_guard.as_ref() {
        if let Some(adapter_name) = adapter_name {
            // Disconnect only the specific named adapter
            let runtime_key =
                crate::config::binding_runtime_adapter_key(platform, Some(adapter_name));
            if let Err(error) = manager.remove_adapter(&runtime_key).await {
                tracing::warn!(%error, adapter = %runtime_key, "failed to shut down adapter during disconnect");
            }
        } else {
            // Disconnect all adapters for this platform
            if let Err(error) = manager.remove_platform_adapters(platform).await {
                tracing::warn!(%error, platform = %platform, "failed to shut down adapters during disconnect");
            }
        }
    }

    if platform == "twitch" {
        let instance_dir = state.instance_dir.load();
        if let Some(name) = adapter_name {
            let safe_name: String = name
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect();
            let token_path = instance_dir.join(format!("twitch_token_{safe_name}.json"));
            match tokio::fs::remove_file(&token_path).await {
                Ok(()) => {
                    tracing::info!(path = %token_path.display(), "twitch token file deleted");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %token_path.display(),
                        "failed to delete twitch token file"
                    );
                }
            }
        } else {
            let token_path = instance_dir.join("twitch_token.json");
            match tokio::fs::remove_file(&token_path).await {
                Ok(()) => {
                    tracing::info!(path = %token_path.display(), "twitch token file deleted");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %token_path.display(),
                        "failed to delete twitch token file"
                    );
                }
            }
        }
    }

    let label = if let Some(name) = adapter_name {
        format!("{platform}:{name}")
    } else {
        platform.to_string()
    };
    tracing::info!(platform = %platform, adapter = %label, "platform disconnected via API");

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("{label} disconnected")
    })))
}

/// Toggle a messaging platform's enabled state. When disabling, shuts down the
/// adapter. When enabling, reads credentials from config and hot-starts it.
pub(super) async fn toggle_platform(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TogglePlatformRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let platform = &request.platform;
    let config_path = state.config_path.read().await.clone();

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let platform_table = doc
        .get_mut("messaging")
        .and_then(|m| m.as_table_mut())
        .and_then(|m| m.get_mut(platform.as_str()))
        .and_then(|p| p.as_table_mut());

    let Some(table) = platform_table else {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": format!("{platform} is not configured")
        })));
    };

    if let Some(adapter_name) = &request.adapter {
        // Toggle a specific named instance
        let adapter_name = adapter_name.trim();
        let mut found = false;
        if let Some(instances) = table
            .get_mut("instances")
            .and_then(|item| item.as_array_of_tables_mut())
        {
            for instance in instances.iter_mut() {
                if instance
                    .get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n == adapter_name)
                {
                    instance["enabled"] = toml_edit::value(request.enabled);
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return Ok(Json(serde_json::json!({
                "success": false,
                "message": format!("instance '{adapter_name}' not found for {platform}")
            })));
        }
    } else {
        // Toggle the default (root-level) instance
        table["enabled"] = toml_edit::value(request.enabled);
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let manager_guard = state.messaging_manager.read().await;
    let manager = manager_guard.as_ref();

    if request.enabled {
        if let Ok(new_config) = crate::config::Config::load_from_path(&config_path)
            && let Some(manager) = manager
        {
            match platform.as_str() {
                "discord" => {
                    if let Some(discord_config) = &new_config.messaging.discord {
                        if !discord_config.token.is_empty() {
                            let perms = {
                                let perms_guard = state.discord_permissions.read().await;
                                match perms_guard.as_ref() {
                                    Some(existing) => existing.clone(),
                                    None => {
                                        drop(perms_guard);
                                        let perms = crate::config::DiscordPermissions::from_config(
                                            discord_config,
                                            &new_config.bindings,
                                        );
                                        let arc_swap = std::sync::Arc::new(
                                            arc_swap::ArcSwap::from_pointee(perms),
                                        );
                                        state.set_discord_permissions(arc_swap.clone()).await;
                                        arc_swap
                                    }
                                }
                            };
                            let adapter = crate::messaging::discord::DiscordAdapter::new(
                                "discord",
                                &discord_config.token,
                                perms,
                            );
                            if let Err(error) = manager.register_and_start(adapter).await {
                                tracing::error!(%error, "failed to start discord adapter on toggle");
                            }
                        }

                        for instance in discord_config
                            .instances
                            .iter()
                            .filter(|instance| instance.enabled)
                        {
                            let runtime_key = crate::config::binding_runtime_adapter_key(
                                "discord",
                                Some(instance.name.as_str()),
                            );
                            let perms = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                                crate::config::DiscordPermissions::from_instance_config(
                                    instance,
                                    &new_config.bindings,
                                ),
                            ));
                            let adapter = crate::messaging::discord::DiscordAdapter::new(
                                runtime_key,
                                &instance.token,
                                perms,
                            );
                            if let Err(error) = manager.register_and_start(adapter).await {
                                tracing::error!(%error, adapter = %instance.name, "failed to start named discord adapter on toggle");
                            }
                        }
                    }
                }
                "slack" => {
                    if let Some(slack_config) = &new_config.messaging.slack {
                        if !slack_config.bot_token.is_empty() && !slack_config.app_token.is_empty()
                        {
                            let perms = {
                                let perms_guard = state.slack_permissions.read().await;
                                match perms_guard.as_ref() {
                                    Some(existing) => existing.clone(),
                                    None => {
                                        drop(perms_guard);
                                        let perms = crate::config::SlackPermissions::from_config(
                                            slack_config,
                                            &new_config.bindings,
                                        );
                                        let arc_swap = std::sync::Arc::new(
                                            arc_swap::ArcSwap::from_pointee(perms),
                                        );
                                        state.set_slack_permissions(arc_swap.clone()).await;
                                        arc_swap
                                    }
                                }
                            };
                            match crate::messaging::slack::SlackAdapter::new(
                                "slack",
                                &slack_config.bot_token,
                                &slack_config.app_token,
                                perms,
                                slack_config.commands.clone(),
                            ) {
                                Ok(adapter) => {
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, "failed to start slack adapter on toggle");
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%error, "failed to build slack adapter on toggle");
                                }
                            }
                        }

                        for instance in slack_config
                            .instances
                            .iter()
                            .filter(|instance| instance.enabled)
                        {
                            let runtime_key = crate::config::binding_runtime_adapter_key(
                                "slack",
                                Some(instance.name.as_str()),
                            );
                            let perms = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                                crate::config::SlackPermissions::from_instance_config(
                                    instance,
                                    &new_config.bindings,
                                ),
                            ));
                            match crate::messaging::slack::SlackAdapter::new(
                                runtime_key,
                                &instance.bot_token,
                                &instance.app_token,
                                perms,
                                instance.commands.clone(),
                            ) {
                                Ok(adapter) => {
                                    if let Err(error) = manager.register_and_start(adapter).await {
                                        tracing::error!(%error, adapter = %instance.name, "failed to start named slack adapter on toggle");
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%error, adapter = %instance.name, "failed to build named slack adapter on toggle");
                                }
                            }
                        }
                    }
                }
                "telegram" => {
                    if let Some(telegram_config) = &new_config.messaging.telegram {
                        if !telegram_config.token.is_empty() {
                            let perms = crate::config::TelegramPermissions::from_config(
                                telegram_config,
                                &new_config.bindings,
                            );
                            let arc_swap =
                                std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                            let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                "telegram",
                                &telegram_config.token,
                                arc_swap,
                            );
                            if let Err(error) = manager.register_and_start(adapter).await {
                                tracing::error!(%error, "failed to start telegram adapter on toggle");
                            }
                        }

                        for instance in telegram_config
                            .instances
                            .iter()
                            .filter(|instance| instance.enabled)
                        {
                            let runtime_key = crate::config::binding_runtime_adapter_key(
                                "telegram",
                                Some(instance.name.as_str()),
                            );
                            let perms = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                                crate::config::TelegramPermissions::from_instance_config(
                                    instance,
                                    &new_config.bindings,
                                ),
                            ));
                            let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                runtime_key,
                                &instance.token,
                                perms,
                            );
                            if let Err(error) = manager.register_and_start(adapter).await {
                                tracing::error!(%error, adapter = %instance.name, "failed to start named telegram adapter on toggle");
                            }
                        }
                    }
                }
                "email" => {
                    if let Some(email_config) = &new_config.messaging.email {
                        match crate::messaging::email::EmailAdapter::from_config(email_config) {
                            Ok(adapter) => {
                                if let Err(error) = manager.register_and_start(adapter).await {
                                    tracing::error!(%error, "failed to start email adapter on toggle");
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, "failed to build email adapter on toggle");
                            }
                        }
                    }
                }
                "webhook" => {
                    if let Some(webhook_config) = &new_config.messaging.webhook {
                        let adapter = crate::messaging::webhook::WebhookAdapter::new(
                            webhook_config.port,
                            &webhook_config.bind,
                            webhook_config.auth_token.clone(),
                        );
                        if let Err(error) = manager.register_and_start(adapter).await {
                            tracing::error!(%error, "failed to start webhook adapter on toggle");
                        }
                    }
                }
                "twitch" => {
                    if let Some(twitch_config) = &new_config.messaging.twitch {
                        if !twitch_config.username.is_empty()
                            && !twitch_config.oauth_token.is_empty()
                        {
                            let perms = crate::config::TwitchPermissions::from_config(
                                twitch_config,
                                &new_config.bindings,
                            );
                            let arc_swap =
                                std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                            let instance_dir = state.instance_dir.load();
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
                                arc_swap,
                            );
                            if let Err(error) = manager.register_and_start(adapter).await {
                                tracing::error!(%error, "failed to start twitch adapter on toggle");
                            }
                        }

                        for instance in twitch_config
                            .instances
                            .iter()
                            .filter(|instance| instance.enabled)
                        {
                            let runtime_key = crate::config::binding_runtime_adapter_key(
                                "twitch",
                                Some(instance.name.as_str()),
                            );
                            let token_file_name = format!(
                                "twitch_token_{}.json",
                                instance
                                    .name
                                    .chars()
                                    .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                                    .collect::<String>()
                            );
                            let instance_dir = state.instance_dir.load();
                            let token_path = instance_dir.join(token_file_name);
                            let perms = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                                crate::config::TwitchPermissions::from_instance_config(
                                    instance,
                                    &new_config.bindings,
                                ),
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
                                perms,
                            );
                            if let Err(error) = manager.register_and_start(adapter).await {
                                tracing::error!(%error, adapter = %instance.name, "failed to start named twitch adapter on toggle");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    } else if let Some(manager) = manager {
        if let Some(ref adapter_name) = request.adapter {
            // Shut down only the specific named instance.
            let runtime_key =
                crate::config::binding_runtime_adapter_key(platform, Some(adapter_name.trim()));
            if let Err(error) = manager.remove_adapter(&runtime_key).await {
                tracing::warn!(%error, adapter = %runtime_key, "failed to shut down named adapter on toggle");
            }
        } else if let Err(error) = manager.remove_platform_adapters(platform).await {
            tracing::warn!(%error, platform = %platform, "failed to shut down adapters on toggle");
        }
    }

    let action = if request.enabled {
        "enabled"
    } else {
        "disabled"
    };
    tracing::info!(platform = %platform, action, "platform toggled via API");

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("{platform} {action}")
    })))
}

/// Create a new adapter instance for a platform.
///
/// If `name` is `None`, creates/updates the default instance (root-level credentials).
/// If `name` is `Some`, adds a `[[messaging.<platform>.instances]]` entry.
/// Starts the adapter at runtime and reloads bindings.
pub(super) async fn create_messaging_instance(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateMessagingInstanceRequest>,
) -> Result<Json<MessagingInstanceActionResponse>, StatusCode> {
    let platform = &request.platform;

    if !matches!(
        platform.as_str(),
        "discord" | "slack" | "telegram" | "twitch" | "email" | "webhook"
    ) {
        return Ok(Json(MessagingInstanceActionResponse {
            success: false,
            message: format!("instances are not supported for '{platform}'"),
        }));
    }

    // Validate instance name
    if let Some(name) = &request.name {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
            return Ok(Json(MessagingInstanceActionResponse {
                success: false,
                message: "instance name cannot be empty or 'default'".to_string(),
            }));
        }
        if trimmed.contains(':') || trimmed.contains(' ') {
            return Ok(Json(MessagingInstanceActionResponse {
                success: false,
                message: "instance name cannot contain ':' or spaces".to_string(),
            }));
        }
    }

    let config_path = state.config_path.read().await.clone();
    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Ensure [messaging] and [messaging.<platform>] tables exist
    if doc.get("messaging").is_none() {
        doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let messaging = doc["messaging"].as_table_mut().ok_or_else(|| {
        tracing::warn!("messaging is not a table");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if messaging.get(platform.as_str()).is_none() {
        let mut platform_table = toml_edit::Table::new();
        platform_table["enabled"] = toml_edit::value(true);
        messaging[platform.as_str()] = toml_edit::Item::Table(platform_table);
    }
    let platform_table = messaging
        .get_mut(platform.as_str())
        .and_then(|item| item.as_table_mut())
        .ok_or_else(|| {
            tracing::warn!(platform = %platform, "platform config is not a table");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let credentials = &request.credentials;
    let enabled = request.enabled.unwrap_or(true);

    match request.name {
        None => {
            // Default instance — set root-level credential fields
            match platform.as_str() {
                "discord" => {
                    if let Some(token) = &credentials.discord_token {
                        platform_table["token"] = toml_edit::value(token.as_str());
                    }
                }
                "slack" => {
                    if let Some(token) = &credentials.slack_bot_token {
                        platform_table["bot_token"] = toml_edit::value(token.as_str());
                    }
                    if let Some(token) = &credentials.slack_app_token {
                        platform_table["app_token"] = toml_edit::value(token.as_str());
                    }
                }
                "telegram" => {
                    if let Some(token) = &credentials.telegram_token {
                        platform_table["token"] = toml_edit::value(token.as_str());
                    }
                }
                "twitch" => {
                    if let Some(username) = &credentials.twitch_username {
                        platform_table["username"] = toml_edit::value(username.as_str());
                    }
                    if let Some(token) = &credentials.twitch_oauth_token {
                        platform_table["oauth_token"] = toml_edit::value(token.as_str());
                    }
                    if let Some(client_id) = &credentials.twitch_client_id {
                        platform_table["client_id"] = toml_edit::value(client_id.as_str());
                    }
                    if let Some(client_secret) = &credentials.twitch_client_secret {
                        platform_table["client_secret"] = toml_edit::value(client_secret.as_str());
                    }
                    if let Some(refresh) = &credentials.twitch_refresh_token {
                        platform_table["refresh_token"] = toml_edit::value(refresh.as_str());
                    }
                }
                "email" => {
                    if let Some(host) = &credentials.email_imap_host {
                        platform_table["imap_host"] = toml_edit::value(host.as_str());
                    }
                    if let Some(port) = credentials.email_imap_port {
                        platform_table["imap_port"] = toml_edit::value(i64::from(port));
                    }
                    if let Some(username) = &credentials.email_imap_username {
                        platform_table["imap_username"] = toml_edit::value(username.as_str());
                    }
                    if let Some(password) = &credentials.email_imap_password {
                        platform_table["imap_password"] = toml_edit::value(password.as_str());
                    }
                    if let Some(host) = &credentials.email_smtp_host {
                        platform_table["smtp_host"] = toml_edit::value(host.as_str());
                    }
                    if let Some(port) = credentials.email_smtp_port {
                        platform_table["smtp_port"] = toml_edit::value(i64::from(port));
                    }
                    if let Some(username) = &credentials.email_smtp_username {
                        platform_table["smtp_username"] = toml_edit::value(username.as_str());
                    }
                    if let Some(password) = &credentials.email_smtp_password {
                        platform_table["smtp_password"] = toml_edit::value(password.as_str());
                    }
                    if let Some(from) = &credentials.email_from_address {
                        platform_table["from_address"] = toml_edit::value(from.as_str());
                    }
                }
                "webhook" => {
                    if let Some(port) = credentials.webhook_port {
                        platform_table["port"] = toml_edit::value(i64::from(port));
                    }
                    if let Some(bind) = &credentials.webhook_bind {
                        platform_table["bind"] = toml_edit::value(bind.as_str());
                    }
                    if let Some(token) = &credentials.webhook_auth_token {
                        platform_table["auth_token"] = toml_edit::value(token.as_str());
                    }
                }
                _ => {}
            }
            platform_table["enabled"] = toml_edit::value(enabled);
        }
        Some(ref name) => {
            // Named instance — add to [[messaging.<platform>.instances]]
            let instance_name = name.trim().to_string();

            // Check for duplicates
            if let Some(existing_instances) = platform_table
                .get("instances")
                .and_then(|item| item.as_array_of_tables())
            {
                for existing in existing_instances {
                    if existing
                        .get("name")
                        .and_then(|value| value.as_str())
                        .is_some_and(|name| name == instance_name)
                    {
                        return Ok(Json(MessagingInstanceActionResponse {
                            success: false,
                            message: format!(
                                "instance '{instance_name}' already exists for {platform}"
                            ),
                        }));
                    }
                }
            }

            let mut instance_table = toml_edit::Table::new();
            instance_table["name"] = toml_edit::value(&instance_name);
            instance_table["enabled"] = toml_edit::value(enabled);

            match platform.as_str() {
                "discord" => {
                    if let Some(token) = &credentials.discord_token {
                        instance_table["token"] = toml_edit::value(token.as_str());
                    }
                }
                "slack" => {
                    if let Some(token) = &credentials.slack_bot_token {
                        instance_table["bot_token"] = toml_edit::value(token.as_str());
                    }
                    if let Some(token) = &credentials.slack_app_token {
                        instance_table["app_token"] = toml_edit::value(token.as_str());
                    }
                }
                "telegram" => {
                    if let Some(token) = &credentials.telegram_token {
                        instance_table["token"] = toml_edit::value(token.as_str());
                    }
                }
                "twitch" => {
                    if let Some(username) = &credentials.twitch_username {
                        instance_table["username"] = toml_edit::value(username.as_str());
                    }
                    if let Some(token) = &credentials.twitch_oauth_token {
                        instance_table["oauth_token"] = toml_edit::value(token.as_str());
                    }
                    if let Some(client_id) = &credentials.twitch_client_id {
                        instance_table["client_id"] = toml_edit::value(client_id.as_str());
                    }
                    if let Some(client_secret) = &credentials.twitch_client_secret {
                        instance_table["client_secret"] = toml_edit::value(client_secret.as_str());
                    }
                    if let Some(refresh) = &credentials.twitch_refresh_token {
                        instance_table["refresh_token"] = toml_edit::value(refresh.as_str());
                    }
                }
                "email" => {
                    if let Some(host) = &credentials.email_imap_host {
                        instance_table["imap_host"] = toml_edit::value(host.as_str());
                    }
                    if let Some(port) = credentials.email_imap_port {
                        instance_table["imap_port"] = toml_edit::value(i64::from(port));
                    }
                    if let Some(username) = &credentials.email_imap_username {
                        instance_table["imap_username"] = toml_edit::value(username.as_str());
                    }
                    if let Some(password) = &credentials.email_imap_password {
                        instance_table["imap_password"] = toml_edit::value(password.as_str());
                    }
                    if let Some(host) = &credentials.email_smtp_host {
                        instance_table["smtp_host"] = toml_edit::value(host.as_str());
                    }
                    if let Some(port) = credentials.email_smtp_port {
                        instance_table["smtp_port"] = toml_edit::value(i64::from(port));
                    }
                    if let Some(username) = &credentials.email_smtp_username {
                        instance_table["smtp_username"] = toml_edit::value(username.as_str());
                    }
                    if let Some(password) = &credentials.email_smtp_password {
                        instance_table["smtp_password"] = toml_edit::value(password.as_str());
                    }
                    if let Some(from) = &credentials.email_from_address {
                        instance_table["from_address"] = toml_edit::value(from.as_str());
                    }
                }
                "webhook" => {
                    if let Some(port) = credentials.webhook_port {
                        instance_table["port"] = toml_edit::value(i64::from(port));
                    }
                    if let Some(bind) = &credentials.webhook_bind {
                        instance_table["bind"] = toml_edit::value(bind.as_str());
                    }
                    if let Some(token) = &credentials.webhook_auth_token {
                        instance_table["auth_token"] = toml_edit::value(token.as_str());
                    }
                }
                _ => {}
            }

            // Append the new instance table
            if platform_table.get("instances").is_none() {
                platform_table.insert(
                    "instances",
                    toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
                );
            }
            if let Some(instances) = platform_table
                .get_mut("instances")
                .and_then(|item| item.as_array_of_tables_mut())
            {
                instances.push(instance_table);
            }
        }
    }

    // Write updated config
    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Reload config and hot-start the new adapter
    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
    }

    // The file watcher will pick up the change and start the adapter.
    // We don't duplicate the adapter-start logic here — the hot-reload
    // path in config.rs handles creating and registering all adapters.

    let label = if let Some(name) = &request.name {
        format!("{platform}:{}", name.trim())
    } else {
        format!("{platform} (default)")
    };
    tracing::info!(platform = %platform, instance = %label, "messaging instance created via API");

    Ok(Json(MessagingInstanceActionResponse {
        success: true,
        message: format!("{label} instance created"),
    }))
}

/// Delete a named adapter instance (or the default instance) for a platform.
///
/// Removes the instance entry from config.toml, removes bindings targeting
/// that adapter, and shuts down the runtime adapter.
pub(super) async fn delete_messaging_instance(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DeleteMessagingInstanceRequest>,
) -> Result<Json<MessagingInstanceActionResponse>, StatusCode> {
    let platform = &request.platform;
    let adapter_name = request.name.as_deref().map(str::trim);

    if !matches!(
        platform.as_str(),
        "discord" | "slack" | "telegram" | "twitch" | "email" | "webhook"
    ) {
        return Ok(Json(MessagingInstanceActionResponse {
            success: false,
            message: format!("instances are not supported for '{platform}'"),
        }));
    }

    let config_path = state.config_path.read().await.clone();
    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let runtime_key = crate::config::binding_runtime_adapter_key(platform, adapter_name);

    if let Some(adapter_name) = adapter_name {
        // Remove a named instance from [[messaging.<platform>.instances]]
        let removed = if let Some(instances) = doc
            .get_mut("messaging")
            .and_then(|m| m.get_mut(platform.as_str()))
            .and_then(|p| p.get_mut("instances"))
            .and_then(|item| item.as_array_of_tables_mut())
        {
            let mut found = false;
            let mut index = 0;
            while index < instances.len() {
                let matches = instances
                    .get(index)
                    .and_then(|t| t.get("name"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| name == adapter_name);
                if matches {
                    instances.remove(index);
                    found = true;
                    break;
                }
                index += 1;
            }
            found
        } else {
            false
        };

        if !removed {
            return Ok(Json(MessagingInstanceActionResponse {
                success: false,
                message: format!("instance '{adapter_name}' not found for {platform}"),
            }));
        }
    } else {
        // Remove default instance — clear root-level credentials
        if let Some(table) = doc
            .get_mut("messaging")
            .and_then(|m| m.get_mut(platform.as_str()))
            .and_then(|p| p.as_table_mut())
        {
            match platform.as_str() {
                "discord" => {
                    table.remove("token");
                }
                "slack" => {
                    table.remove("bot_token");
                    table.remove("app_token");
                }
                "telegram" => {
                    table.remove("token");
                }
                "twitch" => {
                    table.remove("username");
                    table.remove("oauth_token");
                    table.remove("client_id");
                    table.remove("client_secret");
                    table.remove("refresh_token");
                }
                "email" => {
                    table.remove("imap_host");
                    table.remove("imap_port");
                    table.remove("imap_username");
                    table.remove("imap_password");
                    table.remove("smtp_host");
                    table.remove("smtp_port");
                    table.remove("smtp_username");
                    table.remove("smtp_password");
                    table.remove("from_address");
                }
                "webhook" => {
                    table.remove("port");
                    table.remove("bind");
                    table.remove("auth_token");
                }
                _ => {}
            }
        }
    }

    // Remove bindings targeting this adapter
    if let Some(bindings) = doc
        .get_mut("bindings")
        .and_then(|b| b.as_array_of_tables_mut())
    {
        let mut index = 0;
        while index < bindings.len() {
            let binding_channel = bindings
                .get(index)
                .and_then(|t| t.get("channel"))
                .and_then(|v| v.as_str());
            let binding_adapter = bindings
                .get(index)
                .and_then(|t| t.get("adapter"))
                .and_then(|v| v.as_str());

            let matches_platform = binding_channel.is_some_and(|ch| ch == platform);
            let matches_adapter = match (binding_adapter, adapter_name) {
                (None, None) => true,
                (Some(ba), Some(an)) => ba == an,
                _ => false,
            };

            if matches_platform && matches_adapter {
                bindings.remove(index);
            } else {
                index += 1;
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Reload bindings
    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
    }

    // Shut down the runtime adapter
    let manager_guard = state.messaging_manager.read().await;
    if let Some(manager) = manager_guard.as_ref()
        && let Err(error) = manager.remove_adapter(&runtime_key).await
    {
        tracing::warn!(%error, adapter = %runtime_key, "failed to shut down adapter during instance delete");
    }

    // Clean up twitch token file if applicable
    if platform == "twitch" {
        let instance_dir = state.instance_dir.load();
        let token_path = if let Some(name) = adapter_name {
            let safe_name: String = name
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect();
            instance_dir.join(format!("twitch_token_{safe_name}.json"))
        } else {
            instance_dir.join("twitch_token.json")
        };
        match tokio::fs::remove_file(&token_path).await {
            Ok(()) => {
                tracing::info!(path = %token_path.display(), "twitch token file deleted");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %token_path.display(),
                    "failed to delete twitch token file"
                );
            }
        }
    }

    tracing::info!(platform = %platform, adapter = %runtime_key, "messaging instance deleted via API");

    Ok(Json(MessagingInstanceActionResponse {
        success: true,
        message: format!("{runtime_key} instance deleted"),
    }))
}
