//! Filesystem browsing endpoints for directory selection.

use axum::Json;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize)]
pub(super) struct ListDirQuery {
    /// The directory path to list. If omitted, lists user home directory.
    path: Option<String>,
}

#[derive(Serialize)]
pub(super) struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
}

#[derive(Serialize)]
pub(super) struct ListDirResponse {
    /// The absolute path of the listed directory.
    path: String,
    /// Parent directory path, if any.
    parent: Option<String>,
    /// Entries in the directory (directories first, then files).
    entries: Vec<DirEntry>,
}

/// List the contents of a directory. Defaults to the user's home directory.
pub(super) async fn list_dir(
    Query(query): Query<ListDirQuery>,
) -> Result<Json<ListDirResponse>, impl IntoResponse> {
    let dir = match &query.path {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    };

    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid path: {e}") })),
            ));
        }
    };

    if !dir.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Path is not a directory" })),
        ));
    }

    let mut entries = Vec::new();

    let mut read_dir = match tokio::fs::read_dir(&dir).await {
        Ok(rd) => rd,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Cannot read directory: {e}") })),
            ));
        }
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files/directories (starting with .)
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
        let path = entry.path().to_string_lossy().to_string();
        entries.push(DirEntry { name, path, is_dir });
    }

    // Sort: directories first, then alphabetically
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));

    let parent = dir.parent().map(|p| p.to_string_lossy().to_string());

    Ok(Json(ListDirResponse {
        path: dir.to_string_lossy().to_string(),
        parent,
        entries,
    }))
}
