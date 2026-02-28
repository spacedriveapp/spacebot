//! Web chat messaging adapter for browser-based agent interaction.
//!
//! Unlike other adapters, this does not own an HTTP server or inbound stream.
//! Inbound messages are injected by the API handler via `MessagingManager::inject_message`,
//! and outbound responses are delivered through the global SSE event bus — the same
//! path used by all other channels. No per-session SSE streams or dedup needed.

use crate::messaging::traits::{InboundStream, Messaging};
use crate::{InboundMessage, OutboundResponse};

/// Web chat adapter. Stateless — inbound arrives via `inject_message`,
/// outbound is handled by the global SSE event bus in `main.rs`.
pub struct WebChatAdapter;

impl Default for WebChatAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl WebChatAdapter {
    pub fn new() -> Self {
        Self
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
        _message: &InboundMessage,
        _response: OutboundResponse,
    ) -> crate::Result<()> {
        // Outbound delivery is handled by the global SSE event bus in main.rs.
        // The webchat adapter itself doesn't need to do anything — the API events
        // stream already pushes outbound_message events to all connected clients,
        // and the portal chat UI consumes the same timeline as regular channels.
        Ok(())
    }

    async fn health_check(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        tracing::info!("webchat adapter shut down");
        Ok(())
    }
}
