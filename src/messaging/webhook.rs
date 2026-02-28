//! Webhook messaging adapter for programmatic access.
//!
//! Exposes an HTTP server that accepts inbound messages via POST and
//! delivers responses via a per-conversation polling endpoint. This is
//! the integration point for scripts, CI pipelines, and other programs
//! that need to interact with Spacebot programmatically.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use axum::Router;
use axum::extract::{Json, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};

use crate::messaging::traits::{InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse};

/// Webhook adapter state.
pub struct WebhookAdapter {
    port: u16,
    bind: String,
    auth_token: Option<String>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<InboundMessage>>>>,
    /// Buffered responses per conversation_id, waiting to be polled.
    response_buffers: Arc<RwLock<HashMap<String, Vec<WebhookResponse>>>>,
    shutdown_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

/// Shared state for axum handlers.
#[derive(Clone)]
struct AppState {
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<InboundMessage>>>>,
    response_buffers: Arc<RwLock<HashMap<String, Vec<WebhookResponse>>>>,
    auth_token: Option<String>,
    runtime_key: String,
}

/// Inbound webhook request body.
#[derive(Debug, Deserialize)]
struct WebhookRequest {
    /// Unique conversation identifier. Reuse the same ID to continue a conversation.
    conversation_id: String,
    /// Sender identifier (e.g. a username or service name).
    #[serde(default = "default_sender")]
    sender_id: String,
    /// Message text content.
    content: String,
    /// Optional agent to route to (overrides binding resolution).
    agent_id: Option<String>,
}

fn default_sender() -> String {
    "webhook".into()
}

/// A buffered response waiting to be polled.
#[derive(Debug, Clone, Serialize)]
struct WebhookResponse {
    #[serde(rename = "type")]
    response_type: String,
    content: Option<String>,
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caption: Option<String>,
}

/// Response from the poll endpoint.
#[derive(Debug, Serialize)]
struct PollResponse {
    messages: Vec<WebhookResponse>,
}

impl WebhookAdapter {
    pub fn new(port: u16, bind: impl Into<String>, auth_token: Option<String>) -> Self {
        Self {
            port,
            bind: bind.into(),
            auth_token,
            inbound_tx: Arc::new(RwLock::new(None)),
            response_buffers: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx: Arc::new(RwLock::new(None)),
        }
    }
}

impl Messaging for WebhookAdapter {
    fn name(&self) -> &str {
        "webhook"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        *self.inbound_tx.write().await = Some(inbound_tx.clone());
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let state = AppState {
            inbound_tx: self.inbound_tx.clone(),
            response_buffers: self.response_buffers.clone(),
            auth_token: self.auth_token.clone(),
            runtime_key: self.name().to_string(),
        };

        if self.auth_token.is_none() {
            tracing::warn!(
                "webhook authentication is disabled because no auth token is configured"
            );
        }

        let app = Router::new()
            .route("/send", post(handle_send))
            .route("/poll/{conversation_id}", get(handle_poll))
            .route("/health", get(handle_health))
            .with_state(state);

        let bind = if self.bind.contains(':') {
            format!("[{}]:{}", self.bind, self.port)
        } else {
            format!("{}:{}", self.bind, self.port)
        };
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .with_context(|| format!("failed to bind webhook server to {bind}"))?;
        tracing::info!(%bind, "webhook server listening");

        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.recv().await;
                })
                .await
            {
                tracing::error!(%error, "webhook server exited with error");
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(inbound_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let webhook_response = match response {
            OutboundResponse::Text(text) => WebhookResponse {
                response_type: "text".into(),
                content: Some(text),
                filename: None,
                caption: None,
            },
            OutboundResponse::RichMessage { text, .. } => WebhookResponse {
                response_type: "text".into(),
                content: Some(text),
                filename: None,
                caption: None,
            },
            OutboundResponse::ThreadReply { text, .. } => WebhookResponse {
                response_type: "text".into(),
                content: Some(text),
                filename: None,
                caption: None,
            },
            OutboundResponse::File {
                filename, caption, ..
            } => WebhookResponse {
                response_type: "file".into(),
                content: None,
                filename: Some(filename),
                caption,
            },
            OutboundResponse::StreamStart => WebhookResponse {
                response_type: "stream_start".into(),
                content: None,
                filename: None,
                caption: None,
            },
            OutboundResponse::StreamChunk(text) => WebhookResponse {
                response_type: "stream_chunk".into(),
                content: Some(text),
                filename: None,
                caption: None,
            },
            OutboundResponse::StreamEnd => WebhookResponse {
                response_type: "stream_end".into(),
                content: None,
                filename: None,
                caption: None,
            },
            // Reactions, status updates, and remove-reaction aren't meaningful over webhook
            OutboundResponse::Reaction(_)
            | OutboundResponse::RemoveReaction(_)
            | OutboundResponse::Status(_) => return Ok(()),
            // Slack-specific rich variants — fall back to plain text
            OutboundResponse::Ephemeral { text, .. } => WebhookResponse {
                response_type: "text".into(),
                content: Some(text),
                filename: None,
                caption: None,
            },
            OutboundResponse::ScheduledMessage { text, .. } => WebhookResponse {
                response_type: "text".into(),
                content: Some(text),
                filename: None,
                caption: None,
            },
        };

        self.response_buffers
            .write()
            .await
            .entry(message.conversation_id.clone())
            .or_default()
            .push(webhook_response);

        Ok(())
    }

    async fn health_check(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        if let Some(tx) = self.shutdown_tx.read().await.as_ref() {
            tx.send(()).await.ok();
        }
        tracing::info!("webhook adapter shut down");
        Ok(())
    }
}

// -- Axum handlers --

async fn handle_send(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<WebhookRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if !is_authorized(&headers, state.auth_token.as_deref()) {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized".into()));
    }

    let tx = state.inbound_tx.read().await;
    let Some(tx) = tx.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "webhook not initialized".into(),
        ));
    };

    let mut metadata = HashMap::new();
    metadata.insert(
        "webhook_conversation_id".into(),
        serde_json::Value::String(request.conversation_id.clone()),
    );
    metadata.insert(
        "display_name".into(),
        serde_json::Value::String(request.sender_id.clone()),
    );

    let conversation_id = format!("webhook:{}", request.conversation_id);

    let inbound = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "webhook".into(),
        adapter: Some(state.runtime_key.clone()),
        conversation_id,
        sender_id: request.sender_id.clone(),
        agent_id: request.agent_id.map(Into::into),
        content: MessageContent::Text(request.content),
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author: Some(request.sender_id),
    };

    tx.send(inbound)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "channel closed".into()))?;

    Ok(StatusCode::ACCEPTED)
}

async fn handle_poll(
    headers: HeaderMap,
    State(state): State<AppState>,
    axum::extract::Path(conversation_id): axum::extract::Path<String>,
) -> Result<Json<PollResponse>, (StatusCode, String)> {
    if !is_authorized(&headers, state.auth_token.as_deref()) {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized".into()));
    }

    let key = format!("webhook:{conversation_id}");
    let messages = state
        .response_buffers
        .write()
        .await
        .remove(&key)
        .unwrap_or_default();

    Ok(Json(PollResponse { messages }))
}

async fn handle_health() -> StatusCode {
    StatusCode::OK
}

fn is_authorized(headers: &HeaderMap, expected_token: Option<&str>) -> bool {
    let Some(expected_token) = expected_token else {
        return true;
    };

    if headers
        .get("x-webhook-token")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|token| token == expected_token)
    {
        return true;
    }

    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| token == expected_token)
}
