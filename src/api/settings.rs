use super::state::ApiState;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct GlobalSettingsResponse {
    brave_search_key: Option<String>,
    api_enabled: bool,
    api_port: u16,
    api_bind: String,
    worker_log_mode: String,
    opencode: OpenCodeSettingsResponse,
}

#[derive(Serialize)]
pub(super) struct OpenCodeSettingsResponse {
    enabled: bool,
    path: String,
    max_servers: usize,
    server_startup_timeout_secs: u64,
    max_restart_retries: u32,
    permissions: OpenCodePermissionsResponse,
}

#[derive(Serialize)]
pub(super) struct OpenCodePermissionsResponse {
    edit: String,
    bash: String,
    webfetch: String,
}

#[derive(Deserialize)]
pub(super) struct GlobalSettingsUpdate {
    brave_search_key: Option<String>,
    api_enabled: Option<bool>,
    api_port: Option<u16>,
    api_bind: Option<String>,
    worker_log_mode: Option<String>,
    opencode: Option<OpenCodeSettingsUpdate>,
}

#[derive(Deserialize)]
pub(super) struct OpenCodeSettingsUpdate {
    enabled: Option<bool>,
    path: Option<String>,
    max_servers: Option<usize>,
    server_startup_timeout_secs: Option<u64>,
    max_restart_retries: Option<u32>,
    permissions: Option<OpenCodePermissionsUpdate>,
}

#[derive(Deserialize)]
pub(super) struct OpenCodePermissionsUpdate {
    edit: Option<String>,
    bash: Option<String>,
    webfetch: Option<String>,
}

#[derive(Serialize)]
pub(super) struct GlobalSettingsUpdateResponse {
    success: bool,
    message: String,
    requires_restart: bool,
}

#[derive(Serialize)]
pub(super) struct RawConfigResponse {
    content: String,
}

#[derive(Deserialize)]
pub(super) struct RawConfigUpdateRequest {
    content: String,
}

#[derive(Serialize)]
pub(super) struct RawConfigUpdateResponse {
    success: bool,
    message: String,
}

