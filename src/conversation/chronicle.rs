//! Session chronicle persistence (SQLite).
//!
//! A chronicle is an append-only sequence of checkpoints over a channel's
//! conversation log. Each checkpoint summarizes exactly the span since the
//! previous one, so coverage is contiguous and no span is ever summarized
//! twice. Boundaries are anchored to `conversation_messages` rather than to a
//! channel's in-memory history, because the log is what survives restart and
//! what a later expansion can be resolved against.
//!
//! Ordering is `conversation_messages.seq` everywhere — cut selection,
//! boundary comparison, and expansion all use it. `seq` is assigned per
//! channel at INSERT time, so it is insertion order, not wall-clock order.
//! That matters because `ConversationLogger` writes are detached tasks: a row
//! written after a boundary was committed can carry the same whole-second
//! `created_at` and a lexically smaller id, and under a `(created_at, id)`
//! key it would sort behind the boundary and never be selected again.

use crate::conversation::history::ConversationMessage;
use crate::error::Result;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Row as _, SqlitePool};

/// Timestamp format matching SQLite's `CURRENT_TIMESTAMP`, so bound values and
/// stored values compare identically under `datetime()`.
const SQL_TIMESTAMP: &str = "%Y-%m-%d %H:%M:%S";

/// Render a timestamp in the same shape SQLite writes for `CURRENT_TIMESTAMP`.
pub fn sql_timestamp(at: DateTime<Utc>) -> String {
    at.format(SQL_TIMESTAMP).to_string()
}

/// Why a checkpoint was cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// The configured message or token interval elapsed.
    Interval,
    /// The first checkpoint for a channel that already had history.
    Bootstrap,
    /// Context pressure forced a cut before the interval elapsed.
    Pressure,
    /// Emergency truncation discarded the span without summarizing it.
    Emergency,
    /// A higher-level summary over a contiguous run of checkpoints.
    Rollup,
}

impl CheckpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckpointKind::Interval => "interval",
            CheckpointKind::Bootstrap => "bootstrap",
            CheckpointKind::Pressure => "pressure",
            CheckpointKind::Emergency => "emergency",
            CheckpointKind::Rollup => "rollup",
        }
    }

    /// Map the stored column value back to a kind, defaulting to `Interval`
    /// for anything a future version wrote that this one does not know.
    pub fn from_db(value: &str) -> Self {
        match value {
            "bootstrap" => CheckpointKind::Bootstrap,
            "pressure" => CheckpointKind::Pressure,
            "emergency" => CheckpointKind::Emergency,
            "rollup" => CheckpointKind::Rollup,
            _ => CheckpointKind::Interval,
        }
    }
}

/// One end of a coverage range: a position in the durable per-channel
/// `conversation_messages.seq` order.
///
/// `seq` is assigned at INSERT time, so a message written after a boundary was
/// committed always sorts after it. That is what makes "everything after the
/// boundary" exact even though `ConversationLogger` writes are detached tasks
/// whose wall-clock timestamps can collide within a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChronicleBoundary {
    pub seq: i64,
}

impl ChronicleBoundary {
    pub fn new(seq: i64) -> Self {
        Self { seq }
    }

    /// The boundary a channel with no prior chronicle starts from: before the
    /// first message, which is assigned seq 1.
    pub fn origin() -> Self {
        Self { seq: 0 }
    }
}

/// A committed checkpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ChronicleCheckpoint {
    pub id: String,
    pub channel_id: String,
    pub seq: i64,
    pub level: i64,
    pub kind: CheckpointKind,
    pub title: String,
    pub summary: String,
    pub covers_from_at: DateTime<Utc>,
    pub covers_to_at: DateTime<Utc>,
    pub covers_from_message_id: Option<String>,
    pub covers_to_message_id: Option<String>,
    pub covers_from_seq: i64,
    pub covers_to_seq: i64,
    pub message_count: i64,
    pub token_estimate: i64,
    pub rolled_up_into: Option<String>,
    pub rolls_up_from_seq: Option<i64>,
    pub rolls_up_to_seq: Option<i64>,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ChronicleCheckpoint {
    /// The boundary a following checkpoint must start from.
    pub fn end_boundary(&self) -> ChronicleBoundary {
        ChronicleBoundary::new(self.covers_to_seq)
    }

    /// The boundary this checkpoint's own coverage opens at.
    pub fn start_boundary(&self) -> ChronicleBoundary {
        ChronicleBoundary::new(self.covers_from_seq)
    }
}

/// A checkpoint about to be committed.
#[derive(Debug, Clone)]
pub struct NewCheckpoint {
    pub channel_id: String,
    pub level: i64,
    pub kind: CheckpointKind,
    pub title: String,
    pub summary: String,
    pub covers_from: ChronicleBoundary,
    pub covers_to: ChronicleBoundary,
    /// Display range for the covered span. Selection uses the boundaries.
    pub covers_from_at: DateTime<Utc>,
    pub covers_to_at: DateTime<Utc>,
    pub covers_from_message_id: Option<String>,
    pub covers_to_message_id: Option<String>,
    pub message_count: i64,
    pub token_estimate: i64,
    pub rolls_up_from_seq: Option<i64>,
    pub rolls_up_to_seq: Option<i64>,
    pub model: Option<String>,
}

/// What happened to a commit attempt.
#[derive(Debug)]
pub enum CommitOutcome {
    Committed(Box<ChronicleCheckpoint>),
    /// The tail moved while this cut was being summarized, so its start
    /// boundary no longer joins the last committed checkpoint. The span stays
    /// unsummarized and the next cut covers it.
    Superseded {
        expected: i64,
        found: i64,
    },
    /// The write lock could not be taken within the retry budget. The span
    /// stays unsummarized and the next cut covers it.
    Busy,
}

/// Aggregate chronicle state for a channel, used to build the prompt header.
#[derive(Debug, Clone, Default)]
pub struct ChronicleStats {
    pub checkpoint_count: i64,
    pub interval_count: i64,
    pub rollup_count: i64,
    pub total_messages: i64,
    pub first_message_at: Option<DateTime<Utc>>,
    pub last_message_at: Option<DateTime<Utc>>,
    /// Messages logged after the newest checkpoint's end boundary.
    pub unsummarized_messages: i64,
}

