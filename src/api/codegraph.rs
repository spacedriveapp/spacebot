//! REST API handlers for the Code Graph system.
//!
//! Provides endpoints for listing projects, viewing graph details,
//! triggering re-indexing, and searching the code graph.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::codegraph::{
    CodeGraphManager, CommunityInfo, ProcessInfo,
    RegisteredProject, GraphSearchResult, IndexLogEntry, ProjectMemory,
};

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::IntoParams)]
pub(super) struct ProjectQuery {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
pub(super) struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateProjectRequest {
    name: String,
    root_path: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ReindexRequest {
    #[serde(default)]
    force: bool,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProjectListResponse {
    projects: Vec<RegisteredProject>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProjectDetailResponse {
    project: RegisteredProject,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct CommunitiesResponse {
    communities: Vec<CommunityInfo>,
    total: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProcessesResponse {
    processes: Vec<ProcessInfo>,
    total: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct SearchResponse {
    results: Vec<GraphSearchResult>,
    total: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct IndexLogResponse {
    entries: Vec<IndexLogEntry>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProjectMemoriesResponse {
    memories: Vec<ProjectMemory>,
    total: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RemoveInfoResponse {
    node_count: u64,
    edge_count: u64,
    memory_count: u64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ActionResponse {
    success: bool,
    message: String,
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn get_manager(state: &ApiState) -> Result<Arc<CodeGraphManager>, StatusCode> {
    let manager = state.codegraph_manager.load();
    match manager.as_ref() {
        Some(m) => Ok(m.clone()),
        None => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /codegraph/projects — List all indexed projects.
#[utoipa::path(
    get,
    path = "/codegraph/projects",
    params(ProjectQuery),
    responses(
        (status = 200, description = "List of projects", body = ProjectListResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn list_projects(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<ProjectListResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let mut projects = manager.list_projects().await;

    // Filter by status if requested.
    if let Some(status_filter) = &query.status {
        projects.retain(|p| p.status.to_string() == *status_filter);
    }

    Ok(Json(ProjectListResponse { projects }))
}

/// GET /codegraph/projects/:project_id — Get project detail.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project detail", body = ProjectDetailResponse),
        (status = 404, description = "Project not found"),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectDetailResponse>, StatusCode> {
    let manager = get_manager(&state)?;

    match manager.get_project(&project_id).await {
        Some(project) => Ok(Json(ProjectDetailResponse { project })),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// POST /codegraph/projects — Create and index a new project.
#[utoipa::path(
    post,
    path = "/codegraph/projects",
    request_body = CreateProjectRequest,
    responses(
        (status = 201, description = "Project created", body = ProjectDetailResponse),
        (status = 400, description = "Invalid request"),
    ),
    tag = "codegraph"
)]
pub(super) async fn create_project(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<ProjectDetailResponse>), StatusCode> {
    let manager = get_manager(&state)?;

    let project_id = slug_from_name(&req.name);
    let root_path = std::path::PathBuf::from(&req.root_path);

    if !root_path.exists() {
        return Err(StatusCode::BAD_REQUEST);
    }

    match manager
        .add_project(project_id, req.name, root_path)
        .await
    {
        Ok(project) => Ok((
            StatusCode::CREATED,
            Json(ProjectDetailResponse { project }),
        )),
        Err(err) => {
            tracing::error!(%err, "failed to create codegraph project");
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

/// DELETE /codegraph/projects/:project_id — Remove project (cascade delete).
#[utoipa::path(
    delete,
    path = "/codegraph/projects/{project_id}",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project removed", body = ActionResponse),
        (status = 404, description = "Project not found"),
    ),
    tag = "codegraph"
)]
pub(super) async fn delete_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let manager = get_manager(&state)?;

    // Verify project exists.
    if manager.get_project(&project_id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    match manager.remove_project(&project_id).await {
        Ok(()) => Ok(Json(ActionResponse {
            success: true,
            message: format!("Project '{}' removed", project_id),
        })),
        Err(err) => {
            tracing::error!(%err, "failed to remove codegraph project");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// POST /codegraph/projects/:project_id/reindex — Trigger re-indexing.
#[utoipa::path(
    post,
    path = "/codegraph/projects/{project_id}/reindex",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Re-indexing started", body = ActionResponse),
        (status = 404, description = "Project not found"),
    ),
    tag = "codegraph"
)]
pub(super) async fn reindex_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let manager = get_manager(&state)?;

    if manager.get_project(&project_id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    match manager.start_indexing(&project_id).await {
        Ok(()) => Ok(Json(ActionResponse {
            success: true,
            message: format!("Re-indexing started for '{}'", project_id),
        })),
        Err(err) => {
            tracing::error!(%err, "failed to start re-indexing");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// GET /codegraph/projects/:project_id/graph/communities — List communities.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/communities",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Community list", body = CommunitiesResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_communities(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<CommunitiesResponse>, StatusCode> {
    let _manager = get_manager(&state)?;

    // Will query the graph database for community nodes.
    // Stub: return empty.
    Ok(Json(CommunitiesResponse {
        communities: Vec::new(),
        total: 0,
    }))
}

/// GET /codegraph/projects/:project_id/graph/processes — List entry points.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/processes",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Process list", body = ProcessesResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_processes(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProcessesResponse>, StatusCode> {
    let _manager = get_manager(&state)?;

    Ok(Json(ProcessesResponse {
        processes: Vec::new(),
        total: 0,
    }))
}

/// GET /codegraph/projects/:project_id/graph/search — Hybrid search.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/search",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        SearchQuery,
    ),
    responses(
        (status = 200, description = "Search results", body = SearchResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn search_graph(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, StatusCode> {
    let manager = get_manager(&state)?;

    let db = manager
        .get_db(&project_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let results = crate::codegraph::search::hybrid_search(
        &project_id,
        &query.q,
        query.limit,
        &db,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total = results.len();
    Ok(Json(SearchResponse { results, total }))
}

/// GET /codegraph/projects/:project_id/graph/index-log — Index history.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/index-log",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Index log", body = IndexLogResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_index_log(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<IndexLogResponse>, StatusCode> {
    let _manager = get_manager(&state)?;

    Ok(Json(IndexLogResponse {
        entries: Vec::new(),
    }))
}

/// GET /codegraph/projects/:project_id/memories — Project memories.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/memories",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project memories", body = ProjectMemoriesResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_project_memories(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectMemoriesResponse>, StatusCode> {
    let _manager = get_manager(&state)?;

    Ok(Json(ProjectMemoriesResponse {
        memories: Vec::new(),
        total: 0,
    }))
}

/// GET /codegraph/projects/:project_id/remove-info — Get cascade delete info.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/remove-info",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Removal info", body = RemoveInfoResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_remove_info(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<RemoveInfoResponse>, StatusCode> {
    let manager = get_manager(&state)?;

    let project = manager
        .get_project(&project_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    // Get stats from last index (node/edge counts).
    let (node_count, edge_count) = match &project.last_index_stats {
        Some(stats) => (stats.nodes_created, stats.edges_created),
        None => (0, 0),
    };

    Ok(Json(RemoveInfoResponse {
        node_count,
        edge_count,
        memory_count: 0, // Will be queried from centralized memory store.
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a URL-safe slug from a project name.
fn slug_from_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