pub(super) async fn get_global_settings(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<GlobalSettingsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (brave_search_key, api_enabled, api_port, api_bind, worker_log_mode, opencode) =
        if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let doc: toml_edit::DocumentMut = content
                .parse()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let brave_search = doc
                .get("defaults")
                .and_then(|d| d.get("brave_search_key"))
                .and_then(|v| v.as_str())
                .map(|s| {
                    if let Some(var) = s.strip_prefix("env:") {
                        std::env::var(var).ok()
                    } else {
                        Some(s.to_string())
                    }
                })
                .flatten();

            let api_enabled = doc
                .get("api")
                .and_then(|a| a.get("enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let api_port = doc
                .get("api")
                .and_then(|a| a.get("port"))
                .and_then(|v| v.as_integer())
                .and_then(|i| u16::try_from(i).ok())
                .unwrap_or(19898);

            let api_bind = doc
                .get("api")
                .and_then(|a| a.get("bind"))
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();

            let worker_log_mode = doc
                .get("defaults")
                .and_then(|d| d.get("worker_log_mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("errors_only")
                .to_string();

            let opencode_table = doc.get("defaults").and_then(|d| d.get("opencode"));
            let opencode_perms = opencode_table.and_then(|o| o.get("permissions"));
            let opencode = OpenCodeSettingsResponse {
                enabled: opencode_table
                    .and_then(|o| o.get("enabled"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                path: opencode_table
                    .and_then(|o| o.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("opencode")
                    .to_string(),
                max_servers: opencode_table
                    .and_then(|o| o.get("max_servers"))
                    .and_then(|v| v.as_integer())
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or(5),
                server_startup_timeout_secs: opencode_table
                    .and_then(|o| o.get("server_startup_timeout_secs"))
                    .and_then(|v| v.as_integer())
                    .and_then(|i| u64::try_from(i).ok())
                    .unwrap_or(30),
                max_restart_retries: opencode_table
                    .and_then(|o| o.get("max_restart_retries"))
                    .and_then(|v| v.as_integer())
                    .and_then(|i| u32::try_from(i).ok())
                    .unwrap_or(5),
                permissions: OpenCodePermissionsResponse {
                    edit: opencode_perms
                        .and_then(|p| p.get("edit"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                    bash: opencode_perms
                        .and_then(|p| p.get("bash"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                    webfetch: opencode_perms
                        .and_then(|p| p.get("webfetch"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                },
            };

            (
                brave_search,
                api_enabled,
                api_port,
                api_bind,
                worker_log_mode,
                opencode,
            )
        } else {
            (
                None,
                true,
                19898,
                "127.0.0.1".to_string(),
                "errors_only".to_string(),
                OpenCodeSettingsResponse {
                    enabled: false,
                    path: "opencode".to_string(),
                    max_servers: 5,
                    server_startup_timeout_secs: 30,
                    max_restart_retries: 5,
                    permissions: OpenCodePermissionsResponse {
                        edit: "allow".to_string(),
                        bash: "allow".to_string(),
                        webfetch: "allow".to_string(),
                    },
                },
            )
        };

    Ok(Json(GlobalSettingsResponse {
        brave_search_key,
        api_enabled,
        api_port,
        api_bind,
        worker_log_mode,
        opencode,
    }))
}

pub(super) async fn update_global_settings(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GlobalSettingsUpdate>,
) -> Result<Json<GlobalSettingsUpdateResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut requires_restart = false;

    if let Some(key) = request.brave_search_key {
        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if key.is_empty() {
            if let Some(table) = doc["defaults"].as_table_mut() {
                table.remove("brave_search_key");
            }
        } else {
            doc["defaults"]["brave_search_key"] = toml_edit::value(key);
        }
    }

    if request.api_enabled.is_some() || request.api_port.is_some() || request.api_bind.is_some() {
        requires_restart = true;

        if doc.get("api").is_none() {
            doc["api"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(enabled) = request.api_enabled {
            doc["api"]["enabled"] = toml_edit::value(enabled);
        }
        if let Some(port) = request.api_port {
            doc["api"]["port"] = toml_edit::value(i64::from(port));
        }
        if let Some(bind) = request.api_bind {
            doc["api"]["bind"] = toml_edit::value(bind);
        }
    }

    if let Some(mode) = request.worker_log_mode {
        if !["errors_only", "all_separate", "all_combined"].contains(&mode.as_str()) {
            return Ok(Json(GlobalSettingsUpdateResponse {
                success: false,
                message: format!("Invalid worker log mode: {}", mode),
                requires_restart: false,
            }));
        }

        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["defaults"]["worker_log_mode"] = toml_edit::value(mode);
    }

    if let Some(opencode) = request.opencode {
        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if doc["defaults"].get("opencode").is_none() {
            doc["defaults"]["opencode"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(enabled) = opencode.enabled {
            doc["defaults"]["opencode"]["enabled"] = toml_edit::value(enabled);
        }
        if let Some(path) = opencode.path {
            doc["defaults"]["opencode"]["path"] = toml_edit::value(path);
        }
        if let Some(max_servers) = opencode.max_servers {
            doc["defaults"]["opencode"]["max_servers"] = toml_edit::value(max_servers as i64);
        }
        if let Some(timeout) = opencode.server_startup_timeout_secs {
            doc["defaults"]["opencode"]["server_startup_timeout_secs"] =
                toml_edit::value(timeout as i64);
        }
        if let Some(retries) = opencode.max_restart_retries {
            doc["defaults"]["opencode"]["max_restart_retries"] = toml_edit::value(retries as i64);
        }
        if let Some(permissions) = opencode.permissions {
            if doc["defaults"]["opencode"].get("permissions").is_none() {
                doc["defaults"]["opencode"]["permissions"] =
                    toml_edit::Item::Table(toml_edit::Table::new());
            }
            if let Some(edit) = permissions.edit {
                doc["defaults"]["opencode"]["permissions"]["edit"] = toml_edit::value(edit);
            }
            if let Some(bash) = permissions.bash {
                doc["defaults"]["opencode"]["permissions"]["bash"] = toml_edit::value(bash);
            }
            if let Some(webfetch) = permissions.webfetch {
                doc["defaults"]["opencode"]["permissions"]["webfetch"] = toml_edit::value(webfetch);
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let message = if requires_restart {
        "Settings updated. API server changes require a restart to take effect.".to_string()
    } else {
        "Settings updated successfully.".to_string()
    };

    Ok(Json(GlobalSettingsUpdateResponse {
        success: true,
        message,
        requires_restart,
    }))
}

/// Return the current update status (from background check).
pub(super) async fn update_check(
    State(state): State<Arc<ApiState>>,
) -> Json<crate::update::UpdateStatus> {
    let status = state.update_status.load();
    Json((**status).clone())
}

/// Force an immediate update check against GitHub.
pub(super) async fn update_check_now(
    State(state): State<Arc<ApiState>>,
) -> Json<crate::update::UpdateStatus> {
    crate::update::check_for_update(&state.update_status).await;
    let status = state.update_status.load();
    Json((**status).clone())
}

/// Pull the new Docker image and recreate this container.
pub(super) async fn update_apply(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match crate::update::apply_docker_update(&state.update_status).await {
        Ok(()) => Ok(Json(serde_json::json!({ "status": "updating" }))),
        Err(error) => {
            tracing::error!(%error, "update apply failed");
            Ok(Json(serde_json::json!({
                "status": "error",
                "error": error.to_string(),
            })))
        }
    }
}

pub(super) async fn get_raw_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<RawConfigResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
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

    Ok(Json(RawConfigResponse { content }))
}

pub(super) async fn update_raw_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RawConfigUpdateRequest>,
) -> Result<Json<RawConfigUpdateResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    if let Err(error) = crate::config::Config::validate_toml(&request.content) {
        return Ok(Json(RawConfigUpdateResponse {
            success: false,
            message: format!("Validation error: {error}"),
        }));
    }

    tokio::fs::write(&config_path, &request.content)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!("config.toml updated via raw editor");

    match crate::config::Config::load_from_path(&config_path) {
        Ok(new_config) => {
            let runtime_configs = state.runtime_configs.load();
            let mcp_managers = state.mcp_managers.load();
            let reload_targets = runtime_configs
                .iter()
                .filter_map(|(agent_id, runtime_config)| {
                    mcp_managers.get(agent_id).map(|mcp_manager| {
                        (
                            agent_id.clone(),
                            runtime_config.clone(),
                            mcp_manager.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            drop(runtime_configs);
            drop(mcp_managers);

            for (agent_id, runtime_config, mcp_manager) in reload_targets {
                runtime_config
                    .reload_config(&new_config, &agent_id, &mcp_manager)
                    .await;
            }
        }
        Err(error) => {
            tracing::warn!(%error, "config.toml written but failed to reload immediately");
        }
    }

    Ok(Json(RawConfigUpdateResponse {
        success: true,
        message: "Config saved and reloaded.".to_string(),
    }))
}