/// Reads and writes chronicle checkpoints.
///
/// Unlike `ConversationLogger`, commits are awaited rather than fire-and-forget:
/// the boundary read and the insert have to be one transaction for coverage to
/// stay contiguous.
#[derive(Debug, Clone)]
pub struct ChronicleStore {
    pool: SqlitePool,
}

impl ChronicleStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The backing pool, so a test can reopen a store the way a restart would.
    #[cfg(test)]
    pub fn pool_for_tests(&self) -> &SqlitePool {
        &self.pool
    }

    /// The newest checkpoint at a level, or `None` when the chronicle is empty.
    pub async fn latest(
        &self,
        channel_id: &str,
        level: i64,
    ) -> Result<Option<ChronicleCheckpoint>> {
        let row = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints \
             WHERE channel_id = ? AND level = ? \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(channel_id)
        .bind(level)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.as_ref().and_then(|row| match checkpoint_from_row(row) {
            Ok(checkpoint) => Some(checkpoint),
            Err(error) => {
                tracing::warn!(%error, "skipping undecodable chronicle checkpoint row");
                None
            }
        }))
    }

    /// Commit a checkpoint, rejecting it if the tail moved underneath it.
    ///
    /// Runs under `BEGIN IMMEDIATE` so the write lock is taken before the
    /// boundary is read — a deferred transaction would read the boundary, then
    /// fail on lock upgrade at INSERT time, which SQLite reports as
    /// `SQLITE_BUSY` rather than a constraint violation. Busy conflicts are
    /// retried a bounded number of times and then reported as
    /// [`CommitOutcome::Busy`]; the span simply stays unsummarized.
    pub async fn commit(&self, new: NewCheckpoint) -> Result<CommitOutcome> {
        const MAX_BUSY_RETRIES: u32 = 4;
        const BASE_BUSY_DELAY_MS: u64 = 25;

        for attempt in 0..=MAX_BUSY_RETRIES {
            match self.try_commit(&new).await {
                Ok(outcome) => return Ok(outcome),
                Err(error) if is_busy(&error) && attempt < MAX_BUSY_RETRIES => {
                    let delay = BASE_BUSY_DELAY_MS * 2u64.pow(attempt);
                    tracing::debug!(
                        channel_id = %new.channel_id,
                        attempt,
                        delay_ms = delay,
                        "chronicle commit hit a write-lock conflict; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Err(error) if is_busy(&error) => {
                    tracing::warn!(
                        channel_id = %new.channel_id,
                        "chronicle commit gave up after {MAX_BUSY_RETRIES} write-lock conflicts"
                    );
                    return Ok(CommitOutcome::Busy);
                }
                Err(error) => return Err(anyhow::anyhow!(error).into()),
            }
        }

        Ok(CommitOutcome::Busy)
    }

    async fn try_commit(
        &self,
        new: &NewCheckpoint,
    ) -> std::result::Result<CommitOutcome, sqlx::Error> {
        let mut connection = self.pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;

        let outcome = self.commit_in_transaction(new, &mut connection).await;

        match &outcome {
            Ok(CommitOutcome::Committed(_)) => {
                sqlx::query("COMMIT").execute(&mut *connection).await?;
            }
            _ => {
                sqlx::query("ROLLBACK").execute(&mut *connection).await.ok();
            }
        }

        outcome
    }

    async fn commit_in_transaction(
        &self,
        new: &NewCheckpoint,
        connection: &mut sqlx::SqliteConnection,
    ) -> std::result::Result<CommitOutcome, sqlx::Error> {
        let last = sqlx::query(
            "SELECT covers_to_seq FROM channel_chronicle_checkpoints \
             WHERE channel_id = ? AND level = ? \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(&new.channel_id)
        .bind(new.level)
        .fetch_optional(&mut *connection)
        .await?;

        let expected_start: i64 = last
            .as_ref()
            .and_then(|row| row.try_get("covers_to_seq").ok())
            .unwrap_or(0);

        if expected_start != new.covers_from.seq {
            return Ok(CommitOutcome::Superseded {
                expected: expected_start,
                found: new.covers_from.seq,
            });
        }

        // An empty span would commit a checkpoint that covers nothing and
        // still advance the boundary.
        if new.covers_to.seq <= new.covers_from.seq {
            return Ok(CommitOutcome::Superseded {
                expected: expected_start,
                found: new.covers_to.seq,
            });
        }

        let next_seq: i64 = sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) + 1 AS next FROM channel_chronicle_checkpoints \
             WHERE channel_id = ?",
        )
        .bind(&new.channel_id)
        .fetch_one(&mut *connection)
        .await
        .map(|row| row.try_get("next").unwrap_or(1))?;

        let id = uuid::Uuid::new_v4().to_string();
        let created_at = Utc::now();

        let insert = sqlx::query(
            "INSERT INTO channel_chronicle_checkpoints \
             (id, channel_id, seq, level, kind, title, summary, covers_from_at, covers_to_at, \
              covers_from_message_id, covers_to_message_id, covers_from_seq, covers_to_seq, \
              message_count, token_estimate, rolls_up_from_seq, rolls_up_to_seq, model, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&new.channel_id)
        .bind(next_seq)
        .bind(new.level)
        .bind(new.kind.as_str())
        .bind(&new.title)
        .bind(&new.summary)
        .bind(sql_timestamp(new.covers_from_at))
        .bind(sql_timestamp(new.covers_to_at))
        .bind(&new.covers_from_message_id)
        .bind(&new.covers_to_message_id)
        .bind(new.covers_from.seq)
        .bind(new.covers_to.seq)
        .bind(new.message_count)
        .bind(new.token_estimate)
        .bind(new.rolls_up_from_seq)
        .bind(new.rolls_up_to_seq)
        .bind(&new.model)
        .bind(sql_timestamp(created_at))
        .execute(&mut *connection)
        .await;

        if let Err(error) = insert {
            if is_unique_violation(&error) {
                return Ok(CommitOutcome::Superseded {
                    expected: expected_start,
                    found: new.covers_from.seq,
                });
            }
            return Err(error);
        }

        Ok(CommitOutcome::Committed(Box::new(ChronicleCheckpoint {
            id,
            channel_id: new.channel_id.clone(),
            seq: next_seq,
            level: new.level,
            kind: new.kind,
            title: new.title.clone(),
            summary: new.summary.clone(),
            covers_from_at: new.covers_from_at,
            covers_to_at: new.covers_to_at,
            covers_from_message_id: new.covers_from_message_id.clone(),
            covers_to_message_id: new.covers_to_message_id.clone(),
            covers_from_seq: new.covers_from.seq,
            covers_to_seq: new.covers_to.seq,
            message_count: new.message_count,
            token_estimate: new.token_estimate,
            rolled_up_into: None,
            rolls_up_from_seq: new.rolls_up_from_seq,
            rolls_up_to_seq: new.rolls_up_to_seq,
            model: new.model.clone(),
            created_at,
        })))
    }

    /// Checkpoints at a level, newest first.
    pub async fn list(
        &self,
        channel_id: &str,
        level: i64,
        limit: i64,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints \
             WHERE channel_id = ? AND level = ? \
             ORDER BY seq DESC LIMIT ?",
        )
        .bind(channel_id)
        .bind(level)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// Checkpoints at a level whose coverage ends at or after `since`, oldest
    /// first. Used to select the recent window for the prompt.
    pub async fn list_since(
        &self,
        channel_id: &str,
        level: i64,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM ( \
                SELECT * FROM channel_chronicle_checkpoints \
                WHERE channel_id = ? AND level = ? \
                  AND datetime(covers_to_at) >= datetime(?) \
                ORDER BY seq DESC LIMIT ? \
             ) ORDER BY seq ASC",
        )
        .bind(channel_id)
        .bind(level)
        .bind(sql_timestamp(since))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// Checkpoints at a level with a sequence below `before_seq`, oldest first.
    /// Used to select the older-history portion of the prompt view.
    pub async fn list_before_seq(
        &self,
        channel_id: &str,
        level: i64,
        before_seq: i64,
        limit: i64,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM ( \
                SELECT * FROM channel_chronicle_checkpoints \
                WHERE channel_id = ? AND level = ? AND seq < ? \
                ORDER BY seq DESC LIMIT ? \
             ) ORDER BY seq ASC",
        )
        .bind(channel_id)
        .bind(level)
        .bind(before_seq)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// The most compact set of entries covering everything before `before_seq`:
    /// any checkpoint no rollup has absorbed, at whatever level it sits.
    ///
    /// A level-0 checkpoint a rollup covers is skipped, because the rollup
    /// represents it. Ordered oldest first, capped at `limit` — that cap is
    /// what rollups exist to keep meaningful, since without them the oldest
    /// entries simply fall off the end.
    pub async fn uncovered_before_seq(
        &self,
        channel_id: &str,
        before_seq: i64,
        limit: i64,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM ( \
                SELECT * FROM channel_chronicle_checkpoints \
                WHERE channel_id = ? AND seq < ? AND rolled_up_into IS NULL \
                ORDER BY seq DESC LIMIT ? \
             ) ORDER BY covers_from_seq ASC, seq ASC",
        )
        .bind(channel_id)
        .bind(before_seq)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// Checkpoints at every level, newest first.
    pub async fn list_all_levels(
        &self,
        channel_id: &str,
        limit: i64,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints \
             WHERE channel_id = ? ORDER BY seq DESC LIMIT ?",
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// Checkpoints at a level that no rollup covers yet, oldest first.
    ///
    /// This is the pool a rollup draws from. Rolling an already-covered
    /// checkpoint a second time would double-count its span in the view.
    pub async fn unrolled_at_level(
        &self,
        channel_id: &str,
        level: i64,
        limit: i64,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints \
             WHERE channel_id = ? AND level = ? AND rolled_up_into IS NULL \
             ORDER BY seq ASC LIMIT ?",
        )
        .bind(channel_id)
        .bind(level)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// How many checkpoints at a level are not yet covered by a rollup.
    pub async fn unrolled_count(&self, channel_id: &str, level: i64) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total FROM channel_chronicle_checkpoints \
             WHERE channel_id = ? AND level = ? AND rolled_up_into IS NULL",
        )
        .bind(channel_id)
        .bind(level)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.try_get("total").unwrap_or(0))
    }

    /// The checkpoints a rollup covers, oldest first.
    ///
    /// The reverse of `rolled_up_into`: this is what makes a rollup
    /// inspectable rather than an opaque summary-of-summaries.
    pub async fn children_of(&self, rollup_id: &str) -> Result<Vec<ChronicleCheckpoint>> {
        let rows = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints \
             WHERE rolled_up_into = ? ORDER BY seq ASC",
        )
        .bind(rollup_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// Commit a rollup and mark the checkpoints it covers, atomically.
    ///
    /// The insert and the `rolled_up_into` stamping have to be one transaction:
    /// a rollup whose children are unmarked would be re-rolled forever, and
    /// children marked without a surviving parent would vanish from the view
    /// with nothing covering them.
    ///
    /// Children are never deleted or rewritten — only stamped — so a rollup can
    /// always be expanded back into the checkpoints it summarizes.
    pub async fn commit_rollup(
        &self,
        new: NewCheckpoint,
        child_ids: &[String],
    ) -> Result<CommitOutcome> {
        if child_ids.is_empty() {
            return Ok(CommitOutcome::Superseded {
                expected: new.covers_from.seq,
                found: new.covers_from.seq,
            });
        }

        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let outcome = self
            .commit_rollup_in_transaction(&new, child_ids, &mut connection)
            .await;

        match &outcome {
            Ok(CommitOutcome::Committed(_)) => {
                sqlx::query("COMMIT")
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| anyhow::anyhow!(error))?;
            }
            _ => {
                sqlx::query("ROLLBACK").execute(&mut *connection).await.ok();
            }
        }

        outcome.map_err(|error| anyhow::anyhow!(error).into())
    }

    async fn commit_rollup_in_transaction(
        &self,
        new: &NewCheckpoint,
        child_ids: &[String],
        connection: &mut sqlx::SqliteConnection,
    ) -> std::result::Result<CommitOutcome, sqlx::Error> {
        // Re-read the children inside the transaction: another rollup may have
        // claimed them between selection and commit.
        let placeholders = vec!["?"; child_ids.len()].join(",");
        let claimed_sql = format!(
            "SELECT COUNT(*) AS claimed FROM channel_chronicle_checkpoints \
             WHERE id IN ({placeholders}) AND rolled_up_into IS NOT NULL"
        );
        let mut claimed_query = sqlx::query(&claimed_sql);
        for id in child_ids {
            claimed_query = claimed_query.bind(id);
        }
        let claimed: i64 = claimed_query
            .fetch_one(&mut *connection)
            .await?
            .try_get("claimed")
            .unwrap_or(0);
        if claimed > 0 {
            return Ok(CommitOutcome::Superseded {
                expected: 0,
                found: claimed,
            });
        }

        let outcome = self.commit_in_transaction(new, &mut *connection).await?;
        let CommitOutcome::Committed(rollup) = &outcome else {
            return Ok(outcome);
        };

        let stamp_sql = format!(
            "UPDATE channel_chronicle_checkpoints SET rolled_up_into = ? \
             WHERE id IN ({placeholders})"
        );
        let mut stamp_query = sqlx::query(&stamp_sql).bind(&rollup.id);
        for id in child_ids {
            stamp_query = stamp_query.bind(id);
        }
        stamp_query.execute(&mut *connection).await?;

        Ok(outcome)
    }

    /// Checkpoints for the channel timeline, newest first.
    ///
    /// Ordered and filtered by commit time rather than coverage, so a
    /// checkpoint lands inline at the point the conversation reached it.
    pub async fn list_for_timeline(
        &self,
        channel_id: &str,
        limit: i64,
        before: Option<&crate::conversation::history::TimelineCursor>,
    ) -> Result<Vec<ChronicleCheckpoint>> {
        // Same composite cursor the message union uses, so a page boundary
        // landing among same-second peers cannot skip a checkpoint.
        let before_clause = if before.is_some() {
            "AND (datetime(created_at) < datetime(?3) \
                  OR (datetime(created_at) = datetime(?3) AND id < ?4))"
        } else {
            ""
        };
        let sql = format!(
            "SELECT * FROM channel_chronicle_checkpoints \
             WHERE channel_id = ?1 {before_clause} \
             ORDER BY created_at DESC, id DESC LIMIT ?2"
        );

        let mut query = sqlx::query(&sql).bind(channel_id).bind(limit);
        if let Some(cursor) = before {
            query = query.bind(&cursor.timestamp).bind(&cursor.id);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        Ok(checkpoints_from_rows(rows))
    }

    /// A single checkpoint by sequence number.
    pub async fn get_by_seq(
        &self,
        channel_id: &str,
        seq: i64,
    ) -> Result<Option<ChronicleCheckpoint>> {
        let row = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints WHERE channel_id = ? AND seq = ?",
        )
        .bind(channel_id)
        .bind(seq)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.as_ref().and_then(|row| match checkpoint_from_row(row) {
            Ok(checkpoint) => Some(checkpoint),
            Err(error) => {
                tracing::warn!(%error, "skipping undecodable chronicle checkpoint row");
                None
            }
        }))
    }

    /// A single checkpoint by id, scoped to its channel.
    pub async fn get_by_id(
        &self,
        channel_id: &str,
        id: &str,
    ) -> Result<Option<ChronicleCheckpoint>> {
        let row = sqlx::query(
            "SELECT * FROM channel_chronicle_checkpoints WHERE channel_id = ? AND id = ?",
        )
        .bind(channel_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.as_ref().and_then(|row| match checkpoint_from_row(row) {
            Ok(checkpoint) => Some(checkpoint),
            Err(error) => {
                tracing::warn!(%error, "skipping undecodable chronicle checkpoint row");
                None
            }
        }))
    }

    /// Aggregate state for the prompt header.
    pub async fn stats(&self, channel_id: &str) -> Result<ChronicleStats> {
        let counts = sqlx::query(
            "SELECT COUNT(*) AS total, \
                    SUM(CASE WHEN level = 0 THEN 1 ELSE 0 END) AS intervals, \
                    SUM(CASE WHEN level > 0 THEN 1 ELSE 0 END) AS rollups \
             FROM channel_chronicle_checkpoints WHERE channel_id = ?",
        )
        .bind(channel_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let messages = sqlx::query(
            "SELECT COUNT(*) AS total, MIN(created_at) AS first_at, MAX(created_at) AS last_at \
             FROM conversation_messages WHERE channel_id = ?",
        )
        .bind(channel_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let latest = self.latest(channel_id, 0).await?;
        let unsummarized = match &latest {
            Some(checkpoint) => {
                self.count_messages_after(channel_id, checkpoint.end_boundary())
                    .await?
            }
            None => messages.try_get("total").unwrap_or(0),
        };

        Ok(ChronicleStats {
            checkpoint_count: counts.try_get("total").unwrap_or(0),
            interval_count: counts.try_get("intervals").unwrap_or(0),
            rollup_count: counts.try_get("rollups").unwrap_or(0),
            total_messages: messages.try_get("total").unwrap_or(0),
            first_message_at: messages.try_get("first_at").ok(),
            last_message_at: messages.try_get("last_at").ok(),
            unsummarized_messages: unsummarized,
        })
    }

    /// How many logged messages sit after a boundary.
    pub async fn count_messages_after(
        &self,
        channel_id: &str,
        boundary: ChronicleBoundary,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total FROM conversation_messages \
             WHERE channel_id = ? AND seq > ?",
        )
        .bind(channel_id)
        .bind(boundary.seq)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.try_get("total").unwrap_or(0))
    }

    /// Logged messages after a boundary, oldest first.
    pub async fn messages_after(
        &self,
        channel_id: &str,
        boundary: ChronicleBoundary,
        limit: i64,
    ) -> Result<Vec<ConversationMessage>> {
        let rows = sqlx::query(
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at, seq \
             FROM conversation_messages \
             WHERE channel_id = ? AND seq > ? \
             ORDER BY seq ASC LIMIT ?",
        )
        .bind(channel_id)
        .bind(boundary.seq)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(rows.into_iter().map(message_from_row).collect())
    }

    /// The newest logged messages after a boundary, returned oldest-first.
    ///
    /// A resumed channel wants the *end* of its uncovered tail, not the start:
    /// the oldest uncovered rows are the ones a checkpoint will summarize next,
    /// while the newest are the live conversation. The rows are selected
    /// newest-first and reversed so the caller still renders chronologically.
    pub async fn newest_messages_after(
        &self,
        channel_id: &str,
        boundary: ChronicleBoundary,
        limit: i64,
    ) -> Result<Vec<ConversationMessage>> {
        let rows = sqlx::query(
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at, seq \
             FROM conversation_messages \
             WHERE channel_id = ? AND seq > ? \
             ORDER BY seq DESC LIMIT ?",
        )
        .bind(channel_id)
        .bind(boundary.seq)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        let mut messages: Vec<ConversationMessage> =
            rows.into_iter().map(message_from_row).collect();
        messages.reverse();
        Ok(messages)
    }

    /// Logged messages inside a half-open `(from, to]` seq range, oldest first.
    ///
    /// `after` lets a caller page forward through a large checkpoint: pass the
    /// seq of the last row already read.
    pub async fn messages_in_range(
        &self,
        channel_id: &str,
        from: ChronicleBoundary,
        to: ChronicleBoundary,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<ConversationMessage>> {
        let start = from.seq.max(after.unwrap_or(from.seq));
        let rows = sqlx::query(
            "SELECT id, channel_id, role, sender_name, sender_id, content, metadata, created_at, seq \
             FROM conversation_messages \
             WHERE channel_id = ? AND seq > ? AND seq <= ? \
             ORDER BY seq ASC LIMIT ?",
        )
        .bind(channel_id)
        .bind(start)
        .bind(to.seq)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(rows.into_iter().map(message_from_row).collect())
    }

    /// The newest assigned seq for a channel, or 0 when nothing is logged.
    pub async fn max_seq(&self, channel_id: &str) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) AS max_seq FROM conversation_messages \
             WHERE channel_id = ?",
        )
        .bind(channel_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.try_get("max_seq").unwrap_or(0))
    }
}

