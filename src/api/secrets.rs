//! Secret management API endpoints.
//!
//! Provides CRUD operations, encryption lifecycle management, and auto-migration
//! for the instance-level secret store. Secrets are global — shared across all
//! agents in the instance.

use super::state::ApiState;
use crate::config::{
    DefaultsConfig, DiscordConfig, EmailConfig, LlmConfig, SlackConfig, TelegramConfig,
    TwitchConfig,
};
use crate::secrets::store::{
    ExportData, SecretCategory, SecretsStore, StoreState, SystemSecrets, auto_categorize,
};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// Keystore identifier for the instance-level master key.
const KEYSTORE_INSTANCE_ID: &str = "instance";

fn get_secrets_store(
    state: &ApiState,
) -> Result<Arc<SecretsStore>, (StatusCode, Json<serde_json::Value>)> {
    let guard = state.secrets_store.load();
    match (*guard).as_ref() {
        Some(store) => Ok(store.clone()),
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "secrets store not initialized"})),
        )),
    }
}

/// `GET /api/secrets/status` — Store state, counts, encryption status.
pub async fn secrets_status(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    // TODO: detect platform_managed from deployment mode.
    match store.status(false) {
        Ok(status) => Json(status).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct SecretListItem {
    name: String,
    category: SecretCategory,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct SecretListResponse {
    secrets: Vec<SecretListItem>,
}

/// `GET /api/secrets` — List all secrets (name + category, no values).
pub async fn list_secrets(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    match store.list_metadata() {
        Ok(metadata) => {
            let mut secrets: Vec<SecretListItem> = metadata
                .into_iter()
                .map(|(name, meta)| SecretListItem {
                    name,
                    category: meta.category,
                    created_at: meta.created_at,
                    updated_at: meta.updated_at,
                })
                .collect();
            secrets.sort_by(|a, b| a.name.cmp(&b.name));
            Json(SecretListResponse { secrets }).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct PutSecretBody {
    pub value: String,
    /// If not provided, auto-categorized based on the secret name.
    pub category: Option<SecretCategory>,
}

#[derive(Serialize)]
struct PutSecretResponse {
    name: String,
    category: SecretCategory,
    reload_required: bool,
    message: String,
}

/// `PUT /api/secrets/:name` — Add or update a secret.
pub async fn put_secret(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
    Json(body): Json<PutSecretBody>,
) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    if store.state() == StoreState::Locked {
        return (
            StatusCode::LOCKED,
            Json(serde_json::json!({"error": "secret store is locked — unlock with master key first"})),
        )
            .into_response();
    }

    let category = body.category.unwrap_or_else(|| auto_categorize(&name));

    match store.set(&name, &body.value, category) {
        Ok(()) => {
            let reload_required = category == SecretCategory::System;
            let message = if reload_required {
                "Secret updated. Reload config or restart for the new value to take effect."
                    .to_string()
            } else {
                "Secret updated. Available to workers immediately.".to_string()
            };
            Json(PutSecretResponse {
                name,
                category,
                reload_required,
                message,
            })
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct DeleteSecretResponse {
    deleted: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

/// `DELETE /api/secrets/:name` — Delete a secret.
pub async fn delete_secret(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    if store.state() == StoreState::Locked {
        return (
            StatusCode::LOCKED,
            Json(serde_json::json!({"error": "secret store is locked — unlock with master key first"})),
        )
            .into_response();
    }

    match store.delete(&name) {
        Ok(()) => Json(DeleteSecretResponse {
            deleted: name,
            warning: None,
        })
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct SecretInfoResponse {
    name: String,
    category: SecretCategory,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /api/secrets/:name/info` — Secret metadata (no value).
pub async fn secret_info(
    State(state): State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    match store.get_metadata(&name) {
        Ok(meta) => Json(SecretInfoResponse {
            name,
            category: meta.category,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
        })
        .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("secret '{name}' not found")})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct EncryptResponse {
    master_key: String,
    message: String,
}

/// `POST /api/secrets/encrypt` — Enable encryption. Returns the master key (hex).
pub async fn enable_encryption(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    match store.enable_encryption() {
        Ok(key_bytes) => {
            // Store master key in OS credential store.
            let keystore = crate::secrets::keystore::platform_keystore();
            if let Err(error) = keystore.store_key(KEYSTORE_INSTANCE_ID, &key_bytes) {
                tracing::warn!(%error, "failed to store master key in OS credential store — user must save the displayed key");
            }

            Json(EncryptResponse {
                master_key: hex::encode(&key_bytes),
                message: "Encryption enabled. Save this master key — you will need it to unlock \
                          the secret manager after a reboot (Linux) or if the Keychain is reset \
                          (macOS). This is the only time the key will be shown."
                    .to_string(),
            })
            .into_response()
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UnlockBody {
    pub master_key: String,
}

/// `POST /api/secrets/unlock` — Unlock encrypted store with master key (hex).
pub async fn unlock_secrets(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UnlockBody>,
) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    let key_bytes = match hex::decode(&body.master_key) {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid master key format (expected hex)"})),
            )
                .into_response();
        }
    };

    match store.unlock(&key_bytes) {
        Ok(()) => {
            // Also store in OS credential store for automatic unlock on next restart.
            let keystore = crate::secrets::keystore::platform_keystore();
            if let Err(error) = keystore.store_key(KEYSTORE_INSTANCE_ID, &key_bytes) {
                tracing::warn!(%error, "failed to persist master key in OS credential store");
            }

            let status = store.status(false).ok();
            Json(serde_json::json!({
                "state": "unlocked",
                "secret_count": status.map(|s| s.secret_count).unwrap_or(0),
                "message": "Secret manager unlocked."
            }))
            .into_response()
        }
        Err(error) => {
            let status = if error.to_string().contains("invalid")
                || error.to_string().contains("InvalidKey")
            {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

/// `POST /api/secrets/lock` — Lock encrypted store (clear key from memory).
pub async fn lock_secrets(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    // Clear key from OS credential store too.
    let keystore = crate::secrets::keystore::platform_keystore();
    if let Err(error) = keystore.delete_key(KEYSTORE_INSTANCE_ID) {
        tracing::warn!(%error, "failed to delete master key from OS credential store");
    }

    match store.lock() {
        Ok(()) => Json(serde_json::json!({
            "state": "locked",
            "message": "Secret manager locked. Secrets remain encrypted on disk."
        }))
        .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/secrets/rotate` — Rotate master key.
pub async fn rotate_key(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    match store.rotate_key() {
        Ok(new_key) => {
            // Update OS credential store with new key.
            let keystore = crate::secrets::keystore::platform_keystore();
            if let Err(error) = keystore.store_key(KEYSTORE_INSTANCE_ID, &new_key) {
                tracing::warn!(%error, "failed to store rotated key in OS credential store");
            }

            Json(serde_json::json!({
                "master_key": hex::encode(&new_key),
                "message": "Master key rotated. Save the new key — the old key no longer works."
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct MigrationItem {
    config_key: String,
    secret_name: String,
    category: SecretCategory,
}

#[derive(Serialize)]
struct MigrateResponse {
    migrated: Vec<MigrationItem>,
    skipped: Vec<String>,
    message: String,
}

/// `POST /api/secrets/migrate` — Auto-migrate literal keys from config.toml
/// to the secret store.
///
/// Scans the resolved config for plaintext credential values (not `env:` or
/// `secret:` prefixed) and moves them to the secret store, replacing the
/// config.toml entries with `secret:NAME` references.
pub async fn migrate_secrets(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    if store.state() == StoreState::Locked {
        return (
            StatusCode::LOCKED,
            Json(serde_json::json!({"error": "secret store is locked — unlock first"})),
        )
            .into_response();
    }

    // Read the raw config.toml to find literal key values.
    let config_path = state.config_path.read().await.clone();
    let config_content = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("failed to read config.toml: {error}")})),
            )
                .into_response();
        }
    };

    let mut doc = match config_content.parse::<toml_edit::DocumentMut>() {
        Ok(doc) => doc,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("failed to parse config.toml: {error}")})),
            )
                .into_response();
        }
    };

    let mut migrated = Vec::new();
    let skipped = Vec::new();

    // All secret migration is driven by SystemSecrets trait impls.
    // Each config section declares its own credential fields — no hard-coded
    // TOML paths needed.
    //
    // Non-adapter sections (LLM keys, search keys):
    migrate_section_secrets::<LlmConfig>(&store, &mut doc, &mut migrated);
    migrate_section_secrets::<DefaultsConfig>(&store, &mut doc, &mut migrated);
    // Messaging adapters (default + named instances):
    migrate_section_secrets::<DiscordConfig>(&store, &mut doc, &mut migrated);
    migrate_section_secrets::<SlackConfig>(&store, &mut doc, &mut migrated);
    migrate_section_secrets::<TelegramConfig>(&store, &mut doc, &mut migrated);
    migrate_section_secrets::<TwitchConfig>(&store, &mut doc, &mut migrated);
    migrate_section_secrets::<EmailConfig>(&store, &mut doc, &mut migrated);

    // Write updated config.toml if any migrations were made.
    if !migrated.is_empty()
        && let Err(error) = std::fs::write(&config_path, doc.to_string())
    {
        tracing::error!(%error, "failed to write updated config.toml after migration");
        return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("migrated {count} secrets to the store but failed to update config.toml: {error}", count = migrated.len())
                })),
            )
                .into_response();
    }

    let count = migrated.len();
    Json(MigrateResponse {
        migrated,
        skipped,
        message: if count > 0 {
            format!("Migrated {count} secrets. config.toml updated with secret: references.")
        } else {
            "No plaintext secrets found to migrate.".to_string()
        },
    })
    .into_response()
}

/// `POST /api/secrets/export` — Export all secrets as a JSON backup.
pub async fn export_secrets(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    match store.export_all() {
        Ok(export) => {
            let mut response = serde_json::json!({
                "version": export.version,
                "encrypted": export.encrypted,
                "entries": export.entries,
                "count": export.entries.len(),
            });
            if !export.encrypted {
                response["warning"] = serde_json::json!(
                    "Encryption is not enabled. This export contains plaintext secrets. \
                     Store it securely or enable encryption first with: spacebot secrets encrypt"
                );
            }
            Json(response).into_response()
        }
        Err(error) => {
            let status = if error.to_string().contains("locked") {
                StatusCode::LOCKED
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportBody {
    /// The export data to import (JSON object matching ExportData format).
    #[serde(flatten)]
    pub data: ExportData,
    /// Whether to overwrite existing secrets with the same name.
    #[serde(default)]
    pub overwrite: bool,
}

/// `POST /api/secrets/import` — Import secrets from a backup.
pub async fn import_secrets(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<ImportBody>,
) -> impl IntoResponse {
    let store = match get_secrets_store(&state) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };

    match store.import_all(&body.data, body.overwrite) {
        Ok(result) => {
            let message = if result.skipped.is_empty() {
                format!("Imported {} secrets.", result.imported)
            } else {
                format!(
                    "Imported {} secrets. {} conflicts (existing secrets with same name): {}",
                    result.imported,
                    result.skipped.len(),
                    result.skipped.join(", ")
                )
            };
            Json(serde_json::json!({
                "imported": result.imported,
                "skipped": result.skipped,
                "message": message,
            }))
            .into_response()
        }
        Err(error) => {
            let status = if error.to_string().contains("locked") {
                StatusCode::LOCKED
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

/// Try to migrate a single TOML field to the secret store.
///
/// Reads the value at `path` in the TOML document. If it's a plaintext literal
/// (not `env:` or `secret:` prefixed and not empty), stores it in the secret
/// store and replaces the TOML value with a `secret:NAME` reference.
fn try_migrate_field(
    store: &SecretsStore,
    doc: &mut toml_edit::DocumentMut,
    path: &[&str],
    secret_name: &str,
    migrated: &mut Vec<MigrationItem>,
) {
    let value_str = match get_toml_value(doc, path) {
        Some(item) => match item.as_str() {
            Some(s) => s.to_string(),
            None => return,
        },
        None => return,
    };

    if value_str.starts_with("env:") || value_str.starts_with("secret:") || value_str.is_empty() {
        return;
    }

    let category = auto_categorize(secret_name);
    if let Err(error) = store.set(secret_name, &value_str, category) {
        tracing::warn!(%error, secret_name, "failed to migrate secret");
        return;
    }

    set_toml_value(doc, path, &format!("secret:{secret_name}"));

    migrated.push(MigrationItem {
        config_key: path.join("."),
        secret_name: secret_name.to_string(),
        category,
    });
}

/// Migrate all credential fields for a config section that implements [`SystemSecrets`].
///
/// For non-adapter sections (e.g. `LlmConfig`, `DefaultsConfig`), migrates
/// fields at `{section}.{toml_key}`.
///
/// For messaging adapter sections (`is_messaging_adapter() == true`), additionally
/// scans `messaging.{section}.instances` for named instances and migrates each
/// instance's credential fields using the `{PREFIX}_{INSTANCE}_{SUFFIX}` naming
/// pattern (e.g. `DISCORD_ALERTS_BOT_TOKEN`).
fn migrate_section_secrets<T: SystemSecrets>(
    store: &SecretsStore,
    doc: &mut toml_edit::DocumentMut,
    migrated: &mut Vec<MigrationItem>,
) {
    let section = T::section();
    let fields = T::secret_fields();

    // Build the TOML path prefix for this section.
    // Messaging adapters live under `messaging.{section}`, others under `{section}`.
    let is_adapter = T::is_messaging_adapter();

    // Migrate default (top-level) fields.
    for field in fields {
        let path: Vec<&str> = if is_adapter {
            vec!["messaging", section, field.toml_key]
        } else {
            vec![section, field.toml_key]
        };
        try_migrate_field(store, doc, &path, field.secret_name, migrated);
    }

    // Named instances are only relevant for messaging adapters.
    if !is_adapter {
        return;
    }

    // Walk the TOML array at `messaging.{section}.instances` and migrate
    // each named instance's credential fields.
    let instances_array = doc
        .get("messaging")
        .and_then(|m| m.get(section))
        .and_then(|p| p.get("instances"))
        .and_then(|i| i.as_array_of_tables())
        .cloned();

    let Some(instances) = instances_array else {
        return;
    };

    for (index, instance) in instances.iter().enumerate() {
        let instance_name = match instance.get("name").and_then(|n| n.as_str()) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => continue,
        };

        for field in fields {
            let Some(secret_name) = field.instance_name(&instance_name) else {
                continue;
            };

            // Read the value from the instance entry.
            let value_str = match instance.get(field.toml_key).and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() && !s.starts_with("env:") && !s.starts_with("secret:") => {
                    s.to_string()
                }
                _ => continue,
            };

            let category = auto_categorize(&secret_name);
            if let Err(error) = store.set(&secret_name, &value_str, category) {
                tracing::warn!(%error, %secret_name, "failed to migrate instance secret");
                continue;
            }

            // Update the TOML value in-place within the instances array.
            set_instance_toml_value(
                doc,
                section,
                index,
                field.toml_key,
                &format!("secret:{secret_name}"),
            );

            migrated.push(MigrationItem {
                config_key: format!("messaging.{section}.instances[{index}].{}", field.toml_key),
                secret_name,
                category,
            });
        }
    }
}

fn get_toml_value<'a>(
    doc: &'a toml_edit::DocumentMut,
    path: &[&str],
) -> Option<&'a toml_edit::Item> {
    let mut current: &toml_edit::Item = doc.as_item();
    for key in path {
        current = current.get(key)?;
    }
    if current.is_none() {
        return None;
    }
    Some(current)
}

fn set_toml_value(doc: &mut toml_edit::DocumentMut, path: &[&str], value: &str) {
    if path.is_empty() {
        return;
    }
    let (parents, key) = path.split_at(path.len() - 1);
    let mut current: &mut toml_edit::Item = doc.as_item_mut();
    for parent in parents {
        if !current.is_table_like() {
            return;
        }
        current = &mut current[parent];
    }
    current[key[0]] = toml_edit::value(value);
}

/// Set a value inside `messaging.{platform}.instances[{index}].{key}`.
fn set_instance_toml_value(
    doc: &mut toml_edit::DocumentMut,
    platform: &str,
    index: usize,
    key: &str,
    value: &str,
) {
    let Some(instances) = doc
        .get_mut("messaging")
        .and_then(|m| m.get_mut(platform))
        .and_then(|p| p.get_mut("instances"))
        .and_then(|i| i.as_array_of_tables_mut())
    else {
        return;
    };
    if let Some(instance) = instances.get_mut(index) {
        instance[key] = toml_edit::value(value);
    }
}
