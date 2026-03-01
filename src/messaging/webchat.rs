//! Web chat messaging adapter for browser-based agent interaction.
//!
//! Unlike other adapters, this does not own an HTTP server or inbound stream.
//! Inbound messages are injected by the API handler via `MessagingManager::inject_message`,
//! and outbound responses are delivered through the global SSE event bus — the same
//! path used by all other channels. No per-session SSE streams or dedup needed.

use crate::conversation::ConversationLogger;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, OutboundResponse};

use anyhow::Context as _;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;

/// Web chat adapter. Inbound arrives via `inject_message`, outbound is handled
/// by the global SSE event bus in `main.rs`.
pub struct WebChatAdapter {
    conversation_loggers: HashMap<String, ConversationLogger>,
}

impl Default for WebChatAdapter {
    fn default() -> Self {
        Self::new(HashMap::new())
    }
}

impl WebChatAdapter {
    pub fn new(agent_pools: HashMap<String, SqlitePool>) -> Self {
        let conversation_loggers = agent_pools
            .into_iter()
            .map(|(agent_id, pool)| (agent_id, ConversationLogger::new(pool)))
            .collect();
        Self {
            conversation_loggers,
        }
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

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let agent_id = message
            .agent_id
            .as_ref()
            .context("missing agent_id on webchat history message")?;
        let logger = self
            .conversation_loggers
            .get(agent_id.as_ref())
            .with_context(|| {
                format!("no webchat history logger configured for agent '{agent_id}'")
            })?;

        let channel_id: crate::ChannelId = Arc::from(message.conversation_id.as_str());
        let messages = logger.load_recent(&channel_id, limit as i64).await?;

        let history = messages
            .into_iter()
            .map(|message| {
                let is_bot = message.role == "assistant";
                let author = if is_bot {
                    "assistant".to_string()
                } else {
                    message
                        .sender_name
                        .or(message.sender_id)
                        .unwrap_or_else(|| "user".to_string())
                };

                HistoryMessage {
                    author,
                    content: message.content,
                    is_bot,
                }
            })
            .collect::<Vec<_>>();

        tracing::info!(
            agent_id = %agent_id,
            conversation_id = %message.conversation_id,
            count = history.len(),
            "fetched webchat message history"
        );

        Ok(history)
    }

    async fn health_check(&self) -> crate::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        tracing::info!("webchat adapter shut down");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageContent;
    use chrono::Utc;

    #[tokio::test]
    async fn fetch_history_reads_webchat_messages_from_db() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

        sqlx::query(
            "CREATE TABLE conversation_messages (
                id TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL,
                role TEXT NOT NULL,
                sender_name TEXT,
                sender_id TEXT,
                content TEXT NOT NULL,
                metadata TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .expect("conversation_messages table should create");

        sqlx::query(
            "INSERT INTO conversation_messages (
                id, channel_id, role, sender_name, sender_id, content, metadata, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("m1")
        .bind("webchat-session")
        .bind("user")
        .bind("Alice")
        .bind("alice-id")
        .bind("hey there")
        .bind(Option::<String>::None)
        .bind("2026-01-01 00:00:00")
        .execute(&pool)
        .await
        .expect("user row should insert");

        sqlx::query(
            "INSERT INTO conversation_messages (
                id, channel_id, role, sender_name, sender_id, content, metadata, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("m2")
        .bind("webchat-session")
        .bind("assistant")
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind("hello Alice")
        .bind(Option::<String>::None)
        .bind("2026-01-01 00:00:01")
        .execute(&pool)
        .await
        .expect("assistant row should insert");

        let adapter = WebChatAdapter::new(HashMap::from([("agent-a".to_string(), pool)]));

        let inbound = InboundMessage {
            id: "trigger".to_string(),
            source: "webchat".to_string(),
            adapter: Some("webchat".to_string()),
            conversation_id: "webchat-session".to_string(),
            sender_id: "alice-id".to_string(),
            agent_id: Some(Arc::from("agent-a")),
            content: MessageContent::Text("new message".to_string()),
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            formatted_author: Some("Alice".to_string()),
        };

        let history = adapter
            .fetch_history(&inbound, 50)
            .await
            .expect("fetch_history should succeed");

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].author, "Alice");
        assert_eq!(history[0].content, "hey there");
        assert!(!history[0].is_bot);

        assert_eq!(history[1].author, "assistant");
        assert_eq!(history[1].content, "hello Alice");
        assert!(history[1].is_bot);
    }
}
