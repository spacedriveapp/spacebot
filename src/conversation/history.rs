//! Conversation message persistence (SQLite).

use crate::{BranchId, ChannelId, WorkerId};

use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};

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

const WORKER_TERMINAL_RECEIPT_KIND: &str = "worker_terminal";
const WORKER_RECEIPT_MAX_ATTEMPTS: i64 = 6;
const WORKER_RECEIPT_BACKOFF_SECS: [i64; 5] = [5, 15, 45, 120, 300];
const WORKER_RECEIPT_RETENTION_DAYS: i64 = 30;
const WORKER_CONTRACT_STATE_CREATED: &str = "created";
const WORKER_CONTRACT_STATE_ACKED: &str = "acked";
const WORKER_CONTRACT_STATE_PROGRESSING: &str = "progressing";
const WORKER_CONTRACT_STATE_SLA_MISSED: &str = "sla_missed";
const WORKER_CONTRACT_STATE_TERMINAL_PENDING: &str = "terminal_pending";
const WORKER_CONTRACT_STATE_TERMINAL_ACKED: &str = "terminal_acked";
const WORKER_CONTRACT_STATE_TERMINAL_FAILED: &str = "terminal_failed";

fn worker_receipt_backoff_secs(attempt_count: i64) -> Option<i64> {
    if attempt_count <= 0 {
        return WORKER_RECEIPT_BACKOFF_SECS.first().copied();
    }
    WORKER_RECEIPT_BACKOFF_SECS
        .get((attempt_count - 1) as usize)
        .copied()
}

