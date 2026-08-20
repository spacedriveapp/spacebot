//! Conversation message persistence (SQLite).

use crate::{BranchId, ChannelId, WorkerId};

use serde::{Deserialize, Serialize};
use sqlx::{Row as _, Sqlite, SqlitePool, Transaction};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLifecycle {
    Created,
    Running,
    WaitingForInput,
    Cancelling,
    TimingOut,
    Completing,
    Succeeded,
    Partial,
    Cancelled,
    TimedOut,
    Blocked,
    Failed,
}

impl WorkerLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::Cancelling => "cancelling",
            Self::TimingOut => "timing_out",
            Self::Completing => "completing",
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Partial
                | Self::Cancelled
                | Self::TimedOut
                | Self::Blocked
                | Self::Failed
        )
    }

    pub fn can_transition_to(self, target: Self) -> bool {
        use WorkerLifecycle::{
            Blocked, Cancelled, Cancelling, Completing, Created, Failed, Partial, Running,
            Succeeded, TimedOut, TimingOut, WaitingForInput,
        };

        matches!(
            (self, target),
            (Created, Running)
                | (
                    Running,
                    WaitingForInput | Cancelling | TimingOut | Completing | Failed
                )
                | (WaitingForInput, Running | Cancelling | Completing)
                | (Cancelling, Cancelled | Succeeded | Partial)
                | (TimingOut, TimedOut | Succeeded | Partial)
                | (
                    Completing,
                    Succeeded | Partial | Cancelled | Blocked | Failed
                )
        )
    }

    fn display_status(self) -> &'static str {
        match self {
            Self::WaitingForInput => "idle",
            Self::Succeeded | Self::Partial => "done",
            Self::Cancelled => "cancelled",
            Self::TimedOut | Self::Blocked | Self::Failed => "failed",
            Self::Created
            | Self::Running
            | Self::Cancelling
            | Self::TimingOut
            | Self::Completing => "running",
        }
    }
}

impl std::str::FromStr for WorkerLifecycle {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "created" => Ok(Self::Created),
            "running" => Ok(Self::Running),
            "waiting_for_input" => Ok(Self::WaitingForInput),
            "cancelling" => Ok(Self::Cancelling),
            "timing_out" => Ok(Self::TimingOut),
            "completing" => Ok(Self::Completing),
            "succeeded" => Ok(Self::Succeeded),
            "partial" => Ok(Self::Partial),
            "cancelled" => Ok(Self::Cancelled),
            "timed_out" => Ok(Self::TimedOut),
            "blocked" => Ok(Self::Blocked),
            "failed" => Ok(Self::Failed),
            _ => anyhow::bail!("can't parse worker lifecycle: unknown value {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerOutcomeKind {
    Succeeded,
    Partial,
    Cancelled,
    TimedOut,
    Blocked,
    Failed,
}

impl WorkerOutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }

    pub fn lifecycle(self) -> WorkerLifecycle {
        match self {
            Self::Succeeded => WorkerLifecycle::Succeeded,
            Self::Partial => WorkerLifecycle::Partial,
            Self::Cancelled => WorkerLifecycle::Cancelled,
            Self::TimedOut => WorkerLifecycle::TimedOut,
            Self::Blocked => WorkerLifecycle::Blocked,
            Self::Failed => WorkerLifecycle::Failed,
        }
    }

    pub fn is_success(self) -> bool {
        matches!(self, Self::Succeeded | Self::Partial)
    }
}

impl std::str::FromStr for WorkerOutcomeKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "succeeded" => Ok(Self::Succeeded),
            "partial" => Ok(Self::Partial),
            "cancelled" => Ok(Self::Cancelled),
            "timed_out" => Ok(Self::TimedOut),
            "blocked" => Ok(Self::Blocked),
            "failed" => Ok(Self::Failed),
            _ => anyhow::bail!("can't parse worker outcome: unknown value {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerTerminalOwner {
    Worker,
    Cancel,
    Timeout,
    Supervisor,
    Shutdown,
    Reconcile,
}

impl WorkerTerminalOwner {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Cancel => "cancel",
            Self::Timeout => "timeout",
            Self::Supervisor => "supervisor",
            Self::Shutdown => "shutdown",
            Self::Reconcile => "reconcile",
        }
    }
}

