use super::state::ApiState;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct IngestFileInfo {
    content_hash: String,
    filename: String,
    file_size: i64,
    total_chunks: i64,
    chunks_completed: i64,
    status: String,
    started_at: String,
    completed_at: Option<String>,
}

#[derive(Serialize)]
pub(super) struct IngestFilesResponse {
    files: Vec<IngestFileInfo>,
}

#[derive(Serialize)]
pub(super) struct IngestUploadResponse {
    uploaded: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct IngestDeleteResponse {
    success: bool,
}

#[derive(Deserialize)]
pub(super) struct IngestQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct IngestDeleteQuery {
    agent_id: String,
    content_hash: String,
}

/// List ingested files with progress info for in-progress ones.
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
pub(super) async fn upload_ingest_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestQuery>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<IngestUploadResponse>, StatusCode> {
    let workspaces = state.agent_workspaces.load();
    let workspace = workspaces.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let ingest_dir = workspace.join("ingest");

    tokio::fs::create_dir_all(&ingest_dir).await.map_err(|error| {
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
            let unique = format!("{}-{}.{}", stem, &uuid::Uuid::new_v4().to_string()[..8], ext);
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

/// Delete a completed ingestion file record from history.
pub(super) async fn delete_ingest_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestDeleteQuery>,
) -> Result<Json<IngestDeleteResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    sqlx::query("DELETE FROM ingestion_files WHERE content_hash = ?")
        .bind(&query.content_hash)
        .execute(pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to delete ingest file record");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(IngestDeleteResponse { success: true }))
}
