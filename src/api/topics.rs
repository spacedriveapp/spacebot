//! Topic API handlers.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub(super) struct TopicListQuery {
    agent_id: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TopicGetQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct CreateTopicRequest {
    agent_id: String,
    title: String,
    #[serde(default)]
    criteria: Option<crate::topics::TopicCriteria>,
    #[serde(default)]
    pin_ids: Vec<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    max_words: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct UpdateTopicRequest {
    agent_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    criteria: Option<crate::topics::TopicCriteria>,
    #[serde(default)]
    pin_ids: Option<Vec<String>>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    max_words: Option<usize>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct DeleteTopicQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct SyncTopicRequest {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct VersionsQuery {
    agent_id: String,
    #[serde(default = "default_version_limit")]
    limit: i64,
}

fn default_version_limit() -> i64 {
    20
}

#[derive(Serialize)]
pub(super) struct TopicListResponse {
    topics: Vec<crate::topics::Topic>,
}

#[derive(Serialize)]
pub(super) struct TopicResponse {
    topic: crate::topics::Topic,
}

#[derive(Serialize)]
pub(super) struct TopicActionResponse {
    success: bool,
    message: String,
}

#[derive(Serialize)]
pub(super) struct TopicVersionsResponse {
    versions: Vec<crate::topics::TopicVersion>,
}

pub(super) async fn list_topics(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TopicListQuery>,
) -> Result<Json<TopicListResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let topics = if query.status.as_deref() == Some("active") {
        store.list_active(&query.agent_id).await.map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list active topics");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    } else {
        store.list(&query.agent_id).await.map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list topics");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
    };

    Ok(Json(TopicListResponse { topics }))
}

pub(super) async fn get_topic(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<TopicGetQuery>,
) -> Result<Json<TopicResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let topic = store
        .get(&id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, topic_id = %id, "failed to get topic");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(TopicResponse { topic }))
}

pub(super) async fn create_topic(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTopicRequest>,
) -> Result<Json<TopicResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let status = match request.status.as_deref() {
        None | Some("active") => crate::topics::TopicStatus::Active,
        Some("paused") => crate::topics::TopicStatus::Paused,
        Some("archived") => crate::topics::TopicStatus::Archived,
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let topic = crate::topics::Topic {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: request.agent_id.clone(),
        title: request.title,
        content: String::new(),
        criteria: request.criteria.unwrap_or_default(),
        pin_ids: request.pin_ids,
        status,
        max_words: request.max_words.unwrap_or(1500),
        last_memory_at: None,
        last_synced_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    store.create(&topic).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, "failed to create topic");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(TopicResponse { topic }))
}

pub(super) async fn update_topic(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UpdateTopicRequest>,
) -> Result<Json<TopicResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let mut topic = store
        .get(&id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, topic_id = %id, "failed to get topic for update");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(title) = request.title {
        topic.title = title;
    }
    if let Some(criteria) = request.criteria {
        topic.criteria = criteria;
    }
    if let Some(pin_ids) = request.pin_ids {
        topic.pin_ids = pin_ids;
    }
    if let Some(status_str) = request.status {
        topic.status =
            crate::topics::TopicStatus::parse(&status_str).ok_or(StatusCode::BAD_REQUEST)?;
    }
    if let Some(max_words) = request.max_words {
        topic.max_words = max_words;
    }
    if let Some(content) = request.content {
        topic.content = content;
    }

    store.update(&topic).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, topic_id = %id, "failed to update topic");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(TopicResponse { topic }))
}

pub(super) async fn delete_topic(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<DeleteTopicQuery>,
) -> Result<Json<TopicActionResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let deleted = store.delete(&id).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, topic_id = %id, "failed to delete topic");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(TopicActionResponse {
        success: true,
        message: format!("Topic {id} deleted"),
    }))
}

pub(super) async fn sync_topic(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SyncTopicRequest>,
) -> Result<Json<TopicResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let topic = store
        .get(&id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to get topic for sync");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Clear last_synced_at to force a re-sync and wake the sync loop immediately.
    let mut updated = topic;
    updated.last_synced_at = None;
    store.update(&updated).await.map_err(|error| {
        tracing::warn!(%error, "failed to clear topic sync timestamp");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Wake the topic sync loop so it picks up the stale topic immediately.
    let notifiers = state.topic_sync_notifiers.load();
    if let Some(notify) = notifiers.get(&request.agent_id) {
        notify.notify_one();
    }

    Ok(Json(TopicResponse { topic: updated }))
}

pub(super) async fn topic_versions(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<VersionsQuery>,
) -> Result<Json<TopicVersionsResponse>, StatusCode> {
    let stores = state.topic_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let versions = store
        .get_versions(&id, query.limit)
        .await
        .map_err(|error| {
            tracing::warn!(%error, topic_id = %id, "failed to get topic versions");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(TopicVersionsResponse { versions }))
}
