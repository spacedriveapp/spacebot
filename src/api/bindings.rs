use super::config::{
    reload_all_runtime_configs, sync_bindings_and_permissions, write_validated_config,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotReloadDisposition {
    NoCredentials,
    Start,
    MissingConfig,
}

fn hot_reload_disposition(
    platform: &'static str,
    has_credentials: bool,
    has_config: bool,
) -> HotReloadDisposition {
    match (has_credentials, has_config) {
        (false, _) => HotReloadDisposition::NoCredentials,
        (true, true) => HotReloadDisposition::Start,
        (true, false) => {
            tracing::warn!(
                platform,
                "credentials provided but messaging config is missing after reload"
            );
            HotReloadDisposition::MissingConfig
        }
    }
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

    let new_config = write_validated_config(&config_path, doc.to_string()).await?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding created via API"
    );

    sync_bindings_and_permissions(&state, &new_config).await;
    reload_all_runtime_configs(&state, &new_config).await;

    let manager_guard = state.messaging_manager.read().await;
    if let Some(manager) = manager_guard.as_ref() {
        let discord_config = new_config.messaging.discord.as_ref();
        if matches!(
            hot_reload_disposition(
                "discord",
                new_discord_token.is_some(),
                discord_config.is_some(),
            ),
            HotReloadDisposition::Start
        ) {
            if let (Some(token), Some(discord_config)) =
                (new_discord_token.as_ref(), discord_config)
            {
                let discord_perms = {
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
                let adapter = crate::messaging::discord::DiscordAdapter::new(token, discord_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start discord adapter");
                }
            }
        }

        let slack_config = new_config.messaging.slack.as_ref();
        if matches!(
            hot_reload_disposition("slack", new_slack_tokens.is_some(), slack_config.is_some(),),
            HotReloadDisposition::Start
        ) {
            if let (Some((bot_token, app_token)), Some(slack_config)) =
                (new_slack_tokens.as_ref(), slack_config)
            {
                let slack_perms = {
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
                    bot_token,
                    app_token,
                    slack_perms,
                    slack_config.commands.clone(),
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
        }

        let telegram_config = new_config.messaging.telegram.as_ref();
        if matches!(
            hot_reload_disposition(
                "telegram",
                new_telegram_token.is_some(),
                telegram_config.is_some(),
            ),
            HotReloadDisposition::Start
        ) {
            if let (Some(token), Some(telegram_config)) =
                (new_telegram_token.as_ref(), telegram_config)
            {
                let telegram_perms = {
                    let perms = crate::config::TelegramPermissions::from_config(
                        telegram_config,
                        &new_config.bindings,
                    );
                    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                };
                let adapter =
                    crate::messaging::telegram::TelegramAdapter::new(token, telegram_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start telegram adapter");
                }
            }
        }

        let twitch_config = new_config.messaging.twitch.as_ref();
        if matches!(
            hot_reload_disposition(
                "twitch",
                new_twitch_creds.is_some(),
                twitch_config.is_some(),
            ),
            HotReloadDisposition::Start
        ) {
            if let (Some((username, oauth_token)), Some(twitch_config)) =
                (new_twitch_creds.as_ref(), twitch_config)
            {
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
                    username,
                    oauth_token,
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

    let new_config = write_validated_config(&config_path, doc.to_string()).await?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding updated via API"
    );

    sync_bindings_and_permissions(&state, &new_config).await;
    reload_all_runtime_configs(&state, &new_config).await;

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

    let new_config = write_validated_config(&config_path, doc.to_string()).await?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding deleted via API"
    );

    sync_bindings_and_permissions(&state, &new_config).await;
    reload_all_runtime_configs(&state, &new_config).await;

    Ok(Json(DeleteBindingResponse {
        success: true,
        message: "Binding deleted.".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{CreateBindingRequest, HotReloadDisposition, hot_reload_disposition};
    use crate::api::providers;
    use crate::api::state::ApiState;
    use crate::config::{Config, DiscordPermissions, RuntimeConfig, SlackPermissions};
    use arc_swap::ArcSwap;
    use axum::Json;
    use axum::extract::State;
    use axum::http::StatusCode;
    use std::collections::HashMap;
    use std::sync::Arc;

    const VALID_BASE_CONFIG: &str = r#"
[llm]
anthropic_key = "test-anthropic-key"

[defaults.routing]
channel = "anthropic/claude-sonnet-4"
branch = "anthropic/claude-sonnet-4"
worker = "anthropic/claude-sonnet-4"
compactor = "anthropic/claude-sonnet-4"
cortex = "anthropic/claude-sonnet-4"

[[agents]]
id = "main"
default = true
"#;

    const VALID_DISCORD_CONFIG: &str = r#"
[llm]
anthropic_key = "test-anthropic-key"

[[agents]]
id = "main"
default = true

[messaging.discord]
enabled = true
token = "discord-test-token"
dm_allowed_users = ["111"]
"#;

    fn new_test_state() -> Arc<ApiState> {
        let (provider_setup_tx, _) = tokio::sync::mpsc::channel(8);
        let (agent_tx, _) = tokio::sync::mpsc::channel(8);
        let (agent_remove_tx, _) = tokio::sync::mpsc::channel(8);
        Arc::new(ApiState::new_with_provider_sender(
            provider_setup_tx,
            agent_tx,
            agent_remove_tx,
        ))
    }

    async fn configure_runtime_for_main(
        state: &Arc<ApiState>,
        config_path: &std::path::Path,
    ) -> Arc<RuntimeConfig> {
        let config = Config::load_from_path(config_path).expect("config should load for test");
        let resolved_agent = config
            .resolve_agents()
            .into_iter()
            .find(|agent| agent.id == "main")
            .expect("main agent should exist in resolved config");
        let runtime_config = Arc::new(RuntimeConfig::new(
            &config.instance_dir,
            &resolved_agent,
            &config.defaults,
            crate::prompts::PromptEngine::new("en").expect("prompt engine should build"),
            crate::identity::Identity::default(),
            crate::skills::SkillSet::default(),
        ));

        let mut runtime_configs = HashMap::new();
        runtime_configs.insert("main".to_string(), runtime_config.clone());
        state.set_runtime_configs(runtime_configs);

        let mut mcp_managers = HashMap::new();
        mcp_managers.insert(
            "main".to_string(),
            Arc::new(crate::mcp::McpManager::new(resolved_agent.mcp.clone())),
        );
        state.set_mcp_managers(mcp_managers);

        runtime_config
    }

    #[test]
    fn hot_reload_disposition_starts_when_credentials_and_config_exist() {
        assert_eq!(
            hot_reload_disposition("discord", true, true),
            HotReloadDisposition::Start
        );
        assert_eq!(
            hot_reload_disposition("slack", true, true),
            HotReloadDisposition::Start
        );
        assert_eq!(
            hot_reload_disposition("telegram", true, true),
            HotReloadDisposition::Start
        );
        assert_eq!(
            hot_reload_disposition("twitch", true, true),
            HotReloadDisposition::Start
        );
    }

    #[test]
    fn hot_reload_disposition_skips_when_credentials_missing() {
        assert_eq!(
            hot_reload_disposition("discord", false, true),
            HotReloadDisposition::NoCredentials
        );
        assert_eq!(
            hot_reload_disposition("slack", false, false),
            HotReloadDisposition::NoCredentials
        );
    }

    #[test]
    fn hot_reload_disposition_marks_missing_config_when_token_present() {
        assert_eq!(
            hot_reload_disposition("discord", true, false),
            HotReloadDisposition::MissingConfig
        );
        assert_eq!(
            hot_reload_disposition("slack", true, false),
            HotReloadDisposition::MissingConfig
        );
        assert_eq!(
            hot_reload_disposition("telegram", true, false),
            HotReloadDisposition::MissingConfig
        );
        assert_eq!(
            hot_reload_disposition("twitch", true, false),
            HotReloadDisposition::MissingConfig
        );
    }

    #[tokio::test]
    async fn create_binding_returns_bad_request_when_config_validation_fails() {
        const MISSING_ENV_VAR: &str = "SPACEBOT_TEST_MISSING_PROVIDER_KEY_4E845CE0234A45D6AAB6";
        let config_toml = format!(
            r#"
[llm.provider.invalid]
api_type = "openai_completions"
base_url = "https://api.example.com/v1"
api_key = "env:{MISSING_ENV_VAR}"
"#
        );

        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let config_path = temp_dir.path().join("config.toml");
        tokio::fs::write(&config_path, config_toml)
            .await
            .expect("config.toml should be written");

        let state = new_test_state();
        state.set_config_path(config_path).await;

        let result = super::create_binding(
            State(state),
            Json(CreateBindingRequest {
                agent_id: "main".to_string(),
                channel: "discord".to_string(),
                guild_id: Some("123".to_string()),
                workspace_id: None,
                chat_id: None,
                channel_ids: vec!["456".to_string()],
                require_mention: false,
                dm_allowed_users: Vec::new(),
                platform_credentials: None,
            }),
        )
        .await;

        assert!(
            matches!(result, Err(StatusCode::BAD_REQUEST)),
            "invalid config update should return BAD_REQUEST"
        );
    }

    #[tokio::test]
    async fn update_provider_writes_config_and_reloads_runtime_routing() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let config_path = temp_dir.path().join("config.toml");
        tokio::fs::write(&config_path, VALID_BASE_CONFIG)
            .await
            .expect("config.toml should be written");

        let state = new_test_state();
        state.set_config_path(config_path.clone()).await;
        let runtime_config = configure_runtime_for_main(&state, &config_path).await;

        let initial_routing = runtime_config.routing.load();
        assert_eq!(initial_routing.channel, "anthropic/claude-sonnet-4");

        let request_body = serde_json::json!({
            "provider": "openai",
            "api_key": "test-openai-key",
            "model": "openai/gpt-4.1-mini"
        });
        let result = providers::update_provider(
            State(state),
            Json(
                serde_json::from_value(request_body).expect("provider request should deserialize"),
            ),
        )
        .await
        .expect("provider update should succeed");

        let response_json =
            serde_json::to_value(result.0).expect("provider response should serialize");
        assert_eq!(response_json["success"], true);

        let config_after_update = tokio::fs::read_to_string(&config_path)
            .await
            .expect("config should be readable");
        assert!(
            config_after_update.contains("openai_key = \"test-openai-key\""),
            "updated config should persist provider key"
        );

        let updated_routing = runtime_config.routing.load();
        assert_eq!(updated_routing.channel, "openai/gpt-4.1-mini");
        assert_eq!(updated_routing.branch, "openai/gpt-4.1-mini");
        assert_eq!(updated_routing.worker, "openai/gpt-4.1-mini");
        assert_eq!(updated_routing.compactor, "openai/gpt-4.1-mini");
        assert_eq!(updated_routing.cortex, "openai/gpt-4.1-mini");
    }

    #[tokio::test]
    async fn create_binding_writes_config_and_syncs_bindings_and_permissions() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let config_path = temp_dir.path().join("config.toml");
        tokio::fs::write(&config_path, VALID_DISCORD_CONFIG)
            .await
            .expect("config.toml should be written");

        let state = new_test_state();
        state.set_config_path(config_path.clone()).await;
        let _runtime_config = configure_runtime_for_main(&state, &config_path).await;
        state
            .set_bindings(Arc::new(ArcSwap::from_pointee(Vec::new())))
            .await;
        state
            .set_discord_permissions(Arc::new(ArcSwap::from_pointee(
                DiscordPermissions::default(),
            )))
            .await;
        state
            .set_slack_permissions(Arc::new(ArcSwap::from_pointee(SlackPermissions {
                workspace_filter: Some(vec!["stale-workspace".to_string()]),
                channel_filter: HashMap::from([(
                    "stale-workspace".to_string(),
                    vec!["stale-channel".to_string()],
                )]),
                dm_allowed_users: vec!["stale-user".to_string()],
            })))
            .await;

        let result = super::create_binding(
            State(state.clone()),
            Json(CreateBindingRequest {
                agent_id: "main".to_string(),
                channel: "discord".to_string(),
                guild_id: Some("123".to_string()),
                workspace_id: None,
                chat_id: None,
                channel_ids: vec!["456".to_string()],
                require_mention: true,
                dm_allowed_users: vec!["789".to_string()],
                platform_credentials: None,
            }),
        )
        .await
        .expect("binding create should succeed");

        assert!(result.0.success);

        let written_config =
            Config::load_from_path(&config_path).expect("written config should parse");
        assert_eq!(written_config.bindings.len(), 1);
        assert_eq!(written_config.bindings[0].channel, "discord");
        assert_eq!(
            written_config.bindings[0].guild_id.as_deref(),
            Some("123"),
            "binding should be persisted to config.toml"
        );

        let bindings_guard = state.bindings.read().await;
        let bindings_swap = bindings_guard
            .as_ref()
            .expect("bindings ArcSwap should be registered")
            .clone();
        drop(bindings_guard);
        let live_bindings = bindings_swap.load();
        assert_eq!(live_bindings.len(), 1);
        assert_eq!(live_bindings[0].channel, "discord");
        assert_eq!(live_bindings[0].guild_id.as_deref(), Some("123"));

        let discord_guard = state.discord_permissions.read().await;
        let discord_swap = discord_guard
            .as_ref()
            .expect("discord permissions ArcSwap should be registered")
            .clone();
        drop(discord_guard);
        let discord_permissions = discord_swap.load();
        assert_eq!(discord_permissions.guild_filter, Some(vec![123]));
        let guild_channels = discord_permissions
            .channel_filter
            .get(&123)
            .expect("channel filter should include the guild");
        assert_eq!(guild_channels, &vec![456]);
        assert!(
            discord_permissions.dm_allowed_users.contains(&111),
            "dm user from messaging.discord config should be retained"
        );
        assert!(
            discord_permissions.dm_allowed_users.contains(&789),
            "dm user from binding should be merged into permissions"
        );

        let slack_guard = state.slack_permissions.read().await;
        let slack_swap = slack_guard
            .as_ref()
            .expect("slack permissions ArcSwap should be registered")
            .clone();
        drop(slack_guard);
        let slack_permissions = slack_swap.load();
        assert_eq!(
            slack_permissions.workspace_filter, None,
            "slack permissions should reset when slack config is absent"
        );
        assert!(slack_permissions.channel_filter.is_empty());
        assert!(slack_permissions.dm_allowed_users.is_empty());
    }
}
