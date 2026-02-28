//! Durable binary location observability.

use super::state::ApiState;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct ToolsResponse {
    tools_bin: String,
    binaries: Vec<BinaryEntry>,
}

#[derive(Serialize)]
pub(super) struct BinaryEntry {
    name: String,
    size: u64,
    modified: Option<String>,
}

/// List the contents of the durable `tools/bin` directory.
pub(super) async fn list_tools(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ToolsResponse>, StatusCode> {
    let instance_dir = state.instance_dir.load();
    let tools_bin = instance_dir.join("tools/bin");

    let mut binaries = Vec::new();

    if tools_bin.is_dir() {
        let entries = std::fs::read_dir(&tools_bin).map_err(|error| {
            tracing::warn!(path = %tools_bin.display(), %error, "failed to read tools/bin");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::debug!(%error, "skipping unreadable tools/bin entry");
                    continue;
                }
            };

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if !metadata.is_file() {
                continue;
            }

            let modified = metadata.modified().ok().map(|time| {
                let datetime: chrono::DateTime<chrono::Utc> = time.into();
                datetime.to_rfc3339()
            });

            binaries.push(BinaryEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: metadata.len(),
                modified,
            });
        }
    }

    binaries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(ToolsResponse {
        tools_bin: tools_bin.display().to_string(),
        binaries,
    }))
}
