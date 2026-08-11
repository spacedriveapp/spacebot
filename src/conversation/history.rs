//! Conversation message persistence (SQLite).

use crate::{BranchId, ChannelId, WorkerId};

use serde::Serialize;
use sqlx::{Row as _, SqlitePool};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Persists conversation messages (user and assistant) to SQLite.
///
/// All write methods are fire-and-forget — they spawn a tokio task and return
/// immediately so the caller never blocks on a DB write.
#[derive(Debug, Clone)]
pub struct ConversationLogger {
    pool: SqlitePool,
    /// Detached writes that have been spawned but not yet landed.
    ///
    /// Callers that need an exact durable watermark — the chronicle records one
    /// at every turn boundary — must know when the turn's own rows are visible.
    /// Reading `MAX(seq)` while writes are still in flight would under-report
    /// and make trimming lag a turn behind forever.
    pending_writes: Arc<PendingWrites>,
}

/// Tracks in-flight fire-and-forget writes so a caller can wait them out.
#[derive(Debug, Default)]
pub struct PendingWrites {
    count: AtomicUsize,
    drained: tokio::sync::Notify,
}

impl PendingWrites {
    fn begin(&self) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }

    fn finish(&self) {
        if self.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }

    fn is_idle(&self) -> bool {
        self.count.load(Ordering::Acquire) == 0
    }
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
    /// Monotonic per-channel insertion order. Rows written before the
    /// migration that introduced it are backfilled; only a row that somehow
    /// escaped both is `None`.
    pub seq: Option<i64>,
}

impl ConversationLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            pending_writes: Arc::new(PendingWrites::default()),
        }
    }

    /// Wait until every write spawned before this call has landed.
    ///
    /// Bounded: a stuck write must not stall a turn, so this gives up and the
    /// caller falls back to whatever watermark is visible, which is the safe
    /// direction (it keeps more live history, never less).
    pub async fn wait_for_pending_writes(&self, timeout: std::time::Duration) -> bool {
        if self.pending_writes.is_idle() {
            return true;
        }
        let deadline = tokio::time::Instant::now() + timeout;
        while !self.pending_writes.is_idle() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::debug!("timed out waiting for conversation writes to drain");
                return false;
            }
            let notified = self.pending_writes.drained.notified();
            if self.pending_writes.is_idle() {
                return true;
            }
            if tokio::time::timeout(
                remaining.min(std::time::Duration::from_millis(25)),
                notified,
            )
            .await
            .is_err()
            {
                continue;
            }
        }
        true
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

        let pending = self.pending_writes.clone();
        pending.begin();
        tokio::spawn(async move {
            let _finish = PendingWriteGuard(pending);
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages \
                 (id, channel_id, role, sender_name, sender_id, content, metadata, seq) \
                 VALUES (?, ?, 'user', ?, ?, ?, ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = ?), 0) + 1)"
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&sender_name)
            .bind(&sender_id)
            .bind(&content)
            .bind(&metadata_json)
            .bind(&channel_id)
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

        let pending = self.pending_writes.clone();
        pending.begin();
        tokio::spawn(async move {
            let _finish = PendingWriteGuard(pending);
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, sender_name, content, seq) \
                 VALUES (?, ?, 'system', 'system', ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = ?), 0) + 1)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&content)
            .bind(&channel_id)
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
        self.log_bot_message_with_metadata(channel_id, content, sender_name, None);
    }

    /// Log a bot message with optional tool calls packed into metadata. Fire-and-forget.
    pub fn log_bot_message_with_metadata(
        &self,
        channel_id: &ChannelId,
        content: &str,
        sender_name: Option<&str>,
        tool_calls_json: Option<String>,
    ) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let channel_id = channel_id.to_string();
        let content = content.to_string();
        let sender_name = sender_name.map(String::from);

        // Pack tool_calls into the metadata JSON if present.
        let metadata_json = tool_calls_json.map(|tc| format!(r#"{{"tool_calls":{tc}}}"#));

        let pending = self.pending_writes.clone();
        pending.begin();
        tokio::spawn(async move {
            let _finish = PendingWriteGuard(pending);
            if let Err(error) = sqlx::query(
                "INSERT INTO conversation_messages \
                 (id, channel_id, role, sender_name, content, metadata, seq) \
                 VALUES (?, ?, 'assistant', ?, ?, ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = ?), 0) + 1)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&sender_name)
            .bind(&content)
            .bind(&metadata_json)
            .bind(&channel_id)
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
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at, seq \
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
                seq: row.try_get("seq").ok().flatten(),
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
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at, seq \
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
                seq: row.try_get("seq").ok().flatten(),
            })
            .collect();

        // When fetching newest-first, reverse to chronological for the caller
        if !oldest_first {
            messages.reverse();
        }
        Ok(messages)
    }
}

/// Pagination cursor for the channel timeline.
///
/// Timestamp alone is not a total order at SQLite's one-second resolution, so
/// the item id breaks ties. Both timeline sources apply it identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineCursor {
    pub timestamp: String,
    pub id: String,
}

