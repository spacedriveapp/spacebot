//! Durable binary location observability.

use super::state::ApiState;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use std::io::ErrorKind;
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

    let tools_bin_is_dir = match tokio::fs::metadata(&tools_bin).await {
        Ok(metadata) => metadata.is_dir(),
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(path = %tools_bin.display(), %error, "failed to stat tools/bin");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if tools_bin_is_dir {
        let mut entries = tokio::fs::read_dir(&tools_bin).await.map_err(|error| {
            tracing::warn!(path = %tools_bin.display(), %error, "failed to read tools/bin");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            tracing::warn!(path = %tools_bin.display(), %error, "failed during tools/bin iteration");
            StatusCode::INTERNAL_SERVER_ERROR
        })? {
            let entry_path = entry.path();
            let metadata = match tokio::fs::symlink_metadata(&entry_path).await {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::debug!(
                        path = %entry_path.display(),
                        %error,
                        "skipping entry with unreadable metadata"
                    );
                    continue;
                }
            };

            if metadata.file_type().is_symlink() || !metadata.is_file() {
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
