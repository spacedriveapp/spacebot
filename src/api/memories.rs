use super::state::ApiState;

use crate::memory::search::{SearchConfig, SearchMode};
use crate::memory::types::{Association, Memory, MemorySearchResult, MemoryType};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub(super) struct CreateMemoryRequest {
    agent_id: String,
    content: String,
    #[serde(default = "default_memory_type")]
    memory_type: String,
    #[serde(default)]
    importance: Option<f32>,
}

fn default_memory_type() -> String {
    "fact".to_string()
}

#[derive(Deserialize)]
pub(super) struct UpdateMemoryRequest {
    agent_id: String,
    memory_id: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default)]
    importance: Option<f32>,
}

#[derive(Deserialize)]
pub(super) struct DeleteMemoryRequest {
    agent_id: String,
    memory_id: String,
}

#[derive(Serialize)]
pub(super) struct MemoryWriteResponse {
    success: bool,
    memory: Memory,
}

#[derive(Serialize)]
pub(super) struct MemoryDeleteResponse {
    success: bool,
    forgotten: bool,
    message: String,
}

#[derive(Serialize)]
pub(super) struct MemoriesListResponse {
    memories: Vec<Memory>,
    total: usize,
}

#[derive(Serialize)]
pub(super) struct MemoriesSearchResponse {
    results: Vec<MemorySearchResult>,
}

#[derive(Serialize)]
pub(super) struct MemoryGraphResponse {
    nodes: Vec<Memory>,
    edges: Vec<Association>,
    total: usize,
}

#[derive(Serialize)]
pub(super) struct MemoryGraphNeighborsResponse {
    nodes: Vec<Memory>,
    edges: Vec<Association>,
}

#[derive(Deserialize)]
pub(super) struct MemoriesListQuery {
    agent_id: String,
    #[serde(default = "default_memories_limit")]
    limit: i64,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default = "default_memories_sort")]
    sort: String,
}

fn default_memories_limit() -> i64 {
    50
}

pub(super) fn default_memories_sort() -> String {
    "recent".into()
}

pub(super) fn parse_sort(sort: &str) -> crate::memory::search::SearchSort {
    match sort {
        "importance" => crate::memory::search::SearchSort::Importance,
        "most_accessed" => crate::memory::search::SearchSort::MostAccessed,
        _ => crate::memory::search::SearchSort::Recent,
    }
}

pub(super) fn parse_memory_type(type_str: &str) -> Option<MemoryType> {
    match type_str {
        "fact" => Some(MemoryType::Fact),
        "preference" => Some(MemoryType::Preference),
        "decision" => Some(MemoryType::Decision),
        "identity" => Some(MemoryType::Identity),
        "event" => Some(MemoryType::Event),
        "observation" => Some(MemoryType::Observation),
        "goal" => Some(MemoryType::Goal),
        "todo" => Some(MemoryType::Todo),
        _ => None,
    }
}

#[derive(Deserialize)]
pub(super) struct MemoriesSearchQuery {
    agent_id: String,
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default)]
    memory_type: Option<String>,
}

fn default_search_limit() -> usize {
    20
}

#[derive(Deserialize)]
pub(super) struct MemoryGraphQuery {
    agent_id: String,
    #[serde(default = "default_graph_limit")]
    limit: i64,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default = "default_memories_sort")]
    sort: String,
}

fn default_graph_limit() -> i64 {
    200
}

#[derive(Deserialize)]
pub(super) struct MemoryGraphNeighborsQuery {
    agent_id: String,
    memory_id: String,
    #[serde(default = "default_neighbor_depth")]
    depth: u32,
    /// Comma-separated list of memory IDs the client already has.
    #[serde(default)]
    exclude: Option<String>,
}

fn default_neighbor_depth() -> u32 {
    1
}

fn parse_required_memory_type(type_str: &str) -> Result<MemoryType, StatusCode> {
    parse_memory_type(type_str).ok_or(StatusCode::BAD_REQUEST)
}

/// Create a new memory for an agent.
pub(super) async fn create_memory(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateMemoryRequest>,
) -> Result<Json<MemoryWriteResponse>, StatusCode> {
    let content = request.content.trim().to_string();
    if content.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let memory_type = parse_required_memory_type(&request.memory_type)?;

    let searches = state.memory_searches.load();
    let memory_search = searches.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let mut memory = Memory::new(content.clone(), memory_type);
    if let Some(importance) = request.importance {
        memory.importance = importance.clamp(0.0, 1.0);
    }

    store.save(&memory).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, "failed to create memory");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let embedding = memory_search
        .embedding_model_arc()
        .embed_one(&memory.content)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, memory_id = %memory.id, "failed to embed memory content");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    memory_search
        .embedding_table()
        .store(&memory.id, &memory.content, &embedding)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, memory_id = %memory.id, "failed to store memory embedding");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if let Err(error) = memory_search.embedding_table().ensure_fts_index().await {
        tracing::warn!(%error, "failed to ensure FTS index after memory create");
    }

    Ok(Json(MemoryWriteResponse {
        success: true,
        memory,
    }))
}

