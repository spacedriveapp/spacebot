//! Messaging trait and dynamic dispatch companion.

use crate::error::Result;
use crate::{InboundMessage, OutboundResponse, StatusUpdate};
use futures::Stream;
use std::pin::Pin;

/// Message stream type.
pub type InboundStream = Pin<Box<dyn Stream<Item = InboundMessage> + Send>>;

/// A message from platform history used for backfilling channel context.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub author: String,
    pub content: String,
    pub is_bot: bool,
}

/// Static trait for messaging adapters.
/// Use this for type-safe implementations.
pub trait Messaging: Send + Sync + 'static {
    /// Unique name for this adapter.
    fn name(&self) -> &str;

    /// Start the adapter and return inbound message stream.
    fn start(&self) -> impl std::future::Future<Output = Result<InboundStream>> + Send;

    /// Send a response to a message.
    fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Send a status update.
    fn send_status(
        &self,
        message: &InboundMessage,
        status: StatusUpdate,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Broadcast a message.
    fn broadcast(
        &self,
        target: &str,
        response: OutboundResponse,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Fetch recent message history from the platform for context backfill.
    /// Returns messages in chronological order (oldest first).
    /// `before` is the message that triggered channel creation — fetch messages before it.
    fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<HistoryMessage>>> + Send {
        let _ = (message, limit);
        async { Ok(Vec::new()) }
    }

    /// Health check.
    fn health_check(&self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Graceful shutdown.
    fn shutdown(&self) -> impl std::future::Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}

/// Dynamic trait for runtime polymorphism.
/// Use this when you need `Arc<dyn MessagingDyn>` for storing different adapters.
pub trait MessagingDyn: Send + Sync + 'static {
    fn name(&self) -> &str;

    fn start<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<InboundStream>> + Send + 'a>>;

    fn respond<'a>(
        &'a self,
        message: &'a InboundMessage,
        response: OutboundResponse,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

    fn send_status<'a>(
        &'a self,
        message: &'a InboundMessage,
        status: StatusUpdate,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

    fn broadcast<'a>(
        &'a self,
        target: &'a str,
        response: OutboundResponse,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

    fn fetch_history<'a>(
        &'a self,
        message: &'a InboundMessage,
        limit: usize,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<HistoryMessage>>> + Send + 'a>>;

    fn health_check<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;

    fn shutdown<'a>(&'a self)
    -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
}

/// Blanket implementation: any type implementing Messaging automatically implements MessagingDyn.
impl<T: Messaging> MessagingDyn for T {
    fn name(&self) -> &str {
        Messaging::name(self)
    }

    fn start<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<InboundStream>> + Send + 'a>> {
        Box::pin(Messaging::start(self))
    }

    fn respond<'a>(
        &'a self,
        message: &'a InboundMessage,
        response: OutboundResponse,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(Messaging::respond(self, message, response))
    }

    fn send_status<'a>(
        &'a self,
        message: &'a InboundMessage,
        status: StatusUpdate,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(Messaging::send_status(self, message, status))
    }

    fn broadcast<'a>(
        &'a self,
        target: &'a str,
        response: OutboundResponse,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(Messaging::broadcast(self, target, response))
    }

    fn fetch_history<'a>(
        &'a self,
        message: &'a InboundMessage,
        limit: usize,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<HistoryMessage>>> + Send + 'a>> {
        Box::pin(Messaging::fetch_history(self, message, limit))
    }

    fn health_check<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(Messaging::health_check(self))
    }

    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(Messaging::shutdown(self))
    }
}
