use super::state::ApiState;
use crate::{
    InboundMessage, MessageContent,
    conversation::{WebChatConversation, WebChatConversationStore, WebChatConversationSummary},
};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WebChatSendResponse {
    ok: bool,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WebChatHistoryQuery {
    agent_id: String,
    session_id: String,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WebChatHistoryMessage {
    id: String,
    role: String,
    content: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct WebChatConversationsQuery {
    agent_id: String,
    #[serde(default)]
    include_archived: bool,
    #[serde(default = "default_conversation_limit")]
    limit: i64,
}

fn default_conversation_limit() -> i64 {
    100
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WebChatConversationsResponse {
    conversations: Vec<WebChatConversationSummary>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateWebChatConversationRequest {
    agent_id: String,
    title: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WebChatConversationResponse {
    conversation: WebChatConversation,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateWebChatConversationRequest {
    agent_id: String,
    title: Option<String>,
    archived: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct DeleteWebChatConversationQuery {
    agent_id: String,
}

fn conversation_store(
    state: &Arc<ApiState>,
    agent_id: &str,
) -> Result<WebChatConversationStore, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(agent_id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(WebChatConversationStore::new(pool.clone()))
}

/// Fire-and-forget message injection. The response arrives via the global SSE
/// event bus (`/api/events`), same as every other channel.
#[utoipa::path(
    post,
    path = "/webchat/send",
    request_body = WebChatSendRequest,
    responses(
        (status = 200, body = WebChatSendResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Agent not found"),
        (status = 503, description = "Messaging manager not available"),
    ),
    tag = "webchat",
)]
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

    let store = conversation_store(&state, &request.agent_id)?;
    store
        .ensure(&request.agent_id, &request.session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, session_id = %request.session_id, "failed to ensure webchat conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    store
        .maybe_set_generated_title(&request.agent_id, &request.session_id, &request.message)
        .await
        .map_err(|error| {
            tracing::warn!(%error, session_id = %request.session_id, "failed to update generated webchat title");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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

#[utoipa::path(
    get,
    path = "/webchat/history",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("session_id" = String, Query, description = "Session ID"),
        ("limit" = i64, Query, description = "Maximum number of messages to return (default: 100, max: 200)"),
    ),
    responses(
        (status = 200, body = Vec<WebChatHistoryMessage>),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "webchat",
)]
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
        .map(|message| WebChatHistoryMessage {
            id: message.id,
            role: message.role,
            content: message.content,
        })
        .collect();

    Ok(Json(result))
}

#[utoipa::path(
    get,
    path = "/webchat/conversations",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("include_archived" = bool, Query, description = "Include archived conversations"),
        ("limit" = i64, Query, description = "Maximum number of conversations to return (default: 100, max: 500)"),
    ),
    responses(
        (status = 200, body = WebChatConversationsResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "webchat",
)]
pub(super) async fn list_webchat_conversations(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WebChatConversationsQuery>,
) -> Result<Json<WebChatConversationsResponse>, StatusCode> {
    let store = conversation_store(&state, &query.agent_id)?;
    let conversations = store
        .list(&query.agent_id, query.include_archived, query.limit)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list webchat conversations");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(WebChatConversationsResponse { conversations }))
}

#[utoipa::path(
    post,
    path = "/webchat/conversations",
    request_body = CreateWebChatConversationRequest,
    responses(
        (status = 200, body = WebChatConversationResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "webchat",
)]
pub(super) async fn create_webchat_conversation(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateWebChatConversationRequest>,
) -> Result<Json<WebChatConversationResponse>, StatusCode> {
    let store = conversation_store(&state, &request.agent_id)?;
    let conversation = store
        .create(&request.agent_id, request.title.as_deref())
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, "failed to create webchat conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(WebChatConversationResponse { conversation }))
}

#[utoipa::path(
    put,
    path = "/webchat/conversations/{session_id}",
    request_body = UpdateWebChatConversationRequest,
    params(
        ("session_id" = String, Path, description = "Conversation session ID"),
    ),
    responses(
        (status = 200, body = WebChatConversationResponse),
        (status = 404, description = "Conversation not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "webchat",
)]
pub(super) async fn update_webchat_conversation(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
    Json(request): Json<UpdateWebChatConversationRequest>,
) -> Result<Json<WebChatConversationResponse>, StatusCode> {
    let store = conversation_store(&state, &request.agent_id)?;
    let conversation = store
        .update(
            &request.agent_id,
            &session_id,
            request.title.as_deref(),
            request.archived,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, %session_id, "failed to update webchat conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(WebChatConversationResponse { conversation }))
}

#[utoipa::path(
    delete,
    path = "/webchat/conversations/{session_id}",
    params(
        ("session_id" = String, Path, description = "Conversation session ID"),
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = WebChatSendResponse),
        (status = 404, description = "Conversation not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "webchat",
)]
pub(super) async fn delete_webchat_conversation(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
    Query(query): Query<DeleteWebChatConversationQuery>,
) -> Result<Json<WebChatSendResponse>, StatusCode> {
    let store = conversation_store(&state, &query.agent_id)?;
    let deleted = store
        .delete(&query.agent_id, &session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, %session_id, "failed to delete webchat conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(WebChatSendResponse { ok: true }))
}
