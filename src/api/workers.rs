//! Workers API endpoints: list and detail views for worker runs.

use super::state::ApiState;

use crate::ProcessId;
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
    backend: String,
    registration_id: Option<String>,
    runtime_state: Option<String>,
    runtime_attached: bool,
    routable: bool,
    channel_id: Option<String>,
    channel_name: Option<String>,
    started_at: String,
    completed_at: Option<String>,
    has_transcript: bool,
    /// Live status text from the process control registry.
    live_status: Option<String>,
    /// Total tool calls. From DB for completed workers, from the registry for live workers.
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
    backend: String,
    registration_id: Option<String>,
    runtime_state: Option<String>,
    runtime_attached: bool,
    routable: bool,
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

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct ProcessListQuery {
    agent_id: String,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    status: Option<String>,
    kind: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct ProcessDetailQuery {
    agent_id: String,
    kind: String,
    process_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProcessListResponse {
    processes: Vec<ProcessResponse>,
    total: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProcessResponse {
    kind: String,
    id: String,
    input: String,
    output: Option<String>,
    status: String,
    process_type: String,
    profile: Option<String>,
    channel_id: Option<String>,
    channel_name: Option<String>,
    started_at: String,
    completed_at: Option<String>,
    has_transcript: bool,
    transcript: Option<Vec<worker_transcript::TranscriptStep>>,
    tool_calls: i64,
    model: Option<String>,
    max_turns: Option<i64>,
    opencode_session_id: Option<String>,
    opencode_port: Option<i32>,
    directory: Option<String>,
    interactive: bool,
    project_id: Option<String>,
}

/// List worker runs for an agent, with live state merged from the process control registry.
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

    let registries = state.process_control_registries.load();
    let registry = registries
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let live_workers = registry
        .list_worker_snapshots()
        .await
        .into_iter()
        .map(|worker| (worker.worker_id.to_string(), worker))
        .collect::<std::collections::HashMap<_, _>>();

    let workers = rows
        .into_iter()
        .map(|row| {
            let live = live_workers.get(&row.id);
            let live_status = live.map(|worker| worker.status.clone());
            let backend = live
                .map(|worker| worker.backend.to_string())
                .unwrap_or_else(|| row.worker_type.clone());

            WorkerListItem {
                id: row.id,
                task: row.task,
                status: row.status,
                worker_type: row.worker_type,
                backend,
                registration_id: live.map(|worker| worker.registration_id.to_string()),
                runtime_state: live.map(|worker| worker.state.to_string()),
                runtime_attached: live.is_some(),
                routable: live.is_some_and(|worker| worker.routable),
                channel_id: row.channel_id,
                channel_name: row.channel_name,
                started_at: row.started_at,
                completed_at: row.completed_at,
                has_transcript: row.has_transcript,
                live_status,
                tool_calls: live.map_or(row.tool_calls, |worker| {
                    i64::try_from(worker.tool_calls).unwrap_or(i64::MAX)
                }),
                opencode_port: row.opencode_port,
                opencode_session_id: row.opencode_session_id,
                directory: row.directory,
                interactive: live.map_or(row.interactive, |worker| worker.interactive),
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

    let worker_id = query.worker_id.parse().map_err(|error| {
        tracing::warn!(%error, worker_id = %query.worker_id, "invalid worker ID");
        StatusCode::BAD_REQUEST
    })?;
    let registries = state.process_control_registries.load();
    let registry = registries
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let live = registry.worker_snapshot(worker_id).await;

    let transcript = match detail.transcript_blob.as_deref() {
        Some(blob) => worker_transcript::deserialize_transcript(blob)
            .map_err(|error| {
                tracing::warn!(%error, worker_id = %query.worker_id, "failed to decompress transcript");
            })
            .ok(),
        None => {
            // No persisted transcript yet — check the live transcript cache
            // so page refreshes can recover in-progress worker transcripts.
            state
                .get_live_transcript(
                    &ProcessId::Worker(worker_id),
                    live.as_ref().map(|worker| worker.registration_id),
                )
                .await
        }
    };
    let backend = live
        .as_ref()
        .map(|worker| worker.backend.to_string())
        .unwrap_or_else(|| detail.worker_type.clone());

    Ok(Json(WorkerDetailResponse {
        id: detail.id,
        task: detail.task,
        result: detail.result,
        status: detail.status,
        worker_type: detail.worker_type,
        backend,
        registration_id: live
            .as_ref()
            .map(|worker| worker.registration_id.to_string()),
        runtime_state: live.as_ref().map(|worker| worker.state.to_string()),
        runtime_attached: live.is_some(),
        routable: live.as_ref().is_some_and(|worker| worker.routable),
        channel_id: detail.channel_id,
        channel_name: detail.channel_name,
        started_at: detail.started_at,
        completed_at: detail.completed_at,
        transcript,
        tool_calls: live.as_ref().map_or(detail.tool_calls, |worker| {
            i64::try_from(worker.tool_calls).unwrap_or(i64::MAX)
        }),
        opencode_session_id: detail.opencode_session_id,
        opencode_port: detail.opencode_port,
        interactive: live
            .as_ref()
            .map_or(detail.interactive, |worker| worker.interactive),
        directory: detail.directory,
    }))
}

/// List branch and worker runs for an agent.
#[utoipa::path(
    get,
    path = "/agents/processes",
    params(ProcessListQuery),
    responses(
        (status = 200, body = ProcessListResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "workers",
)]
pub(super) async fn list_processes(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProcessListQuery>,
) -> Result<Json<ProcessListResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = ProcessRunLogger::new(pool.clone());
    let (rows, total) = logger
        .list_process_runs(
            &query.agent_id,
            query.limit.clamp(1, 200),
            query.offset.max(0),
            query.status.as_deref(),
            query.kind.as_deref(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to list process runs");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let processes = rows
        .into_iter()
        .map(|run| process_response(run, None))
        .collect();
    Ok(Json(ProcessListResponse { processes, total }))
}

/// Get one branch or worker run with its transcript.
#[utoipa::path(
    get,
    path = "/agents/processes/detail",
    params(ProcessDetailQuery),
    responses(
        (status = 200, body = ProcessResponse),
        (status = 404, description = "Agent or process not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "workers",
)]
pub(super) async fn process_detail(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProcessDetailQuery>,
) -> Result<Json<ProcessResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = ProcessRunLogger::new(pool.clone());
    let detail = logger
        .get_process_detail(&query.agent_id, &query.kind, &query.process_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, process_id = %query.process_id, "failed to load process detail");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    let process_id = match query.kind.as_str() {
        "branch" => ProcessId::Branch(
            query
                .process_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?,
        ),
        "worker" => ProcessId::Worker(
            query
                .process_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?,
        ),
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let transcript = match detail.transcript_blob.as_deref() {
        Some(blob) => worker_transcript::deserialize_transcript(blob).ok(),
        None => {
            let worker_registration_id = if let ProcessId::Worker(worker_id) = &process_id {
                let registries = state.process_control_registries.load();
                let registry = registries
                    .get(&query.agent_id)
                    .ok_or(StatusCode::NOT_FOUND)?;
                registry
                    .worker_snapshot(*worker_id)
                    .await
                    .map(|worker| worker.registration_id)
            } else {
                None
            };
            state
                .get_live_transcript(&process_id, worker_registration_id)
                .await
        }
    };

    Ok(Json(process_response(detail.run, transcript)))
}

fn process_response(
    run: crate::conversation::ProcessRunRow,
    transcript: Option<Vec<worker_transcript::TranscriptStep>>,
) -> ProcessResponse {
    ProcessResponse {
        kind: run.kind,
        id: run.id,
        input: run.input,
        output: run.output,
        status: run.status,
        process_type: run.process_type,
        profile: run.profile,
        channel_id: run.channel_id,
        channel_name: run.channel_name,
        started_at: run.started_at,
        completed_at: run.completed_at,
        has_transcript: run.has_transcript,
        transcript,
        tool_calls: run.tool_calls,
        model: run.model,
        max_turns: run.max_turns,
        opencode_session_id: run.opencode_session_id,
        opencode_port: run.opencode_port,
        directory: run.directory,
        interactive: run.interactive,
        project_id: run.project_id,
    }
}
