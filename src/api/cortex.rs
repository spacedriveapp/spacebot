use super::state::ApiState;

use crate::agent::cortex::{CortexEvent, CortexLogger};
use crate::agent::cortex_chat::{CortexChatEvent, CortexChatMessage, CortexChatStore};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Sse;
use axum::Json;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct CortexEventsResponse {
    events: Vec<CortexEvent>,
    total: i64,
}

#[derive(Serialize)]
pub(super) struct CortexChatMessagesResponse {
    messages: Vec<CortexChatMessage>,
    thread_id: String,
}

#[derive(Deserialize)]
pub(super) struct CortexChatMessagesQuery {
    agent_id: String,
    /// If omitted, loads the latest thread.
    thread_id: Option<String>,
    #[serde(default = "default_cortex_chat_limit")]
    limit: i64,
}

fn default_cortex_chat_limit() -> i64 {
    50
}

#[derive(Deserialize)]
pub(super) struct CortexChatSendRequest {
    agent_id: String,
    thread_id: String,
    message: String,
    channel_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CortexEventsQuery {
    agent_id: String,
    #[serde(default = "default_cortex_events_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    event_type: Option<String>,
}

fn default_cortex_events_limit() -> i64 {
    50
}

/// Load persisted cortex chat history for a thread.
/// If no thread_id is provided, loads the latest thread.
/// If no threads exist, returns an empty list with a fresh thread_id.
pub(super) async fn cortex_chat_messages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CortexChatMessagesQuery>,
) -> Result<Json<CortexChatMessagesResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = CortexChatStore::new(pool.clone());

    let thread_id = if let Some(tid) = query.thread_id {
        tid
    } else {
        store
            .latest_thread_id()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    };

    let messages = store
        .load_history(&thread_id, query.limit.min(200))
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load cortex chat history");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(CortexChatMessagesResponse { messages, thread_id }))
}

/// Send a message to cortex chat. Returns an SSE stream with activity events.
///
/// The stream emits:
/// - `thinking` — cortex is processing
/// - `tool_started` — a tool call began
/// - `tool_completed` — a tool call finished (with result preview)
/// - `done` — full response text
/// - `error` — if something went wrong
pub(super) async fn cortex_chat_send(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<CortexChatSendRequest>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, StatusCode> {
    let sessions = state.cortex_chat_sessions.load();
    let session = sessions
        .get(&request.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    let thread_id = request.thread_id;
    let message = request.message;
    let channel_id = request.channel_id;

    let channel_ref = channel_id.as_deref();
    let mut event_rx = session
        .send_message_with_events(&thread_id, &message, channel_ref)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to start cortex chat send");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let stream = async_stream::stream! {
        yield Ok(axum::response::sse::Event::default()
            .event("thinking")
            .data("{}"));

        while let Some(event) = event_rx.recv().await {
            let event_name = match &event {
                CortexChatEvent::Thinking => "thinking",
                CortexChatEvent::ToolStarted { .. } => "tool_started",
                CortexChatEvent::ToolCompleted { .. } => "tool_completed",
                CortexChatEvent::Done { .. } => "done",
                CortexChatEvent::Error { .. } => "error",
            };
            if let Ok(json) = serde_json::to_string(&event) {
                yield Ok(axum::response::sse::Event::default()
                    .event(event_name)
                    .data(json));
            }
        }
    };

    Ok(Sse::new(stream))
}

/// List cortex events for an agent with optional type filter, newest first.
pub(super) async fn cortex_events(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CortexEventsQuery>,
) -> Result<Json<CortexEventsResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = CortexLogger::new(pool.clone());

    let limit = query.limit.min(200);
    let event_type_ref = query.event_type.as_deref();

    let events = logger
        .load_events(limit, query.offset, event_type_ref)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load cortex events");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = logger
        .count_events(event_type_ref)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to count cortex events");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(CortexEventsResponse { events, total }))
}
