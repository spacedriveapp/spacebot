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
        self.log_bot_message_with_name(channel_id, content, None);
    }

    /// Log a system message (e.g. task delegation audit record). Fire-and-forget.
    ///
    /// System messages are persisted with role `"system"` and are not fed to any
    /// LLM context window. They exist purely for UI display in link channel
    /// timelines and audit logs.
    pub fn log_system_message(&self, channel_id: &str, content: &str) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let channel_id = channel_id.to_string();
        let content = content.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, sender_name, content) \
                 VALUES (?, ?, 'system', 'system', ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&content)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, %channel_id, "failed to persist system message");
            }
        });
    }

    /// Log a bot (assistant) message with an agent display name. Fire-and-forget.
    pub fn log_bot_message_with_name(
        &self,
        channel_id: &ChannelId,
        content: &str,
        sender_name: Option<&str>,
    ) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let channel_id = channel_id.to_string();
        let content = content.to_string();
        let sender_name = sender_name.map(String::from);

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, sender_name, content) \
                 VALUES (?, ?, 'assistant', ?, ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&sender_name)
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

    /// Load messages from any channel (not just the current one).
    ///
    /// Supports optional temporal filtering via `before` and `after` (RFC 3339 strings)
    /// and ordering via `oldest_first`. When `oldest_first` is true, returns the earliest
    /// matching messages instead of the most recent.
    pub async fn load_channel_transcript(
        &self,
        channel_id: &str,
        limit: i64,
        before: Option<&str>,
        after: Option<&str>,
        oldest_first: bool,
    ) -> crate::error::Result<Vec<ConversationMessage>> {
        let mut sql = String::from(
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at \
             FROM conversation_messages \
             WHERE channel_id = ?",
        );

        if before.is_some() {
            sql.push_str(" AND created_at < ?");
        }
        if after.is_some() {
            sql.push_str(" AND created_at > ?");
        }

        if oldest_first {
            sql.push_str(" ORDER BY created_at ASC");
        } else {
            sql.push_str(" ORDER BY created_at DESC");
        }
        sql.push_str(" LIMIT ?");

        let mut query = sqlx::query(&sql).bind(channel_id);
        if let Some(before) = before {
            query = query.bind(before);
        }
        if let Some(after) = after {
            query = query.bind(after);
        }
        query = query.bind(limit);

        let rows = query
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

        // When fetching newest-first, reverse to chronological for the caller
        if !oldest_first {
            messages.reverse();
        }
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
        worker_type: &str,
        agent_id: &crate::AgentId,
    ) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let channel_id = channel_id.map(|c| c.to_string());
        let task = task.to_string();
        let worker_type = worker_type.to_string();
        let agent_id = agent_id.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO worker_runs (id, channel_id, task, worker_type, agent_id) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&task)
            .bind(&worker_type)
            .bind(&agent_id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker start");
            }
        });
    }

    /// Update a worker's status. Fire-and-forget.
    /// Worker status text updates are transient — they're available via the
    /// in-memory StatusBlock for live workers and don't need to be persisted.
    /// The `status` column is reserved for the state enum (running/done/failed).
    pub fn log_worker_status(&self, _worker_id: WorkerId, _status: &str) {
        // Intentionally a no-op. Status text was previously written to the
        // `status` column, overwriting the state enum with free-text like
        // "Searching for weather in Germany" which broke badge rendering
        // and status filtering.
    }

    /// Record a worker completing with its result. Fire-and-forget.
    pub fn log_worker_completed(&self, worker_id: WorkerId, result: &str, success: bool) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let result = result.to_string();
        let status = if success { "done" } else { "failed" };

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE worker_runs SET result = ?, status = ?, completed_at = CURRENT_TIMESTAMP WHERE id = ?"
            )
            .bind(&result)
            .bind(status)
            .bind(&id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker completion");
            }
        });
    }

    /// Mark all orphaned running workers as failed for an agent.
    ///
    /// Called at startup to reconcile rows that were left in `running` when the
    /// process exited before a `WorkerComplete` event was persisted.
    pub async fn reconcile_running_workers_for_agent(
        &self,
        agent_id: &str,
        failure_message: &str,
    ) -> crate::error::Result<u64> {
        let result = sqlx::query(
            "UPDATE worker_runs \
             SET status = 'failed', \
                 completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP), \
                 result = CASE \
                     WHEN result IS NULL OR result = '' THEN ? \
                     ELSE result \
                 END \
             WHERE status = 'running' AND (agent_id = ? OR agent_id IS NULL)",
        )
        .bind(failure_message)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(result.rows_affected())
    }

    /// Mark a detached running worker as cancelled.
    ///
    /// Used by API cancellation when the in-memory channel state no longer has
    /// a live handle for this worker (for example after restart).
    pub async fn cancel_running_worker(
        &self,
        channel_id: &str,
        worker_id: WorkerId,
    ) -> crate::error::Result<bool> {
        let result = sqlx::query(
            "UPDATE worker_runs \
             SET result = CASE \
                     WHEN result IS NULL OR result = '' THEN 'Worker cancelled' \
                     ELSE result \
                 END, \
                 status = 'failed', \
                 completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) \
             WHERE id = ? AND channel_id = ? AND status = 'running'",
        )
        .bind(worker_id.to_string())
        .bind(channel_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(result.rows_affected() > 0)
    }

    /// Mark a detached running worker (`channel_id IS NULL`) as cancelled.
    ///
    /// Used by API cancellation fallback when no in-memory channel state exists.
    pub async fn cancel_running_detached_worker(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<bool> {
        let result = sqlx::query(
            "UPDATE worker_runs \
             SET result = CASE \
                     WHEN result IS NULL OR result = '' THEN 'Worker cancelled' \
                     ELSE result \
                 END, \
                 status = 'failed', \
                 completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) \
             WHERE id = ? AND channel_id IS NULL AND status = 'running'",
        )
        .bind(worker_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(result.rows_affected() > 0)
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

    /// List worker runs for an agent, ordered by most recent first.
    /// Does NOT include the transcript blob — that's fetched separately via `get_worker_detail`.
    pub async fn list_worker_runs(
        &self,
        agent_id: &str,
        limit: i64,
        offset: i64,
        status_filter: Option<&str>,
    ) -> crate::error::Result<(Vec<WorkerRunRow>, i64)> {
        let (count_where_clause, list_where_clause, has_status_filter) = if status_filter.is_some()
        {
            (
                "WHERE w.agent_id = ?1 AND w.status = ?2",
                "WHERE w.agent_id = ?1 AND w.status = ?4",
                true,
            )
        } else {
            ("WHERE w.agent_id = ?1", "WHERE w.agent_id = ?1", false)
        };

        let count_query =
            format!("SELECT COUNT(*) as total FROM worker_runs w {count_where_clause}");
        let list_query = format!(
            "SELECT w.id, w.task, w.status, w.worker_type, w.channel_id, w.started_at, \
                    w.completed_at, w.transcript IS NOT NULL as has_transcript, \
                    w.tool_calls, c.display_name as channel_name \
             FROM worker_runs w \
             LEFT JOIN channels c ON w.channel_id = c.id \
             {list_where_clause} \
             ORDER BY w.started_at DESC \
             LIMIT ?2 OFFSET ?3"
        );

        let mut count_q = sqlx::query(&count_query).bind(agent_id);
        let mut list_q = sqlx::query(&list_query)
            .bind(agent_id)
            .bind(limit)
            .bind(offset);

        if has_status_filter {
            let filter = status_filter.unwrap_or("");
            count_q = count_q.bind(filter);
            list_q = list_q.bind(filter);
        }

        let total: i64 = count_q
            .fetch_one(&self.pool)
            .await
            .map(|row| row.try_get("total").unwrap_or(0))
            .map_err(|e| anyhow::anyhow!(e))?;

        let rows = list_q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        let items = rows
            .into_iter()
            .map(|row| WorkerRunRow {
                id: row.try_get("id").unwrap_or_default(),
                task: row.try_get("task").unwrap_or_default(),
                status: row.try_get("status").unwrap_or_default(),
                worker_type: row
                    .try_get("worker_type")
                    .unwrap_or_else(|_| "builtin".into()),
                channel_id: row.try_get("channel_id").ok(),
                channel_name: row.try_get("channel_name").ok(),
                started_at: row
                    .try_get::<chrono::DateTime<chrono::Utc>, _>("started_at")
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
                completed_at: row
                    .try_get::<chrono::DateTime<chrono::Utc>, _>("completed_at")
                    .ok()
                    .map(|t| t.to_rfc3339()),
                has_transcript: row.try_get::<bool, _>("has_transcript").unwrap_or(false),
                tool_calls: row.try_get::<i64, _>("tool_calls").unwrap_or(0),
            })
            .collect();

        Ok((items, total))
    }

    /// Get full detail for a single worker run, including the compressed transcript blob.
    pub async fn get_worker_detail(
        &self,
        agent_id: &str,
        worker_id: &str,
    ) -> crate::error::Result<Option<WorkerDetailRow>> {
        let row = sqlx::query(
            "SELECT w.id, w.task, w.result, w.status, w.worker_type, w.channel_id, \
                    w.started_at, w.completed_at, w.transcript, w.tool_calls, \
                    c.display_name as channel_name \
             FROM worker_runs w \
             LEFT JOIN channels c ON w.channel_id = c.id \
             WHERE w.agent_id = ? AND w.id = ?",
        )
        .bind(agent_id)
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

        Ok(row.map(|row| WorkerDetailRow {
            id: row.try_get("id").unwrap_or_default(),
            task: row.try_get("task").unwrap_or_default(),
            result: row.try_get("result").ok(),
            status: row.try_get("status").unwrap_or_default(),
            worker_type: row
                .try_get("worker_type")
                .unwrap_or_else(|_| "builtin".into()),
            channel_id: row.try_get("channel_id").ok(),
            channel_name: row.try_get("channel_name").ok(),
            started_at: row
                .try_get::<chrono::DateTime<chrono::Utc>, _>("started_at")
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
            completed_at: row
                .try_get::<chrono::DateTime<chrono::Utc>, _>("completed_at")
                .ok()
                .map(|t| t.to_rfc3339()),
            transcript_blob: row.try_get("transcript").ok(),
            tool_calls: row.try_get::<i64, _>("tool_calls").unwrap_or(0),
        }))
    }
}

/// A worker run row without the transcript blob (for list queries).
#[derive(Debug, Clone, Serialize)]
pub struct WorkerRunRow {
    pub id: String,
    pub task: String,
    pub status: String,
    pub worker_type: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub has_transcript: bool,
    pub tool_calls: i64,
}

/// A worker run row with full detail including the transcript blob.
#[derive(Debug, Clone)]
pub struct WorkerDetailRow {
    pub id: String,
    pub task: String,
    pub result: Option<String>,
    pub status: String,
    pub worker_type: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub transcript_blob: Option<Vec<u8>>,
    pub tool_calls: i64,
}

#[cfg(test)]
mod tests {
    use super::ProcessRunLogger;

    async fn setup_worker_runs_table() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("failed to create sqlite memory pool");

        sqlx::query(
            "CREATE TABLE worker_runs (
                id TEXT PRIMARY KEY,
                channel_id TEXT,
                status TEXT NOT NULL,
                result TEXT,
                completed_at TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .expect("failed to create worker_runs table");

        pool
    }

    #[tokio::test]
    async fn cancel_running_detached_worker_updates_null_channel_rows() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO worker_runs (id, channel_id, status, result) VALUES (?, NULL, 'running', '')")
            .bind(worker_id.to_string())
            .execute(&pool)
            .await
            .expect("failed to insert detached worker row");

        let cancelled = logger
            .cancel_running_detached_worker(worker_id)
            .await
            .expect("cancel should succeed");
        assert!(cancelled);

        let row = sqlx::query("SELECT status, result FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("failed to fetch worker row");

        let status: String = sqlx::Row::try_get(&row, "status").expect("missing status");
        let result: String = sqlx::Row::try_get(&row, "result").expect("missing result");
        assert_eq!(status, "failed");
        assert_eq!(result, "Worker cancelled");
    }

    #[tokio::test]
    async fn cancel_running_detached_worker_does_not_touch_channel_bound_rows() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query(
            "INSERT INTO worker_runs (id, channel_id, status, result) VALUES (?, 'channel-1', 'running', '')",
        )
        .bind(worker_id.to_string())
        .execute(&pool)
        .await
        .expect("failed to insert channel worker row");

        let cancelled = logger
            .cancel_running_detached_worker(worker_id)
            .await
            .expect("cancel should not error");
        assert!(!cancelled);

        let row = sqlx::query("SELECT status FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("failed to fetch worker row");
        let status: String = sqlx::Row::try_get(&row, "status").expect("missing status");
        assert_eq!(status, "running");
    }
}
