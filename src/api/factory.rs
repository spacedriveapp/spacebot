//! Factory API endpoints: preset listing and loading.

use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::factory::presets::PresetRegistry;

/// List all available preset archetypes (metadata only).
#[utoipa::path(
    get,
    path = "/factory/presets",
    responses(
        (status = 200, body = Vec<crate::factory::presets::PresetMeta>),
    ),
    tag = "factory",
)]
pub async fn list_presets() -> impl IntoResponse {
    let presets = PresetRegistry::list();
    Json(serde_json::json!({ "presets": presets }))
}

/// Load a full preset by ID, including soul, identity, and role content.
#[utoipa::path(
    get,
    path = "/factory/presets/{id}",
    params(
        ("id" = String, Path, description = "Preset ID"),
    ),
    responses(
        (status = 200, body = crate::factory::presets::Preset),
        (status = 404, description = "Preset not found"),
    ),
    tag = "factory",
)]
pub async fn get_preset(Path(preset_id): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    let preset = PresetRegistry::load(&preset_id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({ "preset": preset })))
}
