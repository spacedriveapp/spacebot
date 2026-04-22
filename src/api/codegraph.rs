//! REST API handlers for the Code Graph system.
//!
//! Provides endpoints for listing projects, viewing graph details,
//! triggering re-indexing, and searching the code graph.

use super::state::ApiState;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use std::collections::HashMap;

use crate::codegraph::{
    CodeGraphManager, CommunityInfo, ProcessInfo,
    RegisteredProject, GraphSearchResult, IndexLogEntry,
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
}

#[derive(Deserialize, utoipa::IntoParams)]
pub(super) struct NodeListQuery {
    /// Filter by node label (e.g. "Function", "Class").
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_node_limit")]
    limit: usize,
    /// Optional output schema vocabulary (`default` or `gitnexus`).
    /// When `gitnexus`, label names on response items are translated
    /// to GitNexus conventions (e.g. `MacroDef`→`Macro`).
    #[serde(default)]
    schema: super::schema_alias::ApiSchema,
}

fn default_node_limit() -> usize {
    50
}

#[derive(Deserialize, utoipa::IntoParams)]
pub(super) struct NodeDetailQuery {
    /// Label hint for efficient lookup (avoids scanning all tables).
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
pub(super) struct EdgeListQuery {
    /// Direction: "outgoing", "incoming", or "both" (default).
    #[serde(default = "default_direction")]
    direction: String,
    /// Filter by edge type (e.g. "CALLS", "IMPORTS").
    #[serde(default)]
    edge_type: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_node_limit")]
    limit: usize,
    /// Optional output schema vocabulary (`default` or `gitnexus`).
    /// Translates label + edge-type names on response items.
    #[serde(default)]
    schema: super::schema_alias::ApiSchema,
}

fn default_direction() -> String {
    "both".to_string()
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
pub(super) struct RemoveInfoResponse {
    node_count: u64,
    edge_count: u64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ActionResponse {
    success: bool,
    message: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct NodeListResponse {
    nodes: Vec<NodeSummary>,
    total: usize,
    offset: usize,
    limit: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct NodeSummary {
    id: i64,
    qualified_name: String,
    name: String,
    label: String,
    source_file: Option<String>,
    line_start: Option<u32>,
    line_end: Option<u32>,
    /// File size in bytes (only set for File nodes in the bulk endpoint).
    #[serde(skip_serializing_if = "Option::is_none")]
    file_size: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct NodeDetailResponse {
    node: NodeFull,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct NodeFull {
    id: i64,
    qualified_name: String,
    name: String,
    label: String,
    source_file: Option<String>,
    line_start: Option<u32>,
    line_end: Option<u32>,
    source: Option<String>,
    written_by: Option<String>,
    properties: HashMap<String, serde_json::Value>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct EdgeListResponse {
    edges: Vec<EdgeSummary>,
    total: usize,
    offset: usize,
    limit: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct EdgeSummary {
    from_id: i64,
    from_name: String,
    from_label: String,
    to_id: i64,
    to_name: String,
    to_label: String,
    edge_type: String,
    confidence: f64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct GraphStatsResponse {
    total_nodes: u64,
    total_edges: u64,
    nodes_by_label: Vec<LabelCount>,
    edges_by_type: Vec<TypeCount>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BulkNodesResponse {
    nodes: Vec<NodeSummary>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BulkEdgesResponse {
    edges: Vec<BulkEdgeSummary>,
}

/// Edge shape for the bulk endpoint. Uses `qualified_name` for source/target
/// instead of `id(n)` (which LadybugDB returns as 0 for all nodes).
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BulkEdgeSummary {
    from_qname: String,
    from_label: String,
    to_qname: String,
    to_label: String,
    edge_type: String,
    confidence: f64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct LabelCount {
    label: String,
    count: u64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TypeCount {
    edge_type: String,
    count: u64,
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
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let communities = crate::codegraph::graph_queries::query_communities(&db, &project_id)
        .await
        .map_err(|e| {
            tracing::error!(%e, "failed to query communities");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = communities.len();
    Ok(Json(CommunitiesResponse { communities, total }))
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
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let processes = crate::codegraph::graph_queries::query_processes(&db, &project_id)
        .await
        .map_err(|e| {
            tracing::error!(%e, "failed to query processes");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = processes.len();
    Ok(Json(ProcessesResponse { processes, total }))
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
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let results = crate::codegraph::search::hybrid_search(
        &project_id,
        &query.q,
        query.limit,
        &db,
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "hybrid search failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

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
    let manager = get_manager(&state)?;

    let entries = manager
        .get_index_log(&project_id)
        .await
        .map_err(|e| {
            tracing::error!(%e, "failed to get index log");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(IndexLogResponse { entries }))
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
    }))
}

/// GET /codegraph/projects/:project_id/graph/nodes — List/browse nodes.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/nodes",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        NodeListQuery,
    ),
    responses(
        (status = 200, description = "Node list", body = NodeListResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn list_nodes(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Query(query): Query<NodeListQuery>,
) -> Result<Json<NodeListResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let (nodes, total) = crate::codegraph::graph_queries::query_nodes(
        &db,
        &project_id,
        query.label.as_deref(),
        query.offset,
        query.limit,
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "failed to query nodes");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let schema = query.schema;
    let nodes = nodes
        .into_iter()
        .map(|n| NodeSummary {
            id: n.id,
            qualified_name: n.qualified_name,
            name: n.name,
            label: super::schema_alias::translate_label(&n.label, schema).to_string(),
            source_file: n.source_file,
            line_start: n.line_start,
            line_end: n.line_end,
            file_size: None,
        })
        .collect();

    Ok(Json(NodeListResponse {
        nodes,
        total,
        offset: query.offset,
        limit: query.limit,
    }))
}

/// GET /codegraph/projects/:project_id/graph/nodes/:node_id — Node detail.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/nodes/{node_id}",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("node_id" = i64, Path, description = "Node ID"),
        NodeDetailQuery,
    ),
    responses(
        (status = 200, description = "Node detail", body = NodeDetailResponse),
        (status = 404, description = "Node not found"),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_node(
    State(state): State<Arc<ApiState>>,
    Path((project_id, node_id)): Path<(String, i64)>,
    Query(query): Query<NodeDetailQuery>,
) -> Result<Json<NodeDetailResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let node = crate::codegraph::graph_queries::query_node_by_id(
        &db,
        &project_id,
        node_id,
        query.label.as_deref(),
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "failed to query node");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(NodeDetailResponse {
        node: NodeFull {
            id: node.id,
            qualified_name: node.qualified_name,
            name: node.name,
            label: node.label,
            source_file: node.source_file,
            line_start: node.line_start,
            line_end: node.line_end,
            source: node.source,
            written_by: node.written_by,
            properties: node.properties,
        },
    }))
}

/// GET /codegraph/projects/:project_id/graph/nodes/:node_id/edges — Node edges.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/nodes/{node_id}/edges",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("node_id" = i64, Path, description = "Node ID"),
        EdgeListQuery,
    ),
    responses(
        (status = 200, description = "Edge list", body = EdgeListResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_node_edges(
    State(state): State<Arc<ApiState>>,
    Path((project_id, node_id)): Path<(String, i64)>,
    Query(query): Query<EdgeListQuery>,
) -> Result<Json<EdgeListResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // First resolve the node's label so edge queries know which table to use.
    let node = crate::codegraph::graph_queries::query_node_by_id(
        &db,
        &project_id,
        node_id,
        None,
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "failed to resolve node for edge query");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let (edges, total) = crate::codegraph::graph_queries::query_node_edges(
        &db,
        &project_id,
        node_id,
        &node.label,
        &query.direction,
        query.edge_type.as_deref(),
        query.offset,
        query.limit,
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "failed to query node edges");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let schema = query.schema;
    let edges = edges
        .into_iter()
        .map(|e| EdgeSummary {
            from_id: e.from_id,
            from_name: e.from_name,
            from_label: super::schema_alias::translate_label(&e.from_label, schema).to_string(),
            to_id: e.to_id,
            to_name: e.to_name,
            to_label: super::schema_alias::translate_label(&e.to_label, schema).to_string(),
            edge_type: super::schema_alias::translate_edge_type(&e.edge_type, schema).to_string(),
            confidence: e.confidence,
        })
        .collect();

    Ok(Json(EdgeListResponse {
        edges,
        total,
        offset: query.offset,
        limit: query.limit,
    }))
}

/// GET /codegraph/projects/:project_id/graph/stats — Graph statistics.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/stats",
    params(("project_id" = String, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Graph statistics", body = GraphStatsResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_graph_stats(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<GraphStatsResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let stats = crate::codegraph::graph_queries::query_graph_stats(&db, &project_id)
        .await
        .map_err(|e| {
            tracing::error!(%e, "failed to query graph stats");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(GraphStatsResponse {
        total_nodes: stats.total_nodes,
        total_edges: stats.total_edges,
        nodes_by_label: stats
            .nodes_by_label
            .into_iter()
            .map(|(label, count)| LabelCount { label, count })
            .collect(),
        edges_by_type: stats
            .edges_by_type
            .into_iter()
            .map(|(edge_type, count)| TypeCount { edge_type, count })
            .collect(),
    }))
}

/// GET /codegraph/projects/:project_id/graph/bulk-nodes — all nodes.
///
/// Returns every node in the project for the interactive graph canvas.
/// Parameter nodes are deleted before finalization (pipeline-only);
/// everything else — including Variable, Import, and Decorator — is
/// persisted. Hard-capped at 15k nodes with label-priority truncation.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/bulk-nodes",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Bulk node list", body = BulkNodesResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_bulk_nodes(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<BulkNodesResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|e| {
            tracing::error!(project_id = %project_id, err = %e, "bulk-nodes: failed to open DB");
            StatusCode::NOT_FOUND
        })?;

    let queried = crate::codegraph::graph_queries::query_bulk_nodes(
        &db,
        &project_id,
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "failed to query bulk nodes");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Resolve the project root so we can stat File nodes for their sizes.
    let project_root = manager
        .get_project(&project_id)
        .await
        .map(|p| p.root_path.clone());

    let nodes = queried
        .into_iter()
        .map(|n| {
            // For File nodes, stat the file to get its size in bytes.
            let file_size = if n.label == "File" {
                n.source_file.as_ref().and_then(|sf| {
                    project_root.as_ref().and_then(|root| {
                        std::fs::metadata(root.join(sf)).ok().map(|m| m.len())
                    })
                })
            } else {
                None
            };
            NodeSummary {
            id: n.id,
            qualified_name: n.qualified_name,
            name: n.name,
            label: n.label,
            source_file: n.source_file,
            line_start: n.line_start,
            line_end: n.line_end,
            file_size,
        }})
        .collect();

    Ok(Json(BulkNodesResponse { nodes }))
}

/// GET /codegraph/projects/:project_id/graph/bulk-edges — all edges.
///
/// Returns every edge whose endpoints are in the bulk node set.
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/bulk-edges",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Bulk edge list", body = BulkEdgesResponse),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_bulk_edges(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Json<BulkEdgesResponse>, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|e| {
            tracing::error!(project_id = %project_id, err = %e, "bulk-edges: failed to open DB");
            StatusCode::NOT_FOUND
        })?;

    let queried_nodes = crate::codegraph::graph_queries::query_bulk_nodes(
        &db,
        &project_id,
    )
    .await
    .map_err(|e| {
        tracing::error!(%e, "failed to query bulk node id set for edges");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let node_qnames: std::collections::HashSet<String> =
        queried_nodes.iter().map(|n| n.qualified_name.clone()).collect();

    let queried = crate::codegraph::graph_queries::query_bulk_edges(&db, &project_id, &node_qnames)
        .await
        .map_err(|e| {
            tracing::error!(%e, "failed to query bulk edges");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let edges = queried
        .into_iter()
        .map(|e| BulkEdgeSummary {
            from_qname: e.from_name,  // query_bulk_edges returns qualified_name in the name field
            from_label: e.from_label,
            to_qname: e.to_name,
            to_label: e.to_label,
            edge_type: e.edge_type,
            confidence: e.confidence,
        })
        .collect();

    Ok(Json(BulkEdgesResponse { edges }))
}

/// GET /codegraph/projects/:project_id/graph/stream — NDJSON graph.
///
/// Streams every node, then every edge, as newline-delimited JSON:
///
/// ```text
/// {"type":"node","data":{...}}
/// {"type":"edge","data":{...}}
/// {"type":"error","error":"..."}   // terminal, only on mid-stream failure
/// ```
///
/// Uses the native Kuzu cursor end-to-end (`CodeGraphDb::query_stream`) so
/// no result set is ever materialized — this is what lets huge graphs
/// load without crashing LadybugDB's native layer. Ports the pattern
/// from GitNexus's `/api/graph?stream=true` handler (`server/api.ts:707`).
#[utoipa::path(
    get,
    path = "/codegraph/projects/{project_id}/graph/stream",
    params(
        ("project_id" = String, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "NDJSON stream of graph nodes and edges", content_type = "application/x-ndjson"),
    ),
    tag = "codegraph"
)]
pub(super) async fn get_graph_stream(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
) -> Result<Response, StatusCode> {
    let manager = get_manager(&state)?;
    let db = manager
        .get_or_open_db(&project_id)
        .await
        .map_err(|e| {
            tracing::error!(project_id = %project_id, err = %e, "graph/stream: failed to open DB");
            StatusCode::NOT_FOUND
        })?;
    let project_root = manager
        .get_project(&project_id)
        .await
        .map(|p| p.root_path.clone());

    let body_stream = async_stream::stream! {
        // Nodes — flatten the cursor stream into NDJSON lines.
        let mut nodes = Box::pin(crate::codegraph::graph_queries::stream_bulk_nodes(
            db.clone(),
            project_id.clone(),
        ));
        while let Some(item) = nodes.next().await {
            match item {
                Ok(node) => {
                    // Stat file size off the async runtime for File nodes;
                    // everything else has `None`.
                    let file_size = if node.label == "File" {
                        stat_file_size(node.source_file.as_deref(), project_root.as_deref()).await
                    } else {
                        None
                    };
                    let record = serde_json::json!({
                        "type": "node",
                        "data": {
                            "id": node.id,
                            "qualified_name": node.qualified_name,
                            "name": node.name,
                            "label": node.label,
                            "source_file": node.source_file,
                            "line_start": node.line_start,
                            "line_end": node.line_end,
                            "file_size": file_size,
                        }
                    });
                    yield Ok::<_, std::io::Error>(ndjson_line(&record));
                }
                Err(e) => {
                    let record = serde_json::json!({ "type": "error", "error": e.to_string() });
                    yield Ok(ndjson_line(&record));
                    return;
                }
            }
        }

        // Edges
        let mut edges = Box::pin(crate::codegraph::graph_queries::stream_bulk_edges(
            db,
            project_id,
        ));
        while let Some(item) = edges.next().await {
            match item {
                Ok(edge) => {
                    let record = serde_json::json!({
                        "type": "edge",
                        "data": {
                            "from_qname": edge.from_name,
                            "from_label": edge.from_label,
                            "to_qname": edge.to_name,
                            "to_label": edge.to_label,
                            "edge_type": edge.edge_type,
                            "confidence": edge.confidence,
                        }
                    });
                    yield Ok(ndjson_line(&record));
                }
                Err(e) => {
                    let record = serde_json::json!({ "type": "error", "error": e.to_string() });
                    yield Ok(ndjson_line(&record));
                    return;
                }
            }
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson; charset=utf-8")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

fn ndjson_line(record: &serde_json::Value) -> axum::body::Bytes {
    let mut s = serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    axum::body::Bytes::from(s)
}

/// Resolve a file's size in bytes, off the async runtime. Returns `None`
/// on any filesystem error or missing parameters.
async fn stat_file_size(
    source_file: Option<&str>,
    project_root: Option<&std::path::Path>,
) -> Option<u64> {
    let sf = source_file?;
    let root = project_root?;
    let full = root.join(sf);
    tokio::fs::metadata(full).await.ok().map(|m| m.len())
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
