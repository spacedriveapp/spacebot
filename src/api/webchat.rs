use super::state::ApiState;
use crate::{InboundMessage, MessageContent};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize)]
pub(super) struct WebChatSendRequest {
    agent_id: String,
    session_id: String,
    #[serde(default = "default_sender_name")]
    sender_name: String,
    message: String,
}

fn default_sender_name() -> String {
    "user".into()
}

#[derive(Serialize)]
pub(super) struct WebChatSendResponse {
    ok: bool,
}

/// Fire-and-forget message injection. The response arrives via the global SSE
/// event bus (`/api/events`), same as every other channel.
pub(super) async fn webchat_send(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<WebChatSendRequest>,
) -> Result<Json<WebChatSendResponse>, StatusCode> {
    let manager = state
        .messaging_manager
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let conversation_id = request.session_id.clone();

    let mut metadata = HashMap::new();
    metadata.insert(
        "display_name".into(),
        serde_json::Value::String(request.sender_name.clone()),
    );

    let inbound = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "webchat".into(),
        adapter: Some("webchat".into()),
        conversation_id,
        sender_id: request.sender_name.clone(),
        agent_id: Some(request.agent_id.into()),
        content: MessageContent::Text(request.message),
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author: Some(request.sender_name),
    };

    manager.inject_message(inbound).await.map_err(|error| {
        tracing::warn!(%error, "failed to inject webchat message");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(WebChatSendResponse { ok: true }))
}

#[derive(Deserialize)]
pub(super) struct WebChatHistoryQuery {
    agent_id: String,
    session_id: String,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[derive(Serialize)]
pub(super) struct WebChatHistoryMessage {
    id: String,
    role: String,
    content: String,
}

pub(super) async fn webchat_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WebChatHistoryQuery>,
) -> Result<Json<Vec<WebChatHistoryMessage>>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = crate::conversation::ConversationLogger::new(pool.clone());

    let channel_id: crate::ChannelId = Arc::from(query.session_id.as_str());

    let messages = logger
        .load_recent(&channel_id, query.limit.min(200))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to load webchat history");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let result: Vec<WebChatHistoryMessage> = messages
        .into_iter()
        .map(|m| WebChatHistoryMessage {
            id: m.id,
            role: m.role,
            content: m.content,
        })
        .collect();

    Ok(Json(result))
}