/// A write-lock conflict. SQLite reports these when a deferred transaction
/// tries to upgrade, or when another writer holds the lock.
fn is_busy(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db)
        if matches!(db.code().as_deref(), Some("5") | Some("517") | Some("261"))
            || db.message().contains("database is locked")
            || db.message().contains("database table is locked"))
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some("2067")
        || db.message().contains("UNIQUE constraint failed"))
}

/// Decode a checkpoint row.
///
/// Timestamps and ids are not defaulted. A `Utc::now()` fallback on
/// `covers_*_at` would invent a coverage range pointing at the present, and an
/// empty-string `id` produces a checkpoint no lookup can resolve — both fail
/// silently and corrupt the chronicle rather than surfacing a decode problem.
fn checkpoint_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ChronicleCheckpoint> {
    let kind: String = row.try_get("kind").unwrap_or_else(|_| "interval".into());
    Ok(ChronicleCheckpoint {
        id: row.try_get("id").map_err(|error| anyhow::anyhow!(error))?,
        channel_id: row
            .try_get("channel_id")
            .map_err(|error| anyhow::anyhow!(error))?,
        seq: row.try_get("seq").map_err(|error| anyhow::anyhow!(error))?,
        level: row.try_get("level").unwrap_or_default(),
        kind: CheckpointKind::from_db(&kind),
        title: row.try_get("title").unwrap_or_default(),
        summary: row.try_get("summary").unwrap_or_default(),
        covers_from_at: row
            .try_get("covers_from_at")
            .map_err(|error| anyhow::anyhow!(error))?,
        covers_to_at: row
            .try_get("covers_to_at")
            .map_err(|error| anyhow::anyhow!(error))?,
        covers_from_message_id: row.try_get("covers_from_message_id").ok().flatten(),
        covers_to_message_id: row.try_get("covers_to_message_id").ok().flatten(),
        covers_from_seq: row.try_get("covers_from_seq").unwrap_or(0),
        covers_to_seq: row
            .try_get("covers_to_seq")
            .map_err(|error| anyhow::anyhow!(error))?,
        message_count: row.try_get("message_count").unwrap_or_default(),
        token_estimate: row.try_get("token_estimate").unwrap_or_default(),
        rolled_up_into: row.try_get("rolled_up_into").ok().flatten(),
        rolls_up_from_seq: row.try_get("rolls_up_from_seq").ok().flatten(),
        rolls_up_to_seq: row.try_get("rolls_up_to_seq").ok().flatten(),
        model: row.try_get("model").ok().flatten(),
        created_at: row
            .try_get("created_at")
            .map_err(|error| anyhow::anyhow!(error))?,
    })
}