impl TimelineCursor {
    /// Parse the wire form `"<rfc3339>|<id>"`. A bare timestamp is accepted so
    /// a client mid-upgrade still paginates, just without the tiebreak.
    pub fn parse(value: &str) -> Self {
        match value.split_once('|') {
            Some((timestamp, id)) => Self {
                timestamp: timestamp.to_string(),
                id: id.to_string(),
            },
            None => Self {
                timestamp: value.to_string(),
                // Sorts below every real id, so a legacy cursor keeps the old
                // strictly-older-second behaviour rather than skipping rows.
                id: String::new(),
            },
        }
    }

    pub fn encode(timestamp: &str, id: &str) -> String {
        format!("{timestamp}|{id}")
    }
}

/// The id a timeline item paginates by.
pub fn timeline_item_id(item: &TimelineItem) -> &str {
    match item {
        TimelineItem::Message { id, .. }
        | TimelineItem::BranchRun { id, .. }
        | TimelineItem::WorkerRun { id, .. }
        | TimelineItem::ToolCallRun { id, .. }
        | TimelineItem::Checkpoint { id, .. } => id,
    }
}

/// Decrements the in-flight counter however the spawned write exits.
struct PendingWriteGuard(Arc<PendingWrites>);

impl Drop for PendingWriteGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// A unified timeline item combining messages, branch runs, and worker runs.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimelineItem {
    Message {
        id: String,
        role: String,
        sender_name: Option<String>,
        sender_id: Option<String>,
        content: String,
        created_at: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::agent::channel_attachments::SavedAttachmentMeta>,
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
    ToolCallRun {
        id: String,
        tool_name: String,
        args: String,
        result: Option<String>,
        status: String,
        started_at: String,
        completed_at: Option<String>,
    },
    /// A session chronicle checkpoint, placed inline at the point the
    /// conversation reached it. Authored by neither the user nor the agent.
    Checkpoint {
        id: String,
        seq: i64,
        level: i64,
        kind: String,
        title: String,
        summary: String,
        covers_from: String,
        covers_to: String,
        message_count: i64,
        rolled_up_into: Option<String>,
        created_at: String,
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
    #[allow(clippy::too_many_arguments)]
    pub fn log_worker_started(
        &self,
        channel_id: Option<&ChannelId>,
        worker_id: WorkerId,
        task: &str,
        worker_type: &str,
        agent_id: &crate::AgentId,
        interactive: bool,
        directory: Option<&std::path::Path>,
    ) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let channel_id = channel_id.map(|c| c.to_string());
        let task = task.to_string();
        let worker_type = worker_type.to_string();
        let agent_id = agent_id.to_string();
        let directory = directory.map(|d| d.to_string_lossy().to_string());

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO worker_runs (id, channel_id, task, worker_type, agent_id, interactive, directory) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&task)
            .bind(&worker_type)
            .bind(&agent_id)
            .bind(interactive)
            .bind(&directory)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker start");
            }
        });
    }

    /// Link a worker run to a project and/or worktree. Fire-and-forget.
    ///
    /// Called after spawn when `project_id` or `worktree_id` was set in the
    /// spawn args. Uses a separate UPDATE to avoid changing the WorkerStarted
    /// event shape.
    pub fn log_worker_project_link(
        &self,
        worker_id: WorkerId,
        project_id: Option<&str>,
        worktree_id: Option<&str>,
    ) {
        if project_id.is_none() && worktree_id.is_none() {
            return;
        }
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let project_id = project_id.map(|s| s.to_string());
        let worktree_id = worktree_id.map(|s| s.to_string());

        tokio::spawn(async move {
            // The worker_run INSERT may not have committed yet (it's also
            // fire-and-forget). Retry a few times with a short delay so we
            // don't silently update 0 rows.
            for attempt in 0..3u8 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                match sqlx::query(
                    "UPDATE worker_runs SET project_id = COALESCE(?, project_id), \
                     worktree_id = COALESCE(?, worktree_id) WHERE id = ?",
                )
                .bind(&project_id)
                .bind(&worktree_id)
                .bind(&id)
                .execute(&pool)
                .await
                {
                    Ok(result) if result.rows_affected() > 0 => return,
                    Ok(_) => {
                        // Row doesn't exist yet — retry.
                    }
                    Err(error) => {
                        tracing::warn!(%error, worker_id = %id, "failed to link worker to project");
                        return;
                    }
                }
            }
            tracing::debug!(worker_id = %id, "worker_runs row not found after retries for project link");
        });
    }

    /// Update a worker's status. Fire-and-forget.
    /// Most status text updates are transient — they're available via the
    /// in-memory StatusBlock for live workers and don't need to be persisted.
    /// The `status` column is reserved for the state enum (running/idle/done/failed).
    ///
    /// The one exception: when an idle worker resumes (status contains
    /// "processing follow-up" or similar active-work indicators), we persist
    /// `running` to the DB so the frontend doesn't show stale "idle" state.
    pub fn log_worker_status(&self, worker_id: WorkerId, status: &str) {
        // Detect when an idle worker resumes active work and persist the
        // transition. All other status text is transient.
        if status.starts_with("processing") || status == "running" {
            self.log_worker_resumed(worker_id);
        }
    }

    /// Mark an interactive worker as idle (waiting for follow-up input).
    /// Persisted so the frontend shows "idle" instead of "running".
    pub fn log_worker_idle(&self, worker_id: WorkerId) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query("UPDATE worker_runs SET status = 'idle' WHERE id = ?")
                .bind(&id)
                .execute(&pool)
                .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker idle state");
            }
        });
    }

    /// Mark an idle worker as running again (follow-up received).
    pub fn log_worker_resumed(&self, worker_id: WorkerId) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();

        tokio::spawn(async move {
            if let Err(error) =
                sqlx::query("UPDATE worker_runs SET status = 'running' WHERE id = ?")
                    .bind(&id)
                    .execute(&pool)
                    .await
            {
                tracing::warn!(%error, worker_id = %id, "failed to persist worker resumed state");
            }
        });
    }

    /// Record a worker completing with its result. Fire-and-forget.
    pub fn log_worker_completed(&self, worker_id: WorkerId, result: &str, success: bool) {
        let status = if success { "done" } else { "failed" };
        self.log_worker_completed_with_status(worker_id, result, status);
    }

    /// Record a worker as cancelled. Fire-and-forget.
    pub fn log_worker_cancelled(&self, worker_id: WorkerId, result: &str) {
        self.log_worker_completed_with_status(worker_id, result, "cancelled");
    }

    fn log_worker_completed_with_status(&self, worker_id: WorkerId, result: &str, status: &str) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let result = result.to_string();
        let status = status.to_string();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "UPDATE worker_runs SET result = ?, status = ?, completed_at = CURRENT_TIMESTAMP WHERE id = ? AND completed_at IS NULL"
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

    /// Record OpenCode session metadata on a worker run. Fire-and-forget.
    ///
    /// Stores the session ID and server port so the frontend can construct
    /// an iframe URL to the embedded OpenCode web UI.
    ///
    /// The worker row is inserted by `log_worker_started` (also fire-and-forget),
    /// which may not have committed yet when this runs. To handle the race we
    /// retry with a short back-off when the UPDATE affects zero rows.
    pub fn log_opencode_metadata(&self, worker_id: WorkerId, session_id: &str, port: u16) {
        let pool = self.pool.clone();
        let id = worker_id.to_string();
        let session_id = session_id.to_string();

        tokio::spawn(async move {
            const MAX_RETRIES: u32 = 5;
            const BASE_DELAY_MS: u64 = 50;

            for attempt in 0..=MAX_RETRIES {
                match sqlx::query(
                    "UPDATE worker_runs SET opencode_session_id = ?, opencode_port = ? WHERE id = ?",
                )
                .bind(&session_id)
                .bind(port as i32)
                .bind(&id)
                .execute(&pool)
                .await
                {
                    Ok(result) if result.rows_affected() > 0 => {
                        return; // Successfully updated.
                    }
                    Ok(_) => {
                        // Row doesn't exist yet — INSERT hasn't committed.
                        if attempt < MAX_RETRIES {
                            let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                            tracing::debug!(
                                worker_id = %id,
                                attempt,
                                delay_ms = delay,
                                "worker_runs row not yet inserted, retrying opencode metadata update"
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        } else {
                            tracing::warn!(
                                worker_id = %id,
                                "worker_runs row never appeared after {MAX_RETRIES} retries, \
                                 opencode metadata (port={port}) lost"
                            );
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            worker_id = %id,
                            "failed to persist OpenCode metadata"
                        );
                        return;
                    }
                }
            }
        });
    }

    /// Mark orphaned **running** workers as failed for an agent.
    ///
    /// Called at startup to reconcile rows that were left in `running` status
    /// when the process exited before a `WorkerComplete` event was persisted.
    ///
    /// Idle interactive workers are intentionally left alone — they will be
    /// resumed by `get_idle_interactive_workers()` + the reconnection logic.
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

    /// Load all idle interactive workers for an agent.
    ///
    /// Called at startup to find workers that were waiting for follow-up input
    /// when the process exited. These can potentially be reconnected to their
    /// sessions and resumed rather than marked as failed.
    pub async fn get_idle_interactive_workers(
        &self,
        agent_id: &str,
    ) -> crate::error::Result<Vec<IdleWorkerRow>> {
        let rows = sqlx::query_as::<_, IdleWorkerRow>(
            "SELECT id, task, channel_id, worker_type, transcript, \
                    COALESCE(tool_calls, 0) AS tool_calls, \
                    opencode_session_id, opencode_port, directory \
             FROM worker_runs \
             WHERE status = 'idle' AND interactive = TRUE \
                   AND (agent_id = ? OR agent_id IS NULL)",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(rows)
    }

    /// Mark an idle worker as failed (used when reconnection fails at startup).
    pub async fn fail_idle_worker(
        &self,
        worker_id: &str,
        reason: &str,
    ) -> crate::error::Result<()> {
        sqlx::query(
            "UPDATE worker_runs \
             SET status = 'failed', \
                 completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP), \
                 result = CASE \
                     WHEN result IS NULL OR result = '' THEN ? \
                     ELSE result \
                 END \
             WHERE id = ? AND status = 'idle'",
        )
        .bind(reason)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }

    /// Retire an idle worker whose session can no longer be resumed.
    ///
    /// Marks the row as `done` (not `failed`) because the worker completed its
    /// work successfully — only the follow-up session expired. The existing
    /// result and transcript are preserved.
    pub async fn retire_idle_worker(&self, worker_id: &str) -> crate::error::Result<()> {
        sqlx::query(
            "UPDATE worker_runs \
             SET status = 'done', \
                 completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) \
             WHERE id = ? AND status = 'idle'",
        )
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
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
                 status = 'cancelled', \
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
                 status = 'cancelled', \
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
        before: Option<TimelineCursor>,
    ) -> crate::error::Result<Vec<TimelineItem>> {
        // Composite cursor: SQLite timestamps are whole seconds, so a
        // timestamp-only `<` cursor silently skips every peer sharing the
        // boundary second. `(timestamp, id)` is a total order, and both the
        // message/branch/worker union and the checkpoint query use it.
        let before_clause = if before.is_some() {
            "AND (datetime(timestamp) < datetime(?3) \
                  OR (datetime(timestamp) = datetime(?3) AND id < ?4))"
        } else {
            ""
        };

        let query_str = format!(
            "SELECT * FROM ( \
                SELECT 'message' AS item_type, id, role, sender_name, sender_id, content, metadata, \
                       NULL AS description, NULL AS conclusion, NULL AS task, NULL AS result, NULL AS status, \
                       created_at AS timestamp, NULL AS completed_at \
                FROM conversation_messages WHERE channel_id = ?1 \
                UNION ALL \
                SELECT 'branch_run' AS item_type, id, NULL, NULL, NULL, NULL, NULL AS metadata, \
                       description, conclusion, NULL, NULL, NULL, \
                       started_at AS timestamp, completed_at \
                FROM branch_runs WHERE channel_id = ?1 \
                UNION ALL \
                SELECT 'worker_run' AS item_type, id, NULL, NULL, NULL, NULL, NULL AS metadata, \
                       NULL, NULL, task, result, status, \
                       started_at AS timestamp, completed_at \
                FROM worker_runs WHERE channel_id = ?1 \
            ) WHERE 1=1 {before_clause} ORDER BY timestamp DESC, id DESC LIMIT ?2"
        );

        let mut query = sqlx::query(&query_str).bind(channel_id).bind(limit);

        if let Some(cursor) = &before {
            query = query.bind(&cursor.timestamp).bind(&cursor.id);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Each entry carries the timestamp of the row it came from. Tool calls
        // expanded out of a message share that message's key, so the stable
        // merge below cannot separate them from the message they belong to.
        let mut items: Vec<(chrono::DateTime<chrono::Utc>, TimelineItem)> = rows
            .into_iter()
            .filter_map(
                |row| -> Option<Vec<(chrono::DateTime<chrono::Utc>, TimelineItem)>> {
                    let item_type: String = row.try_get("item_type").ok()?;
                    // Sorts last, so an undecodable row is dropped by
                    // truncate first instead of displacing a real newest item.
                    // Keying it at `now()` would also disagree with the
                    // rendered `created_at`, which falls back to empty.
                    let row_timestamp = row
                        .try_get::<chrono::DateTime<chrono::Utc>, _>("timestamp")
                        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
                    match item_type.as_str() {
                        "message" => {
                            let metadata_json: Option<String> =
                                row.try_get("metadata").ok().flatten();
                            let metadata_value = metadata_json.as_deref().and_then(|json| {
                                serde_json::from_str::<serde_json::Value>(json).ok()
                            });
                            let attachments = metadata_value
                                .as_ref()
                                .and_then(|v| v.get("attachments").cloned())
                                .and_then(|a| {
                                    serde_json::from_value::<
                                        Vec<crate::agent::channel_attachments::SavedAttachmentMeta>,
                                    >(a)
                                    .ok()
                                })
                                .unwrap_or_default();

                            // Expand tool calls stored in message metadata into ToolCallRun items.
                            let tool_call_items: Vec<TimelineItem> = metadata_value
                                .as_ref()
                                .and_then(|v| v.get("tool_calls"))
                                .and_then(|tc| {
                                    serde_json::from_value::<Vec<crate::api::ChannelToolCallEntry>>(
                                        tc.clone(),
                                    )
                                    .ok()
                                })
                                .unwrap_or_default()
                                .into_iter()
                                .map(|tc| TimelineItem::ToolCallRun {
                                    id: tc.id,
                                    tool_name: tc.tool_name,
                                    args: tc.args,
                                    result: tc.result,
                                    status: tc.status,
                                    started_at: tc.started_at,
                                    completed_at: tc.completed_at,
                                })
                                .collect();

                            let message = TimelineItem::Message {
                                id: row.try_get("id").unwrap_or_default(),
                                role: row.try_get("role").unwrap_or_default(),
                                sender_name: row.try_get("sender_name").ok().flatten(),
                                sender_id: row.try_get("sender_id").ok().flatten(),
                                content: row.try_get("content").unwrap_or_default(),
                                created_at: row
                                    .try_get::<chrono::DateTime<chrono::Utc>, _>("timestamp")
                                    .map(|t| t.to_rfc3339())
                                    .unwrap_or_default(),
                                attachments,
                            };

                            // Tool calls come before the message they belong to.
                            let mut result = tool_call_items;
                            result.push(message);
                            Some(
                                result
                                    .into_iter()
                                    .map(|item| (row_timestamp, item))
                                    .collect(),
                            )
                        }
                        "branch_run" => Some(vec![(
                            row_timestamp,
                            TimelineItem::BranchRun {
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
                            },
                        )]),
                        "worker_run" => Some(vec![(
                            row_timestamp,
                            TimelineItem::WorkerRun {
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
                            },
                        )]),
                        _ => None,
                    }
                },
            )
            .flatten()
            .collect();

        // Chronicle checkpoints live in their own table, so they are fetched
        // separately and merged. Each source returns up to `limit` rows, which
        // is what makes the merged newest-`limit` slice the correct page.
        let checkpoints = crate::conversation::chronicle::ChronicleStore::new(self.pool.clone())
            .list_for_timeline(channel_id, limit, before.as_ref())
            .await?;
        items.extend(checkpoints.into_iter().map(|checkpoint| {
            (
                checkpoint.created_at,
                TimelineItem::Checkpoint {
                    id: checkpoint.id,
                    seq: checkpoint.seq,
                    level: checkpoint.level,
                    kind: checkpoint.kind.as_str().to_string(),
                    title: checkpoint.title,
                    summary: checkpoint.summary,
                    covers_from: checkpoint.covers_from_at.to_rfc3339(),
                    covers_to: checkpoint.covers_to_at.to_rfc3339(),
                    message_count: checkpoint.message_count,
                    rolled_up_into: checkpoint.rolled_up_into,
                    created_at: checkpoint.created_at.to_rfc3339(),
                },
            )
        }));

        // Both sources arrive newest-first. A stable sort by the shared key
        // leaves an unchecked timeline in exactly the order it already had and
        // slots checkpoints into place; the page is then the newest `limit`.
        items.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| timeline_item_id(&right.1).cmp(timeline_item_id(&left.1)))
        });
        // Truncate on a group boundary. A message row expands into its tool
        // calls plus the message, all sharing one sort key; cutting inside that
        // group leaves orphan tool-call entries whose parent message was
        // dropped. Walk back to the start of a straddled group instead.
        if items.len() > limit as usize {
            let mut cut = limit as usize;
            if cut > 0 {
                let boundary_key = items[cut - 1].0;
                if items.get(cut).is_some_and(|(key, _)| *key == boundary_key) {
                    while cut > 0 && items[cut - 1].0 == boundary_key {
                        cut -= 1;
                    }
                }
            }
            items.truncate(cut);
        }
        let mut items: Vec<TimelineItem> = items.into_iter().map(|(_, item)| item).collect();
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
        // NOTE: The `projects` table lives in the global instance DB (spacebot.db),
        // not in the per-agent DB, as of migration 20260404120000. Worker rows keep
        // their `project_id` column locally, but project names must be resolved by
        // the caller via the global `ProjectStore`.
        let list_query = format!(
            "SELECT w.id, w.task, w.status, w.worker_type, w.channel_id, w.started_at, \
                    w.completed_at, w.transcript IS NOT NULL as has_transcript, \
                    w.tool_calls, w.opencode_port, w.opencode_session_id, w.directory, \
                    w.interactive, \
                    c.display_name as channel_name, \
                    w.project_id \
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
                opencode_port: row.try_get::<i32, _>("opencode_port").ok(),
                opencode_session_id: row.try_get("opencode_session_id").ok().flatten(),
                directory: row.try_get("directory").ok().flatten(),
                interactive: row.try_get::<bool, _>("interactive").unwrap_or(false),
                project_id: row.try_get("project_id").ok().flatten(),
                // Resolved by caller via the global `ProjectStore` — see module note.
                project_name: None,
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
                    w.opencode_session_id, w.opencode_port, w.interactive, w.directory, \
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
            // A NULL blob decodes to an empty vec rather than erroring, so
            // read it as nullable and drop empties — callers treat `None` as
            // "no persisted transcript" and fall back to the live cache.
            transcript_blob: row
                .try_get::<Option<Vec<u8>>, _>("transcript")
                .unwrap_or(None)
                .filter(|blob| !blob.is_empty()),
            tool_calls: row.try_get::<i64, _>("tool_calls").unwrap_or(0),
            opencode_session_id: row.try_get("opencode_session_id").ok(),
            opencode_port: row.try_get::<i32, _>("opencode_port").ok(),
            interactive: row.try_get::<bool, _>("interactive").unwrap_or(false),
            directory: row
                .try_get::<Option<String>, _>("directory")
                .unwrap_or(None),
        }))
    }
}

