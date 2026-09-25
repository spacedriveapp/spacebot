use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct IngestFileInfo {
    pub content_hash: String,
    pub filename: String,
    pub file_size: i64,
    pub total_chunks: i64,
    pub chunks_completed: i64,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct IngestFilesResponse {
    pub files: Vec<IngestFileInfo>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct IngestUploadResponse {
    pub uploaded: Vec<String>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct IngestDeleteResponse {
    pub success: bool,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct IngestQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct IngestDeleteQuery {
    agent_id: String,
    content_hash: String,
}

/// List ingested files with progress info for in-progress ones.
#[utoipa::path(
    get,
    path = "/agents/ingest/files",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = IngestFilesResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "ingest",
)]
pub(super) async fn list_ingest_files(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestQuery>,
) -> Result<Json<IngestFilesResponse>, StatusCode> {
    use sqlx::Row as _;

    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let rows = sqlx::query(
        r#"
        SELECT f.content_hash, f.filename, f.file_size, f.total_chunks, f.status,
               f.started_at, f.completed_at,
               COALESCE(p.done, 0) as chunks_completed
        FROM ingestion_files f
        LEFT JOIN (
            SELECT content_hash, COUNT(*) as done
            FROM ingestion_progress
            GROUP BY content_hash
        ) p ON f.content_hash = p.content_hash
        ORDER BY f.started_at DESC
        LIMIT 100
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to list ingest files");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let files = rows
        .into_iter()
        .map(|row| IngestFileInfo {
            content_hash: row.get("content_hash"),
            filename: row.get("filename"),
            file_size: row.get("file_size"),
            total_chunks: row.get("total_chunks"),
            chunks_completed: row.get("chunks_completed"),
            status: row.get("status"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
        })
        .collect();

    Ok(Json(IngestFilesResponse { files }))
}

/// Upload one or more files to the agent's ingest directory.
#[utoipa::path(
    post,
    path = "/agents/ingest/files",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = IngestUploadResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "ingest",
)]
pub(super) async fn upload_ingest_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestQuery>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<IngestUploadResponse>, StatusCode> {
    let workspaces = state.agent_workspaces.load();
    let workspace = workspaces
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let ingest_dir = workspace.join("ingest");

    tokio::fs::create_dir_all(&ingest_dir)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to create ingest directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut uploaded = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("upload-{}.txt", uuid::Uuid::new_v4()));

        let data = field.bytes().await.map_err(|error| {
            tracing::warn!(%error, "failed to read upload field");
            StatusCode::BAD_REQUEST
        })?;

        if data.is_empty() {
            continue;
        }

        let safe_name = Path::new(&filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload.txt");

        let target = ingest_dir.join(safe_name);

        let target = if target.exists() {
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
            ingest_dir.join(unique)
        } else {
            target
        };

        tokio::fs::write(&target, &data).await.map_err(|error| {
            tracing::warn!(%error, path = %target.display(), "failed to write uploaded file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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

        tracing::info!(
            agent_id = %query.agent_id,
            filename = %safe_name,
            bytes = data.len(),
            "file uploaded to ingest directory"
        );

        uploaded.push(safe_name.to_string());
    }

    Ok(Json(IngestUploadResponse { uploaded }))
}

/// Remove an ingest file from the source of truth (disk) and purge its tracking
/// rows. The disk file is the loop's input; deleting only the DB row lets the
/// next poll cycle re-discover the file and re-create the row ("reappears").
pub(super) async fn purge_ingest_file(
    pool: &sqlx::SqlitePool,
    ingest_dir: &Path,
    content_hash: &str,
) -> anyhow::Result<()> {
    // Look up the on-disk filename for this hash, then remove the file.
    if let Some(filename) = sqlx::query_scalar::<_, String>(
        "SELECT filename FROM ingestion_files WHERE content_hash = ?",
    )
    .bind(content_hash)
    .fetch_optional(pool)
    .await?
    {
        // Defense-in-depth: the filename comes from the DB and is sanitized on
        // write, but this is a destructive delete — reject any non-normal path
        // component (`..`, absolute paths) before joining so we cannot escape
        // ingest_dir.
        let filename_path = Path::new(&filename);
        if filename_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("refusing to delete ingest file with unsafe path: {filename}");
        }
        let path = ingest_dir.join(filename_path);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    sqlx::query("DELETE FROM ingestion_progress WHERE content_hash = ?")
        .bind(content_hash)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM ingestion_files WHERE content_hash = ?")
        .bind(content_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete a completed ingestion file record from history.
#[utoipa::path(
    delete,
    path = "/agents/ingest/files",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("content_hash" = String, Query, description = "Content hash of the file to delete"),
    ),
    responses(
        (status = 200, body = IngestDeleteResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "ingest",
)]
pub(super) async fn delete_ingest_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestDeleteQuery>,
) -> Result<Json<IngestDeleteResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let workspaces = state.agent_workspaces.load();
    let workspace = workspaces
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let ingest_dir = workspace.join("ingest");

    purge_ingest_file(pool, &ingest_dir, &query.content_hash)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to purge ingest file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(IngestDeleteResponse { success: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn test_purge_removes_disk_file_and_rows() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ingest_dir = dir.path().to_path_buf();
        let file = ingest_dir.join("notes.txt");
        tokio::fs::write(&file, b"hello").await.unwrap();
        let hash = crate::agent::ingestion::content_hash("hello");

        sqlx::query("INSERT INTO ingestion_files (content_hash, filename, file_size, total_chunks, status) VALUES (?, 'notes.txt', 5, 1, 'failed')")
            .bind(&hash).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO ingestion_progress (content_hash, chunk_index, total_chunks, filename) VALUES (?, 0, 1, 'notes.txt')")
            .bind(&hash).execute(&pool).await.unwrap();

        purge_ingest_file(&pool, &ingest_dir, &hash).await.unwrap();

        assert!(!file.exists(), "disk file must be removed");
        let files: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingestion_files WHERE content_hash = ?")
                .bind(&hash)
                .fetch_one(&pool)
                .await
                .unwrap();
        let prog: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM ingestion_progress WHERE content_hash = ?")
                .bind(&hash)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(files, 0, "ingestion_files row must be deleted");
        assert_eq!(prog, 0, "ingestion_progress rows must be deleted");
    }
}
