//! Web chat messaging adapter for browser-based agent interaction.
//!
//! Unlike other adapters, this does not own an HTTP server or inbound stream.
//! Inbound messages are injected by the API handler via `MessagingManager::inject_message`,
//! and outbound responses are routed to per-session channels consumed as SSE streams.

use crate::messaging::traits::{InboundStream, Messaging};
use crate::{InboundMessage, OutboundResponse, StatusUpdate};

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// Web chat adapter state.
pub struct WebChatAdapter {
    sessions: Arc<RwLock<HashMap<String, mpsc::Sender<WebChatEvent>>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum WebChatEvent {
    Thinking,
    Text(String),
    StreamStart,
    StreamChunk(String),
    StreamEnd,
    ToolStarted { tool_name: String },
    ToolCompleted { tool_name: String },
    StopTyping,
    Done,
}

impl Default for WebChatAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl WebChatAdapter {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_session(&self, conversation_id: &str) -> mpsc::Receiver<WebChatEvent> {
        let (tx, rx) = mpsc::channel(256);
        self.sessions
            .write()
            .await
            .insert(conversation_id.to_string(), tx);
        tracing::debug!(%conversation_id, "webchat session registered");
        rx
    }

    pub async fn unregister_session(&self, conversation_id: &str) {
        self.sessions.write().await.remove(conversation_id);
        tracing::debug!(%conversation_id, "webchat session unregistered");
    }
}

impl Messaging for WebChatAdapter {
    fn name(&self) -> &str {
        "webchat"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        // Inbound messages bypass the stream via inject_message, so return
        // a stream that stays open but never yields.
        Ok(Box::pin(futures::stream::pending()))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let sessions = self.sessions.read().await;
        let Some(tx) = sessions.get(&message.conversation_id) else {
            tracing::debug!(conversation_id = %message.conversation_id, "no webchat session for response");
            return Ok(());
        };

        let (event, signals_done) = match response {
            OutboundResponse::Text(text) => (WebChatEvent::Text(text), true),
            OutboundResponse::ThreadReply { text, .. } => (WebChatEvent::Text(text), true),
            OutboundResponse::StreamStart => (WebChatEvent::StreamStart, false),
            OutboundResponse::StreamChunk(text) => (WebChatEvent::StreamChunk(text), false),
            OutboundResponse::StreamEnd => (WebChatEvent::StreamEnd, true),
            OutboundResponse::File { .. }
            | OutboundResponse::Reaction(_)
            | OutboundResponse::RemoveReaction(_)
            | OutboundResponse::Ephemeral { .. }
            | OutboundResponse::ScheduledMessage { .. }
            | OutboundResponse::RichMessage { .. }
            | OutboundResponse::Status(_) => return Ok(()),
        };

        let _ = tx.send(event).await;
        if signals_done {
            let _ = tx.send(WebChatEvent::Done).await;
        }
        Ok(())
    }

    async fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> crate::Result<()> {
        let sessions = self.sessions.read().await;
        let Some(tx) = sessions.get(&message.conversation_id) else {
            return Ok(());
        };

        let event = match status {
            StatusUpdate::Thinking => WebChatEvent::Thinking,
            StatusUpdate::StopTyping => WebChatEvent::StopTyping,
            StatusUpdate::ToolStarted { tool_name } => WebChatEvent::ToolStarted { tool_name },
            StatusUpdate::ToolCompleted { tool_name } => WebChatEvent::ToolCompleted { tool_name },
            _ => return Ok(()),
        };

        let _ = tx.send(event).await;
        Ok(())
    }

    async fn health_check(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        self.sessions.write().await.clear();
        tracing::info!("webchat adapter shut down");
        Ok(())
    }
}
