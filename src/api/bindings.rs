use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct BindingResponse {
    agent_id: String,
    channel: String,
    guild_id: Option<String>,
    workspace_id: Option<String>,
    chat_id: Option<String>,
    channel_ids: Vec<String>,
    require_mention: bool,
    dm_allowed_users: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct BindingsListResponse {
    bindings: Vec<BindingResponse>,
}

#[derive(Deserialize)]
pub(super) struct BindingsQuery {
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CreateBindingRequest {
    agent_id: String,
    channel: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    channel_ids: Vec<String>,
    #[serde(default)]
    require_mention: bool,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    /// Optional: set platform credentials if not yet configured.
    #[serde(default)]
    platform_credentials: Option<PlatformCredentials>,
}

#[derive(Deserialize)]
pub(super) struct PlatformCredentials {
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
}

#[derive(Serialize)]
pub(super) struct CreateBindingResponse {
    success: bool,
    /// True if platform credentials were added/changed (adapter needs restart).
    restart_required: bool,
    message: String,
}

#[derive(Deserialize)]
pub(super) struct DeleteBindingRequest {
    agent_id: String,
    channel: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
}

#[derive(Serialize)]
pub(super) struct DeleteBindingResponse {
    success: bool,
    message: String,
}

#[derive(Deserialize)]
pub(super) struct UpdateBindingRequest {
    original_agent_id: String,
    original_channel: String,
    #[serde(default)]
    original_guild_id: Option<String>,
    #[serde(default)]
    original_workspace_id: Option<String>,
    #[serde(default)]
    original_chat_id: Option<String>,

    agent_id: String,
    channel: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    channel_ids: Vec<String>,
    #[serde(default)]
    require_mention: bool,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct UpdateBindingResponse {
    success: bool,
    message: String,
}

/// List all bindings, optionally filtered by agent_id.
pub(super) async fn list_bindings(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BindingsQuery>,
) -> Json<BindingsListResponse> {
    let bindings_guard = state.bindings.read().await;
    let bindings = match bindings_guard.as_ref() {
        Some(arc_swap) => {
            let loaded = arc_swap.load();
            loaded.as_ref().clone()
        }
        None => Vec::new(),
    };
    drop(bindings_guard);

    let filtered: Vec<BindingResponse> = bindings
        .into_iter()
        .filter(|b| query.agent_id.as_ref().is_none_or(|id| &b.agent_id == id))
        .map(|b| BindingResponse {
            agent_id: b.agent_id,
            channel: b.channel,
            guild_id: b.guild_id,
            workspace_id: b.workspace_id,
            chat_id: b.chat_id,
            channel_ids: b.channel_ids,
            require_mention: b.require_mention,
            dm_allowed_users: b.dm_allowed_users,
        })
        .collect();

    Json(BindingsListResponse { bindings: filtered })
}

/// Create a new binding (and optionally configure platform credentials).
pub(super) async fn create_binding(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<CreateBindingRequest>,
) -> Result<Json<CreateBindingResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to read config.toml");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut new_discord_token: Option<String> = None;
    let mut new_slack_tokens: Option<(String, String)> = None;
    let mut new_telegram_token: Option<String> = None;
    let mut new_twitch_creds: Option<(String, String)> = None;

    if let Some(credentials) = &request.platform_credentials {
        if let Some(token) = &credentials.discord_token
            && !token.is_empty()
        {
            if doc.get("messaging").is_none() {
                doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let messaging = doc["messaging"]
                .as_table_mut()
                .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
            if !messaging.contains_key("discord") {
                messaging["discord"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let discord = messaging["discord"]
                .as_table_mut()
                .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
            discord["enabled"] = toml_edit::value(true);
            discord["token"] = toml_edit::value(token.as_str());
            new_discord_token = Some(token.clone());
        }
        if let Some(bot_token) = &credentials.slack_bot_token {
            let app_token = credentials.slack_app_token.as_deref().unwrap_or("");
            if !bot_token.is_empty() && !app_token.is_empty() {
                if doc.get("messaging").is_none() {
                    doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let messaging = doc["messaging"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                if !messaging.contains_key("slack") {
                    messaging["slack"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let slack = messaging["slack"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                slack["enabled"] = toml_edit::value(true);
                slack["bot_token"] = toml_edit::value(bot_token.as_str());
                slack["app_token"] = toml_edit::value(app_token);
                new_slack_tokens = Some((bot_token.clone(), app_token.to_string()));
            }
        }
        if let Some(token) = &credentials.telegram_token
            && !token.is_empty()
        {
            if doc.get("messaging").is_none() {
                doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let messaging = doc["messaging"]
                .as_table_mut()
                .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
            if !messaging.contains_key("telegram") {
                messaging["telegram"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let telegram = messaging["telegram"]
                .as_table_mut()
                .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
            telegram["enabled"] = toml_edit::value(true);
            telegram["token"] = toml_edit::value(token.as_str());
            new_telegram_token = Some(token.clone());
        }
        if let Some(username) = &credentials.twitch_username {
            let oauth_token = credentials.twitch_oauth_token.as_deref().unwrap_or("");
            let client_id = credentials.twitch_client_id.as_deref().unwrap_or("");
            let client_secret = credentials.twitch_client_secret.as_deref().unwrap_or("");
            let refresh_token = credentials.twitch_refresh_token.as_deref().unwrap_or("");
            if !username.is_empty() && !oauth_token.is_empty() {
                if doc.get("messaging").is_none() {
                    doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let messaging = doc["messaging"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                if !messaging.contains_key("twitch") {
                    messaging["twitch"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let twitch = messaging["twitch"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                twitch["enabled"] = toml_edit::value(true);
                twitch["username"] = toml_edit::value(username.as_str());
                twitch["oauth_token"] = toml_edit::value(oauth_token);
                if !client_id.is_empty() {
                    twitch["client_id"] = toml_edit::value(client_id);
                }
                if !client_secret.is_empty() {
                    twitch["client_secret"] = toml_edit::value(client_secret);
                }
                if !refresh_token.is_empty() {
                    twitch["refresh_token"] = toml_edit::value(refresh_token);
                }
                new_twitch_creds = Some((username.clone(), oauth_token.to_string()));
            }
        }
    }

    if doc.get("bindings").is_none() {
        doc["bindings"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let bindings_array = doc["bindings"]
        .as_array_of_tables_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut binding_table = toml_edit::Table::new();
    binding_table["agent_id"] = toml_edit::value(&request.agent_id);
    binding_table["channel"] = toml_edit::value(&request.channel);
    if let Some(guild_id) = &request.guild_id {
        binding_table["guild_id"] = toml_edit::value(guild_id.as_str());
    }
    if let Some(workspace_id) = &request.workspace_id {
        binding_table["workspace_id"] = toml_edit::value(workspace_id.as_str());
    }
    if let Some(chat_id) = &request.chat_id {
        binding_table["chat_id"] = toml_edit::value(chat_id.as_str());
    }
    if !request.channel_ids.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.channel_ids {
            arr.push(id.as_str());
        }
        binding_table["channel_ids"] = toml_edit::value(arr);
    }
    if request.require_mention {
        binding_table["require_mention"] = toml_edit::value(true);
    }
    if !request.dm_allowed_users.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.dm_allowed_users {
            arr.push(id.as_str());
        }
        binding_table["dm_allowed_users"] = toml_edit::value(arr);
    }
    bindings_array.push(binding_table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding created via API"
    );

    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
        drop(bindings_guard);

        if let Some(discord_config) = &new_config.messaging.discord {
            let new_perms = crate::config::DiscordPermissions::from_config(
                discord_config,
                &new_config.bindings,
            );
            let perms = state.discord_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        if let Some(slack_config) = &new_config.messaging.slack {
            let new_perms =
                crate::config::SlackPermissions::from_config(slack_config, &new_config.bindings);
            let perms = state.slack_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        let manager_guard = state.messaging_manager.read().await;
        if let Some(manager) = manager_guard.as_ref() {
            if let Some(token) = new_discord_token {
                let discord_perms = {
                    let perms_guard = state.discord_permissions.read().await;
                    match perms_guard.as_ref() {
                        Some(existing) => existing.clone(),
                        None => {
                            drop(perms_guard);
                            let Some(discord_config) = new_config.messaging.discord.as_ref() else {
                                tracing::error!(
                                    "discord config missing despite token being provided"
                                );
                                return Err(StatusCode::INTERNAL_SERVER_ERROR);
                            };
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
                let adapter = crate::messaging::discord::DiscordAdapter::new(&token, discord_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start discord adapter");
                }
            }

            if let Some((bot_token, app_token)) = new_slack_tokens {
                let slack_perms = {
                    let perms_guard = state.slack_permissions.read().await;
                    match perms_guard.as_ref() {
                        Some(existing) => existing.clone(),
                        None => {
                            drop(perms_guard);
                            let Some(slack_config) = new_config.messaging.slack.as_ref() else {
                                tracing::error!(
                                    "slack config missing despite tokens being provided"
                                );
                                return Err(StatusCode::INTERNAL_SERVER_ERROR);
                            };
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
                let slack_commands = new_config
                    .messaging
                    .slack
                    .as_ref()
                    .map(|s| s.commands.clone())
                    .unwrap_or_default();
                match crate::messaging::slack::SlackAdapter::new(
                    &bot_token,
                    &app_token,
                    slack_perms,
                    slack_commands,
                ) {
                    Ok(adapter) => {
                        if let Err(error) = manager.register_and_start(adapter).await {
                            tracing::error!(%error, "failed to hot-start slack adapter");
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to build slack adapter");
                    }
                }
            }

            if let Some(token) = new_telegram_token {
                let telegram_perms = {
                    let Some(telegram_config) = new_config.messaging.telegram.as_ref() else {
                        tracing::error!("telegram config missing despite token being provided");
                        return Err(StatusCode::INTERNAL_SERVER_ERROR);
                    };
                    let perms = crate::config::TelegramPermissions::from_config(
                        telegram_config,
                        &new_config.bindings,
                    );
                    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                };
                let adapter =
                    crate::messaging::telegram::TelegramAdapter::new(&token, telegram_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start telegram adapter");
                }
            }

            if let Some((username, oauth_token)) = new_twitch_creds {
                let Some(twitch_config) = new_config.messaging.twitch.as_ref() else {
                    tracing::error!("twitch config missing despite credentials being provided");
                    return Err(StatusCode::INTERNAL_SERVER_ERROR);
                };
                let twitch_perms = {
                    let perms = crate::config::TwitchPermissions::from_config(
                        twitch_config,
                        &new_config.bindings,
                    );
                    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                };
                let instance_dir = state.instance_dir.load();
                let token_path = instance_dir.join("twitch_token.json");
                let adapter = crate::messaging::twitch::TwitchAdapter::new(
                    &username,
                    &oauth_token,
                    twitch_config.client_id.clone(),
                    twitch_config.client_secret.clone(),
                    twitch_config.refresh_token.clone(),
                    Some(token_path),
                    twitch_config.channels.clone(),
                    twitch_config.trigger_prefix.clone(),
                    twitch_perms,
                );
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start twitch adapter");
                }
            }
        }
    }

    Ok(Json(CreateBindingResponse {
        success: true,
        restart_required: false,
        message: "Binding created and active.".to_string(),
    }))
}

pub(super) async fn update_binding(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<UpdateBindingRequest>,
) -> Result<Json<UpdateBindingResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

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

    let bindings_array = doc
        .get_mut("bindings")
        .and_then(|b| b.as_array_of_tables_mut())
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut match_idx: Option<usize> = None;
    for (i, table) in bindings_array.iter().enumerate() {
        let matches_agent = table
            .get("agent_id")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == request.original_agent_id);
        let matches_channel = table
            .get("channel")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == request.original_channel);
        let matches_guild = match &request.original_guild_id {
            Some(gid) => table
                .get("guild_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == gid),
            None => table.get("guild_id").is_none(),
        };
        let matches_workspace = match &request.original_workspace_id {
            Some(wid) => table
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == wid),
            None => table.get("workspace_id").is_none(),
        };
        let matches_chat = match &request.original_chat_id {
            Some(cid) => table
                .get("chat_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == cid),
            None => table.get("chat_id").is_none(),
        };
        if matches_agent && matches_channel && matches_guild && matches_workspace && matches_chat {
            match_idx = Some(i);
            break;
        }
    }

    let Some(idx) = match_idx else {
        return Ok(Json(UpdateBindingResponse {
            success: false,
            message: "No matching binding found.".to_string(),
        }));
    };

    let binding = bindings_array
        .get_mut(idx)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    binding["agent_id"] = toml_edit::value(&request.agent_id);
    binding["channel"] = toml_edit::value(&request.channel);

    binding.remove("guild_id");
    binding.remove("workspace_id");
    binding.remove("chat_id");

    if let Some(ref guild_id) = request.guild_id
        && !guild_id.is_empty()
    {
        binding["guild_id"] = toml_edit::value(guild_id);
    }
    if let Some(ref workspace_id) = request.workspace_id
        && !workspace_id.is_empty()
    {
        binding["workspace_id"] = toml_edit::value(workspace_id);
    }
    if let Some(ref chat_id) = request.chat_id
        && !chat_id.is_empty()
    {
        binding["chat_id"] = toml_edit::value(chat_id);
    }

    if !request.channel_ids.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.channel_ids {
            arr.push(id.as_str());
        }
        binding["channel_ids"] = toml_edit::value(arr);
    } else {
        binding.remove("channel_ids");
    }

    if request.require_mention {
        binding["require_mention"] = toml_edit::value(true);
    } else {
        binding.remove("require_mention");
    }

    if !request.dm_allowed_users.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.dm_allowed_users {
            arr.push(id.as_str());
        }
        binding["dm_allowed_users"] = toml_edit::value(arr);
    } else {
        binding.remove("dm_allowed_users");
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding updated via API"
    );

    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
        drop(bindings_guard);

        if let Some(discord_config) = &new_config.messaging.discord {
            let new_perms = crate::config::DiscordPermissions::from_config(
                discord_config,
                &new_config.bindings,
            );
            let perms = state.discord_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        if let Some(slack_config) = &new_config.messaging.slack {
            let new_perms =
                crate::config::SlackPermissions::from_config(slack_config, &new_config.bindings);
            let perms = state.slack_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }
    }

    Ok(Json(UpdateBindingResponse {
        success: true,
        message: "Binding updated.".to_string(),
    }))
}

/// Delete a binding by matching agent_id + channel + platform-specific identifiers.
pub(super) async fn delete_binding(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<DeleteBindingRequest>,
) -> Result<Json<DeleteBindingResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

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

    let bindings_array = doc
        .get_mut("bindings")
        .and_then(|b| b.as_array_of_tables_mut())
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut match_idx: Option<usize> = None;
    for (i, table) in bindings_array.iter().enumerate() {
        let matches_agent = table
            .get("agent_id")
            .and_then(|v: &toml_edit::Item| v.as_str())
            .is_some_and(|v| v == request.agent_id);
        let matches_channel = table
            .get("channel")
            .and_then(|v: &toml_edit::Item| v.as_str())
            .is_some_and(|v| v == request.channel);
        let matches_guild = match &request.guild_id {
            Some(gid) => table
                .get("guild_id")
                .and_then(|v: &toml_edit::Item| v.as_str())
                .is_some_and(|v| v == gid),
            None => table.get("guild_id").is_none(),
        };
        let matches_workspace = match &request.workspace_id {
            Some(wid) => table
                .get("workspace_id")
                .and_then(|v: &toml_edit::Item| v.as_str())
                .is_some_and(|v| v == wid),
            None => table.get("workspace_id").is_none(),
        };
        let matches_chat = match &request.chat_id {
            Some(cid) => table
                .get("chat_id")
                .and_then(|v: &toml_edit::Item| v.as_str())
                .is_some_and(|v| v == cid),
            None => table.get("chat_id").is_none(),
        };
        if matches_agent && matches_channel && matches_guild && matches_workspace && matches_chat {
            match_idx = Some(i);
            break;
        }
    }

    let Some(idx) = match_idx else {
        return Ok(Json(DeleteBindingResponse {
            success: false,
            message: "No matching binding found.".to_string(),
        }));
    };

    bindings_array.remove(idx);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding deleted via API"
    );

    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
        drop(bindings_guard);

        if let Some(discord_config) = &new_config.messaging.discord {
            let new_perms = crate::config::DiscordPermissions::from_config(
                discord_config,
                &new_config.bindings,
            );
            let perms = state.discord_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        if let Some(slack_config) = &new_config.messaging.slack {
            let new_perms =
                crate::config::SlackPermissions::from_config(slack_config, &new_config.bindings);
            let perms = state.slack_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }
    }

    Ok(Json(DeleteBindingResponse {
        success: true,
        message: "Binding deleted.".to_string(),
    }))
}
