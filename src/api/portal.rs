//! Portal API endpoints for conversation management.

use super::state::ApiState;
use crate::{
    Attachment, InboundMessage, MessageContent,
    conversation::{
        ConversationDefaultsResponse, ConversationSettings, DelegationMode, MemoryMode,
        ModelOption, PortalConversation, PortalConversationStore, PortalConversationSummary,
        WorkerContextMode,
    },
};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct PortalSendRequest {
    agent_id: String,
    session_id: String,
    #[serde(default = "default_sender_name")]
    sender_name: String,
    message: String,
    /// IDs of pre-uploaded attachments to include with this message.
    #[serde(default)]
    attachment_ids: Vec<String>,
}

fn default_sender_name() -> String {
    "user".into()
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PortalSendResponse {
    ok: bool,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct PortalHistoryQuery {
    agent_id: String,
    session_id: String,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PortalHistoryMessage {
    id: String,
    role: String,
    content: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct PortalConversationsQuery {
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
pub(super) struct PortalConversationsResponse {
    conversations: Vec<PortalConversationSummary>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreatePortalConversationRequest {
    agent_id: String,
    title: Option<String>,
    settings: Option<ConversationSettings>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PortalConversationResponse {
    conversation: PortalConversation,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdatePortalConversationRequest {
    agent_id: String,
    title: Option<String>,
    archived: Option<bool>,
    settings: Option<ConversationSettings>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct DeletePortalConversationQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct ConversationDefaultsQuery {
    agent_id: String,
}

fn conversation_store(
    state: &Arc<ApiState>,
    agent_id: &str,
) -> Result<PortalConversationStore, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(agent_id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(PortalConversationStore::new(pool.clone()))
}

/// Fire-and-forget message injection. The response arrives via the global SSE
/// event bus (`/api/events`), same as every other channel.
#[utoipa::path(
    post,
    path = "/portal/send",
    request_body = PortalSendRequest,
    responses(
        (status = 200, body = PortalSendResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Agent not found"),
        (status = 503, description = "Messaging manager not available"),
    ),
    tag = "portal",
)]
pub(super) async fn portal_send(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<PortalSendRequest>,
) -> Result<Json<PortalSendResponse>, StatusCode> {
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
            tracing::warn!(%error, session_id = %request.session_id, "failed to ensure portal conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    store
        .maybe_set_generated_title(&request.agent_id, &request.session_id, &request.message)
        .await
        .map_err(|error| {
            tracing::warn!(%error, session_id = %request.session_id, "failed to update generated portal title");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let conversation_id = request.session_id.clone();

    let mut metadata = HashMap::new();
    metadata.insert(
        "display_name".into(),
        serde_json::Value::String(request.sender_name.clone()),
    );

    // Resolve pre-uploaded attachments from saved_attachments table.
    let attachments: Vec<Attachment> = if request.attachment_ids.is_empty() {
        Vec::new()
    } else {
        use sqlx::Row as _;
        let pools = state.agent_pools.load();
        let pool = pools.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

        let mut resolved = Vec::with_capacity(request.attachment_ids.len());
        let mut attachment_metas: Vec<crate::agent::channel_attachments::SavedAttachmentMeta> =
            Vec::with_capacity(request.attachment_ids.len());
        for attachment_id in &request.attachment_ids {
            // Filter by channel_id to prevent cross-conversation attachment
            // references — a user in conversation A should not be able to
            // reference an attachment uploaded in conversation B.
            let row = sqlx::query(
                "SELECT id, original_filename, saved_filename, mime_type, size_bytes \
                 FROM saved_attachments WHERE id = ? AND channel_id = ?",
            )
            .bind(attachment_id)
            .bind(&conversation_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to look up attachment");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

            if let Some(row) = row {
                let id: String = row.try_get("id").map_err(|error| {
                    tracing::error!(%error, "saved_attachments row missing id");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
                let filename: String = row.try_get("original_filename").map_err(|error| {
                    tracing::error!(%error, "saved_attachments row missing original_filename");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
                let saved_filename: String = row.try_get("saved_filename").map_err(|error| {
                    tracing::error!(%error, "saved_attachments row missing saved_filename");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
                let mime_type: String = row.try_get("mime_type").map_err(|error| {
                    tracing::error!(%error, "saved_attachments row missing mime_type");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
                let size_bytes = row
                    .try_get::<i64, _>("size_bytes")
                    .ok()
                    .and_then(|n| u64::try_from(n).ok())
                    .unwrap_or(0);
                attachment_metas.push(crate::agent::channel_attachments::SavedAttachmentMeta {
                    id: id.clone(),
                    filename: filename.clone(),
                    saved_filename,
                    mime_type: mime_type.clone(),
                    size_bytes,
                });
                resolved.push(Attachment {
                    filename,
                    mime_type,
                    url: String::new(),
                    size_bytes: Some(size_bytes),
                    auth_header: None,
                    pre_saved_id: Some(id),
                });
            }
        }
        if !attachment_metas.is_empty() {
            metadata.insert(
                "portal_attachment_metas".into(),
                serde_json::to_value(&attachment_metas).unwrap_or_default(),
            );
        }
        resolved
    };

    let content = if attachments.is_empty() {
        MessageContent::Text(request.message)
    } else {
        MessageContent::Media {
            text: Some(request.message),
            attachments,
        }
    };

    let inbound = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "portal".into(),
        adapter: Some("portal".into()),
        conversation_id,
        sender_id: request.sender_name.clone(),
        agent_id: Some(request.agent_id.into()),
        content,
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author: Some(request.sender_name),
    };

    manager.inject_message(inbound).await.map_err(|error| {
        tracing::warn!(%error, "failed to inject portal message");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(PortalSendResponse { ok: true }))
}

#[utoipa::path(
    get,
    path = "/portal/history",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("session_id" = String, Query, description = "Session ID"),
        ("limit" = i64, Query, description = "Maximum number of messages to return (default: 100, max: 200)"),
    ),
    responses(
        (status = 200, body = Vec<PortalHistoryMessage>),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "portal",
)]
pub(super) async fn portal_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PortalHistoryQuery>,
) -> Result<Json<Vec<PortalHistoryMessage>>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = crate::conversation::ConversationLogger::new(pool.clone());

    let channel_id: crate::ChannelId = Arc::from(query.session_id.as_str());

    let messages = logger
        .load_recent(&channel_id, query.limit.min(200))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to load portal history");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let result: Vec<PortalHistoryMessage> = messages
        .into_iter()
        .map(|message| PortalHistoryMessage {
            id: message.id,
            role: message.role,
            content: message.content,
        })
        .collect();

    Ok(Json(result))
}

#[utoipa::path(
    get,
    path = "/portal/conversations",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("include_archived" = bool, Query, description = "Include archived conversations"),
        ("limit" = i64, Query, description = "Maximum number of conversations to return (default: 100, max: 500)"),
    ),
    responses(
        (status = 200, body = PortalConversationsResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "portal",
)]
pub(super) async fn list_portal_conversations(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PortalConversationsQuery>,
) -> Result<Json<PortalConversationsResponse>, StatusCode> {
    let store = conversation_store(&state, &query.agent_id)?;
    let conversations = store
        .list(&query.agent_id, query.include_archived, query.limit)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list portal conversations");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(PortalConversationsResponse { conversations }))
}

#[utoipa::path(
    post,
    path = "/portal/conversations",
    request_body = CreatePortalConversationRequest,
    responses(
        (status = 200, body = PortalConversationResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "portal",
)]
pub(super) async fn create_portal_conversation(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreatePortalConversationRequest>,
) -> Result<Json<PortalConversationResponse>, StatusCode> {
    let store = conversation_store(&state, &request.agent_id)?;
    let conversation = store
        .create(&request.agent_id, request.title.as_deref(), request.settings)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, "failed to create portal conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(PortalConversationResponse { conversation }))
}

#[utoipa::path(
    put,
    path = "/portal/conversations/{session_id}",
    request_body = UpdatePortalConversationRequest,
    params(
        ("session_id" = String, Path, description = "Conversation session ID"),
    ),
    responses(
        (status = 200, body = PortalConversationResponse),
        (status = 404, description = "Conversation not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "portal",
)]
pub(super) async fn update_portal_conversation(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
    Json(request): Json<UpdatePortalConversationRequest>,
) -> Result<Json<PortalConversationResponse>, StatusCode> {
    let store = conversation_store(&state, &request.agent_id)?;
    let has_settings_update = request.settings.is_some();
    let conversation = store
        .update(
            &request.agent_id,
            &session_id,
            request.title.as_deref(),
            request.archived,
            request.settings,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, %session_id, "failed to update portal conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Notify the running channel to hot-reload its settings.
    if has_settings_update {
        let channel_states = state.channel_states.read().await;
        if let Some(channel_state) = channel_states.get(&session_id) {
            let _ = channel_state
                .deps
                .event_tx
                .send(crate::ProcessEvent::SettingsUpdated {
                    agent_id: channel_state.deps.agent_id.clone(),
                    channel_id: channel_state.channel_id.clone(),
                });
        }
    }

    Ok(Json(PortalConversationResponse { conversation }))
}

#[utoipa::path(
    delete,
    path = "/portal/conversations/{session_id}",
    params(
        ("session_id" = String, Path, description = "Conversation session ID"),
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = PortalSendResponse),
        (status = 404, description = "Conversation not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "portal",
)]
pub(super) async fn delete_portal_conversation(
    State(state): State<Arc<ApiState>>,
    Path(session_id): Path<String>,
    Query(query): Query<DeletePortalConversationQuery>,
) -> Result<Json<PortalSendResponse>, StatusCode> {
    let store = conversation_store(&state, &query.agent_id)?;
    let deleted = store
        .delete(&query.agent_id, &session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, %session_id, "failed to delete portal conversation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(PortalSendResponse { ok: true }))
}

/// Get conversation defaults for an agent.
/// Returns the resolved default settings and available options for new conversations.
#[utoipa::path(
    get,
    path = "/conversation-defaults",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = ConversationDefaultsResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "portal",
)]
pub(super) async fn conversation_defaults(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ConversationDefaultsQuery>,
) -> Result<Json<ConversationDefaultsResponse>, StatusCode> {
    // Verify agent exists by checking agent_configs
    let agent_configs = state.agent_configs.load();
    let agent_exists = agent_configs.iter().any(|a| a.id == query.agent_id);
    if !agent_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    // Resolve default model from agent's routing config.
    let default_model = {
        let runtime_configs = state.runtime_configs.load();
        runtime_configs
            .get(&query.agent_id)
            .map(|rc| rc.routing.load().channel.clone())
            .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string())
    };

    // Build available models from configured providers via the models catalog.
    let config_path = state.config_path.read().await.clone();
    let configured = super::models::configured_providers(&config_path).await;
    let catalog = super::models::ensure_models_cache().await;

    let available_models: Vec<ModelOption> = catalog
        .into_iter()
        .filter(|m| configured.contains(&m.provider.as_str()) && m.tool_call)
        .map(|m| ModelOption {
            id: m.id,
            name: m.name,
            provider: m.provider,
            context_window: m.context_window.unwrap_or(0) as usize,
            supports_tools: m.tool_call,
            supports_thinking: m.reasoning,
        })
        .collect();

    let response = ConversationDefaultsResponse {
        model: default_model,
        memory: MemoryMode::Full,
        delegation: DelegationMode::Standard,
        worker_context: WorkerContextMode::default(),
        available_models,
        memory_modes: vec!["full".to_string(), "ambient".to_string(), "off".to_string()],
        delegation_modes: vec!["standard".to_string(), "direct".to_string()],
        worker_history_modes: vec![
            "none".to_string(),
            "summary".to_string(),
            "recent".to_string(),
            "full".to_string(),
        ],
        worker_memory_modes: vec![
            "none".to_string(),
            "ambient".to_string(),
            "tools".to_string(),
            "full".to_string(),
        ],
    };

    Ok(Json(response))
}
