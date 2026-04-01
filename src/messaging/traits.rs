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
    /// When the message was sent. Used to give the LLM temporal context
    /// for backfilled history so it can distinguish old from recent messages.
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastFailureKind {
    Transient,
    Permanent,
}

impl std::fmt::Display for BroadcastFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient => formatter.write_str("transient"),
            Self::Permanent => formatter.write_str("permanent"),
        }
    }
}

/// Typed proactive-send failure classification carried through the generic error wrapper.
#[derive(Debug, thiserror::Error)]
#[error("{kind} broadcast failure: {source}")]
pub struct BroadcastFailureError {
    pub kind: BroadcastFailureKind,
    #[source]
    pub source: anyhow::Error,
}

pub fn mark_broadcast_failure(
    kind: BroadcastFailureKind,
    error: impl Into<anyhow::Error>,
) -> crate::Error {
    crate::error::Error::Other(
        BroadcastFailureError {
            kind,
            source: error.into(),
        }
        .into(),
    )
}

pub fn mark_retryable_broadcast(error: impl Into<anyhow::Error>) -> crate::Error {
    mark_broadcast_failure(BroadcastFailureKind::Transient, error)
}

pub fn mark_permanent_broadcast(error: impl Into<anyhow::Error>) -> crate::Error {
    mark_broadcast_failure(BroadcastFailureKind::Permanent, error)
}

/// Classify a platform/API send failure into permanent vs transient without changing retry ownership.
///
/// Adapters should use this when the error comes from platform delivery APIs where
/// permission/not-found failures should not be retried.
pub fn mark_classified_broadcast(error: impl Into<anyhow::Error>) -> crate::Error {
    let error = error.into();
    let kind = classify_platform_broadcast_failure(&error);
    mark_broadcast_failure(kind, error)
}

/// Reject unsupported proactive broadcast variants unless an adapter opts into an explicit fallback.
pub fn unsupported_broadcast_variant_error(
    platform: &str,
    response: &OutboundResponse,
) -> crate::Error {
    mark_permanent_broadcast(anyhow::anyhow!(
        "unsupported {platform} broadcast response variant: {}",
        broadcast_variant_name(response)
    ))
}

pub fn ensure_supported_broadcast_response(
    platform: &str,
    response: &OutboundResponse,
    supported: fn(&OutboundResponse) -> bool,
) -> crate::Result<()> {
    if supported(response) {
        Ok(())
    } else {
        Err(unsupported_broadcast_variant_error(platform, response))
    }
}

pub fn broadcast_variant_name(response: &OutboundResponse) -> &'static str {
    match response {
        OutboundResponse::Text(_) => "Text",
        OutboundResponse::ThreadReply { .. } => "ThreadReply",
        OutboundResponse::File { .. } => "File",
        OutboundResponse::Reaction(_) => "Reaction",
        OutboundResponse::RemoveReaction(_) => "RemoveReaction",
        OutboundResponse::Ephemeral { .. } => "Ephemeral",
        OutboundResponse::RichMessage { .. } => "RichMessage",
        OutboundResponse::ScheduledMessage { .. } => "ScheduledMessage",
        OutboundResponse::StreamStart => "StreamStart",
        OutboundResponse::StreamChunk(_) => "StreamChunk",
        OutboundResponse::StreamEnd => "StreamEnd",
        OutboundResponse::Status(_) => "Status",
    }
}

pub fn broadcast_failure_kind(error: &crate::Error) -> BroadcastFailureKind {
    if let crate::error::Error::Other(error) = error {
        for cause in error.chain() {
            if let Some(error) = cause.downcast_ref::<BroadcastFailureError>() {
                return error.kind;
            }
        }
    }

    let kind = if is_heuristically_transient_broadcast_error(error) {
        BroadcastFailureKind::Transient
    } else {
        BroadcastFailureKind::Permanent
    };

    tracing::warn!(
        classified_kind = %kind,
        %error,
        "broadcast failure used heuristic classification fallback"
    );

    kind
}

fn is_heuristically_transient_broadcast_error(error: &crate::Error) -> bool {
    let lower = error.to_string().to_lowercase();
    is_transient_broadcast_message(&lower)
}

fn classify_platform_broadcast_failure(error: &anyhow::Error) -> BroadcastFailureKind {
    let lower = error.to_string().to_lowercase();
    if is_permanent_platform_broadcast_message(&lower) {
        BroadcastFailureKind::Permanent
    } else if is_transient_broadcast_message(&lower) {
        BroadcastFailureKind::Transient
    } else {
        // Preserve prior behavior for unknown platform errors: retry by default.
        BroadcastFailureKind::Transient
    }
}

fn is_transient_broadcast_message(lower: &str) -> bool {
    lower.contains("429")
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("rate limit")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("temporarily unavailable")
        || lower.contains("temporary failure")
        || lower.contains("overloaded")
        || lower.contains("server error")
        || lower.contains("service unavailable")
        || lower.contains("error sending request")
}