/// Persists skill-reflection run records for the activity timeline.
///
/// Follows the same fire-and-forget pattern as [`ProcessRunLogger`].
/// A reflection run starts when the persistence branch spawns with
/// `skill_reflection = true` and completes when that branch's result
/// arrives, recording the trigger provenance, outcome summary, affected
/// skills, and token usage.
#[derive(Debug, Clone)]
pub struct ReflectionRunLogger {
    pool: SqlitePool,
}

impl ReflectionRunLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Record a reflection run starting. Fire-and-forget.
    pub fn log_reflection_started(
        &self,
        branch_id: crate::BranchId,
        agent_id: &crate::AgentId,
        channel_id: &crate::ChannelId,
        trigger_source: &str,
        referenced_workers: &[(crate::WorkerId, bool)],
    ) {
        let pool = self.pool.clone();
        let id = branch_id.to_string();
        let agent_id = agent_id.to_string();
        let channel_id = channel_id.to_string();
        let trigger_source = trigger_source.to_string();
        let referenced_workers = serde_json::to_string(
            &referenced_workers
                .iter()
                .map(|(wid, success)| {
                    serde_json::json!({
                        "id": wid.to_string(),
                        "success": success,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO reflection_runs \
                 (id, agent_id, channel_id, trigger_source, referenced_workers) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&agent_id)
            .bind(&channel_id)
            .bind(&trigger_source)
            .bind(&referenced_workers)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, branch_id = %id, "failed to persist reflection run start");
            }
        });
    }

    /// Mark a reflection run as completed. Called when the persistence
    /// branch result arrives. Fire-and-forget.
    ///
    /// `observed_actions` is a JSON array of `{action, skill_name, detail?}`
    /// derived from the branch's tool-call results — what actually
    /// happened, not what the branch claimed it did.
    pub fn log_reflection_completed(
        &self,
        branch_id: crate::BranchId,
        status: &str,
        outcome_summary: &str,
        declared_rationale: Option<&str>,
        observed_actions: &[serde_json::Value],
        terminal_reason: Option<&str>,
        token_usage: Option<serde_json::Value>,
        affected_skills: &str,
    ) {
        let pool = self.pool.clone();
        let id = branch_id.to_string();
        let status = status.to_string();
        let outcome_summary = outcome_summary.to_string();
        let declared_rationale = declared_rationale.map(|s| s.to_string());
        let observed_actions = serde_json::to_string(observed_actions).unwrap_or_default();
        let terminal_reason = terminal_reason.map(|s| s.to_string());
        let token_usage = token_usage.map(|v| serde_json::to_string(&v).unwrap_or_default());
        let affected_skills = affected_skills.to_string();

        // Guard against duplicate completions: only update if still 'running'.
        tokio::spawn(async move {
            let result = sqlx::query(
                "UPDATE reflection_runs SET \
                 status = ?, completed_at = CURRENT_TIMESTAMP, \
                 declared_rationale = ?, observed_actions = ?, \
                 outcome_summary = ?, terminal_reason = ?, \
                 token_usage = ?, affected_skills = ? \
                 WHERE id = ? AND status = 'running'",
            )
            .bind(&status)
            .bind(&declared_rationale)
            .bind(&observed_actions)
            .bind(&outcome_summary)
            .bind(&terminal_reason)
            .bind(&token_usage)
            .bind(&affected_skills)
            .bind(&id)
            .execute(&pool)
            .await;

            match result {
                Ok(result) if result.rows_affected() > 0 => {
                    tracing::info!(branch_id = %id, status = %status, "reflection run completed");
                }
                Ok(_) => {
                    tracing::debug!(branch_id = %id, "reflection run already completed or not found");
                }
                Err(error) => {
                    tracing::warn!(%error, branch_id = %id, "failed to persist reflection run completion");
                }
            }
        });
    }

    /// Load reflection runs for a channel, ordered by start time descending.
    pub async fn load_for_channel(
        &self,
        channel_id: &crate::ChannelId,
        limit: i64,
    ) -> std::result::Result<Vec<ReflectionRunRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, ReflectionRunRow>(
            "SELECT id, agent_id, channel_id, trigger_source, referenced_workers, \
             started_at, completed_at, status, declared_rationale, observed_actions, \
             outcome_summary, terminal_reason, token_usage, affected_skills \
             FROM reflection_runs WHERE channel_id = ? \
             ORDER BY started_at DESC LIMIT ?",
        )
        .bind(channel_id.as_ref())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }
}