/// Decode a list of checkpoint rows, dropping and logging any that fail.
///
/// A single unreadable row must not take down a prompt render, but it must not
/// masquerade as a valid checkpoint either.
fn checkpoints_from_rows(rows: Vec<sqlx::sqlite::SqliteRow>) -> Vec<ChronicleCheckpoint> {
    rows.iter()
        .filter_map(|row| match checkpoint_from_row(row) {
            Ok(checkpoint) => Some(checkpoint),
            Err(error) => {
                tracing::warn!(%error, "skipping undecodable chronicle checkpoint row");
                None
            }
        })
        .collect()
}

fn message_from_row(row: sqlx::sqlite::SqliteRow) -> ConversationMessage {
    ConversationMessage {
        id: row.try_get("id").unwrap_or_default(),
        channel_id: row.try_get("channel_id").unwrap_or_default(),
        role: row.try_get("role").unwrap_or_default(),
        sender_name: row.try_get("sender_name").ok(),
        sender_id: row.try_get("sender_id").ok(),
        content: row.try_get("content").unwrap_or_default(),
        metadata: row.try_get("metadata").ok(),
        created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
        seq: row.try_get("seq").ok().flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::history::ConversationLogger;

    async fn setup() -> ChronicleStore {
        // A shared cache is required so the logger's detached writes and the
        // store see one database, but the name must be unique per test or
        // every test in the binary would share it.
        let url = format!(
            "sqlite:file:chronicle_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4().simple()
        );
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(&url)
            .await
            .expect("sqlite memory pool");

        for migration in [
            include_str!("../../migrations/20260211000002_conversations.sql"),
            include_str!("../../migrations/20260809000004_session_chronicles.sql"),
            include_str!("../../migrations/20260810000001_conversation_message_seq.sql"),
        ] {
            sqlx::raw_sql(migration)
                .execute(&pool)
                .await
                .expect("migration");
        }

        ChronicleStore::new(pool)
    }

    /// Insert a row the way the logger does, letting SQLite assign both the
    /// timestamp and the sequence.
    async fn insert_message(store: &ChronicleStore, channel: &str, id: &str, at: &str) {
        sqlx::query(
            "INSERT INTO conversation_messages (id, channel_id, role, content, created_at, seq) \
             VALUES (?, ?, 'user', 'hello', ?, \
                COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = ?), 0) + 1)",
        )
        .bind(id)
        .bind(channel)
        .bind(at)
        .bind(channel)
        .execute(&store.pool)
        .await
        .expect("insert message");
    }

    fn new_checkpoint(channel: &str, from: i64, to: i64, count: i64) -> NewCheckpoint {
        let at = Utc::now();
        NewCheckpoint {
            channel_id: channel.to_string(),
            level: 0,
            kind: CheckpointKind::Interval,
            title: "a span".into(),
            summary: "things happened".into(),
            covers_from: ChronicleBoundary::new(from),
            covers_to: ChronicleBoundary::new(to),
            covers_from_at: at,
            covers_to_at: at,
            covers_from_message_id: None,
            covers_to_message_id: None,
            message_count: count,
            token_estimate: 10,
            rolls_up_from_seq: None,
            rolls_up_to_seq: None,
            model: Some("test-model".into()),
        }
    }

    #[tokio::test]
    async fn commits_allocate_sequential_boundaries_with_no_gap() {
        let store = setup().await;
        let CommitOutcome::Committed(first) = store
            .commit(new_checkpoint("ch", 0, 10, 10))
            .await
            .expect("commit")
        else {
            panic!("first commit should succeed")
        };
        assert_eq!(first.seq, 1);

        let CommitOutcome::Committed(second) = store
            .commit(new_checkpoint("ch", first.covers_to_seq, 20, 10))
            .await
            .expect("commit")
        else {
            panic!("second commit should succeed")
        };

        assert_eq!(second.seq, 2);
        assert_eq!(
            second.covers_from_seq, first.covers_to_seq,
            "checkpoint N starts exactly where N-1 ended"
        );
    }

    #[tokio::test]
    async fn stale_cut_is_superseded_and_writes_nothing() {
        let store = setup().await;
        let CommitOutcome::Committed(first) = store
            .commit(new_checkpoint("ch", 0, 10, 10))
            .await
            .expect("commit")
        else {
            panic!("expected commit")
        };

        let outcome = store
            .commit(new_checkpoint("ch", 0, 5, 5))
            .await
            .expect("commit call should not error");

        assert!(matches!(outcome, CommitOutcome::Superseded { .. }));
        let all = store.list("ch", 0, 10).await.expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, first.id);
    }

    #[tokio::test]
    async fn duplicate_commit_yields_one_row() {
        let store = setup().await;
        let cut = new_checkpoint("ch", 0, 10, 10);

        assert!(matches!(
            store.commit(cut.clone()).await.expect("commit"),
            CommitOutcome::Committed(_)
        ));
        assert!(matches!(
            store.commit(cut).await.expect("commit"),
            CommitOutcome::Superseded { .. }
        ));
        assert_eq!(store.list("ch", 0, 10).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn empty_span_is_refused() {
        let store = setup().await;
        let outcome = store
            .commit(new_checkpoint("ch", 0, 0, 0))
            .await
            .expect("commit");
        assert!(
            matches!(outcome, CommitOutcome::Superseded { .. }),
            "a checkpoint covering nothing must not advance the boundary"
        );
    }

    /// The blocker this ordering key exists for: a detached logger write that
    /// lands after a checkpoint commits, sharing the same whole second and
    /// carrying a lexically smaller id, must still be discoverable.
    #[tokio::test]
    async fn late_same_second_insert_stays_after_a_committed_boundary() {
        let store = setup().await;
        insert_message(&store, "ch", "zzz-first", "2026-08-01 00:00:00").await;
        insert_message(&store, "ch", "yyy-second", "2026-08-01 00:00:00").await;

        let tail = store.max_seq("ch").await.expect("max seq");
        let CommitOutcome::Committed(checkpoint) = store
            .commit(new_checkpoint("ch", 0, tail, 2))
            .await
            .expect("commit")
        else {
            panic!("expected commit")
        };

        // Same whole second, id sorts below BOTH covered rows. Under
        // (created_at, id) ordering this row would be lost forever.
        insert_message(&store, "ch", "aaa-late", "2026-08-01 00:00:00").await;

        let uncovered = store
            .count_messages_after("ch", checkpoint.end_boundary())
            .await
            .expect("count");
        assert_eq!(uncovered, 1, "the late row must remain discoverable");

        let found = store
            .messages_after("ch", checkpoint.end_boundary(), 10)
            .await
            .expect("messages");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "aaa-late");
    }

    /// The same guarantee through the real logger, whose writes are detached
    /// and whose timestamps come from CURRENT_TIMESTAMP.
    #[tokio::test]
    async fn logger_writes_after_a_boundary_are_never_stranded() {
        let store = setup().await;
        let logger = ConversationLogger::new(store.pool.clone());
        let channel: crate::ChannelId = std::sync::Arc::from("ch");

        logger.log_user_message(&channel, "jamie", "u1", "first", &Default::default());
        wait_for_message_count(&store, "ch", 1).await;

        let tail = store.max_seq("ch").await.expect("max seq");
        let CommitOutcome::Committed(checkpoint) = store
            .commit(new_checkpoint("ch", 0, tail, 1))
            .await
            .expect("commit")
        else {
            panic!("expected commit")
        };

        // Several writes in the same second, after the boundary committed.
        for index in 0..5 {
            logger.log_user_message(
                &channel,
                "jamie",
                "u1",
                &format!("late {index}"),
                &Default::default(),
            );
        }
        wait_for_message_count(&store, "ch", 6).await;

        let uncovered = store
            .count_messages_after("ch", checkpoint.end_boundary())
            .await
            .expect("count");
        assert_eq!(uncovered, 5, "every late write stays after the boundary");
    }

    async fn wait_for_message_count(store: &ChronicleStore, channel: &str, want: i64) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let row = sqlx::query(
                "SELECT COUNT(*) AS total FROM conversation_messages WHERE channel_id = ?",
            )
            .bind(channel)
            .fetch_one(&store.pool)
            .await
            .expect("count");
            let total: i64 = row.try_get("total").unwrap_or(0);
            if total >= want {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "logger writes did not land: wanted {want}, saw {total}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn range_selection_is_half_open_and_contiguous() {
        let store = setup().await;
        for (index, id) in ["m1", "m2", "m3", "m4", "m5"].iter().enumerate() {
            insert_message(&store, "ch", id, &format!("2026-08-01 00:00:0{index}")).await;
        }

        let first = store
            .messages_in_range(
                "ch",
                ChronicleBoundary::new(0),
                ChronicleBoundary::new(3),
                None,
                100,
            )
            .await
            .expect("range");
        let second = store
            .messages_in_range(
                "ch",
                ChronicleBoundary::new(3),
                ChronicleBoundary::new(5),
                None,
                100,
            )
            .await
            .expect("range");

        let first_ids: Vec<&str> = first.iter().map(|m| m.id.as_str()).collect();
        let second_ids: Vec<&str> = second.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(first_ids, vec!["m1", "m2", "m3"]);
        assert_eq!(second_ids, vec!["m4", "m5"]);
        assert!(first_ids.iter().all(|id| !second_ids.contains(id)));
    }

    /// Every message in a large span is reachable by paging forward.
    #[tokio::test]
    async fn range_pagination_reaches_the_end_of_a_large_span() {
        let store = setup().await;
        for index in 0..25 {
            insert_message(&store, "ch", &format!("m{index:03}"), "2026-08-01 00:00:00").await;
        }

        let to = ChronicleBoundary::new(store.max_seq("ch").await.expect("max"));
        let mut cursor: Option<i64> = None;
        let mut seen = Vec::new();
        loop {
            let page = store
                .messages_in_range("ch", ChronicleBoundary::new(0), to, cursor, 10)
                .await
                .expect("page");
            if page.is_empty() {
                break;
            }
            cursor = page.last().and_then(|message| message.seq);
            seen.extend(page.into_iter().map(|message| message.id));
        }

        assert_eq!(seen.len(), 25, "pagination reaches every covered message");
    }

    /// Restart wants the newest uncovered messages, rendered oldest-first.
    #[tokio::test]
    async fn newest_tail_keeps_the_end_of_the_span_in_chronological_order() {
        let store = setup().await;
        for index in 1..=6 {
            insert_message(&store, "ch", &format!("m{index}"), "2026-08-01 00:00:00").await;
        }

        let tail = store
            .newest_messages_after("ch", ChronicleBoundary::new(0), 3)
            .await
            .expect("tail");
        let ids: Vec<&str> = tail.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["m4", "m5", "m6"],
            "the newest three, still in chronological order"
        );
    }

    async fn commit_chain(store: &ChronicleStore, count: i64) -> Vec<ChronicleCheckpoint> {
        let mut out = Vec::new();
        let mut from = 0i64;
        for index in 0..count {
            let to = from + 10;
            let CommitOutcome::Committed(cp) = store
                .commit(new_checkpoint("ch", from, to, 10))
                .await
                .expect("commit")
            else {
                panic!("commit {index} should succeed")
            };
            from = to;
            out.push(*cp);
        }
        out
    }

    fn rollup_over(children: &[ChronicleCheckpoint]) -> NewCheckpoint {
        let first = children.first().expect("children");
        let last = children.last().expect("children");
        NewCheckpoint {
            channel_id: "ch".into(),
            level: first.level + 1,
            kind: CheckpointKind::Rollup,
            title: "a stretch".into(),
            summary: "several spans condensed".into(),
            covers_from: first.start_boundary(),
            covers_to: last.end_boundary(),
            covers_from_at: first.covers_from_at,
            covers_to_at: last.covers_to_at,
            covers_from_message_id: None,
            covers_to_message_id: None,
            message_count: children.iter().map(|c| c.message_count).sum(),
            token_estimate: 20,
            rolls_up_from_seq: Some(first.seq),
            rolls_up_to_seq: Some(last.seq),
            model: None,
        }
    }

    /// A rollup must never destroy what it summarizes: children keep their
    /// rows, get stamped, and stay reachable in both directions.
    #[tokio::test]
    async fn a_rollup_preserves_its_children_in_both_directions() {
        let store = setup().await;
        let children = commit_chain(&store, 4).await;
        let ids: Vec<String> = children.iter().map(|c| c.id.clone()).collect();

        let CommitOutcome::Committed(rollup) = store
            .commit_rollup(rollup_over(&children), &ids)
            .await
            .expect("rollup")
        else {
            panic!("rollup should commit")
        };

        assert_eq!(rollup.level, 1);
        assert_eq!(rollup.rolls_up_from_seq, Some(children[0].seq));
        assert_eq!(rollup.rolls_up_to_seq, Some(children[3].seq));

        // Coverage is exactly the union of the children's.
        assert_eq!(rollup.covers_from_seq, children[0].covers_from_seq);
        assert_eq!(rollup.covers_to_seq, children[3].covers_to_seq);
        assert_eq!(
            rollup.message_count,
            children.iter().map(|c| c.message_count).sum::<i64>()
        );

        // Parent -> children.
        let listed = store.children_of(&rollup.id).await.expect("children");
        assert_eq!(listed.len(), 4);
        assert_eq!(
            listed.iter().map(|c| c.seq).collect::<Vec<_>>(),
            children.iter().map(|c| c.seq).collect::<Vec<_>>()
        );

        // Children -> parent, and every original row still exists.
        for child in &children {
            let reloaded = store
                .get_by_seq("ch", child.seq)
                .await
                .expect("get")
                .expect("child row survives the rollup");
            assert_eq!(reloaded.rolled_up_into.as_deref(), Some(rollup.id.as_str()));
            assert_eq!(reloaded.summary, child.summary, "child text is untouched");
        }
    }

    /// The stamping and the insert are one transaction, so the same children
    /// can never be rolled twice.
    #[tokio::test]
    async fn children_cannot_be_rolled_up_twice() {
        let store = setup().await;
        let children = commit_chain(&store, 3).await;
        let ids: Vec<String> = children.iter().map(|c| c.id.clone()).collect();

        assert!(matches!(
            store
                .commit_rollup(rollup_over(&children), &ids)
                .await
                .expect("first"),
            CommitOutcome::Committed(_)
        ));
        assert!(matches!(
            store
                .commit_rollup(rollup_over(&children), &ids)
                .await
                .expect("second"),
            CommitOutcome::Superseded { .. }
        ));

        assert_eq!(
            store.list_all_levels("ch", 50).await.expect("list").len(),
            4,
            "three checkpoints and exactly one rollup"
        );
    }

    /// Once absorbed, a checkpoint is represented by its rollup in the view.
    #[tokio::test]
    async fn the_view_selection_prefers_a_rollup_over_its_children() {
        let store = setup().await;
        let children = commit_chain(&store, 4).await;
        let ids: Vec<String> = children.iter().map(|c| c.id.clone()).collect();

        let before = store
            .uncovered_before_seq("ch", i64::MAX, 50)
            .await
            .expect("before");
        assert_eq!(before.len(), 4, "no rollup yet: the children are the view");

        let CommitOutcome::Committed(rollup) = store
            .commit_rollup(rollup_over(&children), &ids)
            .await
            .expect("rollup")
        else {
            panic!("rollup should commit")
        };

        let after = store
            .uncovered_before_seq("ch", i64::MAX, 50)
            .await
            .expect("after");
        assert_eq!(after.len(), 1, "the rollup now stands for all four");
        assert_eq!(after[0].id, rollup.id);
        assert_eq!(
            store.unrolled_count("ch", 0).await.expect("count"),
            0,
            "no level-0 checkpoint is left uncovered"
        );
    }

    /// Rollups nest: level-1 entries roll into level-2 the same way.
    #[tokio::test]
    async fn rollups_nest_into_higher_levels() {
        let store = setup().await;
        let children = commit_chain(&store, 6).await;

        let first_ids: Vec<String> = children[..3].iter().map(|c| c.id.clone()).collect();
        let CommitOutcome::Committed(first_rollup) = store
            .commit_rollup(rollup_over(&children[..3]), &first_ids)
            .await
            .expect("rollup")
        else {
            panic!("first rollup")
        };
        let second_ids: Vec<String> = children[3..].iter().map(|c| c.id.clone()).collect();
        let CommitOutcome::Committed(second_rollup) = store
            .commit_rollup(rollup_over(&children[3..]), &second_ids)
            .await
            .expect("rollup")
        else {
            panic!("second rollup")
        };

        let level_one = vec![*first_rollup, *second_rollup];
        let level_one_ids: Vec<String> = level_one.iter().map(|c| c.id.clone()).collect();
        let CommitOutcome::Committed(top) = store
            .commit_rollup(rollup_over(&level_one), &level_one_ids)
            .await
            .expect("level 2")
        else {
            panic!("level-2 rollup")
        };

        assert_eq!(top.level, 2);
        assert_eq!(top.covers_from_seq, children[0].covers_from_seq);
        assert_eq!(top.covers_to_seq, children[5].covers_to_seq);

        let view = store
            .uncovered_before_seq("ch", i64::MAX, 50)
            .await
            .expect("view");
        assert_eq!(view.len(), 1, "one entry stands for the whole session");
        assert_eq!(view[0].id, top.id);

        // And the chain back down is intact.
        let mid = store.children_of(&top.id).await.expect("mid");
        assert_eq!(mid.len(), 2);
        let leaves = store.children_of(&mid[0].id).await.expect("leaves");
        assert_eq!(leaves.len(), 3);
    }

    #[tokio::test]
    async fn stats_report_unsummarized_tail() {
        let store = setup().await;
        for (index, id) in ["m1", "m2", "m3", "m4"].iter().enumerate() {
            insert_message(&store, "ch", id, &format!("2026-08-01 00:00:0{index}")).await;
        }

        let empty = store.stats("ch").await.expect("stats");
        assert_eq!(empty.checkpoint_count, 0);
        assert_eq!(empty.unsummarized_messages, 4);

        store
            .commit(new_checkpoint("ch", 0, 3, 3))
            .await
            .expect("commit");

        let stats = store.stats("ch").await.expect("stats");
        assert_eq!(stats.checkpoint_count, 1);
        assert_eq!(stats.total_messages, 4);
        assert_eq!(stats.unsummarized_messages, 1);
    }
}