impl std::str::FromStr for WorkerTerminalOwner {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "worker" => Ok(Self::Worker),
            "cancel" => Ok(Self::Cancel),
            "timeout" => Ok(Self::Timeout),
            "supervisor" => Ok(Self::Supervisor),
            "shutdown" => Ok(Self::Shutdown),
            "reconcile" => Ok(Self::Reconcile),
            _ => anyhow::bail!("can't parse worker terminal owner: unknown value {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerTransitionResult {
    Applied {
        previous: WorkerLifecycle,
        current: WorkerLifecycle,
    },
    Conflict {
        current: WorkerLifecycle,
    },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStartResult {
    Started,
    Existing {
        lifecycle: WorkerLifecycle,
        run_id: Option<String>,
        origin_branch_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerTranscriptCommit {
    Applied {
        transcript_version: i64,
    },
    Conflict {
        lifecycle: WorkerLifecycle,
        transcript_version: i64,
    },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTerminalOutcome {
    pub worker_id: String,
    pub lifecycle: WorkerLifecycle,
    pub outcome_kind: WorkerOutcomeKind,
    pub outcome_summary: Option<String>,
    pub result: String,
    pub outcome_version: i64,
    pub transcript_version: i64,
    pub terminal_owner: Option<WorkerTerminalOwner>,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCompletionCommit {
    Committed(WorkerTerminalOutcome),
    Existing(WorkerTerminalOutcome),
    Conflict { current: WorkerLifecycle },
    NotFound,
}

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
    ///
    /// Returns the id assigned to the message so a caller can reference it
    /// before the write lands — the row is written on a spawned task, but the
    /// id is decided here.
    pub fn log_user_message(
        &self,
        channel_id: &ChannelId,
        sender_name: &str,
        sender_id: &str,
        content: &str,
        metadata: &HashMap<String, serde_json::Value>,
    ) -> String {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let message_id = id.clone();
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

        message_id
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

    /// Log an assistant message with a caller-owned id. Duplicate writes are
    /// ignored so a durable terminal transition can safely retry publication.
    pub fn log_bot_message_with_id(&self, channel_id: &ChannelId, id: &str, content: &str) {
        let pool = self.pool.clone();
        let id = id.to_string();
        let channel_id = channel_id.to_string();
        let content = content.to_string();

        let pending = self.pending_writes.clone();
        pending.begin();
        tokio::spawn(async move {
            let _finish = PendingWriteGuard(pending);
            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO conversation_messages \
                 (id, channel_id, role, content, seq) \
                 VALUES (?, ?, 'assistant', ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = ?), 0) + 1)",
            )
            .bind(&id)
            .bind(&channel_id)
            .bind(&content)
            .bind(&channel_id)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, %id, "failed to persist bot message");
            }
        });
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
#[derive(Debug, Clone)]
pub struct ProcessRunLogger {
    pool: SqlitePool,
}

impl ProcessRunLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Record a branch before its execution task is spawned.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_branch_started(
        &self,
        channel_id: &ChannelId,
        branch_id: BranchId,
        description: &str,
        input: &str,
        profile: &str,
        model: &str,
        max_turns: usize,
        run_id: Option<&str>,
    ) -> crate::error::Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO branch_runs \
             (id, channel_id, description, input, status, profile, model, max_turns, run_id) \
             VALUES (?, ?, ?, ?, 'running', ?, ?, ?, ?)",
        )
        .bind(branch_id.to_string())
        .bind(channel_id.as_ref())
        .bind(description)
        .bind(input)
        .bind(profile)
        .bind(model)
        .bind(i64::try_from(max_turns).unwrap_or(i64::MAX))
        .bind(run_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }

    /// Commit a branch terminal outcome if the run is still active.
    ///
    /// Returns `true` when this call won the terminal transition. Once a row
    /// leaves `running`, duplicate completion or cancellation events are inert.
    pub async fn log_branch_terminal(
        &self,
        branch_id: BranchId,
        conclusion: &str,
        status: &str,
        transcript: Option<&[u8]>,
        tool_calls: i64,
    ) -> crate::error::Result<bool> {
        if !matches!(status, "done" | "failed" | "cancelled") {
            return Err(
                anyhow::anyhow!("can't complete branch: invalid terminal status {status}").into(),
            );
        }

        let result = sqlx::query(
            "UPDATE branch_runs \
             SET conclusion = ?, status = ?, transcript = ?, tool_calls = ?, \
                 completed_at = CURRENT_TIMESTAMP \
             WHERE id = ? AND status = 'running'",
        )
        .bind(conclusion)
        .bind(status)
        .bind(transcript)
        .bind(tool_calls)
        .bind(branch_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(result.rows_affected() == 1)
    }

    /// Record a worker before its execution task is spawned.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_worker_started(
        &self,
        channel_id: Option<&ChannelId>,
        worker_id: WorkerId,
        task: &str,
        worker_type: &str,
        agent_id: &crate::AgentId,
        interactive: bool,
        directory: Option<&std::path::Path>,
        run_id: Option<&str>,
        origin_branch_id: Option<BranchId>,
    ) -> crate::error::Result<WorkerStartResult> {
        let worker_id = worker_id.to_string();
        let result = sqlx::query(
            "INSERT OR IGNORE INTO worker_runs \
             (id, channel_id, task, worker_type, agent_id, interactive, directory, lifecycle, \
              status, run_id, origin_branch_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'running', 'running', ?, ?)",
        )
        .bind(&worker_id)
        .bind(channel_id.map(|channel_id| channel_id.as_ref()))
        .bind(task)
        .bind(worker_type)
        .bind(agent_id.as_ref())
        .bind(interactive)
        .bind(directory.map(|directory| directory.to_string_lossy().to_string()))
        .bind(run_id)
        .bind(origin_branch_id.map(|branch_id| branch_id.to_string()))
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        if result.rows_affected() == 1 {
            return Ok(WorkerStartResult::Started);
        }

        let row =
            sqlx::query("SELECT lifecycle, run_id, origin_branch_id FROM worker_runs WHERE id = ?")
                .bind(&worker_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| anyhow::anyhow!(error))?;
        Ok(WorkerStartResult::Existing {
            lifecycle: parse_lifecycle(&row, "lifecycle")?,
            run_id: row.try_get("run_id").ok().flatten(),
            origin_branch_id: row.try_get("origin_branch_id").ok().flatten(),
        })
    }

    /// Return the worker directly delegated by a branch, if it was persisted
    /// before a branch replayed after process restart.
    pub async fn worker_for_origin_branch(
        &self,
        branch_id: BranchId,
    ) -> crate::error::Result<Option<(WorkerId, String, bool)>> {
        let row = sqlx::query(
            "SELECT id, task, interactive FROM worker_runs \
             WHERE origin_branch_id = ? ORDER BY started_at ASC LIMIT 1",
        )
        .bind(branch_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        row.map(|row| {
            let worker_id = row
                .try_get::<String, _>("id")
                .map_err(|error| anyhow::anyhow!(error))?
                .parse()
                .map_err(|error| anyhow::anyhow!("invalid persisted worker ID: {error}"))?;
            Ok::<(WorkerId, String, bool), crate::error::Error>((
                worker_id,
                row.try_get("task")
                    .map_err(|error| anyhow::anyhow!(error))?,
                row.try_get("interactive")
                    .map_err(|error| anyhow::anyhow!(error))?,
            ))
        })
        .transpose()
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
            // Some callers link from a separate event loop before worker start has
            // been observed. Retry a few times so the link is not silently lost.
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

    /// Update a worker's status.
    /// Most status text updates are transient — they're available via the
    /// in-memory StatusBlock for live workers and don't need to be persisted.
    /// The `status` column is reserved for the state enum (running/idle/done/failed).
    ///
    /// The one exception: when an idle worker resumes (status contains
    /// "processing follow-up" or similar active-work indicators), we persist
    /// `running` to the DB so the frontend doesn't show stale "idle" state.
    pub async fn log_worker_status(
        &self,
        worker_id: WorkerId,
        status: &str,
    ) -> crate::error::Result<Option<WorkerTransitionResult>> {
        // Detect when an idle worker resumes active work and persist the
        // transition. All other status text is transient.
        if status.starts_with("processing") || status == "running" {
            return self.log_worker_resumed(worker_id).await.map(Some);
        }
        Ok(None)
    }

    /// Mark an interactive worker as idle (waiting for follow-up input).
    /// Persisted so the frontend shows "idle" instead of "running".
    pub async fn log_worker_idle(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<WorkerTransitionResult> {
        self.transition_worker(
            worker_id,
            WorkerLifecycle::Running,
            WorkerLifecycle::WaitingForInput,
        )
        .await
    }

    /// Mark an idle worker as running again (follow-up received).
    pub async fn log_worker_resumed(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<WorkerTransitionResult> {
        self.transition_worker(
            worker_id,
            WorkerLifecycle::WaitingForInput,
            WorkerLifecycle::Running,
        )
        .await
    }

    pub async fn transition_worker(
        &self,
        worker_id: WorkerId,
        expected: WorkerLifecycle,
        target: WorkerLifecycle,
    ) -> crate::error::Result<WorkerTransitionResult> {
        if expected.is_terminal() || target.is_terminal() || !expected.can_transition_to(target) {
            return self.worker_transition_conflict(worker_id).await;
        }

        let result = sqlx::query(
            "UPDATE worker_runs SET lifecycle = ?, status = ? \
             WHERE id = ? AND lifecycle = ? AND completed_at IS NULL",
        )
        .bind(target.as_str())
        .bind(target.display_status())
        .bind(worker_id.to_string())
        .bind(expected.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        if result.rows_affected() == 1 {
            Ok(WorkerTransitionResult::Applied {
                previous: expected,
                current: target,
            })
        } else {
            self.worker_transition_conflict(worker_id).await
        }
    }

    pub async fn claim_worker_completion(
        &self,
        worker_id: WorkerId,
        expected: WorkerLifecycle,
    ) -> crate::error::Result<WorkerTransitionResult> {
        self.transition_worker(worker_id, expected, WorkerLifecycle::Completing)
            .await
    }

    pub async fn read_worker_lifecycle(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<Option<WorkerLifecycle>> {
        let row = sqlx::query("SELECT lifecycle FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        row.map(|row| parse_lifecycle(&row, "lifecycle"))
            .transpose()
    }

    async fn worker_transition_conflict(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<WorkerTransitionResult> {
        let row = sqlx::query("SELECT lifecycle FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        match row {
            Some(row) => Ok(WorkerTransitionResult::Conflict {
                current: parse_lifecycle(&row, "lifecycle")?,
            }),
            None => Ok(WorkerTransitionResult::NotFound),
        }
    }

    pub async fn checkpoint_worker_transcript(
        &self,
        worker_id: WorkerId,
        expected: WorkerLifecycle,
        transcript: &[u8],
        tool_calls: i64,
    ) -> crate::error::Result<WorkerTranscriptCommit> {
        if expected.is_terminal() {
            return self.worker_transcript_conflict(worker_id).await;
        }
        let result = sqlx::query(
            "UPDATE worker_runs \
             SET transcript = ?, tool_calls = ?, transcript_version = transcript_version + 1 \
             WHERE id = ? AND lifecycle = ? AND completed_at IS NULL",
        )
        .bind(transcript)
        .bind(tool_calls)
        .bind(worker_id.to_string())
        .bind(expected.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
        if result.rows_affected() == 1 {
            let version = sqlx::query_scalar::<_, i64>(
                "SELECT transcript_version FROM worker_runs WHERE id = ?",
            )
            .bind(worker_id.to_string())
            .fetch_one(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
            Ok(WorkerTranscriptCommit::Applied {
                transcript_version: version,
            })
        } else {
            self.worker_transcript_conflict(worker_id).await
        }
    }

    async fn worker_transcript_conflict(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<WorkerTranscriptCommit> {
        let row = sqlx::query("SELECT lifecycle, transcript_version FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        match row {
            Some(row) => Ok(WorkerTranscriptCommit::Conflict {
                lifecycle: parse_lifecycle(&row, "lifecycle")?,
                transcript_version: row.try_get("transcript_version").unwrap_or(0),
            }),
            None => Ok(WorkerTranscriptCommit::NotFound),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete_worker(
        &self,
        worker_id: WorkerId,
        expected: WorkerLifecycle,
        outcome_kind: WorkerOutcomeKind,
        outcome_summary: Option<&str>,
        result: &str,
        transcript: Option<&[u8]>,
        tool_calls: i64,
        terminal_owner: WorkerTerminalOwner,
    ) -> crate::error::Result<WorkerCompletionCommit> {
        let terminal_lifecycle = outcome_kind.lifecycle();
        if !expected.can_transition_to(terminal_lifecycle) {
            return self.worker_completion_conflict(worker_id).await;
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        let transcript_increment = i64::from(transcript.is_some());
        let update = sqlx::query(
            "UPDATE worker_runs \
             SET lifecycle = ?, status = ?, outcome_kind = ?, outcome_summary = ?, result = ?, \
                 transcript = CASE WHEN ? IS NULL THEN transcript ELSE ? END, \
                  tool_calls = CASE WHEN ? IS NULL THEN tool_calls ELSE ? END, \
                  outcome_version = outcome_version + 1, \
                 transcript_version = transcript_version + ?, terminal_owner = ?, \
                 completed_at = CURRENT_TIMESTAMP \
             WHERE id = ? AND lifecycle = ? AND completed_at IS NULL AND outcome_version = 0",
        )
        .bind(terminal_lifecycle.as_str())
        .bind(terminal_lifecycle.display_status())
        .bind(outcome_kind.as_str())
        .bind(outcome_summary)
        .bind(result)
        .bind(transcript)
        .bind(transcript)
        .bind(transcript)
        .bind(tool_calls)
        .bind(transcript_increment)
        .bind(terminal_owner.as_str())
        .bind(worker_id.to_string())
        .bind(expected.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let existing = read_worker_terminal_in_transaction(&mut transaction, worker_id).await?;
        transaction
            .commit()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        if update.rows_affected() == 1 {
            Ok(WorkerCompletionCommit::Committed(existing.ok_or_else(
                || anyhow::anyhow!("can't complete worker: committed outcome is missing"),
            )?))
        } else if let Some(outcome) = existing {
            Ok(WorkerCompletionCommit::Existing(outcome))
        } else {
            self.worker_completion_conflict(worker_id).await
        }
    }

    pub async fn read_worker_terminal(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<Option<WorkerTerminalOutcome>> {
        let row = sqlx::query(&worker_terminal_select("WHERE id = ?"))
            .bind(worker_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        row.map(|row| worker_terminal_from_row(&row)).transpose()
    }

    pub async fn reconcile_worker_terminal_display(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<Option<WorkerTerminalOutcome>> {
        let Some(outcome) = self.read_worker_terminal(worker_id).await? else {
            return Ok(None);
        };
        sqlx::query("UPDATE worker_runs SET status = ? WHERE id = ? AND lifecycle = ?")
            .bind(outcome.lifecycle.display_status())
            .bind(worker_id.to_string())
            .bind(outcome.lifecycle.as_str())
            .execute(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(outcome))
    }

    async fn worker_completion_conflict(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<WorkerCompletionCommit> {
        if let Some(outcome) = self.read_worker_terminal(worker_id).await? {
            return Ok(WorkerCompletionCommit::Existing(outcome));
        }
        let row = sqlx::query("SELECT lifecycle FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        match row {
            Some(row) => Ok(WorkerCompletionCommit::Conflict {
                current: parse_lifecycle(&row, "lifecycle")?,
            }),
            None => Ok(WorkerCompletionCommit::NotFound),
        }
    }

    /// Record a worker completing with its result. Fire-and-forget.
    pub fn log_worker_completed(&self, worker_id: WorkerId, result: &str, success: bool) {
        let outcome = if success {
            WorkerOutcomeKind::Succeeded
        } else {
            WorkerOutcomeKind::Failed
        };
        self.log_worker_completed_with_outcome(
            worker_id,
            result,
            outcome,
            WorkerTerminalOwner::Worker,
        );
    }

    /// Record a worker as cancelled. Fire-and-forget.
    pub fn log_worker_cancelled(&self, worker_id: WorkerId, result: &str) {
        self.log_worker_completed_with_outcome(
            worker_id,
            result,
            WorkerOutcomeKind::Cancelled,
            WorkerTerminalOwner::Cancel,
        );
    }

    fn log_worker_completed_with_outcome(
        &self,
        worker_id: WorkerId,
        result: &str,
        outcome_kind: WorkerOutcomeKind,
        terminal_owner: WorkerTerminalOwner,
    ) {
        let logger = self.clone();
        let result = result.to_string();
        tokio::spawn(async move {
            if let Err(error) = logger
                .complete_worker_compat(worker_id, &result, outcome_kind, terminal_owner)
                .await
            {
                tracing::warn!(%error, %worker_id, "failed to persist worker completion");
            }
        });
    }

    async fn complete_worker_compat(
        &self,
        worker_id: WorkerId,
        result: &str,
        outcome_kind: WorkerOutcomeKind,
        terminal_owner: WorkerTerminalOwner,
    ) -> crate::error::Result<WorkerCompletionCommit> {
        let Some((current, tool_calls)) = self.read_worker_state(worker_id).await? else {
            return Ok(WorkerCompletionCommit::NotFound);
        };
        if current.is_terminal() {
            return self.worker_completion_conflict(worker_id).await;
        }

        let expected = if outcome_kind == WorkerOutcomeKind::Succeeded {
            if current != WorkerLifecycle::Completing {
                match self.claim_worker_completion(worker_id, current).await? {
                    WorkerTransitionResult::Applied { .. } => WorkerLifecycle::Completing,
                    WorkerTransitionResult::Conflict { current } if current.is_terminal() => {
                        return self.worker_completion_conflict(worker_id).await;
                    }
                    WorkerTransitionResult::Conflict { current } => current,
                    WorkerTransitionResult::NotFound => {
                        return Ok(WorkerCompletionCommit::NotFound);
                    }
                }
            } else {
                current
            }
        } else if outcome_kind == WorkerOutcomeKind::Cancelled
            && matches!(
                current,
                WorkerLifecycle::Running | WorkerLifecycle::WaitingForInput
            )
        {
            match self
                .transition_worker(worker_id, current, WorkerLifecycle::Cancelling)
                .await?
            {
                WorkerTransitionResult::Applied { .. } => WorkerLifecycle::Cancelling,
                WorkerTransitionResult::Conflict { current } if current.is_terminal() => {
                    return self.worker_completion_conflict(worker_id).await;
                }
                WorkerTransitionResult::Conflict { current } => current,
                WorkerTransitionResult::NotFound => return Ok(WorkerCompletionCommit::NotFound),
            }
        } else {
            current
        };

        self.complete_worker(
            worker_id,
            expected,
            outcome_kind,
            Some(result),
            result,
            None,
            tool_calls,
            terminal_owner,
        )
        .await
    }

    async fn read_worker_state(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<Option<(WorkerLifecycle, i64)>> {
        let row = sqlx::query("SELECT lifecycle, tool_calls FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        row.map(|row| {
            Ok((
                parse_lifecycle(&row, "lifecycle")?,
                row.try_get("tool_calls").unwrap_or(0),
            ))
        })
        .transpose()
    }

    /// Record OpenCode session metadata on a worker run. Fire-and-forget.
    ///
    /// Stores the session ID and server port so the frontend can construct
    /// an iframe URL to the embedded OpenCode web UI.
    ///
    /// The worker start event may not have been observed when this runs, so a
    /// zero-row update is retried with a short back-off.
    pub fn log_opencode_metadata(&self, worker_id: WorkerId, session_id: &str, port: u16) {
        let logger = self.clone();
        let id = worker_id.to_string();
        let session_id = session_id.to_string();

        tokio::spawn(async move {
            const MAX_RETRIES: u32 = 5;
            const BASE_DELAY_MS: u64 = 50;

            for attempt in 0..=MAX_RETRIES {
                match logger
                    .update_opencode_metadata(worker_id, &session_id, port)
                    .await
                {
                    Ok(true) => {
                        return; // Successfully updated.
                    }
                    Ok(false) => {
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

    /// Persist the provider session receipt for an OpenCode worker.
    ///
    /// Returns `false` when the worker row does not exist yet.
    pub async fn update_opencode_metadata(
        &self,
        worker_id: WorkerId,
        session_id: &str,
        port: u16,
    ) -> crate::error::Result<bool> {
        let result = sqlx::query(
            "UPDATE worker_runs SET opencode_session_id = ?, opencode_port = ? WHERE id = ?",
        )
        .bind(session_id)
        .bind(port as i32)
        .bind(worker_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(result.rows_affected() > 0)
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
        let workers = sqlx::query(
            "SELECT id, result, tool_calls FROM worker_runs \
             WHERE lifecycle = 'running' AND completed_at IS NULL \
                   AND (agent_id = ? OR agent_id IS NULL)",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut reconciled = 0;
        for worker in workers {
            let worker_id_text: String = worker.try_get("id").unwrap_or_default();
            let worker_id = worker_id_text.parse().map_err(|error| {
                anyhow::anyhow!("invalid persisted worker ID {worker_id_text}: {error}")
            })?;
            let result = worker
                .try_get::<Option<String>, _>("result")
                .unwrap_or(None)
                .filter(|result| !result.is_empty())
                .unwrap_or_else(|| failure_message.to_string());
            let tool_calls = worker.try_get("tool_calls").unwrap_or(0);
            if matches!(
                self.complete_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerOutcomeKind::Failed,
                    Some(failure_message),
                    &result,
                    None,
                    tool_calls,
                    WorkerTerminalOwner::Reconcile,
                )
                .await?,
                WorkerCompletionCommit::Committed(_)
            ) {
                reconciled += 1;
            }
        }
        Ok(reconciled)
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
             WHERE lifecycle = 'waiting_for_input' AND completed_at IS NULL AND interactive = TRUE \
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
        let row = sqlx::query("SELECT result, tool_calls FROM worker_runs WHERE id = ?")
            .bind(worker_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        let Some(row) = row else {
            return Ok(());
        };
        let result = row
            .try_get::<Option<String>, _>("result")
            .unwrap_or(None)
            .filter(|result| !result.is_empty())
            .unwrap_or_else(|| reason.to_string());
        let tool_calls = row.try_get("tool_calls").unwrap_or(0);
        let worker_id = worker_id
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid worker ID {worker_id}: {error}"))?;
        if matches!(
            self.claim_worker_completion(worker_id, WorkerLifecycle::WaitingForInput)
                .await?,
            WorkerTransitionResult::Applied { .. }
        ) {
            self.complete_worker(
                worker_id,
                WorkerLifecycle::Completing,
                WorkerOutcomeKind::Failed,
                Some(reason),
                &result,
                None,
                tool_calls,
                WorkerTerminalOwner::Reconcile,
            )
            .await?;
        }
        Ok(())
    }

    /// Retire an idle worker whose session can no longer be resumed.
    ///
    /// Marks the row as `done` (not `failed`) because the worker completed its
    /// work successfully — only the follow-up session expired. The existing
    /// result and transcript are preserved.
    pub async fn retire_idle_worker(&self, worker_id: &str) -> crate::error::Result<()> {
        let row = sqlx::query("SELECT result, tool_calls FROM worker_runs WHERE id = ?")
            .bind(worker_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        let Some(row) = row else {
            return Ok(());
        };
        let result = row
            .try_get::<Option<String>, _>("result")
            .unwrap_or(None)
            .unwrap_or_else(|| "Worker completed".to_string());
        let tool_calls = row.try_get("tool_calls").unwrap_or(0);
        let worker_id = worker_id
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid worker ID {worker_id}: {error}"))?;
        if matches!(
            self.claim_worker_completion(worker_id, WorkerLifecycle::WaitingForInput)
                .await?,
            WorkerTransitionResult::Applied { .. }
        ) {
            self.complete_worker(
                worker_id,
                WorkerLifecycle::Completing,
                WorkerOutcomeKind::Succeeded,
                Some(&result),
                &result,
                None,
                tool_calls,
                WorkerTerminalOwner::Reconcile,
            )
            .await?;
        }
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
        let row = sqlx::query("SELECT lifecycle FROM worker_runs WHERE id = ? AND channel_id = ?")
            .bind(worker_id.to_string())
            .bind(channel_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        if row.is_none_or(|row| {
            parse_lifecycle(&row, "lifecycle").ok() != Some(WorkerLifecycle::Running)
        }) {
            return Ok(false);
        }
        self.cancel_worker_from_running(worker_id).await
    }

    /// Mark a detached running worker (`channel_id IS NULL`) as cancelled.
    ///
    /// Used by API cancellation fallback when no in-memory channel state exists.
    pub async fn cancel_running_detached_worker(
        &self,
        worker_id: WorkerId,
    ) -> crate::error::Result<bool> {
        let row =
            sqlx::query("SELECT lifecycle FROM worker_runs WHERE id = ? AND channel_id IS NULL")
                .bind(worker_id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| anyhow::anyhow!(error))?;
        if row.is_none_or(|row| {
            parse_lifecycle(&row, "lifecycle").ok() != Some(WorkerLifecycle::Running)
        }) {
            return Ok(false);
        }
        self.cancel_worker_from_running(worker_id).await
    }

    async fn cancel_worker_from_running(&self, worker_id: WorkerId) -> crate::error::Result<bool> {
        if !matches!(
            self.transition_worker(
                worker_id,
                WorkerLifecycle::Running,
                WorkerLifecycle::Cancelling,
            )
            .await?,
            WorkerTransitionResult::Applied { .. }
        ) {
            return Ok(false);
        }
        Ok(matches!(
            self.complete_worker(
                worker_id,
                WorkerLifecycle::Cancelling,
                WorkerOutcomeKind::Cancelled,
                Some("Worker cancelled"),
                "Worker cancelled",
                None,
                0,
                WorkerTerminalOwner::Cancel,
            )
            .await?,
            WorkerCompletionCommit::Committed(_)
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
                     w.interactive, w.lifecycle, w.outcome_kind, w.outcome_version, \
                     w.transcript_version, w.run_id, w.origin_branch_id, w.terminal_owner, \
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
                lifecycle: row.try_get("lifecycle").unwrap_or_default(),
                outcome_kind: row.try_get("outcome_kind").ok().flatten(),
                outcome_version: row.try_get("outcome_version").unwrap_or(0),
                transcript_version: row.try_get("transcript_version").unwrap_or(0),
                run_id: row.try_get("run_id").ok().flatten(),
                origin_branch_id: row.try_get("origin_branch_id").ok().flatten(),
                terminal_owner: row.try_get("terminal_owner").ok().flatten(),
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
                     w.lifecycle, w.outcome_kind, w.outcome_version, w.transcript_version, \
                     w.run_id, w.origin_branch_id, w.terminal_owner, \
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
            lifecycle: row.try_get("lifecycle").unwrap_or_default(),
            outcome_kind: row.try_get("outcome_kind").ok().flatten(),
            outcome_version: row.try_get("outcome_version").unwrap_or(0),
            transcript_version: row.try_get("transcript_version").unwrap_or(0),
            run_id: row.try_get("run_id").ok().flatten(),
            origin_branch_id: row.try_get("origin_branch_id").ok().flatten(),
            terminal_owner: row.try_get("terminal_owner").ok().flatten(),
        }))
    }

    /// List branch and worker runs in a uniform shape, newest first.
    pub async fn list_process_runs(
        &self,
        agent_id: &str,
        limit: i64,
        offset: i64,
        status_filter: Option<&str>,
        kind_filter: Option<&str>,
    ) -> crate::error::Result<(Vec<ProcessRunRow>, i64)> {
        if kind_filter.is_some_and(|kind| !matches!(kind, "branch" | "worker")) {
            return Err(
                anyhow::anyhow!("can't list processes: kind must be branch or worker").into(),
            );
        }

        let union = "SELECT 'worker' AS kind, w.id, w.task AS input, w.result AS output, \
                            w.status, w.worker_type AS process_type, NULL AS profile, \
                            w.channel_id, c.display_name AS channel_name, w.started_at, \
                            w.completed_at, w.transcript IS NOT NULL AS has_transcript, \
                            w.tool_calls, NULL AS model, NULL AS max_turns, \
                            w.opencode_session_id, w.opencode_port, w.directory, \
                             w.interactive, w.project_id, w.lifecycle, w.outcome_kind, \
                             w.outcome_version, w.transcript_version, w.run_id, \
                             w.origin_branch_id, w.terminal_owner \
                     FROM worker_runs w \
                     LEFT JOIN channels c ON w.channel_id = c.id \
                     WHERE w.agent_id = ?1 \
                     UNION ALL \
                     SELECT 'branch' AS kind, b.id, b.input, b.conclusion AS output, \
                            b.status, b.profile AS process_type, b.profile, \
                            b.channel_id, c.display_name AS channel_name, b.started_at, \
                            b.completed_at, b.transcript IS NOT NULL AS has_transcript, \
                            b.tool_calls, b.model, b.max_turns, \
                            NULL AS opencode_session_id, NULL AS opencode_port, \
                             NULL AS directory, 0 AS interactive, NULL AS project_id, \
                             NULL AS lifecycle, NULL AS outcome_kind, 0 AS outcome_version, \
                             0 AS transcript_version, b.run_id, b.origin_branch_id, \
                             NULL AS terminal_owner \
                     FROM branch_runs b \
                     LEFT JOIN channels c ON b.channel_id = c.id";
        let filters = "WHERE (?2 IS NULL OR status = ?2) AND (?3 IS NULL OR kind = ?3)";
        let count_query = format!("SELECT COUNT(*) AS total FROM ({union}) processes {filters}");
        let list_query = format!(
            "SELECT * FROM ({union}) processes {filters} \
             ORDER BY started_at DESC LIMIT ?4 OFFSET ?5"
        );

        let total = sqlx::query(&count_query)
            .bind(agent_id)
            .bind(status_filter)
            .bind(kind_filter)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .try_get("total")
            .unwrap_or(0);
        let rows = sqlx::query(&list_query)
            .bind(agent_id)
            .bind(status_filter)
            .bind(kind_filter)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .into_iter()
            .map(|row| process_run_from_row(&row))
            .collect::<Vec<_>>();

        Ok((rows, total))
    }

    /// Get one branch or worker run, including its compressed transcript.
    pub async fn get_process_detail(
        &self,
        agent_id: &str,
        kind: &str,
        process_id: &str,
    ) -> crate::error::Result<Option<ProcessDetailRow>> {
        let query = match kind {
            "worker" => {
                "SELECT 'worker' AS kind, w.id, w.task AS input, w.result AS output, \
                        w.status, w.worker_type AS process_type, NULL AS profile, \
                        w.channel_id, c.display_name AS channel_name, w.started_at, \
                        w.completed_at, w.transcript, w.transcript IS NOT NULL AS has_transcript, \
                        w.tool_calls, NULL AS model, \
                        NULL AS max_turns, w.opencode_session_id, w.opencode_port, \
                        w.directory, w.interactive, w.project_id, w.lifecycle, w.outcome_kind, \
                        w.outcome_version, w.transcript_version, w.run_id, w.origin_branch_id, \
                        w.terminal_owner \
                 FROM worker_runs w LEFT JOIN channels c ON w.channel_id = c.id \
                 WHERE w.agent_id = ?1 AND w.id = ?2"
            }
            "branch" => {
                "SELECT 'branch' AS kind, b.id, b.input, b.conclusion AS output, \
                        b.status, b.profile AS process_type, b.profile, \
                        b.channel_id, c.display_name AS channel_name, b.started_at, \
                        b.completed_at, b.transcript, b.transcript IS NOT NULL AS has_transcript, \
                        b.tool_calls, b.model, \
                        b.max_turns, NULL AS opencode_session_id, NULL AS opencode_port, \
                        NULL AS directory, 0 AS interactive, NULL AS project_id, \
                        NULL AS lifecycle, NULL AS outcome_kind, 0 AS outcome_version, \
                        0 AS transcript_version, b.run_id, b.origin_branch_id, \
                        NULL AS terminal_owner \
                 FROM branch_runs b LEFT JOIN channels c ON b.channel_id = c.id \
                 WHERE ?1 IS NOT NULL AND b.id = ?2"
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "can't get process detail: kind must be branch or worker"
                )
                .into());
            }
        };

        let row = sqlx::query(query)
            .bind(agent_id)
            .bind(process_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.map(|row| ProcessDetailRow {
            run: process_run_from_row(&row),
            transcript_blob: row
                .try_get::<Option<Vec<u8>>, _>("transcript")
                .unwrap_or(None)
                .filter(|blob| !blob.is_empty()),
        }))
    }
}

fn worker_terminal_select(filter: &str) -> String {
    format!(
        "SELECT id, lifecycle, outcome_kind, outcome_summary, result, outcome_version, \
                transcript_version, terminal_owner, completed_at \
         FROM worker_runs {filter} AND completed_at IS NOT NULL AND outcome_version > 0"
    )
}

async fn read_worker_terminal_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    worker_id: WorkerId,
) -> crate::error::Result<Option<WorkerTerminalOutcome>> {
    let row = sqlx::query(&worker_terminal_select("WHERE id = ?"))
        .bind(worker_id.to_string())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    row.map(|row| worker_terminal_from_row(&row)).transpose()
}

fn worker_terminal_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> crate::error::Result<WorkerTerminalOutcome> {
    let lifecycle = parse_lifecycle(row, "lifecycle")?;
    if !lifecycle.is_terminal() {
        return Err(anyhow::anyhow!(
            "can't read worker outcome: lifecycle {} is not terminal",
            lifecycle.as_str()
        )
        .into());
    }
    let outcome_kind = row
        .try_get::<String, _>("outcome_kind")
        .map_err(|error| anyhow::anyhow!(error))?
        .parse()
        .map_err(crate::error::Error::from)?;
    let terminal_owner = row
        .try_get::<Option<String>, _>("terminal_owner")
        .unwrap_or(None)
        .map(|owner| owner.parse())
        .transpose()
        .map_err(crate::error::Error::from)?;
    let completed_at = row
        .try_get::<chrono::DateTime<chrono::Utc>, _>("completed_at")
        .map(|time| time.to_rfc3339())
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(WorkerTerminalOutcome {
        worker_id: row.try_get("id").unwrap_or_default(),
        lifecycle,
        outcome_kind,
        outcome_summary: row.try_get("outcome_summary").ok().flatten(),
        result: row.try_get("result").unwrap_or_default(),
        outcome_version: row.try_get("outcome_version").unwrap_or(0),
        transcript_version: row.try_get("transcript_version").unwrap_or(0),
        terminal_owner,
        completed_at,
    })
}

fn parse_lifecycle(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> crate::error::Result<WorkerLifecycle> {
    row.try_get::<String, _>(column)
        .map_err(|error| anyhow::anyhow!(error))?
        .parse()
        .map_err(crate::error::Error::from)
}

fn process_run_from_row(row: &sqlx::sqlite::SqliteRow) -> ProcessRunRow {
    ProcessRunRow {
        kind: row.try_get("kind").unwrap_or_default(),
        id: row.try_get("id").unwrap_or_default(),
        input: row.try_get("input").unwrap_or_default(),
        output: row.try_get("output").ok().flatten(),
        status: row.try_get("status").unwrap_or_default(),
        process_type: row.try_get("process_type").unwrap_or_default(),
        profile: row.try_get("profile").ok().flatten(),
        channel_id: row.try_get("channel_id").ok().flatten(),
        channel_name: row.try_get("channel_name").ok().flatten(),
        started_at: row
            .try_get::<chrono::DateTime<chrono::Utc>, _>("started_at")
            .map(|time| time.to_rfc3339())
            .unwrap_or_default(),
        completed_at: row
            .try_get::<chrono::DateTime<chrono::Utc>, _>("completed_at")
            .ok()
            .map(|time| time.to_rfc3339()),
        has_transcript: row.try_get("has_transcript").unwrap_or(false),
        tool_calls: row.try_get("tool_calls").unwrap_or(0),
        model: row.try_get("model").ok().flatten(),
        max_turns: row.try_get("max_turns").ok().flatten(),
        opencode_session_id: row.try_get("opencode_session_id").ok().flatten(),
        opencode_port: row.try_get("opencode_port").ok().flatten(),
        directory: row.try_get("directory").ok().flatten(),
        interactive: row.try_get("interactive").unwrap_or(false),
        project_id: row.try_get("project_id").ok().flatten(),
        lifecycle: row.try_get("lifecycle").ok().flatten(),
        outcome_kind: row.try_get("outcome_kind").ok().flatten(),
        outcome_version: row.try_get("outcome_version").unwrap_or(0),
        transcript_version: row.try_get("transcript_version").unwrap_or(0),
        run_id: row.try_get("run_id").ok().flatten(),
        origin_branch_id: row.try_get("origin_branch_id").ok().flatten(),
        terminal_owner: row.try_get("terminal_owner").ok().flatten(),
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
    pub opencode_port: Option<i32>,
    pub opencode_session_id: Option<String>,
    pub directory: Option<String>,
    pub interactive: bool,
    pub lifecycle: String,
    pub outcome_kind: Option<String>,
    pub outcome_version: i64,
    pub transcript_version: i64,
    pub run_id: Option<String>,
    pub origin_branch_id: Option<String>,
    pub terminal_owner: Option<String>,
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
    pub lifecycle: String,
    pub outcome_kind: Option<String>,
    pub outcome_version: i64,
    pub transcript_version: i64,
    pub run_id: Option<String>,
    pub origin_branch_id: Option<String>,
    pub terminal_owner: Option<String>,
}

/// A branch or worker run without its transcript blob.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessRunRow {
    pub kind: String,
    pub id: String,
    pub input: String,
    pub output: Option<String>,
    pub status: String,
    pub process_type: String,
    pub profile: Option<String>,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub has_transcript: bool,
    pub tool_calls: i64,
    pub model: Option<String>,
    pub max_turns: Option<i64>,
    pub opencode_session_id: Option<String>,
    pub opencode_port: Option<i32>,
    pub directory: Option<String>,
    pub interactive: bool,
    pub project_id: Option<String>,
    pub lifecycle: Option<String>,
    pub outcome_kind: Option<String>,
    pub outcome_version: i64,
    pub transcript_version: i64,
    pub run_id: Option<String>,
    pub origin_branch_id: Option<String>,
    pub terminal_owner: Option<String>,
}

/// A branch or worker run with its compressed transcript blob.
#[derive(Debug, Clone)]
pub struct ProcessDetailRow {
    pub run: ProcessRunRow,
    pub transcript_blob: Option<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::{
        ProcessRunLogger, TimelineCursor, TimelineItem, WorkerCompletionCommit, WorkerLifecycle,
        WorkerOutcomeKind, WorkerStartResult, WorkerTerminalOwner, WorkerTransitionResult,
        timeline_item_id,
    };
    use crate::conversation::worker_transcript::{ActionContent, TranscriptStep};
    use sqlx::Row as _;
    use std::sync::Arc;
    use tokio::sync::Barrier;

    async fn setup_worker_runs_table() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("failed to create sqlite memory pool");
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        for channel_id in ["ch-1", "channel-1"] {
            sqlx::query("INSERT INTO channels (id, platform) VALUES (?, 'test')")
                .bind(channel_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    async fn setup_process_runs_tables() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("failed to create sqlite memory pool");
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO channels (id, platform, display_name) \
             VALUES ('channel-1', 'test', 'Test Channel')",
        )
        .execute(&pool)
        .await
        .expect("failed to insert channel");
        pool
    }

    async fn start_worker(logger: &ProcessRunLogger, worker_id: uuid::Uuid, interactive: bool) {
        let agent_id: crate::AgentId = Arc::from("agent-1");
        assert_eq!(
            logger
                .log_worker_started(
                    None,
                    worker_id,
                    "test task",
                    "builtin",
                    &agent_id,
                    interactive,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap(),
            WorkerStartResult::Started
        );
    }

    async fn complete_succeeded(
        logger: &ProcessRunLogger,
        worker_id: uuid::Uuid,
        result: &str,
    ) -> WorkerCompletionCommit {
        assert!(matches!(
            logger
                .claim_worker_completion(worker_id, WorkerLifecycle::Running)
                .await
                .unwrap(),
            WorkerTransitionResult::Applied {
                current: WorkerLifecycle::Completing,
                ..
            }
        ));
        logger
            .complete_worker(
                worker_id,
                WorkerLifecycle::Completing,
                WorkerOutcomeKind::Succeeded,
                Some(result),
                result,
                Some(b"transcript"),
                3,
                WorkerTerminalOwner::Worker,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn opencode_session_metadata_is_persisted_by_worker_id() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        assert!(
            !logger
                .update_opencode_metadata(worker_id, "session-1", 12_345)
                .await
                .unwrap()
        );

        start_worker(&logger, worker_id, true).await;
        assert!(
            logger
                .update_opencode_metadata(worker_id, "session-1", 12_345)
                .await
                .unwrap()
        );

        let row =
            sqlx::query("SELECT opencode_session_id, opencode_port FROM worker_runs WHERE id = ?")
                .bind(worker_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            row.try_get::<String, _>("opencode_session_id").unwrap(),
            "session-1"
        );
        assert_eq!(row.try_get::<i64, _>("opencode_port").unwrap(), 12_345);
    }

    #[tokio::test]
    async fn running_completing_succeeded_commits_terminal_payload() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id, false).await;

        let commit = complete_succeeded(&logger, worker_id, "finished").await;
        let WorkerCompletionCommit::Committed(outcome) = commit else {
            panic!("expected committed outcome, got {commit:?}");
        };
        assert_eq!(outcome.lifecycle, WorkerLifecycle::Succeeded);
        assert_eq!(outcome.outcome_kind, WorkerOutcomeKind::Succeeded);
        assert_eq!(outcome.outcome_version, 1);
        assert_eq!(outcome.transcript_version, 1);
        assert_eq!(outcome.terminal_owner, Some(WorkerTerminalOwner::Worker));

        let row = sqlx::query("SELECT status, tool_calls FROM worker_runs WHERE id = ?")
            .bind(worker_id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.try_get::<String, _>("status").unwrap(), "done");
        assert_eq!(row.try_get::<i64, _>("tool_calls").unwrap(), 3);
    }

    #[tokio::test]
    async fn running_cancelling_cancelled_commits_terminal_payload() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id, false).await;

        assert!(matches!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerLifecycle::Cancelling,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::Applied { .. }
        ));
        let commit = logger
            .complete_worker(
                worker_id,
                WorkerLifecycle::Cancelling,
                WorkerOutcomeKind::Cancelled,
                Some("operator request"),
                "Worker cancelled: operator request",
                None,
                0,
                WorkerTerminalOwner::Cancel,
            )
            .await
            .unwrap();
        assert!(matches!(
            commit,
            WorkerCompletionCommit::Committed(ref outcome)
                if outcome.lifecycle == WorkerLifecycle::Cancelled
                    && outcome.outcome_version == 1
        ));
    }

    #[tokio::test]
    async fn waiting_for_input_resumes_running() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id, true).await;

        assert!(matches!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerLifecycle::WaitingForInput,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::Applied { .. }
        ));
        assert_eq!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::WaitingForInput,
                    WorkerLifecycle::Running,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::Applied {
                previous: WorkerLifecycle::WaitingForInput,
                current: WorkerLifecycle::Running,
            }
        );
    }

    #[tokio::test]
    async fn illegal_transition_returns_conflict() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id, false).await;

        assert_eq!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerLifecycle::Created,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::Conflict {
                current: WorkerLifecycle::Running,
            }
        );
    }

    #[tokio::test]
    async fn delayed_idle_and_resume_cannot_overwrite_terminal_outcomes() {
        for (terminal, terminal_kind, expected_source) in [
            (
                WorkerLifecycle::Succeeded,
                WorkerOutcomeKind::Succeeded,
                WorkerLifecycle::Completing,
            ),
            (
                WorkerLifecycle::Cancelled,
                WorkerOutcomeKind::Cancelled,
                WorkerLifecycle::Cancelling,
            ),
        ] {
            let pool = setup_worker_runs_table().await;
            let logger = ProcessRunLogger::new(pool);
            let worker_id = uuid::Uuid::new_v4();
            start_worker(&logger, worker_id, false).await;
            let intermediate = if terminal == WorkerLifecycle::Succeeded {
                WorkerLifecycle::Completing
            } else {
                WorkerLifecycle::Cancelling
            };
            assert!(matches!(
                logger
                    .transition_worker(worker_id, WorkerLifecycle::Running, intermediate)
                    .await
                    .unwrap(),
                WorkerTransitionResult::Applied { .. }
            ));
            logger
                .complete_worker(
                    worker_id,
                    expected_source,
                    terminal_kind,
                    Some("first"),
                    "first",
                    None,
                    0,
                    WorkerTerminalOwner::Worker,
                )
                .await
                .unwrap();

            assert_eq!(
                logger
                    .transition_worker(
                        worker_id,
                        WorkerLifecycle::Running,
                        WorkerLifecycle::WaitingForInput,
                    )
                    .await
                    .unwrap(),
                WorkerTransitionResult::Conflict { current: terminal }
            );
            assert_eq!(
                logger
                    .transition_worker(
                        worker_id,
                        WorkerLifecycle::WaitingForInput,
                        WorkerLifecycle::Running,
                    )
                    .await
                    .unwrap(),
                WorkerTransitionResult::Conflict { current: terminal }
            );
        }
    }

    #[tokio::test]
    async fn duplicate_terminal_commit_preserves_first_outcome_and_version() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id, false).await;
        let first = complete_succeeded(&logger, worker_id, "first").await;
        assert!(matches!(first, WorkerCompletionCommit::Committed(_)));

        let duplicate = logger
            .complete_worker(
                worker_id,
                WorkerLifecycle::Completing,
                WorkerOutcomeKind::Failed,
                Some("second"),
                "second",
                Some(b"replacement"),
                9,
                WorkerTerminalOwner::Supervisor,
            )
            .await
            .unwrap();
        let WorkerCompletionCommit::Existing(outcome) = duplicate else {
            panic!("expected existing outcome, got {duplicate:?}");
        };
        assert_eq!(outcome.result, "first");
        assert_eq!(outcome.outcome_version, 1);
        assert_eq!(outcome.transcript_version, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_and_completion_competing_commits_choose_one_terminal_outcome() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect("sqlite::memory:?cache=shared")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        start_worker(&logger, worker_id, false).await;
        assert!(matches!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerLifecycle::TimingOut,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::Applied { .. }
        ));
        let barrier = Arc::new(Barrier::new(3));

        let completion_logger = logger.clone();
        let completion_barrier = barrier.clone();
        let completion = tokio::spawn(async move {
            completion_barrier.wait().await;
            completion_logger
                .complete_worker(
                    worker_id,
                    WorkerLifecycle::TimingOut,
                    WorkerOutcomeKind::Succeeded,
                    Some("completed before timeout committed"),
                    "completed before timeout committed",
                    None,
                    1,
                    WorkerTerminalOwner::Worker,
                )
                .await
                .unwrap()
        });
        let timeout_logger = logger.clone();
        let timeout_barrier = barrier.clone();
        let timeout = tokio::spawn(async move {
            timeout_barrier.wait().await;
            timeout_logger
                .complete_worker(
                    worker_id,
                    WorkerLifecycle::TimingOut,
                    WorkerOutcomeKind::TimedOut,
                    Some("timed out"),
                    "timed out",
                    None,
                    1,
                    WorkerTerminalOwner::Timeout,
                )
                .await
                .unwrap()
        });
        barrier.wait().await;
        let completion_result = completion.await.unwrap();
        let timeout_result = timeout.await.unwrap();
        assert!(matches!(
            completion_result,
            WorkerCompletionCommit::Committed(_) | WorkerCompletionCommit::Existing(_)
        ));
        assert!(matches!(
            timeout_result,
            WorkerCompletionCommit::Committed(_) | WorkerCompletionCommit::Existing(_)
        ));
        let outcome = logger
            .read_worker_terminal(worker_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.outcome_version, 1);
        assert!(matches!(
            outcome.lifecycle,
            WorkerLifecycle::Succeeded | WorkerLifecycle::TimedOut
        ));
    }

    #[tokio::test]
    async fn missing_worker_results_are_explicit() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool);
        let worker_id = uuid::Uuid::new_v4();
        assert_eq!(
            logger
                .transition_worker(
                    worker_id,
                    WorkerLifecycle::Running,
                    WorkerLifecycle::WaitingForInput,
                )
                .await
                .unwrap(),
            WorkerTransitionResult::NotFound
        );
        assert!(matches!(
            logger
                .complete_worker(
                    worker_id,
                    WorkerLifecycle::Completing,
                    WorkerOutcomeKind::Succeeded,
                    None,
                    "missing",
                    None,
                    0,
                    WorkerTerminalOwner::Worker,
                )
                .await
                .unwrap(),
            WorkerCompletionCommit::NotFound
        ));
    }

    #[tokio::test]
    async fn worker_start_ownership_round_trip() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();
        let branch_id = uuid::Uuid::new_v4();
        let agent_id: crate::AgentId = Arc::from("agent-1");
        assert_eq!(
            logger
                .log_worker_started(
                    None,
                    worker_id,
                    "owned task",
                    "builtin",
                    &agent_id,
                    false,
                    None,
                    Some("run-7"),
                    Some(branch_id),
                )
                .await
                .unwrap(),
            WorkerStartResult::Started
        );

        let row =
            sqlx::query("SELECT lifecycle, run_id, origin_branch_id FROM worker_runs WHERE id = ?")
                .bind(worker_id.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.try_get::<String, _>("lifecycle").unwrap(), "running");
        assert_eq!(row.try_get::<String, _>("run_id").unwrap(), "run-7");
        assert_eq!(
            row.try_get::<String, _>("origin_branch_id").unwrap(),
            branch_id.to_string()
        );
        assert_eq!(
            logger.worker_for_origin_branch(branch_id).await.unwrap(),
            Some((worker_id, "owned task".to_string(), false))
        );
    }

    #[tokio::test]
    async fn branch_detail_round_trip_includes_transcript_and_metadata() {
        let pool = setup_process_runs_tables().await;
        let logger = ProcessRunLogger::new(pool);
        let branch_id = uuid::Uuid::new_v4();
        let channel_id: crate::ChannelId = Arc::from("channel-1");
        logger
            .log_branch_started(
                &channel_id,
                branch_id,
                "Investigating",
                "actual branch prompt",
                "memory_persistence",
                "claude-test",
                12,
                Some("run-7"),
            )
            .await
            .unwrap();
        let steps = vec![TranscriptStep::Action {
            content: vec![
                ActionContent::Text {
                    text: "thinking".to_string(),
                },
                ActionContent::ToolCall {
                    id: "call-1".to_string(),
                    name: "memory_recall".to_string(),
                    args: "{}".to_string(),
                },
            ],
        }];
        let transcript = crate::conversation::worker_transcript::serialize_steps(&steps);
        assert!(
            logger
                .log_branch_terminal(branch_id, "finished", "done", Some(&transcript), 1,)
                .await
                .unwrap()
        );

        let detail = logger
            .get_process_detail("agent", "branch", &branch_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.run.kind, "branch");
        assert_eq!(detail.run.input, "actual branch prompt");
        assert_eq!(detail.run.output.as_deref(), Some("finished"));
        assert_eq!(detail.run.status, "done");
        assert_eq!(detail.run.profile.as_deref(), Some("memory_persistence"));
        assert_eq!(detail.run.model.as_deref(), Some("claude-test"));
        assert_eq!(detail.run.max_turns, Some(12));
        assert_eq!(detail.run.run_id.as_deref(), Some("run-7"));
        assert_eq!(detail.run.tool_calls, 1);
        assert_eq!(detail.run.channel_name.as_deref(), Some("Test Channel"));
        let restored = crate::conversation::worker_transcript::deserialize_transcript(
            detail.transcript_blob.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(restored.len(), 1);
    }

    #[tokio::test]
    async fn duplicate_branch_terminal_write_preserves_first_outcome() {
        let pool = setup_process_runs_tables().await;
        let logger = ProcessRunLogger::new(pool);
        let branch_id = uuid::Uuid::new_v4();
        let channel_id: crate::ChannelId = Arc::from("channel-1");
        logger
            .log_branch_started(
                &channel_id,
                branch_id,
                "Branch",
                "prompt",
                "default",
                "model",
                10,
                None,
            )
            .await
            .unwrap();

        assert!(
            logger
                .log_branch_terminal(branch_id, "cancelled first", "cancelled", None, 0)
                .await
                .unwrap()
        );
        assert!(
            !logger
                .log_branch_terminal(branch_id, "completed late", "done", None, 3)
                .await
                .unwrap()
        );

        let detail = logger
            .get_process_detail("agent", "branch", &branch_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.run.status, "cancelled");
        assert_eq!(detail.run.output.as_deref(), Some("cancelled first"));
        assert_eq!(detail.run.tool_calls, 0);
        assert!(detail.run.completed_at.is_some());
    }

    #[tokio::test]
    async fn cancel_running_detached_worker_updates_null_channel_rows() {
        let pool = setup_worker_runs_table().await;
        let logger = ProcessRunLogger::new(pool.clone());
        let worker_id = uuid::Uuid::new_v4();

        sqlx::query("INSERT INTO worker_runs (id, channel_id, task, status, lifecycle, result) VALUES (?, NULL, 'task', 'running', 'running', '')")
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

        sqlx::query("INSERT INTO worker_runs (id, channel_id, task, status, lifecycle, result) VALUES (?, 'ch-1', 'task', 'running', 'running', '')")
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

        sqlx::query("INSERT INTO worker_runs (id, channel_id, task, status, lifecycle, result) VALUES (?, 'ch-1', 'task', 'running', 'running', '')")
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

        sqlx::query("INSERT INTO worker_runs (id, channel_id, task, status, lifecycle, result) VALUES (?, 'ch-1', 'task', 'running', 'running', '')")
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

        sqlx::query("INSERT INTO worker_runs (id, channel_id, task, status, lifecycle, result) VALUES (?, 'ch-1', 'task', 'running', 'running', '')")
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
            "INSERT INTO worker_runs (id, channel_id, task, status, lifecycle, result) VALUES (?, 'channel-1', 'task', 'running', 'running', '')",
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
