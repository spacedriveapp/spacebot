//! Shared admin-plane helpers for config mutation and secret persistence.

use super::state::ApiState;
use crate::secrets::store::{SecretField, SecretsStore, SystemSecrets};

use axum::http::StatusCode;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) async fn load_config_doc<'a>(
    state: &'a Arc<ApiState>,
) -> Result<
    (
        tokio::sync::MutexGuard<'a, ()>,
        PathBuf,
        toml_edit::DocumentMut,
    ),
    StatusCode,
> {
    let config_guard = state.config_write_mutex.lock().await;
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

    let doc = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok((config_guard, config_path, doc))
}

pub(super) async fn write_config_doc(
    config_path: &Path,
    doc: &toml_edit::DocumentMut,
) -> Result<(), StatusCode> {
    tokio::fs::write(config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, path = %config_path.display(), "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub(super) async fn reload_runtime_configs(
    state: &Arc<ApiState>,
    config_path: &Path,
) -> Result<crate::config::Config, StatusCode> {
    if let Ok(store) = secrets_store(state) {
        crate::config::set_resolve_secrets_store(store);
    }

    let config_path = config_path.to_path_buf();
    let new_config =
        tokio::task::spawn_blocking(move || crate::config::Config::load_from_path(&config_path))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "config reload task failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .map_err(|error| {
                tracing::warn!(%error, "config.toml written but failed to reload immediately");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    state
        .set_api_auth_token(new_config.api.auth_token.clone())
        .await;

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

    Ok(new_config)
}

pub(super) fn secrets_store(state: &ApiState) -> Result<Arc<SecretsStore>, StatusCode> {
    let guard = state.secrets_store.load();
    match (*guard).as_ref() {
        Some(store) => Ok(store.clone()),
        None => {
            tracing::warn!("secrets store not initialized");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

pub(super) async fn require_api_auth_token(state: &ApiState) -> Result<(), StatusCode> {
    let auth_token = state.auth_token.read().await;
    if auth_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub(super) fn secret_reference<T: SystemSecrets>(
    store: &SecretsStore,
    toml_key: &str,
    instance_name: Option<&str>,
    value: &str,
) -> Result<String, StatusCode> {
    let field = T::secret_fields()
        .iter()
        .find(|field| field.toml_key == toml_key)
        .ok_or_else(|| {
            tracing::error!(section = T::section(), toml_key, "unknown secret field");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    secret_reference_for_field(store, field, instance_name, value)
}

pub(super) fn secret_reference_for_field(
    store: &SecretsStore,
    field: &SecretField,
    instance_name: Option<&str>,
    value: &str,
) -> Result<String, StatusCode> {
    let trimmed_value = value.trim();
    if trimmed_value.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let secret_name = match instance_name {
        Some(instance_name) => field.instance_name(instance_name).ok_or_else(|| {
            tracing::error!(toml_key = field.toml_key, "missing instance naming pattern");
            StatusCode::INTERNAL_SERVER_ERROR
        })?,
        None => field.secret_name.to_string(),
    };

    let category = crate::secrets::store::auto_categorize(&secret_name);
    store
        .set(&secret_name, trimmed_value, category)
        .map_err(|error| {
            tracing::warn!(%error, secret_name, "failed to persist secret");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(format!("secret:{secret_name}"))
}