/// Update an existing memory for an agent.
pub(super) async fn update_memory(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UpdateMemoryRequest>,
) -> Result<Json<MemoryWriteResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let mut memory = store.load(&request.memory_id).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, memory_id = %request.memory_id, "failed to load memory for update");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let mut content_updated = false;

    if let Some(content) = request.content {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        if memory.content != trimmed {
            memory.content = trimmed.to_string();
            content_updated = true;
        }
    }

    if let Some(memory_type) = request.memory_type {
        memory.memory_type = parse_required_memory_type(&memory_type)?;
    }

    if let Some(importance) = request.importance {
        memory.importance = importance.clamp(0.0, 1.0);
    }

    memory.updated_at = chrono::Utc::now();

    store.update(&memory).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, memory_id = %request.memory_id, "failed to update memory");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if content_updated {
        if let Err(error) = memory_search.embedding_table().delete(&memory.id).await {
            tracing::warn!(%error, memory_id = %memory.id, "failed to delete previous embedding during memory update");
        }

        let embedding = memory_search
            .embedding_model_arc()
            .embed_one(&memory.content)
            .await
            .map_err(|error| {
                tracing::warn!(%error, agent_id = %request.agent_id, memory_id = %memory.id, "failed to embed updated memory content");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        memory_search
            .embedding_table()
            .store(&memory.id, &memory.content, &embedding)
            .await
            .map_err(|error| {
                tracing::warn!(%error, agent_id = %request.agent_id, memory_id = %memory.id, "failed to store updated memory embedding");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        if let Err(error) = memory_search.embedding_table().ensure_fts_index().await {
            tracing::warn!(%error, "failed to ensure FTS index after memory update");
        }
    }

    Ok(Json(MemoryWriteResponse {
        success: true,
        memory,
    }))
}

/// Soft-delete a memory for an agent (sets forgotten = true).
pub(super) async fn delete_memory(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeleteMemoryRequest>,
) -> Result<Json<MemoryDeleteResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let Some(memory) = store.load(&query.memory_id).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, memory_id = %query.memory_id, "failed to load memory for deletion");
        StatusCode::INTERNAL_SERVER_ERROR
    })? else {
        return Err(StatusCode::NOT_FOUND);
    };

    if memory.forgotten {
        return Ok(Json(MemoryDeleteResponse {
            success: true,
            forgotten: false,
            message: "Memory is already forgotten".to_string(),
        }));
    }

    let forgotten = store.forget(&query.memory_id).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, memory_id = %query.memory_id, "failed to forget memory");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if forgotten
        && let Err(error) = memory_search.embedding_table().delete(&query.memory_id).await
    {
        tracing::warn!(%error, memory_id = %query.memory_id, "failed to delete memory embedding after forget");
    }

    Ok(Json(MemoryDeleteResponse {
        success: forgotten,
        forgotten,
        message: if forgotten {
            "Memory forgotten".to_string()
        } else {
            "Memory was not forgotten".to_string()
        },
    }))
}

/// List memories for an agent with sorting, filtering, and pagination.
pub(super) async fn list_memories(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoriesListQuery>,
) -> Result<Json<MemoriesListResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let limit = query.limit.min(200);
    let sort = parse_sort(&query.sort);
    let memory_type = query.memory_type.as_deref().and_then(parse_memory_type);

    let fetch_limit = limit + query.offset as i64;
    let all = store
        .get_sorted(sort, fetch_limit, memory_type)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list memories");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = all.len();
    let memories = all.into_iter().skip(query.offset).collect();

    Ok(Json(MemoriesListResponse { memories, total }))
}

/// Search memories using hybrid search (vector + FTS + graph).
pub(super) async fn search_memories(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoriesSearchQuery>,
) -> Result<Json<MemoriesSearchResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let config = SearchConfig {
        mode: SearchMode::Hybrid,
        memory_type: query.memory_type.as_deref().and_then(parse_memory_type),
        max_results: query.limit.min(100),
        ..SearchConfig::default()
    };

    let results = memory_search.search(&query.q, &config)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, query = %query.q, "memory search failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoriesSearchResponse { results }))
}

/// Get a subgraph of memories: nodes + all edges between them.
pub(super) async fn memory_graph(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoryGraphQuery>,
) -> Result<Json<MemoryGraphResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let limit = query.limit.min(500);
    let sort = parse_sort(&query.sort);
    let memory_type = query.memory_type.as_deref().and_then(parse_memory_type);

    let fetch_limit = limit + query.offset as i64;
    let all = store
        .get_sorted(sort, fetch_limit, memory_type)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load graph nodes");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = all.len();
    let nodes: Vec<Memory> = all.into_iter().skip(query.offset).collect();
    let node_ids: Vec<String> = nodes.iter().map(|m| m.id.clone()).collect();

    let edges = store
        .get_associations_between(&node_ids)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load graph edges");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoryGraphResponse {
        nodes,
        edges,
        total,
    }))
}

/// Get the neighbors of a specific memory node. Returns new nodes
/// and edges not already present in the client's graph.
pub(super) async fn memory_graph_neighbors(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoryGraphNeighborsQuery>,
) -> Result<Json<MemoryGraphNeighborsResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let depth = query.depth.min(3);
    let exclude_ids: Vec<String> = query
        .exclude
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let (nodes, edges) = store.get_neighbors(&query.memory_id, depth, &exclude_ids)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, memory_id = %query.memory_id, "failed to load neighbors");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoryGraphNeighborsResponse { nodes, edges }))
}
