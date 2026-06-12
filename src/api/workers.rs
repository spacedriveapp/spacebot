//! Workers API endpoints: list and detail views for worker runs.

use super::state::ApiState;

use crate::conversation::history::ProcessRunLogger;
use crate::conversation::worker_transcript;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WorkerListQuery {
    agent_id: String,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    status: Option<String>,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkerListResponse {
    workers: Vec<WorkerListItem>,
    total: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkerListItem {
    id: String,
    task: String,
    status: String,
    worker_type: String,
    channel_id: Option<String>,
    channel_name: Option<String>,
    started_at: String,
    completed_at: Option<String>,
    has_transcript: bool,
    /// Live status text from StatusBlock (running workers only).
    live_status: Option<String>,
    /// Total tool calls. From DB for completed workers, from StatusBlock for running.
    tool_calls: i64,
    /// OpenCode server port (for workers with an embeddable web UI).
    opencode_port: Option<i32>,
    /// OpenCode session ID (for workers with an embeddable web UI).
    opencode_session_id: Option<String>,
    /// Working directory for OpenCode workers.
    directory: Option<String>,
    /// Whether this worker accepts follow-up input via route.
    interactive: bool,
    /// Project ID this worker is linked to.
    project_id: Option<String>,
    /// Project name (resolved via join).
    project_name: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WorkerDetailQuery {
    agent_id: String,
    worker_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkerDetailResponse {
    id: String,
    task: String,
    result: Option<String>,
    status: String,
    worker_type: String,
    channel_id: Option<String>,
    channel_name: Option<String>,
    started_at: String,
    completed_at: Option<String>,
    transcript: Option<Vec<worker_transcript::TranscriptStep>>,
    tool_calls: i64,
    /// OpenCode session ID (for workers with an embeddable web UI).
    opencode_session_id: Option<String>,
    /// OpenCode server port (for workers with an embeddable web UI).
    opencode_port: Option<i32>,
    /// Whether this worker accepts follow-up input via route.
    interactive: bool,
    /// Working directory for OpenCode workers.
    directory: Option<String>,
}

/// List worker runs for an agent, with live status merged from StatusBlocks.
#[utoipa::path(
    get,
    path = "/agents/workers",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("limit" = i64, Query, description = "Maximum number of results to return"),
        ("offset" = i64, Query, description = "Number of results to skip"),
        ("status" = Option<String>, Query, description = "Filter by worker status"),
    ),
    responses(
        (status = 200, body = WorkerListResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "workers",
)]
pub(super) async fn list_workers(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WorkerListQuery>,
) -> Result<Json<WorkerListResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = ProcessRunLogger::new(pool.clone());

    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);
    let (rows, total) = logger
        .list_worker_runs(&query.agent_id, limit, offset, query.status.as_deref())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to list worker runs");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Resolve project names from the global ProjectStore (projects live in the
    // instance DB, not in per-agent DBs, so history.rs can't JOIN them in SQL).
    let project_names: std::collections::HashMap<String, String> = {
        let mut names = std::collections::HashMap::new();
        let mut seen = std::collections::HashSet::new();
        let store_guard = state.project_store.load();
        if let Some(store) = store_guard.as_ref() {
            for row in &rows {
                let Some(project_id) = row.project_id.as_deref() else {
                    continue;
                };
                if !seen.insert(project_id.to_string()) {
                    continue;
                }
                if let Ok(Some(project)) = store.get_project(project_id).await {
                    names.insert(project_id.to_string(), project.name);
                }
            }
        }
        names
    };

    // Build a live status lookup from all channel StatusBlocks
    let live_statuses = {
        let blocks = state.channel_status_blocks.read().await;
        let mut map = std::collections::HashMap::new();
        for (_channel_id, status_block) in blocks.iter() {
            let block = status_block.read().await;
            for worker in &block.active_workers {
                map.insert(
                    worker.id.to_string(),
                    (worker.status.clone(), worker.tool_calls),
                );
            }
        }
        map
    };

    let workers = rows
        .into_iter()
        .map(|row| {
            let (live_status, live_tool_calls) = live_statuses
                .get(&row.id)
                .map(|(status, calls)| (Some(status.clone()), *calls as i64))
                .unwrap_or((None, 0));

            // Use live tool call count for running workers, DB count for completed
            let tool_calls = if row.status == "running" && live_tool_calls > 0 {
                live_tool_calls
            } else {
                row.tool_calls
            };

            WorkerListItem {
                id: row.id,
                task: row.task,
                status: row.status,
                worker_type: row.worker_type,
                channel_id: row.channel_id,
                channel_name: row.channel_name,
                started_at: row.started_at,
                completed_at: row.completed_at,
                has_transcript: row.has_transcript,
                live_status,
                tool_calls,
                opencode_port: row.opencode_port,
                opencode_session_id: row.opencode_session_id,
                directory: row.directory,
                interactive: row.interactive,
                project_name: row
                    .project_id
                    .as_deref()
                    .and_then(|id| project_names.get(id).cloned()),
                project_id: row.project_id,
            }
        })
        .collect();

    Ok(Json(WorkerListResponse { workers, total }))
}

/// Get full detail for a single worker run, including decompressed transcript.
#[utoipa::path(
    get,
    path = "/agents/workers/detail",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("worker_id" = String, Query, description = "Worker ID"),
    ),
    responses(
        (status = 200, body = WorkerDetailResponse),
        (status = 404, description = "Agent or worker not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "workers",
)]
pub(super) async fn worker_detail(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WorkerDetailQuery>,
) -> Result<Json<WorkerDetailResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = ProcessRunLogger::new(pool.clone());

    let detail = logger
        .get_worker_detail(&query.agent_id, &query.worker_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, worker_id = %query.worker_id, "failed to load worker detail");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let transcript = match detail.transcript_blob.as_deref() {
        Some(blob) => worker_transcript::deserialize_transcript(blob)
            .map_err(|error| {
                tracing::warn!(%error, worker_id = %query.worker_id, "failed to decompress transcript");
            })
            .ok(),
        None => {
            // No persisted transcript yet — check the live transcript cache
            // so page refreshes can recover in-progress worker transcripts.
            state.get_live_transcript(&query.worker_id).await
        }
    };

    Ok(Json(WorkerDetailResponse {
        id: detail.id,
        task: detail.task,
        result: detail.result,
        status: detail.status,
        worker_type: detail.worker_type,
        channel_id: detail.channel_id,
        channel_name: detail.channel_name,
        started_at: detail.started_at,
        completed_at: detail.completed_at,
        transcript,
        tool_calls: detail.tool_calls,
        opencode_session_id: detail.opencode_session_id,
        opencode_port: detail.opencode_port,
        interactive: detail.interactive,
        directory: detail.directory,
    }))
}
