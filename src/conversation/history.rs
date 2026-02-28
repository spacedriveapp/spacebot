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

/// A pending retrigger outbox row for durable relay replay.
#[derive(Debug, Clone)]
pub(crate) struct RetriggerOutboxRow {
    pub id: String,
    pub payload_json: String,
    pub attempt_count: i64,
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
        const MAX_RETRIES: usize = 3;

        tokio::spawn(async move {
            for attempt in 1..=MAX_RETRIES {
                let query_result = sqlx::query(
                    "UPDATE worker_runs \
                     SET result = ?, status = ?, completed_at = CURRENT_TIMESTAMP \
                     WHERE id = ? AND completed_at IS NULL",
                )
                .bind(&result)
                .bind(status)
                .bind(&id)
                .execute(&pool)
                .await;

                match query_result {
                    Ok(done) if done.rows_affected() > 0 => return,
                    Ok(_) if attempt < MAX_RETRIES => {
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    Ok(_) => {
                        tracing::debug!(
                            worker_id = %id,
                            attempts = attempt,
                            "worker completion update skipped (already completed or missing start row)"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%error, worker_id = %id, "failed to persist worker completion");
                        return;
                    }
                }
            }
        });
    }

    /// Persist a retrigger payload for durable replay.
    pub async fn enqueue_retrigger_outbox(
        &self,
        agent_id: &crate::AgentId,
        channel_id: &ChannelId,
        payload_json: &str,
    ) -> crate::error::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO channel_retrigger_outbox \
             (id, agent_id, channel_id, result_payload) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(agent_id.as_ref())
        .bind(channel_id.as_ref())
        .bind(payload_json)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        Ok(id)
    }

    /// Overwrite the payload for a pending outbox row.
    pub async fn update_retrigger_outbox_payload(
        &self,
        outbox_id: &str,
        payload_json: &str,
    ) -> crate::error::Result<()> {
        sqlx::query(
            "UPDATE channel_retrigger_outbox \
             SET result_payload = ? \
             WHERE id = ? AND delivered_at IS NULL",
        )
        .bind(payload_json)
        .bind(outbox_id)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    /// List due retrigger outbox rows for replay.
    pub(crate) async fn list_due_retrigger_outbox(
        &self,
        agent_id: &crate::AgentId,
        channel_id: &ChannelId,
        limit: i64,
    ) -> crate::error::Result<Vec<RetriggerOutboxRow>> {
        let rows = sqlx::query(
            "SELECT id, result_payload, attempt_count \
             FROM channel_retrigger_outbox \
             WHERE agent_id = ? \
               AND channel_id = ? \
               AND delivered_at IS NULL \
               AND datetime(next_attempt_at) <= datetime(CURRENT_TIMESTAMP) \
             ORDER BY created_at ASC, id ASC \
             LIMIT ?",
        )
        .bind(agent_id.as_ref())
        .bind(channel_id.as_ref())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(|error| {
                    anyhow::anyhow!("failed to decode retrigger outbox id: {error}")
                })?;
                let payload_json: String = row.try_get("result_payload").map_err(|error| {
                    anyhow::anyhow!(
                        "failed to decode retrigger outbox payload for row {id}: {error}"
                    )
                })?;
                let attempt_count: i64 = row.try_get("attempt_count").map_err(|error| {
                    anyhow::anyhow!(
                        "failed to decode retrigger outbox attempt_count for row {id}: {error}"
                    )
                })?;

                Ok(RetriggerOutboxRow {
                    id,
                    payload_json,
                    attempt_count,
                })
            })
            .collect()
    }

    /// Mark a retrigger outbox row as delivered.
    pub async fn mark_retrigger_outbox_delivered(
        &self,
        outbox_id: &str,
    ) -> crate::error::Result<()> {
        sqlx::query(
            "UPDATE channel_retrigger_outbox \
             SET delivered_at = CURRENT_TIMESTAMP, \
                 last_error = NULL \
             WHERE id = ? AND delivered_at IS NULL",
        )
        .bind(outbox_id)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    /// Lease a retrigger outbox row while a synthetic retrigger is in-flight.
    pub async fn lease_retrigger_outbox_delivery(
        &self,
        outbox_id: &str,
        lease_secs: u64,
    ) -> crate::error::Result<bool> {
        let lease_modifier = format!("+{} seconds", lease_secs.max(1));
        let result = sqlx::query(
            "UPDATE channel_retrigger_outbox \
             SET next_attempt_at = datetime(CURRENT_TIMESTAMP, ?), \
                 last_error = NULL \
             WHERE id = ? \
               AND delivered_at IS NULL \
               AND datetime(next_attempt_at) <= datetime(CURRENT_TIMESTAMP)",
        )
        .bind(lease_modifier)
        .bind(outbox_id)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        Ok(result.rows_affected() == 1)
    }

    /// Record a failed delivery attempt and schedule retry with backoff.
    pub async fn mark_retrigger_outbox_retry(
        &self,
        outbox_id: &str,
        error: &str,
    ) -> crate::error::Result<()> {
        let current_attempt_count: i64 = match sqlx::query(
            "SELECT attempt_count \
             FROM channel_retrigger_outbox \
             WHERE id = ? AND delivered_at IS NULL",
        )
        .bind(outbox_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?
        {
            Some(row) => row.try_get("attempt_count").map_err(|error| {
                anyhow::anyhow!(
                    "failed to decode retrigger outbox attempt_count for row {outbox_id}: {error}"
                )
            })?,
            None => return Ok(()),
        };

        let next_attempt_count = current_attempt_count.saturating_add(1);
        let retry_delay_secs = retrigger_outbox_retry_delay_secs(next_attempt_count);
        let retry_modifier = format!("+{retry_delay_secs} seconds");

        sqlx::query(
            "UPDATE channel_retrigger_outbox \
             SET attempt_count = ?, \
                 last_error = ?, \
                 next_attempt_at = datetime(CURRENT_TIMESTAMP, ?) \
             WHERE id = ? AND delivered_at IS NULL",
        )
        .bind(next_attempt_count)
        .bind(error)
        .bind(retry_modifier)
        .bind(outbox_id)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    /// Best-effort cleanup for delivered outbox rows.
    pub async fn prune_delivered_retrigger_outbox(&self, limit: i64) -> crate::error::Result<()> {
        sqlx::query(
            "DELETE FROM channel_retrigger_outbox \
             WHERE id IN ( \
                 SELECT id \
                 FROM channel_retrigger_outbox \
                 WHERE delivered_at IS NOT NULL \
                 ORDER BY delivered_at ASC \
                 LIMIT ? \
             )",
        )
        .bind(limit)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
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

fn retrigger_outbox_retry_delay_secs(attempt_count: i64) -> u64 {
    match attempt_count {
        i if i <= 1 => 1,
        2 => 5,
        3 => 30,
        4 => 120,
        _ => 600,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessRunLogger, retrigger_outbox_retry_delay_secs};
    use sqlx::{Row, SqlitePool};
    use std::sync::Arc;
    use std::time::Duration;

    async fn create_outbox_schema(pool: &SqlitePool) {
        sqlx::query(
            "CREATE TABLE channel_retrigger_outbox (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                result_payload TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                last_error TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                delivered_at TIMESTAMP
            )",
        )
        .execute(pool)
        .await
        .expect("outbox schema should create");
    }

    async fn read_outbox_attempt_and_next_attempt_at(
        pool: &SqlitePool,
        outbox_id: &str,
    ) -> (i64, String) {
        let row = sqlx::query(
            "SELECT attempt_count, datetime(next_attempt_at) AS next_attempt_at
             FROM channel_retrigger_outbox
             WHERE id = ?",
        )
        .bind(outbox_id)
        .fetch_one(pool)
        .await
        .expect("outbox row should exist");

        let attempt_count: i64 = row
            .try_get("attempt_count")
            .expect("attempt_count should decode");
        let next_attempt_at: String = row
            .try_get("next_attempt_at")
            .expect("next_attempt_at should decode");
        (attempt_count, next_attempt_at)
    }

    #[test]
    fn retrigger_outbox_retry_delay_caps_at_ten_minutes() {
        assert_eq!(retrigger_outbox_retry_delay_secs(0), 1);
        assert_eq!(retrigger_outbox_retry_delay_secs(1), 1);
        assert_eq!(retrigger_outbox_retry_delay_secs(2), 5);
        assert_eq!(retrigger_outbox_retry_delay_secs(3), 30);
        assert_eq!(retrigger_outbox_retry_delay_secs(4), 120);
        assert_eq!(retrigger_outbox_retry_delay_secs(5), 600);
        assert_eq!(retrigger_outbox_retry_delay_secs(50), 600);
    }

    #[tokio::test]
    async fn list_due_retrigger_outbox_fails_on_attempt_count_decode_error() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        create_outbox_schema(&pool).await;

        sqlx::query(
            "INSERT INTO channel_retrigger_outbox
                (id, agent_id, channel_id, result_payload, attempt_count, next_attempt_at)
             VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind("row-1")
        .bind("agent-1")
        .bind("channel-1")
        .bind("{}")
        .bind("not-an-integer")
        .execute(&pool)
        .await
        .expect("malformed row should insert in sqlite dynamic typing mode");

        let logger = ProcessRunLogger::new(pool);
        let agent_id: crate::AgentId = Arc::<str>::from("agent-1");
        let channel_id: crate::ChannelId = Arc::<str>::from("channel-1");

        let error = logger
            .list_due_retrigger_outbox(&agent_id, &channel_id, 10)
            .await
            .expect_err("decode error should not be silently defaulted");

        assert!(
            error
                .to_string()
                .contains("failed to decode retrigger outbox attempt_count"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn lease_retrigger_outbox_delivery_temporarily_hides_due_row() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        create_outbox_schema(&pool).await;

        sqlx::query(
            "INSERT INTO channel_retrigger_outbox
                (id, agent_id, channel_id, result_payload, attempt_count, next_attempt_at)
             VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind("row-lease")
        .bind("agent-1")
        .bind("channel-1")
        .bind("{\"results\":[]}")
        .bind(0)
        .execute(&pool)
        .await
        .expect("outbox row should insert");

        let logger = ProcessRunLogger::new(pool);
        let agent_id: crate::AgentId = Arc::<str>::from("agent-1");
        let channel_id: crate::ChannelId = Arc::<str>::from("channel-1");

        let due_before = logger
            .list_due_retrigger_outbox(&agent_id, &channel_id, 10)
            .await
            .expect("list_due_retrigger_outbox should succeed before lease");
        assert_eq!(due_before.len(), 1);

        let first_lease = logger
            .lease_retrigger_outbox_delivery("row-lease", 60)
            .await
            .expect("leasing outbox row should succeed");
        assert!(first_lease, "first lease attempt should claim the row");

        let second_lease = logger
            .lease_retrigger_outbox_delivery("row-lease", 60)
            .await
            .expect("second lease attempt should return false, not error");
        assert!(
            !second_lease,
            "already-leased row should not be claimed by another consumer"
        );

        let due_after = logger
            .list_due_retrigger_outbox(&agent_id, &channel_id, 10)
            .await
            .expect("list_due_retrigger_outbox should succeed after lease");
        assert!(
            due_after.is_empty(),
            "leased row should not be replay-eligible immediately"
        );
    }

    #[tokio::test]
    async fn lease_miss_does_not_increment_attempt_count_until_retry_marked() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        create_outbox_schema(&pool).await;

        sqlx::query(
            "INSERT INTO channel_retrigger_outbox
                (id, agent_id, channel_id, result_payload, attempt_count, next_attempt_at)
             VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind("row-e2e")
        .bind("agent-1")
        .bind("channel-1")
        .bind("{\"results\":[]}")
        .bind(0)
        .execute(&pool)
        .await
        .expect("outbox row should insert");

        let logger = ProcessRunLogger::new(pool.clone());

        let first_lease = logger
            .lease_retrigger_outbox_delivery("row-e2e", 1)
            .await
            .expect("first lease should succeed");
        assert!(first_lease, "first lease should claim due row");

        let (attempt_count_after_first_lease, leased_next_attempt_at) =
            read_outbox_attempt_and_next_attempt_at(&pool, "row-e2e").await;
        assert_eq!(attempt_count_after_first_lease, 0);

        // Simulate repeated channel flush ticks while the row is still leased/not due.
        for _ in 0..3 {
            let leased = logger
                .lease_retrigger_outbox_delivery("row-e2e", 1)
                .await
                .expect("lease miss should be a non-error false");
            assert!(
                !leased,
                "lease should not be reacquired until next_attempt_at is due"
            );
        }

        let (attempt_count_after_lease_misses, next_attempt_after_lease_misses) =
            read_outbox_attempt_and_next_attempt_at(&pool, "row-e2e").await;
        assert_eq!(
            attempt_count_after_lease_misses, 0,
            "lease misses must not inflate attempt_count/backoff"
        );
        assert_eq!(
            next_attempt_after_lease_misses, leased_next_attempt_at,
            "lease misses must not push next_attempt_at farther into the future"
        );

        // Wait until the 1-second lease expires, then verify row can be claimed again.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let lease_after_expiry = logger
            .lease_retrigger_outbox_delivery("row-e2e", 1)
            .await
            .expect("lease should succeed after previous lease expires");
        assert!(lease_after_expiry);

        // A real retry-worthy failure should increment attempt_count.
        logger
            .mark_retrigger_outbox_retry("row-e2e", "synthetic send failure")
            .await
            .expect("retry marker should succeed");
        let (attempt_count_after_retry, _) =
            read_outbox_attempt_and_next_attempt_at(&pool, "row-e2e").await;
        assert_eq!(attempt_count_after_retry, 1);
    }

    #[tokio::test]
    async fn mark_retrigger_outbox_retry_fails_on_attempt_count_decode_error() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        create_outbox_schema(&pool).await;

        sqlx::query(
            "INSERT INTO channel_retrigger_outbox
                (id, agent_id, channel_id, result_payload, attempt_count, next_attempt_at)
             VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind("row-bad-retry")
        .bind("agent-1")
        .bind("channel-1")
        .bind("{}")
        .bind("bad-int")
        .execute(&pool)
        .await
        .expect("malformed row should insert in sqlite dynamic typing mode");

        let logger = ProcessRunLogger::new(pool);
        let error = logger
            .mark_retrigger_outbox_retry("row-bad-retry", "test failure")
            .await
            .expect_err("decode error should not be silently defaulted");

        assert!(
            error
                .to_string()
                .contains("failed to decode retrigger outbox attempt_count"),
            "unexpected error: {error}"
        );
    }
}
