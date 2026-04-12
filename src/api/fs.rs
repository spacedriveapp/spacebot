//! Filesystem browsing endpoints for directory selection.

use super::state::ApiState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

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

// ---------------------------------------------------------------------------
// Sandboxed file read — for the Code Graph inspector panel
// ---------------------------------------------------------------------------

/// Maximum file size the read endpoint will return, in bytes (2 MB).
const MAX_READ_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Deserialize, utoipa::IntoParams)]
pub(super) struct ReadFileQuery {
    /// Code graph project ID — used to resolve the sandbox root.
    project_id: String,
    /// Path to read. Either absolute (must live under the project root) or
    /// relative (resolved against the project root).
    path: String,
    /// Optional 1-indexed inclusive start line.
    #[serde(default)]
    start_line: Option<u32>,
    /// Optional 1-indexed inclusive end line.
    #[serde(default)]
    end_line: Option<u32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ReadFileResponse {
    /// Absolute path that was actually read (post-canonicalization).
    path: String,
    /// UTF-8 file content (possibly sliced by line range).
    content: String,
    /// 1-indexed line number of the first line in `content`. 1 when the
    /// whole file was returned, otherwise the clamped `start_line`.
    start_line: u32,
    /// Total number of lines in the file (before slicing).
    total_lines: u32,
    /// Language hint derived from the file extension (e.g. `rust`, `typescript`).
    language: String,
}

/// GET /fs/read-file — read a file inside a registered code graph project.
///
/// Sandbox rules:
/// 1. `project_id` is required and must resolve to a registered project.
/// 2. The requested path is canonicalized, then compared (case-insensitively
///    on Windows) against the canonicalized project root. Any path that
///    escapes the root is rejected with 400.
/// 3. File size is capped at 2 MB.
#[utoipa::path(
    get,
    path = "/fs/read-file",
    params(ReadFileQuery),
    responses(
        (status = 200, description = "File content", body = ReadFileResponse),
        (status = 400, description = "Path escape or invalid input"),
        (status = 404, description = "Project or file not found"),
        (status = 413, description = "File exceeds 2 MB cap"),
    ),
    tag = "fs"
)]
pub(super) async fn read_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ReadFileQuery>,
) -> Result<Json<ReadFileResponse>, StatusCode> {
    // Resolve the project root via the code graph manager. Mirrors the
    // get_manager helper pattern used in super::codegraph.
    let manager: std::sync::Arc<crate::codegraph::CodeGraphManager> = {
        let guard = state.codegraph_manager.load();
        match guard.as_ref() {
            Some(m) => m.clone(),
            None => return Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    };

    let project = manager
        .get_project(&query.project_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    // Canonicalize both sides. On Windows this returns \\?\C:\… extended
    // paths — that's fine, we compare component-wise via starts_with.
    let root = project.root_path.canonicalize().map_err(|e| {
        tracing::warn!(%e, root = %project.root_path.display(), "failed to canonicalize project root");
        StatusCode::NOT_FOUND
    })?;

    // Build the target path: if the caller supplied an absolute path use
    // it as-is, otherwise resolve relative to the project root.
    let requested = PathBuf::from(&query.path);
    let joined = if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    };

    let target = joined.canonicalize().map_err(|e| {
        tracing::debug!(%e, path = %joined.display(), "canonicalize target failed");
        StatusCode::NOT_FOUND
    })?;

    // Sandbox check. starts_with on PathBuf is component-wise, so the
    // \\?\ prefix difference between root and target on Windows matters —
    // both have been canonicalized so they share the same prefix style.
    // Windows filesystems are case-insensitive, so we lowercase both sides
    // for the comparison.
    let target_str = target.to_string_lossy().to_lowercase();
    let root_str = root.to_string_lossy().to_lowercase();
    if !target_str.starts_with(&root_str) {
        tracing::warn!(
            target = %target.display(),
            root = %root.display(),
            "rejected file read outside project root"
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    // Enforce the size cap before reading.
    let metadata = tokio::fs::metadata(&target).await.map_err(|e| {
        tracing::debug!(%e, path = %target.display(), "metadata failed");
        StatusCode::NOT_FOUND
    })?;
    if !metadata.is_file() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if metadata.len() > MAX_READ_FILE_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let raw = tokio::fs::read(&target).await.map_err(|e| {
        tracing::error!(%e, path = %target.display(), "read_file failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Tolerate non-UTF8 bytes — replacement chars are fine for display.
    let content = String::from_utf8_lossy(&raw).into_owned();
    let total_lines = content.lines().count() as u32;

    // Apply line slicing if requested. Both bounds are 1-indexed inclusive.
    let (content, start_line) = match (query.start_line, query.end_line) {
        (Some(s), _) | (None, Some(s)) if s > 0 => {
            let start = s.saturating_sub(1) as usize;
            let end = query
                .end_line
                .map(|e| e as usize)
                .unwrap_or(total_lines as usize)
                .max(s as usize);
            let sliced: String = content
                .lines()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect::<Vec<_>>()
                .join("\n");
            (sliced, s)
        }
        _ => (content, 1u32),
    };

    let language = language_from_path(&target);

    Ok(Json(ReadFileResponse {
        path: target.to_string_lossy().to_string(),
        content,
        start_line,
        total_lines,
        language,
    }))
}

/// Map a file extension to a language slug the frontend CodeViewer
/// understands. Unknown extensions fall back to `"plaintext"`.
fn language_from_path(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "php" => "php",
        "scala" => "scala",
        "sh" | "bash" | "zsh" => "shell",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "sql" => "sql",
        _ => "plaintext",
    }
    .to_string()
}
