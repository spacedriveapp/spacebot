//! Conversation message persistence (SQLite).

use crate::{BranchId, ChannelId, WorkerId};

use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use std::collections::HashMap;

/// Persists conversation messages (user and assistant) to SQLite.
///
/// All write methods are fire-and-forget — they spawn a tokio task and return
/// immediately so the caller never blocks on a DB write.
#[derive(Debug, Clone)]
pub struct ConversationLogger {
    pool: SqlitePool,
}

/// A persisted conversation message.
#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub id: String,
    pub channel_id: String,
    pub role: String,
    pub sender_name: Option<String>,
    pub sender_id: Option<String>,
    pub content: String,
    pub metadata: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl ConversationLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Log a user message. Fire-and-forget.
    pub fn log_user_message(
        &self,
        channel_id: &ChannelId,
        sender_name: &str,
        sender_id: &str,
        content: &str,
        metadata: &HashMap<String, serde_json::Value>,
    ) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let channel_id = channel_id.to_string();
        let sender_name = sender_name.to_string();
        let sender_id = sender_id.to_string();
        let content = content.to_string();
        let metadata_json = serde_json::to_string(metadata).ok();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, sender_name, sender_id, content, metadata) \
                 VALUES (?, ?, 'user', ?, ?, ?, ?)"
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&sender_name)
            .bind(&sender_id)
            .bind(&content)
            .bind(&metadata_json)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, "failed to persist user message");
            }
        });
    }

    /// Log a bot (assistant) message. Fire-and-forget.
    pub fn log_bot_message(&self, channel_id: &ChannelId, content: &str) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let channel_id = channel_id.to_string();
        let content = content.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, content) \
                 VALUES (?, ?, 'assistant', ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&content)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, "failed to persist bot message");
            }
        });
    }

    /// Load recent messages for a channel (oldest first).
    pub async fn load_recent(
        &self,
        channel_id: &ChannelId,
        limit: i64,
    ) -> crate::error::Result<Vec<ConversationMessage>> {
        let rows = sqlx::query(
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at \
             FROM conversation_messages \
             WHERE channel_id = ? \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(channel_id.as_ref())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

        let mut messages: Vec<ConversationMessage> = rows
            .into_iter()
            .map(|row| ConversationMessage {
                id: row.try_get("id").unwrap_or_default(),
                channel_id: row.try_get("channel_id").unwrap_or_default(),
                role: row.try_get("role").unwrap_or_default(),
                sender_name: row.try_get("sender_name").ok(),
                sender_id: row.try_get("sender_id").ok(),
                content: row.try_get("content").unwrap_or_default(),
                metadata: row.try_get("metadata").ok(),
                created_at: row
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect();

        // Reverse to chronological order
        messages.reverse();

        Ok(messages)
    }

    /// Load recent messages from any channel (not just the current one).
    pub async fn load_channel_transcript(
        &self,
        channel_id: &str,
        limit: i64,
    ) -> crate::error::Result<Vec<ConversationMessage>> {
        let rows = sqlx::query(
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at \
             FROM conversation_messages \
             WHERE channel_id = ? \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

        let mut messages: Vec<ConversationMessage> = rows
            .into_iter()
            .map(|row| ConversationMessage {
                id: row.try_get("id").unwrap_or_default(),
                channel_id: row.try_get("channel_id").unwrap_or_default(),
                role: row.try_get("role").unwrap_or_default(),
                sender_name: row.try_get("sender_name").ok(),
                sender_id: row.try_get("sender_id").ok(),
                content: row.try_get("content").unwrap_or_default(),
                metadata: row.try_get("metadata").ok(),
                created_at: row
                    .try_get("created_at")
                    .unwrap_or_else(|_| chrono::Utc::now()),
            })
            .collect();

        messages.reverse();
        Ok(messages)
    }
}

/// A unified timeline item combining messages, branch runs, and worker runs.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimelineItem {
    Message {
        id: String,
        role: String,
        sender_name: Option<String>,
        sender_id: Option<String>,
        content: String,
        created_at: String,
    },
    BranchRun {
        id: String,
        description: String,
        conclusion: Option<String>,
        started_at: String,
        completed_at: Option<String>,
    },
    WorkerRun {
        id: String,
        task: String,
        result: Option<String>,
        status: String,
        started_at: String,
        completed_at: Option<String>,
    },
}

/// Persists branch and worker run records for channel timeline history.
///
/// All write methods are fire-and-forget, same pattern as ConversationLogger.
#[derive(Debug, Clone)]
pub struct ProcessRunLogger {
    pool: SqlitePool,
}

impl ProcessRunLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Record a branch starting. Fire-and-forget.
    pub fn log_branch_started(
        &self,
        channel_id: &ChannelId,
        branch_id: BranchId,
        description: &str,
    ) {
        let pool = self.pool.clone();
        let id = branch_id.to_string();
        let channel_id = channel_id.to_string();
        let description = description.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO branch_runs (id, channel_id, description) VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&description)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, branch_id = %id, "failed to persist branch start");
            }
        });
    }

    /// Record a branch completing with its conclusion. Fire-and-forget.
    pub fn log_branch_completed(&self, branch_id: BranchId, conclusion: &str) {
        let pool = self.pool.clone();
        let id = branch_id.to_string();
        let conclusion = conclusion.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE branch_runs SET conclusion = ?, completed_at = CURRENT_TIMESTAMP WHERE id = ?"
            )
            .bind(&conclusion)
            .bind(&id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, branch_id = %id, "failed to persist branch completion");
            }
        });
    }

    /// Record a worker starting. Fire-and-forget.
    pub fn log_worker_started(
        &self,
        channel_id: Option<&ChannelId>,
        worker_id: WorkerId,
        task: &str,
    ) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let channel_id = channel_id.map(|c| c.to_string());
        let task = task.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO worker_runs (id, channel_id, task) VALUES (?, ?, ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&task)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker start");
            }
        });
    }

    /// Update a worker's status. Fire-and-forget.
    pub fn log_worker_status(&self, worker_id: WorkerId, status: &str) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let status = status.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query("UPDATE worker_runs SET status = ? WHERE id = ?")
                .bind(&status)
                .bind(&id)
                .execute(&pool)
                .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker status");
            }
        });
    }

    /// Record a worker completing with its result. Fire-and-forget.
    pub fn log_worker_completed(&self, worker_id: WorkerId, result: &str) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let result = result.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE worker_runs SET result = ?, status = 'done', completed_at = CURRENT_TIMESTAMP WHERE id = ?"
            )
            .bind(&result)
            .bind(&id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker completion");
            }
        });
    }

    /// Load a unified timeline for a channel: messages, branch runs, and worker runs
    /// interleaved chronologically (oldest first).
    ///
    /// When `before` is provided, only items with a timestamp strictly before that
    /// value are returned, enabling cursor-based pagination.
    pub async fn load_channel_timeline(
        &self,
        channel_id: &str,
        limit: i64,
        before: Option<&str>,
    ) -> crate::error::Result<Vec<TimelineItem>> {
        let before_clause = if before.is_some() {
            "AND datetime(timestamp) < datetime(?3)"
        } else {
            ""
        };

        let query_str = format!(
            "SELECT * FROM ( \
                SELECT 'message' AS item_type, id, role, sender_name, sender_id, content, \
                       NULL AS description, NULL AS conclusion, NULL AS task, NULL AS result, NULL AS status, \
                       created_at AS timestamp, NULL AS completed_at \
                FROM conversation_messages WHERE channel_id = ?1 \
                UNION ALL \
                SELECT 'branch_run' AS item_type, id, NULL, NULL, NULL, NULL, \
                       description, conclusion, NULL, NULL, NULL, \
                       started_at AS timestamp, completed_at \
                FROM branch_runs WHERE channel_id = ?1 \
                UNION ALL \
                SELECT 'worker_run' AS item_type, id, NULL, NULL, NULL, NULL, \
                       NULL, NULL, task, result, status, \
                       started_at AS timestamp, completed_at \
                FROM worker_runs WHERE channel_id = ?1 \
            ) WHERE 1=1 {before_clause} ORDER BY timestamp DESC LIMIT ?2"
        );

        let mut query = sqlx::query(&query_str).bind(channel_id).bind(limit);

        if let Some(before_ts) = before {
            query = query.bind(before_ts);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        let mut items: Vec<TimelineItem> = rows
            .into_iter()
            .filter_map(|row| {
                let item_type: String = row.try_get("item_type").ok()?;
                match item_type.as_str() {
                    "message" => Some(TimelineItem::Message {
                        id: row.try_get("id").unwrap_or_default(),
                        role: row.try_get("role").unwrap_or_default(),
                        sender_name: row.try_get("sender_name").ok(),
                        sender_id: row.try_get("sender_id").ok(),
                        content: row.try_get("content").unwrap_or_default(),
                        created_at: row
                            .try_get::<chrono::DateTime<chrono::Utc>, _>("timestamp")
                            .map(|t| t.to_rfc3339())
                            .unwrap_or_default(),
                    }),
                    "branch_run" => Some(TimelineItem::BranchRun {
                        id: row.try_get("id").unwrap_or_default(),
                        description: row.try_get("description").unwrap_or_default(),
                        conclusion: row.try_get("conclusion").ok(),
                        started_at: row
                            .try_get::<chrono::DateTime<chrono::Utc>, _>("timestamp")
                            .map(|t| t.to_rfc3339())
                            .unwrap_or_default(),
                        completed_at: row
                            .try_get::<chrono::DateTime<chrono::Utc>, _>("completed_at")
                            .ok()
                            .map(|t| t.to_rfc3339()),
                    }),
                    "worker_run" => Some(TimelineItem::WorkerRun {
                        id: row.try_get("id").unwrap_or_default(),
                        task: row.try_get("task").unwrap_or_default(),
                        result: row.try_get("result").ok(),
                        status: row.try_get("status").unwrap_or_default(),
                        started_at: row
                            .try_get::<chrono::DateTime<chrono::Utc>, _>("timestamp")
                            .map(|t| t.to_rfc3339())
                            .unwrap_or_default(),
                        completed_at: row
                            .try_get::<chrono::DateTime<chrono::Utc>, _>("completed_at")
                            .ok()
                            .map(|t| t.to_rfc3339()),
                    }),
                    _ => None,
                }
            })
            .collect();

        // Reverse to chronological order
        items.reverse();
        Ok(items)
    }
}
