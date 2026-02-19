use super::state::ApiState;

use crate::memory::search::{SearchConfig, SearchMode};
use crate::memory::types::{Association, Memory, MemorySearchResult, MemoryType};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    let all = store.get_sorted(sort, fetch_limit, memory_type)
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
    let all = store.get_sorted(sort, fetch_limit, memory_type)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load graph nodes");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = all.len();
    let nodes: Vec<Memory> = all.into_iter().skip(query.offset).collect();
    let node_ids: Vec<String> = nodes.iter().map(|m| m.id.clone()).collect();

    let edges = store.get_associations_between(&node_ids)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load graph edges");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoryGraphResponse { nodes, edges, total }))
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
    let exclude_ids: Vec<String> = query.exclude
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