fn is_permanent_platform_broadcast_message(lower: &str) -> bool {
    lower.contains("forbidden")
        || lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("missing access")
        || lower.contains("missing permission")
        || lower.contains("not authorized")
        || lower.contains("unauthorized")
        || lower.contains("invalid_auth")
        || lower.contains("not_authed")
        || lower.contains("account_inactive")
        || lower.contains("channel_not_found")
        || lower.contains("chat not found")
        || lower.contains("unknown channel")
        || lower.contains("unknown chat")
        || lower.contains("unknown user")
        || lower.contains("user not found")
        || lower.contains("bot was blocked by the user")
        || lower.contains("chat_write_forbidden")
        || lower.contains("cannot send messages to this user")
        || lower.contains("recipient not found")
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
        _message: &InboundMessage,
        _status: StatusUpdate,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Broadcast a message.
    fn broadcast(
        &self,
        _target: &str,
        _response: OutboundResponse,
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

/// Rewrite a conversation ID's platform prefix to use a named adapter's runtime key.
///
/// Given a `runtime_key` like `"discord:ops"` and a `base_conversation_id` like
/// `"discord:12345"`, returns `"discord:ops:12345"`. If the runtime key matches
/// the existing platform prefix (i.e. the default adapter), the ID is returned
/// unchanged.
pub fn apply_runtime_adapter_to_conversation_id(
    runtime_key: &str,
    base_conversation_id: String,
) -> String {
    let Some((platform, remainder)) = base_conversation_id.split_once(':') else {
        return base_conversation_id;
    };

    if runtime_key == platform {
        base_conversation_id
    } else {
        format!("{runtime_key}:{remainder}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BroadcastFailureKind, broadcast_failure_kind, broadcast_variant_name,
        ensure_supported_broadcast_response, mark_broadcast_failure, mark_classified_broadcast,
    };
    use crate::OutboundResponse;

    #[test]
    fn typed_broadcast_failures_preserve_explicit_kind() {
        let error = mark_broadcast_failure(
            BroadcastFailureKind::Permanent,
            anyhow::anyhow!("invalid destination"),
        );

        assert_eq!(
            broadcast_failure_kind(&error),
            BroadcastFailureKind::Permanent
        );
    }

    #[test]
    fn untyped_broadcast_failures_fall_back_to_heuristic_classification() {
        let transient_error = crate::error::Error::Other(anyhow::anyhow!("HTTP 503 upstream"));
        let permanent_error = crate::error::Error::Other(anyhow::anyhow!("invalid destination"));

        assert_eq!(
            broadcast_failure_kind(&transient_error),
            BroadcastFailureKind::Transient
        );
        assert_eq!(
            broadcast_failure_kind(&permanent_error),
            BroadcastFailureKind::Permanent
        );
    }

    #[test]
    fn classified_platform_broadcast_failures_mark_permanent_for_not_found_or_forbidden() {
        let not_found =
            mark_classified_broadcast(anyhow::anyhow!("Slack API error: channel_not_found"));
        let forbidden = mark_classified_broadcast(anyhow::anyhow!(
            "Telegram send failed: chat_write_forbidden"
        ));

        assert_eq!(
            broadcast_failure_kind(&not_found),
            BroadcastFailureKind::Permanent
        );
        assert_eq!(
            broadcast_failure_kind(&forbidden),
            BroadcastFailureKind::Permanent
        );
    }

    #[test]
    fn classified_platform_broadcast_failures_keep_transient_timeouts_retryable() {
        let timeout =
            mark_classified_broadcast(anyhow::anyhow!("request timed out talking to API"));

        assert_eq!(
            broadcast_failure_kind(&timeout),
            BroadcastFailureKind::Transient
        );
    }

    #[test]
    fn unsupported_broadcast_response_helper_returns_permanent_error() {
        let error = ensure_supported_broadcast_response(
            "test",
            &OutboundResponse::Reaction("thumbsup".to_string()),
            |_| false,
        )
        .expect_err("unsupported variants should error");

        assert_eq!(
            broadcast_failure_kind(&error),
            BroadcastFailureKind::Permanent
        );
        assert!(
            error
                .to_string()
                .contains("unsupported test broadcast response variant: Reaction")
        );
        assert_eq!(
            broadcast_variant_name(&OutboundResponse::StreamChunk("hi".to_string())),
            "StreamChunk"
        );
    }

    #[test]
    fn wrapped_typed_broadcast_failures_preserve_explicit_kind() {
        let error = match mark_broadcast_failure(
            BroadcastFailureKind::Permanent,
            anyhow::anyhow!("invalid destination"),
        ) {
            crate::error::Error::Other(error) => crate::error::Error::Other(error.context("wrap")),
            other => other,
        };

        assert_eq!(
            broadcast_failure_kind(&error),
            BroadcastFailureKind::Permanent
        );
    }
}