fn status_fingerprint(status: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    status.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Debug, Clone)]
pub struct PendingWorkerDeliveryReceipt {
    pub id: String,
    pub worker_id: String,
    pub channel_id: String,
    pub terminal_state: String,
    pub payload_text: String,
    pub attempt_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerDeliveryReceiptStats {
    pub pending: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerDeliveryRetryOutcome {
    pub status: String,
    pub attempt_count: i64,
    pub next_attempt_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DueWorkerTaskContractAck {
    pub worker_id: WorkerId,
    pub task_summary: String,
    pub attempt_count: i64,
}

#[derive(Debug, Clone)]
pub struct DueWorkerTaskContractProgress {
    pub worker_id: WorkerId,
    pub task_summary: String,
}

#[derive(Debug, Clone)]
pub struct DueWorkerTaskContractTerminal {
    pub worker_id: WorkerId,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkerTaskContractTiming {
    pub ack_secs: u64,
    pub progress_secs: u64,
    pub terminal_secs: u64,
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

    fn log_worker_event_with_context(
        &self,
        worker_id: String,
        channel_id: Option<String>,
        agent_id: Option<String>,
        event_type: String,
        payload_json: Option<String>,
    ) {
        let pool = self.pool.clone();

        tokio::spawn(async move {
            let event_id = uuid::Uuid::new_v4().to_string();
            if let Err(error) = sqlx::query(
                "INSERT INTO worker_events \
                 (id, worker_id, channel_id, agent_id, event_type, payload_json) \
                 VALUES ( \
                     ?, \
                     ?, \
                     COALESCE(?, (SELECT channel_id FROM worker_runs WHERE id = ?)), \
                     COALESCE(?, (SELECT agent_id FROM worker_runs WHERE id = ?)), \
                     ?, \
                     ? \
                 )",
            )
            .bind(&event_id)
            .bind(&worker_id)
            .bind(&channel_id)
            .bind(&worker_id)
            .bind(&agent_id)
            .bind(&worker_id)
            .bind(&event_type)
            .bind(&payload_json)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    %error,
                    worker_id = %worker_id,
                    event_type = %event_type,
                    "failed to persist worker event"
                );
            }
        });
    }

    /// Record a worker lifecycle event. Fire-and-forget.
    pub fn log_worker_event(
        &self,
        worker_id: WorkerId,
        event_type: &str,
        payload: serde_json::Value,
    ) {
        self.log_worker_event_with_context(
            worker_id.to_string(),
            None,
            None,
            event_type.to_string(),
            Some(payload.to_string()),
        );
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
                return;
            }

            let payload_json = serde_json::json!({
                "task": task,
                "worker_type": worker_type,
            })
            .to_string();
            let event_id = uuid::Uuid::new_v4().to_string();

            if let Err(error) = sqlx::query(
                "INSERT INTO worker_events \
                 (id, worker_id, channel_id, agent_id, event_type, payload_json) \
                 VALUES (?, ?, ?, ?, 'started', ?)",
            )
            .bind(&event_id)
            .bind(&id)
            .bind(&channel_id)
            .bind(&agent_id)
            .bind(&payload_json)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker start event");
            }
        });
    }

    /// Update a worker's status. Fire-and-forget.
    /// Worker status text is kept in a separate append-only event table so the
    /// worker_runs status enum remains queryable (`running`, `done`, etc.).
    pub fn log_worker_status(&self, worker_id: WorkerId, status: &str) {
        self.log_worker_event(
            worker_id,
            "status",
            serde_json::json!({
                "status": status,
            }),
        );
    }

    /// Record a worker completing with its result. Fire-and-forget.
    pub fn log_worker_completed(&self, worker_id: WorkerId, result: &str, success: bool) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let result = result.to_string();
        let success_int = if success { 1_i64 } else { 0_i64 };

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE worker_runs \
                 SET result = ?, \
                     status = CASE \
                         WHEN status IN ('cancelled', 'failed', 'timed_out') THEN status \
                         WHEN ? LIKE 'Worker cancelled:%' THEN 'cancelled' \
                         WHEN ? LIKE 'Worker failed:%' THEN 'failed' \
                         WHEN ? LIKE 'Worker timed out after %' THEN 'timed_out' \
                         WHEN ? = 1 THEN 'done' \
                         ELSE 'failed' \
                     END, \
                     completed_at = CURRENT_TIMESTAMP \
                 WHERE id = ?",
            )
            .bind(&result)
            .bind(&result)
            .bind(&result)
            .bind(&result)
            .bind(success_int)
            .bind(&id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker completion");
            }

            let payload_json = serde_json::json!({
                "result": result,
                "success": success,
            })
            .to_string();
            let event_id = uuid::Uuid::new_v4().to_string();

            if let Err(error) = sqlx::query(
                "INSERT INTO worker_events \
                 (id, worker_id, channel_id, agent_id, event_type, payload_json) \
                 VALUES ( \
                     ?, \
                     ?, \
                     (SELECT channel_id FROM worker_runs WHERE id = ?), \
                     (SELECT agent_id FROM worker_runs WHERE id = ?), \
                     'completed', \
                     ? \
                 )",
            )
            .bind(&event_id)
            .bind(&id)
            .bind(&id)
            .bind(&id)
            .bind(&payload_json)
            .execute(&pool)
            .await
            {
                tracing::warn!(
                    %error,
                    worker_id = %id,
                    "failed to persist worker completion event"
                );
            }
        });
    }

    /// Create or refresh the deterministic task contract for a worker.
    pub async fn upsert_worker_task_contract(
        &self,
        agent_id: &crate::AgentId,
        channel_id: &ChannelId,
        worker_id: WorkerId,
        task_summary: &str,
        timing: WorkerTaskContractTiming,
    ) -> crate::error::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let worker_id = worker_id.to_string();
        let channel_id = channel_id.to_string();
        let status_hash = status_fingerprint(task_summary);

        sqlx::query(
            "INSERT OR IGNORE INTO worker_task_contracts \
             (id, agent_id, channel_id, worker_id, task_summary, state, \
              ack_deadline_at, progress_deadline_at, terminal_deadline_at, \
              last_status_hash, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, \
                     datetime('now', '+' || ? || ' seconds'), \
                     datetime('now', '+' || ? || ' seconds'), \
                     datetime('now', '+' || ? || ' seconds'), \
                     ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&id)
        .bind(agent_id.as_ref())
        .bind(&channel_id)
        .bind(&worker_id)
        .bind(task_summary)
        .bind(WORKER_CONTRACT_STATE_CREATED)
        .bind(timing.ack_secs as i64)
        .bind(timing.progress_secs as i64)
        .bind(timing.terminal_secs as i64)
        .bind(&status_hash)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        sqlx::query(
            "UPDATE worker_task_contracts \
             SET task_summary = ?, \
                 state = ?, \
                 ack_deadline_at = datetime('now', '+' || ? || ' seconds'), \
                 progress_deadline_at = datetime('now', '+' || ? || ' seconds'), \
                 terminal_deadline_at = datetime('now', '+' || ? || ' seconds'), \
                 last_status_hash = ?, \
                 sla_nudge_sent = 0, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE worker_id = ? \
               AND state NOT IN (?, ?)",
        )
        .bind(task_summary)
        .bind(WORKER_CONTRACT_STATE_CREATED)
        .bind(timing.ack_secs as i64)
        .bind(timing.progress_secs as i64)
        .bind(timing.terminal_secs as i64)
        .bind(&status_hash)
        .bind(&worker_id)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_ACKED)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(())
    }

    /// Mark that a user-visible acknowledgement has been delivered for a worker.
    pub async fn mark_worker_task_contract_acknowledged(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<()> {
        sqlx::query(
            "UPDATE worker_task_contracts \
             SET state = CASE \
                     WHEN state IN (?, ?, ?) THEN state \
                     ELSE ? \
                 END, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE worker_id = ?",
        )
        .bind(WORKER_CONTRACT_STATE_TERMINAL_PENDING)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_ACKED)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
        .bind(WORKER_CONTRACT_STATE_ACKED)
        .bind(worker_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(())
    }

    /// Refresh progress heartbeat information for a worker contract.
    pub async fn touch_worker_task_contract_progress(
        &self,
        worker_id: WorkerId,
        status: Option<&str>,
        progress_secs: u64,
    ) -> crate::error::Result<()> {
        let status_hash = status.map(status_fingerprint);

        sqlx::query(
            "UPDATE worker_task_contracts \
             SET state = CASE \
                     WHEN state IN (?, ?, ?, ?) THEN ? \
                     ELSE state \
                 END, \
                 last_progress_at = CURRENT_TIMESTAMP, \
                 progress_deadline_at = datetime('now', '+' || ? || ' seconds'), \
                 last_status_hash = COALESCE(?, last_status_hash), \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE worker_id = ? \
               AND state NOT IN (?, ?)",
        )
        .bind(WORKER_CONTRACT_STATE_CREATED)
        .bind(WORKER_CONTRACT_STATE_ACKED)
        .bind(WORKER_CONTRACT_STATE_PROGRESSING)
        .bind(WORKER_CONTRACT_STATE_SLA_MISSED)
        .bind(WORKER_CONTRACT_STATE_PROGRESSING)
        .bind(progress_secs as i64)
        .bind(status_hash)
        .bind(worker_id.to_string())
        .bind(WORKER_CONTRACT_STATE_TERMINAL_ACKED)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(())
    }

    /// Mark a worker contract as terminal pending while delivery receipts are in-flight.
    pub async fn mark_worker_task_contract_terminal_pending(
        &self,
        worker_id: WorkerId,
        terminal_state: &str,
        terminal_secs: u64,
    ) -> crate::error::Result<()> {
        sqlx::query(
            "UPDATE worker_task_contracts \
             SET state = ?, \
                 terminal_state = ?, \
                 terminal_deadline_at = datetime('now', '+' || ? || ' seconds'), \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE worker_id = ? \
               AND state NOT IN (?, ?)",
        )
        .bind(WORKER_CONTRACT_STATE_TERMINAL_PENDING)
        .bind(terminal_state)
        .bind(terminal_secs as i64)
        .bind(worker_id.to_string())
        .bind(WORKER_CONTRACT_STATE_TERMINAL_ACKED)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(())
    }

    /// Claim workers whose acknowledgement deadline has expired.
    pub async fn claim_due_worker_task_contract_ack_deadlines(
        &self,
        channel_id: &ChannelId,
        limit: i64,
        retry_secs: u64,
    ) -> crate::error::Result<Vec<DueWorkerTaskContractAck>> {
        let channel_id = channel_id.to_string();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let rows = sqlx::query(
            "SELECT id, worker_id, task_summary, attempt_count \
             FROM worker_task_contracts \
             WHERE channel_id = ? \
               AND state = ? \
               AND ack_deadline_at <= CURRENT_TIMESTAMP \
             ORDER BY ack_deadline_at ASC, created_at ASC \
             LIMIT ?",
        )
        .bind(&channel_id)
        .bind(WORKER_CONTRACT_STATE_CREATED)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut due = Vec::with_capacity(rows.len());
        for row in rows {
            let contract_id: String = row.try_get("id").unwrap_or_default();
            let worker_id_raw: String = row.try_get("worker_id").unwrap_or_default();
            let task_summary: String = row.try_get("task_summary").unwrap_or_default();
            let attempt_count: i64 = row.try_get("attempt_count").unwrap_or_default();

            let updated = sqlx::query(
                "UPDATE worker_task_contracts \
                 SET ack_deadline_at = datetime('now', '+' || ? || ' seconds'), \
                     attempt_count = attempt_count + 1, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ? \
                   AND state = ? \
                   AND ack_deadline_at <= CURRENT_TIMESTAMP",
            )
            .bind(retry_secs as i64)
            .bind(&contract_id)
            .bind(WORKER_CONTRACT_STATE_CREATED)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .rows_affected();

            if updated == 0 {
                continue;
            }

            match uuid::Uuid::parse_str(&worker_id_raw) {
                Ok(worker_id) => due.push(DueWorkerTaskContractAck {
                    worker_id,
                    task_summary,
                    attempt_count: attempt_count + 1,
                }),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        worker_id = %worker_id_raw,
                        "skipping malformed worker task contract id"
                    );
                }
            }
        }

        tx.commit().await.map_err(|error| anyhow::anyhow!(error))?;
        Ok(due)
    }

    /// Claim workers whose progress deadline has expired and have not been nudged yet.
    pub async fn claim_due_worker_task_contract_progress_deadlines(
        &self,
        channel_id: &ChannelId,
        limit: i64,
    ) -> crate::error::Result<Vec<DueWorkerTaskContractProgress>> {
        let channel_id = channel_id.to_string();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let rows = sqlx::query(
            "SELECT id, worker_id, task_summary \
             FROM worker_task_contracts \
             WHERE channel_id = ? \
               AND state IN (?, ?, ?) \
               AND sla_nudge_sent = 0 \
               AND progress_deadline_at <= CURRENT_TIMESTAMP \
             ORDER BY progress_deadline_at ASC, created_at ASC \
             LIMIT ?",
        )
        .bind(&channel_id)
        .bind(WORKER_CONTRACT_STATE_CREATED)
        .bind(WORKER_CONTRACT_STATE_ACKED)
        .bind(WORKER_CONTRACT_STATE_PROGRESSING)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut due = Vec::with_capacity(rows.len());
        for row in rows {
            let contract_id: String = row.try_get("id").unwrap_or_default();
            let worker_id_raw: String = row.try_get("worker_id").unwrap_or_default();
            let task_summary: String = row.try_get("task_summary").unwrap_or_default();

            let updated = sqlx::query(
                "UPDATE worker_task_contracts \
                 SET state = ?, \
                     sla_nudge_sent = 1, \
                     attempt_count = attempt_count + 1, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ? \
                   AND state IN (?, ?, ?) \
                   AND sla_nudge_sent = 0 \
                   AND progress_deadline_at <= CURRENT_TIMESTAMP",
            )
            .bind(WORKER_CONTRACT_STATE_SLA_MISSED)
            .bind(&contract_id)
            .bind(WORKER_CONTRACT_STATE_CREATED)
            .bind(WORKER_CONTRACT_STATE_ACKED)
            .bind(WORKER_CONTRACT_STATE_PROGRESSING)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .rows_affected();

            if updated == 0 {
                continue;
            }

            match uuid::Uuid::parse_str(&worker_id_raw) {
                Ok(worker_id) => due.push(DueWorkerTaskContractProgress {
                    worker_id,
                    task_summary,
                }),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        worker_id = %worker_id_raw,
                        "skipping malformed worker task contract id"
                    );
                }
            }
        }

        tx.commit().await.map_err(|error| anyhow::anyhow!(error))?;
        Ok(due)
    }

    /// Claim terminal-pending contracts whose delivery window elapsed.
    ///
    /// Overdue contracts are transitioned to `terminal_failed` and any pending
    /// terminal delivery receipts are marked `failed` to stop retry churn.
    pub async fn claim_due_worker_task_contract_terminal_deadlines(
        &self,
        channel_id: &ChannelId,
        limit: i64,
    ) -> crate::error::Result<Vec<DueWorkerTaskContractTerminal>> {
        let channel_id = channel_id.to_string();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let rows = sqlx::query(
            "SELECT id, worker_id \
             FROM worker_task_contracts \
             WHERE channel_id = ? \
               AND state = ? \
               AND terminal_deadline_at <= CURRENT_TIMESTAMP \
             ORDER BY terminal_deadline_at ASC, created_at ASC \
             LIMIT ?",
        )
        .bind(&channel_id)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_PENDING)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut due = Vec::with_capacity(rows.len());
        for row in rows {
            let contract_id: String = row.try_get("id").unwrap_or_default();
            let worker_id_raw: String = row.try_get("worker_id").unwrap_or_default();

            let updated = sqlx::query(
                "UPDATE worker_task_contracts \
                 SET state = ?, \
                     terminal_state = COALESCE(terminal_state, 'failed'), \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ? \
                   AND state = ? \
                   AND terminal_deadline_at <= CURRENT_TIMESTAMP",
            )
            .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
            .bind(&contract_id)
            .bind(WORKER_CONTRACT_STATE_TERMINAL_PENDING)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .rows_affected();

            if updated == 0 {
                continue;
            }

            sqlx::query(
                "UPDATE worker_delivery_receipts \
                 SET status = 'failed', \
                     last_error = ?, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE worker_id = ? \
                   AND kind = ? \
                   AND status IN ('pending', 'sending')",
            )
            .bind("terminal deadline elapsed before adapter acknowledgement")
            .bind(&worker_id_raw)
            .bind(WORKER_TERMINAL_RECEIPT_KIND)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

            match uuid::Uuid::parse_str(&worker_id_raw) {
                Ok(worker_id) => due.push(DueWorkerTaskContractTerminal { worker_id }),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        worker_id = %worker_id_raw,
                        "skipping malformed worker task contract id"
                    );
                }
            }
        }

        tx.commit().await.map_err(|error| anyhow::anyhow!(error))?;
        Ok(due)
    }

    /// Create (or refresh) the durable terminal delivery receipt for a worker.
    ///
    /// One terminal receipt exists per worker (`kind = worker_terminal`). If the
    /// receipt already exists and is not acked, it is reset to pending so it can
    /// be retried.
    pub async fn upsert_worker_terminal_receipt(
        &self,
        channel_id: &ChannelId,
        worker_id: WorkerId,
        terminal_state: &str,
        payload_text: &str,
    ) -> crate::error::Result<String> {
        let worker_id = worker_id.to_string();
        let channel_id = channel_id.to_string();

        let existing = sqlx::query(
            "SELECT id, status \
             FROM worker_delivery_receipts \
             WHERE worker_id = ? AND kind = ?",
        )
        .bind(&worker_id)
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        if let Some(row) = existing {
            let receipt_id: String = row.try_get("id").unwrap_or_default();
            let status: String = row.try_get("status").unwrap_or_default();

            if status != "acked" {
                sqlx::query(
                    "UPDATE worker_delivery_receipts \
                     SET channel_id = ?, \
                         terminal_state = ?, \
                         payload_text = ?, \
                         status = 'pending', \
                         last_error = NULL, \
                         next_attempt_at = CURRENT_TIMESTAMP, \
                         updated_at = CURRENT_TIMESTAMP \
                     WHERE id = ?",
                )
                .bind(&channel_id)
                .bind(terminal_state)
                .bind(payload_text)
                .bind(&receipt_id)
                .execute(&self.pool)
                .await
                .map_err(|error| anyhow::anyhow!(error))?;
            }

            return Ok(receipt_id);
        }

        let receipt_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO worker_delivery_receipts \
             (id, worker_id, channel_id, kind, status, terminal_state, payload_text, next_attempt_at) \
             VALUES (?, ?, ?, ?, 'pending', ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&receipt_id)
        .bind(&worker_id)
        .bind(&channel_id)
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind(terminal_state)
        .bind(payload_text)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(receipt_id)
    }

    /// Claim due pending terminal receipts for delivery.
    ///
    /// Claimed receipts are transitioned to `sending` so we can distinguish in-flight
    /// deliveries from queued retries.
    pub async fn claim_due_worker_terminal_receipts(
        &self,
        channel_id: &ChannelId,
        limit: i64,
    ) -> crate::error::Result<Vec<PendingWorkerDeliveryReceipt>> {
        let channel_id = channel_id.to_string();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let rows = sqlx::query(
            "SELECT id, worker_id, channel_id, terminal_state, payload_text, attempt_count \
             FROM worker_delivery_receipts \
             WHERE channel_id = ? \
               AND kind = ? \
               AND status = 'pending' \
               AND next_attempt_at <= CURRENT_TIMESTAMP \
             ORDER BY next_attempt_at ASC, created_at ASC \
             LIMIT ?",
        )
        .bind(&channel_id)
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut claimed = Vec::with_capacity(rows.len());
        for row in rows {
            let receipt_id: String = row.try_get("id").unwrap_or_default();
            let updated = sqlx::query(
                "UPDATE worker_delivery_receipts \
                 SET status = 'sending', updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ? AND status = 'pending'",
            )
            .bind(&receipt_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .rows_affected();

            if updated == 0 {
                continue;
            }

            claimed.push(PendingWorkerDeliveryReceipt {
                id: receipt_id,
                worker_id: row.try_get("worker_id").unwrap_or_default(),
                channel_id: row.try_get("channel_id").unwrap_or_default(),
                terminal_state: row.try_get("terminal_state").unwrap_or_default(),
                payload_text: row.try_get("payload_text").unwrap_or_default(),
                attempt_count: row.try_get("attempt_count").unwrap_or_default(),
            });
        }

        tx.commit().await.map_err(|error| anyhow::anyhow!(error))?;
        Ok(claimed)
    }

    /// Claim due pending terminal receipts across all channels.
    ///
    /// Used by the global receipt dispatcher to drain terminal notices even
    /// when no channel loop is currently active.
    pub async fn claim_due_worker_terminal_receipts_any(
        &self,
        limit: i64,
    ) -> crate::error::Result<Vec<PendingWorkerDeliveryReceipt>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let rows = sqlx::query(
            "SELECT id, worker_id, channel_id, terminal_state, payload_text, attempt_count \
             FROM worker_delivery_receipts \
             WHERE kind = ? \
               AND status = 'pending' \
               AND next_attempt_at <= CURRENT_TIMESTAMP \
             ORDER BY next_attempt_at ASC, created_at ASC \
             LIMIT ?",
        )
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut claimed = Vec::with_capacity(rows.len());
        for row in rows {
            let receipt_id: String = row.try_get("id").unwrap_or_default();
            let updated = sqlx::query(
                "UPDATE worker_delivery_receipts \
                 SET status = 'sending', updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ? AND status = 'pending'",
            )
            .bind(&receipt_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .rows_affected();

            if updated == 0 {
                continue;
            }

            claimed.push(PendingWorkerDeliveryReceipt {
                id: receipt_id,
                worker_id: row.try_get("worker_id").unwrap_or_default(),
                channel_id: row.try_get("channel_id").unwrap_or_default(),
                terminal_state: row.try_get("terminal_state").unwrap_or_default(),
                payload_text: row.try_get("payload_text").unwrap_or_default(),
                attempt_count: row.try_get("attempt_count").unwrap_or_default(),
            });
        }

        tx.commit().await.map_err(|error| anyhow::anyhow!(error))?;
        Ok(claimed)
    }

    /// Mark a terminal receipt as delivered.
    ///
    /// Returns true if this call transitioned the row to acked.
    pub async fn ack_worker_delivery_receipt(
        &self,
        receipt_id: &str,
    ) -> crate::error::Result<bool> {
        let updated = sqlx::query(
            "UPDATE worker_delivery_receipts \
             SET status = 'acked', \
                 acked_at = CURRENT_TIMESTAMP, \
                 updated_at = CURRENT_TIMESTAMP, \
                 last_error = NULL \
             WHERE id = ? AND status != 'acked'",
        )
        .bind(receipt_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?
        .rows_affected();

        if updated > 0 {
            sqlx::query(
                "UPDATE worker_task_contracts \
                 SET state = ?, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE worker_id = (
                     SELECT worker_id FROM worker_delivery_receipts WHERE id = ?
                 ) \
                   AND state IN (?, ?, ?, ?, ?)",
            )
            .bind(WORKER_CONTRACT_STATE_TERMINAL_ACKED)
            .bind(receipt_id)
            .bind(WORKER_CONTRACT_STATE_CREATED)
            .bind(WORKER_CONTRACT_STATE_ACKED)
            .bind(WORKER_CONTRACT_STATE_PROGRESSING)
            .bind(WORKER_CONTRACT_STATE_SLA_MISSED)
            .bind(WORKER_CONTRACT_STATE_TERMINAL_PENDING)
            .execute(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        }

        Ok(updated > 0)
    }

    /// Record a delivery failure and schedule the next retry (or terminal failure).
    pub async fn fail_worker_delivery_receipt_attempt(
        &self,
        receipt_id: &str,
        error: &str,
    ) -> crate::error::Result<WorkerDeliveryRetryOutcome> {
        let row = sqlx::query(
            "SELECT status, attempt_count \
             FROM worker_delivery_receipts \
             WHERE id = ?",
        )
        .bind(receipt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|db_error| anyhow::anyhow!(db_error))?
        .ok_or_else(|| anyhow::anyhow!("worker delivery receipt not found: {receipt_id}"))?;

        let current_status: String = row.try_get("status").unwrap_or_default();
        let current_attempts: i64 = row.try_get("attempt_count").unwrap_or_default();

        if current_status == "acked" {
            return Ok(WorkerDeliveryRetryOutcome {
                status: "acked".to_string(),
                attempt_count: current_attempts,
                next_attempt_at: None,
            });
        }

        let attempt_count = current_attempts + 1;
        if attempt_count >= WORKER_RECEIPT_MAX_ATTEMPTS {
            sqlx::query(
                "UPDATE worker_delivery_receipts \
                 SET status = 'failed', \
                     attempt_count = ?, \
                     last_error = ?, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ?",
            )
            .bind(attempt_count)
            .bind(error)
            .bind(receipt_id)
            .execute(&self.pool)
            .await
            .map_err(|db_error| anyhow::anyhow!(db_error))?;

            sqlx::query(
                "UPDATE worker_task_contracts \
                 SET state = ?, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE worker_id = (
                     SELECT worker_id FROM worker_delivery_receipts WHERE id = ?
                 ) \
                   AND state IN (?, ?, ?, ?, ?)",
            )
            .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
            .bind(receipt_id)
            .bind(WORKER_CONTRACT_STATE_CREATED)
            .bind(WORKER_CONTRACT_STATE_ACKED)
            .bind(WORKER_CONTRACT_STATE_PROGRESSING)
            .bind(WORKER_CONTRACT_STATE_SLA_MISSED)
            .bind(WORKER_CONTRACT_STATE_TERMINAL_PENDING)
            .execute(&self.pool)
            .await
            .map_err(|db_error| anyhow::anyhow!(db_error))?;

            return Ok(WorkerDeliveryRetryOutcome {
                status: "failed".to_string(),
                attempt_count,
                next_attempt_at: None,
            });
        }

        let delay_secs = worker_receipt_backoff_secs(attempt_count).unwrap_or(300);
        sqlx::query(
            "UPDATE worker_delivery_receipts \
             SET status = 'pending', \
                 attempt_count = ?, \
                 last_error = ?, \
                 next_attempt_at = datetime('now', '+' || ? || ' seconds'), \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?",
        )
        .bind(attempt_count)
        .bind(error)
        .bind(delay_secs)
        .bind(receipt_id)
        .execute(&self.pool)
        .await
        .map_err(|db_error| anyhow::anyhow!(db_error))?;

        let next_attempt_at = chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(delay_secs))
            .map(|timestamp| timestamp.to_rfc3339());

        Ok(WorkerDeliveryRetryOutcome {
            status: "pending".to_string(),
            attempt_count,
            next_attempt_at,
        })
    }

    /// Load worker delivery receipt stats for a channel.
    pub async fn load_worker_delivery_receipt_stats(
        &self,
        channel_id: &str,
    ) -> crate::error::Result<WorkerDeliveryReceiptStats> {
        let row = sqlx::query(
            "SELECT \
                SUM(CASE WHEN status IN ('pending', 'sending') THEN 1 ELSE 0 END) AS pending_count, \
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) AS failed_count \
             FROM worker_delivery_receipts \
             WHERE channel_id = ? \
               AND kind = ?",
        )
        .bind(channel_id)
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let pending = row.try_get::<i64, _>("pending_count").unwrap_or(0).max(0) as u64;
        let failed = row.try_get::<i64, _>("failed_count").unwrap_or(0).max(0) as u64;

        Ok(WorkerDeliveryReceiptStats { pending, failed })
    }

    /// Delete old terminal delivery receipts that are no longer actionable.
    ///
    /// Keeps `pending` and `sending` rows intact, and only removes terminal rows
    /// (`acked`, `failed`) older than the configured retention period.
    pub async fn prune_worker_delivery_receipts(&self) -> crate::error::Result<u64> {
        let deleted = sqlx::query(
            "DELETE FROM worker_delivery_receipts \
             WHERE status IN ('acked', 'failed') \
               AND julianday(updated_at) < julianday('now', '-' || ? || ' days')",
        )
        .bind(WORKER_RECEIPT_RETENTION_DAYS)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?
        .rows_affected();

        Ok(deleted)
    }

    /// Close orphaned branch and worker runs from a previous process lifetime.
    ///
    /// This is called on startup before channels begin handling messages. Any
    /// rows with NULL `completed_at` cannot be resumed and should be marked
    /// terminal so timelines and analytics stay accurate.
    pub async fn close_orphaned_runs(&self) -> crate::error::Result<(u64, u64, u64, u64)> {
        let worker_result = sqlx::query(
            "UPDATE worker_runs \
             SET status = 'failed', \
                 result = COALESCE(result, 'Worker interrupted by restart before completion.'), \
                 completed_at = CURRENT_TIMESTAMP \
             WHERE completed_at IS NULL",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let branch_result = sqlx::query(
            "UPDATE branch_runs \
             SET conclusion = COALESCE(conclusion, 'Branch interrupted by restart before completion.'), \
                 completed_at = CURRENT_TIMESTAMP \
             WHERE completed_at IS NULL",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let receipt_result = sqlx::query(
            "UPDATE worker_delivery_receipts \
             SET status = 'pending', \
                 next_attempt_at = CURRENT_TIMESTAMP, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE status = 'sending'",
        )
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let contract_result = sqlx::query(
            "UPDATE worker_task_contracts \
             SET state = ?, \
                 terminal_state = COALESCE(terminal_state, 'failed'), \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE state NOT IN (?, ?)",
        )
        .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_ACKED)
        .bind(WORKER_CONTRACT_STATE_TERMINAL_FAILED)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok((
            worker_result.rows_affected(),
            branch_result.rows_affected(),
            receipt_result.rows_affected(),
            contract_result.rows_affected(),
        ))
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
            "AND timestamp < ?3"
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

    /// List recent worker events for a worker, oldest first.
    pub async fn list_worker_events(
        &self,
        worker_id: &str,
        limit: i64,
    ) -> crate::error::Result<Vec<WorkerEventRow>> {
        let rows = sqlx::query(
            "SELECT id, worker_id, channel_id, agent_id, event_type, payload_json, created_at \
             FROM worker_events \
             WHERE worker_id = ? \
             ORDER BY created_at DESC \
             LIMIT ?",
        )
        .bind(worker_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut events = rows
            .into_iter()
            .map(|row| WorkerEventRow {
                id: row.try_get("id").unwrap_or_default(),
                worker_id: row.try_get("worker_id").unwrap_or_default(),
                channel_id: row.try_get("channel_id").ok(),
                agent_id: row.try_get("agent_id").ok(),
                event_type: row.try_get("event_type").unwrap_or_default(),
                payload_json: row.try_get("payload_json").ok(),
                created_at: row
                    .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();

        events.reverse();
        Ok(events)
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

/// A worker lifecycle event row.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerEventRow {
    pub id: String,
    pub worker_id: String,
    pub channel_id: Option<String>,
    pub agent_id: Option<String>,
    pub event_type: String,
    pub payload_json: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::sync::Arc;

    async fn connect_logger() -> ProcessRunLogger {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("in-memory SQLite");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        ProcessRunLogger::new(pool)
    }

    #[tokio::test]
    async fn worker_event_journal_records_lifecycle_updates() {
        let logger = connect_logger().await;
        let agent_id: crate::AgentId = Arc::from("agent:test");
        let worker_id = uuid::Uuid::new_v4();
        let worker_id_text = worker_id.to_string();

        logger.log_worker_started(None, worker_id, "research task", "builtin", &agent_id);

        let mut started_seen = false;
        for _ in 0..20 {
            let events = logger
                .list_worker_events(&worker_id_text, 20)
                .await
                .expect("list worker events");
            if events.iter().any(|event| event.event_type == "started") {
                started_seen = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(started_seen, "expected started event");

        logger.log_worker_status(worker_id, "searching source material");
        logger.log_worker_event(
            worker_id,
            "tool_started",
            serde_json::json!({ "tool_name": "web_search" }),
        );
        logger.log_worker_completed(worker_id, "done", true);

        let mut events = Vec::new();
        for _ in 0..20 {
            events = logger
                .list_worker_events(&worker_id_text, 20)
                .await
                .expect("list worker events");
            if events.iter().any(|event| event.event_type == "status")
                && events
                    .iter()
                    .any(|event| event.event_type == "tool_started")
                && events.iter().any(|event| event.event_type == "completed")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert!(
            events.iter().any(|event| event.event_type == "status"),
            "expected status event"
        );
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "tool_started"),
            "expected tool_started event"
        );
        assert!(
            events.iter().any(|event| event.event_type == "completed"),
            "expected completed event"
        );
    }

    #[tokio::test]
    async fn worker_terminal_receipt_claim_ack_and_stats() {
        let logger = connect_logger().await;
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "done",
                "Background task completed: finished indexing",
            )
            .await
            .expect("upsert receipt");

        let initial_stats = logger
            .load_worker_delivery_receipt_stats(channel_id.as_ref())
            .await
            .expect("load initial stats");
        assert_eq!(initial_stats.pending, 1);
        assert_eq!(initial_stats.failed, 0);

        let claimed = logger
            .claim_due_worker_terminal_receipts(&channel_id, 8)
            .await
            .expect("claim due receipts");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, receipt_id);
        assert_eq!(claimed[0].terminal_state, "done");
        assert_eq!(claimed[0].attempt_count, 0);

        let acked_now = logger
            .ack_worker_delivery_receipt(&receipt_id)
            .await
            .expect("ack receipt");
        assert!(acked_now);

        let acked_again = logger
            .ack_worker_delivery_receipt(&receipt_id)
            .await
            .expect("idempotent ack");
        assert!(!acked_again);

        let final_stats = logger
            .load_worker_delivery_receipt_stats(channel_id.as_ref())
            .await
            .expect("load final stats");
        assert_eq!(final_stats.pending, 0);
        assert_eq!(final_stats.failed, 0);
    }

    #[tokio::test]
    async fn worker_terminal_receipt_failure_retries_then_fails() {
        let logger = connect_logger().await;
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "failed",
                "Background task failed: network timeout",
            )
            .await
            .expect("upsert receipt");

        let first_outcome = logger
            .fail_worker_delivery_receipt_attempt(&receipt_id, "temporary send failure")
            .await
            .expect("record first failure");
        assert_eq!(first_outcome.status, "pending");
        assert_eq!(first_outcome.attempt_count, 1);
        assert!(first_outcome.next_attempt_at.is_some());

        sqlx::query(
            "UPDATE worker_delivery_receipts \
             SET next_attempt_at = CURRENT_TIMESTAMP \
             WHERE id = ?",
        )
        .bind(&receipt_id)
        .execute(&logger.pool)
        .await
        .expect("advance retry deadline");

        let claimed = logger
            .claim_due_worker_terminal_receipts(&channel_id, 8)
            .await
            .expect("claim receipt after retry scheduling");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].attempt_count, 1);

        for attempt in 2..=WORKER_RECEIPT_MAX_ATTEMPTS {
            let outcome = logger
                .fail_worker_delivery_receipt_attempt(&receipt_id, "adapter unavailable")
                .await
                .expect("record retry failure");
            assert_eq!(outcome.attempt_count, attempt);
            if attempt < WORKER_RECEIPT_MAX_ATTEMPTS {
                assert_eq!(outcome.status, "pending");
                assert!(outcome.next_attempt_at.is_some());
            } else {
                assert_eq!(outcome.status, "failed");
                assert!(outcome.next_attempt_at.is_none());
            }
        }

        let stats = logger
            .load_worker_delivery_receipt_stats(channel_id.as_ref())
            .await
            .expect("load retry stats");
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.failed, 1);
    }

    #[tokio::test]
    async fn close_orphaned_runs_requeues_sending_receipts() {
        let logger = connect_logger().await;
        let receipt_id = "receipt-test";

        sqlx::query(
            "INSERT INTO worker_delivery_receipts \
             (id, worker_id, channel_id, kind, status, terminal_state, payload_text, next_attempt_at) \
             VALUES (?, ?, ?, ?, 'sending', ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(receipt_id)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind("channel:test")
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind("done")
        .bind("Background task completed: done")
        .execute(&logger.pool)
        .await
        .expect("insert sending receipt");

        let (_, _, recovered_receipts, recovered_contracts) = logger
            .close_orphaned_runs()
            .await
            .expect("recover orphaned runs");
        assert_eq!(recovered_receipts, 1);
        assert_eq!(recovered_contracts, 0);

        let status: String =
            sqlx::query_scalar("SELECT status FROM worker_delivery_receipts WHERE id = ?")
                .bind(receipt_id)
                .fetch_one(&logger.pool)
                .await
                .expect("load receipt status");
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn worker_task_contract_deadline_claims_and_terminal_ack_flow() {
        let logger = connect_logger().await;
        let agent_id: crate::AgentId = Arc::from("agent:test");
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        logger
            .upsert_worker_task_contract(
                &agent_id,
                &channel_id,
                worker_id,
                "research task",
                WorkerTaskContractTiming {
                    ack_secs: 0,
                    progress_secs: 0,
                    terminal_secs: 60,
                },
            )
            .await
            .expect("upsert contract");

        let due_ack = logger
            .claim_due_worker_task_contract_ack_deadlines(&channel_id, 10, 5)
            .await
            .expect("claim due ack deadlines");
        assert_eq!(due_ack.len(), 1);
        assert_eq!(due_ack[0].worker_id, worker_id);
        assert_eq!(due_ack[0].attempt_count, 1);

        logger
            .mark_worker_task_contract_acknowledged(worker_id)
            .await
            .expect("mark acknowledged");
        logger
            .touch_worker_task_contract_progress(worker_id, Some("indexing source data"), 30)
            .await
            .expect("touch progress");

        sqlx::query(
            "UPDATE worker_task_contracts \
             SET state = ?, \
                 progress_deadline_at = CURRENT_TIMESTAMP, \
                 sla_nudge_sent = 0 \
             WHERE worker_id = ?",
        )
        .bind(WORKER_CONTRACT_STATE_PROGRESSING)
        .bind(worker_id.to_string())
        .execute(&logger.pool)
        .await
        .expect("force progress deadline");

        let due_progress = logger
            .claim_due_worker_task_contract_progress_deadlines(&channel_id, 10)
            .await
            .expect("claim due progress deadlines");
        assert_eq!(due_progress.len(), 1);
        assert_eq!(due_progress[0].worker_id, worker_id);

        let due_progress_again = logger
            .claim_due_worker_task_contract_progress_deadlines(&channel_id, 10)
            .await
            .expect("second progress claim should be empty");
        assert!(due_progress_again.is_empty());

        logger
            .mark_worker_task_contract_terminal_pending(worker_id, "done", 60)
            .await
            .expect("mark terminal pending");

        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "done",
                "Background task completed: done",
            )
            .await
            .expect("upsert receipt");
        let acked = logger
            .ack_worker_delivery_receipt(&receipt_id)
            .await
            .expect("ack receipt");
        assert!(acked);

        let state: String =
            sqlx::query_scalar("SELECT state FROM worker_task_contracts WHERE worker_id = ?")
                .bind(worker_id.to_string())
                .fetch_one(&logger.pool)
                .await
                .expect("load contract state");
        assert_eq!(state, WORKER_CONTRACT_STATE_TERMINAL_ACKED);
    }

    #[tokio::test]
    async fn worker_task_contract_moves_to_terminal_failed_on_receipt_exhaustion() {
        let logger = connect_logger().await;
        let agent_id: crate::AgentId = Arc::from("agent:test");
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        logger
            .upsert_worker_task_contract(
                &agent_id,
                &channel_id,
                worker_id,
                "analysis task",
                WorkerTaskContractTiming {
                    ack_secs: 5,
                    progress_secs: 45,
                    terminal_secs: 60,
                },
            )
            .await
            .expect("upsert contract");
        logger
            .mark_worker_task_contract_terminal_pending(worker_id, "failed", 60)
            .await
            .expect("mark terminal pending");

        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "failed",
                "Background task failed: request error",
            )
            .await
            .expect("upsert receipt");

        for _ in 0..WORKER_RECEIPT_MAX_ATTEMPTS {
            let _ = logger
                .fail_worker_delivery_receipt_attempt(&receipt_id, "adapter unavailable")
                .await
                .expect("record delivery failure");
        }

        let state: String =
            sqlx::query_scalar("SELECT state FROM worker_task_contracts WHERE worker_id = ?")
                .bind(worker_id.to_string())
                .fetch_one(&logger.pool)
                .await
                .expect("load contract state");
        assert_eq!(state, WORKER_CONTRACT_STATE_TERMINAL_FAILED);
    }

    #[tokio::test]
    async fn worker_task_contract_terminal_deadline_claim_marks_failed_and_stops_receipts() {
        let logger = connect_logger().await;
        let agent_id: crate::AgentId = Arc::from("agent:test");
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        logger
            .upsert_worker_task_contract(
                &agent_id,
                &channel_id,
                worker_id,
                "deadline task",
                WorkerTaskContractTiming {
                    ack_secs: 5,
                    progress_secs: 45,
                    terminal_secs: 1,
                },
            )
            .await
            .expect("upsert contract");
        logger
            .mark_worker_task_contract_terminal_pending(worker_id, "done", 0)
            .await
            .expect("mark terminal pending");
        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "done",
                "Background task completed: done",
            )
            .await
            .expect("upsert receipt");

        let due_terminal = logger
            .claim_due_worker_task_contract_terminal_deadlines(&channel_id, 10)
            .await
            .expect("claim due terminal deadlines");
        assert_eq!(due_terminal.len(), 1);
        assert_eq!(due_terminal[0].worker_id, worker_id);

        let state: String =
            sqlx::query_scalar("SELECT state FROM worker_task_contracts WHERE worker_id = ?")
                .bind(worker_id.to_string())
                .fetch_one(&logger.pool)
                .await
                .expect("load contract state");
        assert_eq!(state, WORKER_CONTRACT_STATE_TERMINAL_FAILED);

        let receipt_status: String =
            sqlx::query_scalar("SELECT status FROM worker_delivery_receipts WHERE id = ?")
                .bind(receipt_id)
                .fetch_one(&logger.pool)
                .await
                .expect("load receipt status");
        assert_eq!(receipt_status, "failed");
    }

    #[tokio::test]
    async fn claim_due_worker_terminal_receipts_any_claims_multiple_channels() {
        let logger = connect_logger().await;
        let channel_a: ChannelId = Arc::from("discord:1:100");
        let channel_b: ChannelId = Arc::from("discord:1:200");

        logger
            .upsert_worker_terminal_receipt(
                &channel_a,
                uuid::Uuid::new_v4(),
                "done",
                "Background task completed: channel a",
            )
            .await
            .expect("upsert channel a receipt");
        logger
            .upsert_worker_terminal_receipt(
                &channel_b,
                uuid::Uuid::new_v4(),
                "done",
                "Background task completed: channel b",
            )
            .await
            .expect("upsert channel b receipt");

        let claimed = logger
            .claim_due_worker_terminal_receipts_any(10)
            .await
            .expect("claim due receipts across channels");
        assert_eq!(claimed.len(), 2);
        assert!(
            claimed
                .iter()
                .any(|receipt| receipt.channel_id == channel_a.as_ref())
        );
        assert!(
            claimed
                .iter()
                .any(|receipt| receipt.channel_id == channel_b.as_ref())
        );
    }

    #[tokio::test]
    async fn worker_terminal_receipt_cancelled_claim_ack_round_trip() {
        let logger = connect_logger().await;
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "cancelled",
                "Background task was cancelled.",
            )
            .await
            .expect("upsert cancelled receipt");

        let claimed = logger
            .claim_due_worker_terminal_receipts(&channel_id, 8)
            .await
            .expect("claim cancelled receipt");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, receipt_id);
        assert_eq!(claimed[0].terminal_state, "cancelled");
        assert_eq!(claimed[0].payload_text, "Background task was cancelled.");

        let acked = logger
            .ack_worker_delivery_receipt(&receipt_id)
            .await
            .expect("ack cancelled receipt");
        assert!(acked);

        let stats = logger
            .load_worker_delivery_receipt_stats(channel_id.as_ref())
            .await
            .expect("load stats");
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.failed, 0);
    }

    #[tokio::test]
    async fn cancelled_receipt_delivery_failure_retries_then_acks() {
        let logger = connect_logger().await;
        let channel_id: ChannelId = Arc::from("channel:test");
        let worker_id = uuid::Uuid::new_v4();

        let receipt_id = logger
            .upsert_worker_terminal_receipt(
                &channel_id,
                worker_id,
                "cancelled",
                "Background task was cancelled.",
            )
            .await
            .expect("upsert cancelled receipt");

        let first_claim = logger
            .claim_due_worker_terminal_receipts(&channel_id, 8)
            .await
            .expect("first claim");
        assert_eq!(first_claim.len(), 1);
        assert_eq!(first_claim[0].id, receipt_id);
        assert_eq!(first_claim[0].attempt_count, 0);

        let retry = logger
            .fail_worker_delivery_receipt_attempt(&receipt_id, "adapter unavailable")
            .await
            .expect("record first delivery failure");
        assert_eq!(retry.status, "pending");
        assert_eq!(retry.attempt_count, 1);
        assert!(retry.next_attempt_at.is_some());

        sqlx::query(
            "UPDATE worker_delivery_receipts \
             SET next_attempt_at = CURRENT_TIMESTAMP \
             WHERE id = ?",
        )
        .bind(&receipt_id)
        .execute(&logger.pool)
        .await
        .expect("advance retry deadline");

        let second_claim = logger
            .claim_due_worker_terminal_receipts(&channel_id, 8)
            .await
            .expect("second claim after retry");
        assert_eq!(second_claim.len(), 1);
        assert_eq!(second_claim[0].id, receipt_id);
        assert_eq!(second_claim[0].attempt_count, 1);

        let acked = logger
            .ack_worker_delivery_receipt(&receipt_id)
            .await
            .expect("ack retried receipt");
        assert!(acked);

        let status: String =
            sqlx::query_scalar("SELECT status FROM worker_delivery_receipts WHERE id = ?")
                .bind(&receipt_id)
                .fetch_one(&logger.pool)
                .await
                .expect("load receipt status");
        assert_eq!(status, "acked");

        let stats = logger
            .load_worker_delivery_receipt_stats(channel_id.as_ref())
            .await
            .expect("load stats after ack");
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.failed, 0);
    }

    #[tokio::test]
    async fn prune_worker_delivery_receipts_deletes_only_old_terminal_rows() {
        let logger = connect_logger().await;
        let worker_old_acked = uuid::Uuid::new_v4().to_string();
        let worker_old_failed = uuid::Uuid::new_v4().to_string();
        let worker_old_pending = uuid::Uuid::new_v4().to_string();
        let worker_recent_acked = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO worker_delivery_receipts \
             (id, worker_id, channel_id, kind, status, terminal_state, payload_text, next_attempt_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, ?)",
        )
        .bind("old-acked")
        .bind(&worker_old_acked)
        .bind("channel:test")
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind("acked")
        .bind("done")
        .bind("Background task completed: old")
        .bind("2000-01-01T00:00:00Z")
        .execute(&logger.pool)
        .await
        .expect("insert old acked");

        sqlx::query(
            "INSERT INTO worker_delivery_receipts \
             (id, worker_id, channel_id, kind, status, terminal_state, payload_text, next_attempt_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, ?)",
        )
        .bind("old-failed")
        .bind(&worker_old_failed)
        .bind("channel:test")
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind("failed")
        .bind("failed")
        .bind("Background task failed: old")
        .bind("2000-01-01T00:00:00Z")
        .execute(&logger.pool)
        .await
        .expect("insert old failed");

        sqlx::query(
            "INSERT INTO worker_delivery_receipts \
             (id, worker_id, channel_id, kind, status, terminal_state, payload_text, next_attempt_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, ?)",
        )
        .bind("old-pending")
        .bind(&worker_old_pending)
        .bind("channel:test")
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind("pending")
        .bind("done")
        .bind("Background task completed: pending")
        .bind("2000-01-01T00:00:00Z")
        .execute(&logger.pool)
        .await
        .expect("insert old pending");

        sqlx::query(
            "INSERT INTO worker_delivery_receipts \
             (id, worker_id, channel_id, kind, status, terminal_state, payload_text, next_attempt_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind("recent-acked")
        .bind(&worker_recent_acked)
        .bind("channel:test")
        .bind(WORKER_TERMINAL_RECEIPT_KIND)
        .bind("acked")
        .bind("done")
        .bind("Background task completed: recent")
        .execute(&logger.pool)
        .await
        .expect("insert recent acked");

        let deleted = logger
            .prune_worker_delivery_receipts()
            .await
            .expect("prune receipts");
        assert_eq!(deleted, 2);

        let remaining: Vec<String> =
            sqlx::query_scalar("SELECT id FROM worker_delivery_receipts ORDER BY id ASC")
                .fetch_all(&logger.pool)
                .await
                .expect("load remaining receipt ids");
        assert_eq!(remaining, vec!["old-pending", "recent-acked"]);
    }
}
