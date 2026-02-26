use super::state::ApiState;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct SshStatusResponse {
    enabled: bool,
    running: bool,
    port: u16,
    has_authorized_key: bool,
}

#[derive(Deserialize)]
pub(super) struct SetAuthorizedKeyRequest {
    public_key: String,
}

#[derive(Serialize)]
pub(super) struct SetAuthorizedKeyResponse {
    success: bool,
    message: String,
}

/// GET /api/ssh/status — returns current SSH server state.
pub(super) async fn ssh_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<SshStatusResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (enabled, port) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let enabled = doc
            .get("ssh")
            .and_then(|s| s.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let port = doc
            .get("ssh")
            .and_then(|s| s.get("port"))
            .and_then(|v| v.as_integer())
            .and_then(|i| u16::try_from(i).ok())
            .unwrap_or(22);
        (enabled, port)
    } else {
        (false, 22)
    };

    let mut manager = state.ssh_manager.lock().await;
    let running = manager.is_running();
    let has_authorized_key = manager.has_authorized_key();

    Ok(Json(SshStatusResponse {
        enabled,
        running,
        port,
        has_authorized_key,
    }))
}

/// PUT /api/ssh/authorized-key — set the authorized public key for SSH access.
pub(super) async fn set_authorized_key(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetAuthorizedKeyRequest>,
) -> Result<Json<SetAuthorizedKeyResponse>, StatusCode> {
    let pubkey = request.public_key.trim();
    if pubkey.is_empty() {
        return Ok(Json(SetAuthorizedKeyResponse {
            success: false,
            message: "public_key is required".to_string(),
        }));
    }

    // Validate it looks like an SSH public key
    if !pubkey.starts_with("ssh-") && !pubkey.starts_with("ecdsa-") {
        return Ok(Json(SetAuthorizedKeyResponse {
            success: false,
            message: "Invalid SSH public key format".to_string(),
        }));
    }

    let manager = state.ssh_manager.lock().await;
    if let Err(error) = manager.set_authorized_key(pubkey).await {
        tracing::error!(%error, "failed to set SSH authorized key");
        return Ok(Json(SetAuthorizedKeyResponse {
            success: false,
            message: format!("Failed to write authorized key: {error}"),
        }));
    }

    Ok(Json(SetAuthorizedKeyResponse {
        success: true,
        message: "Authorized key updated".to_string(),
    }))
}

/// DELETE /api/ssh/authorized-key — remove all authorized keys.
pub(super) async fn clear_authorized_keys(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<SetAuthorizedKeyResponse>, StatusCode> {
    let manager = state.ssh_manager.lock().await;
    if let Err(error) = manager.clear_authorized_keys().await {
        tracing::error!(%error, "failed to clear SSH authorized keys");
        return Ok(Json(SetAuthorizedKeyResponse {
            success: false,
            message: format!("Failed to clear authorized keys: {error}"),
        }));
    }

    Ok(Json(SetAuthorizedKeyResponse {
        success: true,
        message: "Authorized keys cleared".to_string(),
    }))
}
