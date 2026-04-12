use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct IntegrationEntry {
    id: String,
    name: String,
    description: String,
    enabled: bool,
    status: String,
    config: serde_json::Value,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct IntegrationsResponse {
    integrations: Vec<IntegrationEntry>,
}

/// Derive a simple status string from config state.
fn derive_status(enabled: bool, has_endpoint: bool) -> &'static str {
    if !enabled {
        "disabled"
    } else if has_endpoint {
        "connected"
    } else {
        "unconfigured"
    }
}

#[utoipa::path(
    get,
    path = "/integrations",
    responses(
        (status = 200, body = IntegrationsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "integrations",
)]
pub(super) async fn get_integrations(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<IntegrationsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    // Load via Config::load_from_path so all defaults are resolved identically
    // to the running system (base defaults, env overrides, etc.).
    let config =
        tokio::task::spawn_blocking(move || crate::config::Config::load_from_path(&config_path))
            .await
            .map_err(|error| {
                tracing::error!(%error, "failed to spawn config load task");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .map_err(|error| {
                tracing::error!(%error, "failed to load config for integrations");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    // --- OpenCode ---
    let oc = &config.integrations.opencode;
    let opencode_entry = IntegrationEntry {
        id: "opencode".into(),
        name: "OpenCode".into(),
        description: "Spawn coding agents as worker subprocesses".into(),
        enabled: oc.enabled,
        status: derive_status(oc.enabled, !oc.path.is_empty()).into(),
        config: serde_json::json!({
            "path": oc.path,
            "max_servers": oc.max_servers,
            "server_startup_timeout_secs": oc.server_startup_timeout_secs,
            "max_restart_retries": oc.max_restart_retries,
            "permissions": {
                "edit": oc.permissions.edit,
                "bash": oc.permissions.bash,
                "webfetch": oc.permissions.webfetch,
            },
        }),
    };

    // --- Spacedrive ---
    let sd = &config.integrations.spacedrive;
    let sd_web_url = sd.resolved_web_url();
    let spacedrive_entry = IntegrationEntry {
        id: "spacedrive".into(),
        name: "Spacedrive".into(),
        description: "Embedded file explorer".into(),
        enabled: sd.enabled,
        status: derive_status(sd.enabled, sd_web_url.is_some()).into(),
        config: serde_json::json!({
            "api_url": sd.api_url.as_deref().unwrap_or(""),
            "web_url": sd_web_url.as_deref().unwrap_or(""),
            "library_id": sd.library_id.as_deref().unwrap_or(""),
        }),
    };

    // --- Voicebox ---
    let vb = &config.integrations.voicebox;
    let voicebox_entry = IntegrationEntry {
        id: "voicebox".into(),
        name: "Voicebox".into(),
        description: "Voice input and TTS output".into(),
        enabled: vb.enabled,
        status: derive_status(vb.enabled, vb.url.is_some()).into(),
        config: serde_json::json!({
            "url": vb.url.as_deref().unwrap_or(""),
            "profile_id": vb.profile_id.as_deref().unwrap_or(""),
        }),
    };

    Ok(Json(IntegrationsResponse {
        integrations: vec![opencode_entry, spacedrive_entry, voicebox_entry],
    }))
}

// --- PUT /integrations/{id} ---

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct IntegrationUpdateRequest {
    config: serde_json::Value,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct IntegrationUpdateResponse {
    success: bool,
    message: String,
}

#[utoipa::path(
    put,
    path = "/integrations/{id}",
    params(
        ("id" = String, Path, description = "Integration ID (opencode, spacedrive, voicebox)")
    ),
    request_body = IntegrationUpdateRequest,
    responses(
        (status = 200, body = IntegrationUpdateResponse),
        (status = 404, description = "Unknown integration"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "integrations",
)]
pub(super) async fn update_integration(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<IntegrationUpdateRequest>,
) -> Result<Json<IntegrationUpdateResponse>, StatusCode> {
    let valid_ids = ["opencode", "spacedrive", "voicebox"];
    if !valid_ids.contains(&id.as_str()) {
        return Ok(Json(IntegrationUpdateResponse {
            success: false,
            message: format!("Unknown integration: {id}"),
        }));
    }

    let config_path = state.config_path.read().await.clone();

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path).await.map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::error!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Ensure [integrations] table exists
    if doc.get("integrations").is_none() {
        doc["integrations"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    // Ensure [integrations.{id}] table exists
    if doc["integrations"].get(&id).is_none() {
        doc["integrations"][&id] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    // Apply fields from the JSON config object
    let config_obj = request.config.as_object().ok_or_else(|| {
        tracing::error!("integration update config must be a JSON object");
        StatusCode::BAD_REQUEST
    })?;

    for (key, value) in config_obj {
        match value {
            serde_json::Value::Bool(b) => {
                doc["integrations"][&id][key.as_str()] = toml_edit::value(*b);
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    doc["integrations"][&id][key.as_str()] = toml_edit::value(i);
                } else if let Some(f) = n.as_f64() {
                    doc["integrations"][&id][key.as_str()] = toml_edit::value(f);
                }
            }
            serde_json::Value::String(s) => {
                doc["integrations"][&id][key.as_str()] = toml_edit::value(s.as_str());
            }
            serde_json::Value::Object(obj) => {
                // One level of nesting (e.g., permissions)
                if doc["integrations"][&id].get(key.as_str()).is_none() {
                    doc["integrations"][&id][key.as_str()] =
                        toml_edit::Item::Table(toml_edit::Table::new());
                }
                for (sub_key, sub_value) in obj {
                    match sub_value {
                        serde_json::Value::Bool(b) => {
                            doc["integrations"][&id][key.as_str()][sub_key.as_str()] =
                                toml_edit::value(*b);
                        }
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                doc["integrations"][&id][key.as_str()][sub_key.as_str()] =
                                    toml_edit::value(i);
                            }
                        }
                        serde_json::Value::String(s) => {
                            doc["integrations"][&id][key.as_str()][sub_key.as_str()] =
                                toml_edit::value(s.as_str());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Remove legacy locations when writing to [integrations.*]
    match id.as_str() {
        "opencode" => {
            if let Some(defaults) = doc.get_mut("defaults").and_then(|d| d.as_table_mut()) {
                defaults.remove("opencode");
            }
        }
        "spacedrive" => {
            doc.remove("spacedrive");
        }
        _ => {}
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %config_path.display(), "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Trigger config reload
    let reload_path = config_path.clone();
    match tokio::task::spawn_blocking(move || crate::config::Config::load_from_path(&reload_path))
        .await
    {
        Ok(Ok(new_config)) => {
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
        Ok(Err(error)) => {
            tracing::warn!(%error, "integration updated but config reload failed");
        }
        Err(error) => {
            tracing::warn!(%error, "integration updated but config reload task failed");
        }
    }

    Ok(Json(IntegrationUpdateResponse {
        success: true,
        message: format!("{id} integration updated"),
    }))
}