/// Query row for a reflection run.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ReflectionRunRow {
    pub id: String,
    pub agent_id: String,
    pub channel_id: String,
    pub trigger_source: String,
    pub referenced_workers: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub status: String,
    pub declared_rationale: Option<String>,
    pub observed_actions: Option<String>,
    pub outcome_summary: Option<String>,
    pub terminal_reason: Option<String>,
    pub token_usage: Option<String>,
    pub affected_skills: Option<String>,
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
    pub opencode_port: Option<i32>,
    pub opencode_session_id: Option<String>,
    pub directory: Option<String>,
    pub interactive: bool,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
}

/// A worker that was idle at shutdown, loaded for reconnection at startup.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IdleWorkerRow {
    pub id: String,
    pub task: String,
    pub channel_id: Option<String>,
    pub worker_type: String,
    pub transcript: Option<Vec<u8>>,
    pub tool_calls: i64,
    pub opencode_session_id: Option<String>,
    pub opencode_port: Option<i32>,
    pub directory: Option<String>,
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
    pub opencode_session_id: Option<String>,
    pub opencode_port: Option<i32>,
    pub interactive: bool,
    pub directory: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ProcessRunLogger, TimelineCursor, TimelineItem, timeline_item_id};

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
        assert_eq!(status, "cancelled");
        assert_eq!(result, "Worker cancelled");
    }

    #[tokio::test]
    async fn cancel_running_worker_sets_cancelled_status() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO worker_runs (id, channel_id, status, result) VALUES (?, 'ch-1', 'running', '')")
            .bind(worker_id.to_string())
            .execute(&pool)
            .await
            .expect("insert");

        let cancelled = logger
            .cancel_running_worker("ch-1", worker_id)
            .await
            .expect("cancel should succeed");
        assert!(cancelled);

        let row = sqlx::query("SELECT status, result FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("fetch");
        let status: String = sqlx::Row::try_get(&row, "status").expect("status");
        let result: String = sqlx::Row::try_get(&row, "result").expect("result");
        assert_eq!(status, "cancelled");
        assert_eq!(result, "Worker cancelled");
    }

    /// Poll until a worker's status changes from "running", with a timeout.
    async fn poll_worker_status(
        pool: &sqlx::SqlitePool,
        worker_id: uuid::Uuid,
        timeout_ms: u64,
    ) -> String {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let row = sqlx::query("SELECT status FROM worker_runs WHERE id = ?")
                .bind(worker_id.to_string())
                .fetch_one(pool)
                .await
                .expect("fetch");
            let status: String = sqlx::Row::try_get(&row, "status").expect("status");
            if status != "running" {
                return status;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("worker status did not transition within {timeout_ms}ms");
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn log_worker_completed_success_sets_done_status() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO worker_runs (id, channel_id, status, result) VALUES (?, 'ch-1', 'running', '')")
            .bind(worker_id.to_string())
            .execute(&pool)
            .await
            .expect("insert");

        logger.log_worker_completed(worker_id, "task finished", true);
        let status = poll_worker_status(&pool, worker_id, 2000).await;

        assert_eq!(status, "done");
        let row = sqlx::query("SELECT result FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("fetch");
        let result: String = sqlx::Row::try_get(&row, "result").expect("result");
        assert_eq!(result, "task finished");
    }

    #[tokio::test]
    async fn log_worker_completed_failure_sets_failed_status() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO worker_runs (id, channel_id, status, result) VALUES (?, 'ch-1', 'running', '')")
            .bind(worker_id.to_string())
            .execute(&pool)
            .await
            .expect("insert");

        logger.log_worker_completed(worker_id, "something broke", false);
        let status = poll_worker_status(&pool, worker_id, 2000).await;

        assert_eq!(status, "failed");
    }

    #[tokio::test]
    async fn log_worker_cancelled_sets_cancelled_status() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO worker_runs (id, channel_id, status, result) VALUES (?, 'ch-1', 'running', '')")
            .bind(worker_id.to_string())
            .execute(&pool)
            .await
            .expect("insert");

        logger.log_worker_cancelled(worker_id, "Worker cancelled: user requested");
        let status = poll_worker_status(&pool, worker_id, 2000).await;

        assert_eq!(status, "cancelled");
        let row = sqlx::query("SELECT result FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("fetch");
        let result: String = sqlx::Row::try_get(&row, "result").expect("result");
        assert_eq!(result, "Worker cancelled: user requested");
    }

    #[tokio::test]
    async fn timeline_places_checkpoints_between_the_messages_they_follow() {
        use crate::conversation::chronicle::{
            CheckpointKind, ChronicleBoundary, ChronicleStore, NewCheckpoint,
        };

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        for migration in [
            include_str!("../../migrations/20260211000002_conversations.sql"),
            include_str!("../../migrations/20260213000003_process_runs.sql"),
            include_str!("../../migrations/20260809000004_session_chronicles.sql"),
            include_str!("../../migrations/20260810000001_conversation_message_seq.sql"),
        ] {
            sqlx::raw_sql(migration)
                .execute(&pool)
                .await
                .expect("migration");
        }

        for (id, at) in [
            ("m1", "2026-08-01 00:00:01"),
            ("m2", "2026-08-01 00:00:02"),
            ("m3", "2026-08-01 00:00:09"),
        ] {
            sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, content, created_at, seq) \
                 VALUES (?, 'ch', 'user', 'hi', ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = 'ch'), 0) + 1)",
            )
            .bind(id)
            .bind(at)
            .execute(&pool)
            .await
            .expect("insert message");
        }

        let at = |value: &str| {
            chrono::DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        ChronicleStore::new(pool.clone())
            .commit(NewCheckpoint {
                channel_id: "ch".into(),
                level: 0,
                kind: CheckpointKind::Interval,
                title: "Opening span".into(),
                summary: "They greeted each other twice.".into(),
                covers_from: ChronicleBoundary::origin(),
                covers_to: ChronicleBoundary::new(2),
                covers_from_at: at("2026-08-01T00:00:01Z"),
                covers_to_at: at("2026-08-01T00:00:02Z"),
                covers_from_message_id: None,
                covers_to_message_id: Some("m2".into()),
                message_count: 2,
                token_estimate: 5,
                rolls_up_from_seq: None,
                rolls_up_to_seq: None,
                model: None,
            })
            .await
            .expect("commit");
        // The commit stamps `created_at` at wall-clock now; pin it between m2
        // and m3 so its timeline position is deterministic.
        sqlx::query("UPDATE channel_chronicle_checkpoints SET created_at = '2026-08-01 00:00:05'")
            .execute(&pool)
            .await
            .expect("pin commit time");

        let items = ProcessRunLogger::new(pool)
            .load_channel_timeline("ch", 20, None)
            .await
            .expect("timeline");

        let shape: Vec<String> = items
            .iter()
            .map(|item| match item {
                TimelineItem::Message { id, .. } => format!("message:{id}"),
                TimelineItem::Checkpoint { seq, .. } => format!("checkpoint:{seq}"),
                other => format!("other:{other:?}"),
            })
            .collect();

        assert_eq!(
            shape,
            vec!["message:m1", "message:m2", "checkpoint:1", "message:m3"],
            "a checkpoint sits inline after the messages it covers"
        );
    }

    /// A page boundary landing among same-second peers must not skip any of
    /// them. Timestamp-only cursors did exactly that.
    #[tokio::test]
    async fn timeline_pagination_does_not_skip_same_second_peers() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        for migration in [
            include_str!("../../migrations/20260211000002_conversations.sql"),
            include_str!("../../migrations/20260213000003_process_runs.sql"),
            include_str!("../../migrations/20260809000004_session_chronicles.sql"),
            include_str!("../../migrations/20260810000001_conversation_message_seq.sql"),
        ] {
            sqlx::raw_sql(migration)
                .execute(&pool)
                .await
                .expect("migration");
        }

        // Five messages sharing one whole second.
        for index in 0..5 {
            sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, content, created_at, seq) \
                 VALUES (?, 'ch', 'user', 'hi', '2026-08-01 00:00:00', ?)",
            )
            .bind(format!("msg-{index}"))
            .bind(index + 1)
            .execute(&pool)
            .await
            .expect("insert");
        }

        let logger = ProcessRunLogger::new(pool);
        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<TimelineCursor> = None;

        // Page two at a time through a block that shares a timestamp.
        for _ in 0..5 {
            let page = logger
                .load_channel_timeline("ch", 2, cursor.clone())
                .await
                .expect("page");
            if page.is_empty() {
                break;
            }
            let oldest = page.first().expect("oldest");
            let (timestamp, id) = match oldest {
                TimelineItem::Message { created_at, id, .. } => (created_at.clone(), id.clone()),
                other => panic!("unexpected item: {other:?}"),
            };
            cursor = Some(TimelineCursor::parse(&TimelineCursor::encode(
                &timestamp, &id,
            )));
            for item in &page {
                seen.push(timeline_item_id(item).to_string());
            }
        }

        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            5,
            "every same-second message must be reachable by paging: {seen:?}"
        );
    }

    /// Truncating a page must not leave tool calls whose parent message was
    /// dropped: they share one sort key and are inserted as a group.
    #[test]
    fn page_truncation_never_orphans_tool_calls() {
        // Newest-first, as the merge produces: two tool calls then their
        // message, all sharing one key, followed by an older message.
        let key_new = chrono::DateTime::<chrono::Utc>::MIN_UTC + chrono::Duration::seconds(10);
        let key_old = chrono::DateTime::<chrono::Utc>::MIN_UTC;
        let mut items = vec![
            (key_new, tool_call_item("tc-1")),
            (key_new, tool_call_item("tc-2")),
            (key_new, message_item("msg-new")),
            (key_old, message_item("msg-old")),
        ];

        // A limit of 2 lands inside the group; the whole group must go.
        let limit = 2usize;
        if items.len() > limit {
            let mut cut = limit;
            if cut > 0 {
                let boundary_key = items[cut - 1].0;
                if items.get(cut).is_some_and(|(key, _)| *key == boundary_key) {
                    while cut > 0 && items[cut - 1].0 == boundary_key {
                        cut -= 1;
                    }
                }
            }
            items.truncate(cut);
        }

        let kept: Vec<&str> = items
            .iter()
            .map(|(_, item)| timeline_item_id(item))
            .collect();
        assert!(
            !kept.contains(&"tc-1") && !kept.contains(&"tc-2"),
            "tool calls must not outlive their message: {kept:?}"
        );
    }

    fn tool_call_item(id: &str) -> TimelineItem {
        TimelineItem::ToolCallRun {
            id: id.to_string(),
            tool_name: "shell".into(),
            args: "{}".into(),
            result: None,
            status: "completed".into(),
            started_at: String::new(),
            completed_at: None,
        }
    }

    fn message_item(id: &str) -> TimelineItem {
        TimelineItem::Message {
            id: id.to_string(),
            role: "user".into(),
            sender_name: None,
            sender_id: None,
            content: "hi".into(),
            created_at: String::new(),
            attachments: Vec::new(),
        }
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
