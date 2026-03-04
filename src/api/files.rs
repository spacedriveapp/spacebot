//! Workspace filesystem explorer API.
//!
//! Provides endpoints for browsing, reading, downloading, writing, deleting,
//! renaming, and uploading files within an agent's workspace directory.

use super::state::ApiState;
use crate::tools::file::best_effort_canonicalize;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Names of root-level directories that cannot be renamed or deleted.
const PROTECTED_DIRS: &[&str] = &["ingest", "skills"];

/// Names of identity files that cannot be written or deleted via this API.
const PROTECTED_IDENTITY_FILES: &[&str] = &["SOUL.md", "IDENTITY.md", "USER.md", "ROLE.md"];

/// Maximum file size for text preview reads (1 MiB).
const MAX_READ_BYTES: u64 = 1024 * 1024;

// -- Query/request types --

#[derive(Deserialize)]
pub(super) struct FilesQuery {
    agent_id: String,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
pub(super) struct FileWriteRequest {
    agent_id: String,
    path: String,
    /// File content (required for files, omitted for directories).
    content: Option<String>,
    /// When true, create a directory instead of a file.
    #[serde(default)]
    is_directory: bool,
}

#[derive(Deserialize)]
pub(super) struct FileRenameRequest {
    agent_id: String,
    old_path: String,
    new_path: String,
}

// -- Response types --

#[derive(Serialize)]
pub(super) struct FileEntry {
    name: String,
    entry_type: String,
    size: u64,
    modified_at: Option<String>,
}

#[derive(Serialize)]
pub(super) struct FileListResponse {
    entries: Vec<FileEntry>,
    path: String,
}

#[derive(Serialize)]
pub(super) struct FileReadResponse {
    content: String,
    size: u64,
    truncated: bool,
}

#[derive(Serialize)]
pub(super) struct FileWriteResponse {
    success: bool,
}

#[derive(Serialize)]
pub(super) struct FileDeleteResponse {
    success: bool,
}

#[derive(Serialize)]
pub(super) struct FileRenameResponse {
    success: bool,
}

#[derive(Serialize)]
pub(super) struct FileUploadResponse {
    uploaded: Vec<String>,
}

// -- Path resolution --

/// Resolve a user-supplied path relative to the workspace, ensuring it stays
/// within the workspace boundary. Rejects symlinks and `..` traversal.
fn resolve_workspace_path(workspace: &Path, raw: &str) -> Result<PathBuf, StatusCode> {
    let cleaned = raw.trim_start_matches('/');
    let path = if cleaned.is_empty() {
        workspace.to_path_buf()
    } else {
        workspace.join(cleaned)
    };

    let canonical = best_effort_canonicalize(&path);
    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    if !canonical.starts_with(&workspace_canonical) {
        tracing::warn!(
            path = %raw,
            workspace = %workspace.display(),
            "files API: path escapes workspace boundary"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    // Reject symlinks within the resolved path to prevent TOCTOU races.
    let mut check = workspace_canonical.clone();
    if let Ok(relative) = canonical.strip_prefix(&workspace_canonical) {
        for component in relative.components() {
            check.push(component);
            if let Ok(metadata) = std::fs::symlink_metadata(&check)
                && metadata.file_type().is_symlink()
            {
                tracing::warn!(
                    path = %check.display(),
                    "files API: symlink in path rejected"
                );
                return Err(StatusCode::FORBIDDEN);
            }
        }
    }

    Ok(canonical)
}

/// Check whether a path refers to a protected root directory (e.g. `ingest/`, `skills/`).
fn is_protected_root_dir(workspace: &Path, path: &Path) -> bool {
    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    if let Ok(relative) = path.strip_prefix(&workspace_canonical) {
        let components: Vec<_> = relative.components().collect();
        if components.len() == 1 {
            if let Some(name) = relative.file_name().and_then(|n| n.to_str()) {
                return PROTECTED_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d));
            }
        }
    }
    false
}

/// Check whether a path refers to a protected identity file at workspace root.
fn is_protected_identity_file(workspace: &Path, path: &Path) -> bool {
    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    if let Ok(relative) = path.strip_prefix(&workspace_canonical) {
        let components: Vec<_> = relative.components().collect();
        if components.len() == 1 {
            if let Some(name) = relative.file_name().and_then(|n| n.to_str()) {
                return PROTECTED_IDENTITY_FILES
                    .iter()
                    .any(|f| name.eq_ignore_ascii_case(f));
            }
        }
    }
    false
}

fn format_system_time(time: std::time::SystemTime) -> String {
    let duration = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Format as ISO 8601 UTC.
    let datetime = chrono::DateTime::from_timestamp(secs as i64, 0).unwrap_or_default();
    datetime.to_rfc3339()
}

fn get_workspace(state: &ApiState, agent_id: &str) -> Result<PathBuf, StatusCode> {
    let workspaces = state.agent_workspaces.load();
    workspaces
        .get(agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

// -- Handlers --

/// List directory contents within the agent's workspace.
pub(super) async fn list_files(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FilesQuery>,
) -> Result<Json<FileListResponse>, StatusCode> {
    let workspace = get_workspace(&state, &query.agent_id)?;
    let path = resolve_workspace_path(&workspace, &query.path)?;

    if !path.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut entries = Vec::new();
    let mut reader = tokio::fs::read_dir(&path).await.map_err(|error| {
        tracing::warn!(%error, path = %path.display(), "files API: failed to read directory");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    while let Some(entry) = reader.next_entry().await.map_err(|error| {
        tracing::warn!(%error, "files API: failed to read directory entry");
        StatusCode::INTERNAL_SERVER_ERROR
    })? {
        let metadata = entry.metadata().await.map_err(|error| {
            tracing::warn!(%error, "files API: failed to read entry metadata");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let entry_type = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };

        let modified_at = metadata.modified().ok().map(format_system_time);

        entries.push(FileEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            entry_type: entry_type.to_string(),
            size: metadata.len(),
            modified_at,
        });
    }

    // Sort: directories first, then files, both alphabetically.
    entries.sort_by(|a, b| {
        let a_is_dir = a.entry_type == "directory";
        let b_is_dir = b.entry_type == "directory";
        match (a_is_dir, b_is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    Ok(Json(FileListResponse {
        entries,
        path: query.path,
    }))
}

/// Read a text file's content for preview.
pub(super) async fn read_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FilesQuery>,
) -> Result<Json<FileReadResponse>, StatusCode> {
    let workspace = get_workspace(&state, &query.agent_id)?;
    let path = resolve_workspace_path(&workspace, &query.path)?;

    if !path.is_file() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let metadata = tokio::fs::metadata(&path).await.map_err(|error| {
        tracing::warn!(%error, path = %path.display(), "files API: failed to read file metadata");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let file_size = metadata.len();
    let truncated = file_size > MAX_READ_BYTES;
    let read_size = file_size.min(MAX_READ_BYTES) as usize;

    let raw = tokio::fs::read(&path).await.map_err(|error| {
        tracing::warn!(%error, path = %path.display(), "files API: failed to read file");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Take only up to the read limit and convert to string (lossy for binary files).
    let slice = &raw[..read_size.min(raw.len())];
    let content = String::from_utf8_lossy(slice).to_string();

    Ok(Json(FileReadResponse {
        content,
        size: file_size,
        truncated,
    }))
}

/// Download a file with Content-Disposition attachment header.
pub(super) async fn download_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FilesQuery>,
) -> Result<Response, StatusCode> {
    let workspace = get_workspace(&state, &query.agent_id)?;
    let path = resolve_workspace_path(&workspace, &query.path)?;

    if !path.is_file() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let data = tokio::fs::read(&path).await.map_err(|error| {
        tracing::warn!(%error, path = %path.display(), "files API: failed to read file for download");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");

    let content_disposition = format!("attachment; filename=\"{}\"", filename);
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_DISPOSITION, content_disposition),
        ],
        data,
    )
        .into_response())
}

/// Create a file or directory within the workspace.
pub(super) async fn write_file(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<FileWriteRequest>,
) -> Result<Json<FileWriteResponse>, StatusCode> {
    let workspace = get_workspace(&state, &request.agent_id)?;
    let path = resolve_workspace_path(&workspace, &request.path)?;

    // Block writes to protected identity files.
    if is_protected_identity_file(&workspace, &path) {
        tracing::warn!(
            path = %request.path,
            "files API: write rejected for protected identity file"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    if request.is_directory {
        tokio::fs::create_dir_all(&path).await.map_err(|error| {
            tracing::warn!(%error, path = %path.display(), "files API: failed to create directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    } else {
        let content = request.content.unwrap_or_default();

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                tracing::warn!(%error, path = %parent.display(), "files API: failed to create parent directory");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        }

        tokio::fs::write(&path, content).await.map_err(|error| {
            tracing::warn!(%error, path = %path.display(), "files API: failed to write file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    Ok(Json(FileWriteResponse { success: true }))
}

/// Delete a file or empty directory within the workspace.
pub(super) async fn delete_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FilesQuery>,
) -> Result<Json<FileDeleteResponse>, StatusCode> {
    let workspace = get_workspace(&state, &query.agent_id)?;
    let path = resolve_workspace_path(&workspace, &query.path)?;

    // Prevent deleting the workspace root itself.
    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    if path == workspace_canonical {
        return Err(StatusCode::FORBIDDEN);
    }

    // Prevent deleting protected root directories and identity files.
    if is_protected_root_dir(&workspace, &path) {
        tracing::warn!(
            path = %query.path,
            "files API: delete rejected for protected root directory"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    if is_protected_identity_file(&workspace, &path) {
        tracing::warn!(
            path = %query.path,
            "files API: delete rejected for protected identity file"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    if path.is_dir() {
        tokio::fs::remove_dir_all(&path).await.map_err(|error| {
            tracing::warn!(%error, path = %path.display(), "files API: failed to delete directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    } else if path.is_file() {
        tokio::fs::remove_file(&path).await.map_err(|error| {
            tracing::warn!(%error, path = %path.display(), "files API: failed to delete file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    } else {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(FileDeleteResponse { success: true }))
}

/// Rename or move a file/directory within the workspace.
pub(super) async fn rename_file(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<FileRenameRequest>,
) -> Result<Json<FileRenameResponse>, StatusCode> {
    let workspace = get_workspace(&state, &request.agent_id)?;
    let old_path = resolve_workspace_path(&workspace, &request.old_path)?;
    let new_path = resolve_workspace_path(&workspace, &request.new_path)?;

    // Prevent renaming protected root directories.
    if is_protected_root_dir(&workspace, &old_path) {
        tracing::warn!(
            path = %request.old_path,
            "files API: rename rejected for protected root directory"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    if is_protected_identity_file(&workspace, &old_path) {
        tracing::warn!(
            path = %request.old_path,
            "files API: rename rejected for protected identity file"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    if !old_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    // Ensure the parent of the new path exists.
    if let Some(parent) = new_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            tracing::warn!(%error, path = %parent.display(), "files API: failed to create parent for rename target");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    tokio::fs::rename(&old_path, &new_path)
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                from = %old_path.display(),
                to = %new_path.display(),
                "files API: failed to rename"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(FileRenameResponse { success: true }))
}

/// Upload files to a directory within the workspace via multipart form data.
/// When uploading to the `ingest/` directory, ingestion tracking records are
/// created so the background ingestion pipeline picks them up.
pub(super) async fn upload_files(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<FilesQuery>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<FileUploadResponse>, StatusCode> {
    let workspace = get_workspace(&state, &query.agent_id)?;
    let target_dir = resolve_workspace_path(&workspace, &query.path)?;

    tokio::fs::create_dir_all(&target_dir)
        .await
        .map_err(|error| {
            tracing::warn!(%error, path = %target_dir.display(), "files API: failed to create upload target directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Detect whether we're uploading into the ingest directory tree.
    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let ingest_dir = workspace_canonical.join("ingest");
    let is_ingest = target_dir.starts_with(&ingest_dir) || target_dir == ingest_dir;

    let mut uploaded = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("upload-{}.txt", uuid::Uuid::new_v4()));

        let data = field.bytes().await.map_err(|error| {
            tracing::warn!(%error, "files API: failed to read upload field");
            StatusCode::BAD_REQUEST
        })?;

        if data.is_empty() {
            continue;
        }

        let safe_name = Path::new(&filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload.txt");

        let mut target = target_dir.join(safe_name);

        // Deduplicate filenames if the target already exists.
        if target.exists() {
            let stem = Path::new(safe_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("upload");
            let ext = Path::new(safe_name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("txt");
            let unique = format!(
                "{}-{}.{}",
                stem,
                &uuid::Uuid::new_v4().to_string()[..8],
                ext
            );
            target = target_dir.join(unique);
        }

        tokio::fs::write(&target, &data).await.map_err(|error| {
            tracing::warn!(%error, path = %target.display(), "files API: failed to write uploaded file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        // If uploading to ingest/, create an ingestion tracking record.
        if is_ingest {
            if let Ok(content) = std::str::from_utf8(&data) {
                let hash = crate::agent::ingestion::content_hash(content);
                let pools = state.agent_pools.load();
                if let Some(pool) = pools.get(&query.agent_id) {
                    let file_size = data.len() as i64;
                    let _ = sqlx::query(
                        r#"
                        INSERT OR IGNORE INTO ingestion_files (content_hash, filename, file_size, total_chunks, status)
                        VALUES (?, ?, ?, 0, 'queued')
                        "#,
                    )
                    .bind(&hash)
                    .bind(safe_name)
                    .bind(file_size)
                    .execute(pool)
                    .await;
                }
            }
        }

        tracing::info!(
            agent_id = %query.agent_id,
            filename = %safe_name,
            bytes = data.len(),
            is_ingest,
            "file uploaded via files API"
        );

        uploaded.push(safe_name.to_string());
    }

    Ok(Json(FileUploadResponse { uploaded }))
}
