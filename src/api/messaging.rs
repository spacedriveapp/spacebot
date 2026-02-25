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

#[derive(Serialize)]
pub(super) struct MessagingStatusResponse {
    discord: PlatformStatus,
    slack: PlatformStatus,
    telegram: PlatformStatus,
    webhook: PlatformStatus,
    twitch: PlatformStatus,
}

#[derive(Deserialize)]
pub(super) struct DisconnectPlatformRequest {
    platform: String,
}

#[derive(Deserialize)]
pub(super) struct TogglePlatformRequest {
    platform: String,
    enabled: bool,
}

/// Get which messaging platforms are configured and enabled.
pub(super) async fn messaging_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<MessagingStatusResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (discord, slack, telegram, webhook, twitch) = if config_path.exists() {
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

        let discord_status = doc
            .get("messaging")
            .and_then(|m| m.get("discord"))
            .map(|d| {
                let has_token = d
                    .get("token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enabled = d.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
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
                PlatformStatus {
                    configured: true,
                    enabled,
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
            webhook_status,
            twitch_status,
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
            default,
        )
    };

    Ok(Json(MessagingStatusResponse {
        discord,
        slack,
        telegram,
        webhook,
        twitch,
    }))
}

/// Disconnect a messaging platform: remove credentials from config, remove all
/// bindings for that platform, and shut down the adapter.
pub(super) async fn disconnect_platform(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DisconnectPlatformRequest>,
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
    if let Some(manager) = manager_guard.as_ref()
        && let Err(error) = manager.remove_adapter(platform).await
    {
        tracing::warn!(%error, platform = %platform, "failed to shut down adapter during disconnect");
    }

    tracing::info!(platform = %platform, "platform disconnected via API");

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("{platform} disconnected")
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

    table["enabled"] = toml_edit::value(request.enabled);

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
                                    let arc_swap =
                                        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                                    state.set_discord_permissions(arc_swap.clone()).await;
                                    arc_swap
                                }
                            }
                        };
                        let adapter = crate::messaging::discord::DiscordAdapter::new(
                            &discord_config.token,
                            perms,
                        );
                        if let Err(error) = manager.register_and_start(adapter).await {
                            tracing::error!(%error, "failed to start discord adapter on toggle");
                        }
                    }
                }
                "slack" => {
                    if let Some(slack_config) = &new_config.messaging.slack {
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
                                    let arc_swap =
                                        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                                    state.set_slack_permissions(arc_swap.clone()).await;
                                    arc_swap
                                }
                            }
                        };
                        match crate::messaging::slack::SlackAdapter::new(
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
                }
                "telegram" => {
                    if let Some(telegram_config) = &new_config.messaging.telegram {
                        let perms = crate::config::TelegramPermissions::from_config(
                            telegram_config,
                            &new_config.bindings,
                        );
                        let arc_swap = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                        let adapter = crate::messaging::telegram::TelegramAdapter::new(
                            &telegram_config.token,
                            arc_swap,
                        );
                        if let Err(error) = manager.register_and_start(adapter).await {
                            tracing::error!(%error, "failed to start telegram adapter on toggle");
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
                        let perms = crate::config::TwitchPermissions::from_config(
                            twitch_config,
                            &new_config.bindings,
                        );
                        let arc_swap = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                        let instance_dir = state.instance_dir.load();
                        let token_path = instance_dir.join("twitch_token.json");
                        let adapter = crate::messaging::twitch::TwitchAdapter::new(
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
                }
                _ => {}
            }
        }
    } else if let Some(manager) = manager
        && let Err(error) = manager.remove_adapter(platform).await
    {
        tracing::warn!(%error, platform = %platform, "failed to shut down adapter on toggle");
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
