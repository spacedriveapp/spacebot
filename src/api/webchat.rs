use super::state::ApiState;
use crate::{InboundMessage, MessageContent};

use axum::Json;
use axum::extract::{Multipart, Query, State};
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

/// Accept audio via multipart form, transcribe using the voice model, and
/// inject as a text message into the portal chat channel.
///
/// Form fields:
///   - `agent_id` (required): target agent
///   - `session_id` (required): portal chat session (e.g. `portal:chat:main`)
///   - `sender_name` (optional, default "user")
///   - `audio` (required): audio file (any supported format: webm, ogg, wav, mp3, etc.)
pub(super) async fn webchat_send_audio(
    State(state): State<Arc<ApiState>>,
    mut multipart: Multipart,
) -> Result<Json<WebChatSendResponse>, StatusCode> {
    let mut agent_id: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut sender_name = "user".to_string();
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut audio_mime: Option<String> = None;
    let mut audio_filename: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "agent_id" => {
                agent_id = Some(field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?);
            }
            "session_id" => {
                session_id = Some(field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?);
            }
            "sender_name" => {
                sender_name = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            }
            "audio" => {
                audio_mime = field.content_type().map(|s| s.to_string());
                audio_filename = field.file_name().map(|s| s.to_string());
                audio_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| StatusCode::BAD_REQUEST)?
                        .to_vec(),
                );
            }
            _ => {
                // Skip unknown fields
                let _ = field.bytes().await;
            }
        }
    }

    let agent_id = agent_id.ok_or(StatusCode::BAD_REQUEST)?;
    let session_id = session_id.ok_or(StatusCode::BAD_REQUEST)?;
    let audio_bytes = audio_bytes.ok_or(StatusCode::BAD_REQUEST)?;
    let mime_type = audio_mime.unwrap_or_else(|| "audio/webm".into());
    let filename = audio_filename.unwrap_or_else(|| "voice.webm".into());

    if audio_bytes.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Resolve the agent's LLM manager and runtime config for transcription
    let llm_manager = state
        .llm_manager
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let runtime_configs = state.runtime_configs.load();
    let runtime_config = runtime_configs
        .get(&agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    // Transcribe the audio using the configured voice model
    let transcript = crate::agent::channel_attachments::transcribe_audio_bytes(
        &llm_manager,
        &runtime_config,
        &audio_bytes,
        &filename,
        &mime_type,
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "audio transcription failed");
        StatusCode::UNPROCESSABLE_ENTITY
    })?;

    // Inject as a text message with the transcript wrapped in voice tags
    let manager = state
        .messaging_manager
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let conversation_id = session_id.clone();
    let mut metadata = HashMap::new();
    metadata.insert(
        "display_name".into(),
        serde_json::Value::String(sender_name.clone()),
    );

    let message_text = format!(
        "<voice_transcript name=\"{filename}\" mime=\"{mime_type}\">\n{transcript}\n</voice_transcript>"
    );

    let inbound = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "webchat".into(),
        adapter: Some("webchat".into()),
        conversation_id,
        sender_id: sender_name.clone(),
        agent_id: Some(agent_id.into()),
        content: MessageContent::Text(message_text),
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author: Some(sender_name),
    };

    manager.inject_message(inbound).await.map_err(|error| {
        tracing::warn!(%error, "failed to inject transcribed audio message");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(WebChatSendResponse { ok: true }))
}
