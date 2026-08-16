//! Autonomy run history storage.
//!
//! One row per autonomy channel run. The `autonomy_complete` tool records the
//! summary and actions; the run driver records the consumed wake events and
//! finishes runs that end without a completion call. Recent summaries are the
//! channel's primary continuity mechanism between runs.

use crate::error::Result;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

/// A single action taken during an autonomy run.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyAction {
    /// "enriched", "created", or "executed".
    pub kind: String,
    /// Task the action touched, when applicable.
    pub task_number: Option<i64>,
    /// One-line description of what was done.
    pub detail: String,
}

impl AutonomyAction {
    pub const KINDS: [&'static str; 3] = ["enriched", "created", "executed"];

    pub fn kind_is_valid(kind: &str) -> bool {
        Self::KINDS.contains(&kind)
    }
}

/// Terminal status of an autonomy run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyRunStatus {
    Running,
    Completed,
    Timeout,
    Failed,
}

impl AutonomyRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AutonomyRunStatus::Running => "running",
            AutonomyRunStatus::Completed => "completed",
            AutonomyRunStatus::Timeout => "timeout",
            AutonomyRunStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(AutonomyRunStatus::Running),
            "completed" => Some(AutonomyRunStatus::Completed),
            "timeout" => Some(AutonomyRunStatus::Timeout),
            "failed" => Some(AutonomyRunStatus::Failed),
            _ => None,
        }
    }
}

impl std::fmt::Display for AutonomyRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyRun {
    pub id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_secs: Option<i64>,
    pub status: AutonomyRunStatus,
    pub summary: Option<String>,
    pub actions: Vec<AutonomyAction>,
    pub wake_event_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AutonomyRunStore {
    pool: SqlitePool,
}

impl AutonomyRunStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a new run in `running` state and return its id.
    pub async fn begin_run(&self) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO autonomy_runs (id) VALUES (?)")
            .bind(&id)
            .execute(&self.pool)
            .await
            .context("failed to insert autonomy run")?;
        Ok(id)
    }

    /// Atomically begin a run when no other run is active.
    pub async fn try_begin_run(&self) -> Result<Option<String>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open autonomy run transaction")?;
        let active: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM autonomy_runs WHERE status = 'running'")
                .fetch_one(&mut *tx)
                .await
                .context("failed to count active autonomy runs")?;
        if active > 0 {
            tx.commit()
                .await
                .context("failed to close autonomy run transaction")?;
            return Ok(None);
        }

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO autonomy_runs (id) VALUES (?)")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .context("failed to insert autonomy run")?;
        tx.commit().await.context("failed to commit autonomy run")?;
        Ok(Some(id))
    }

    /// Record the wake events this run consumed ("woken by" provenance).
    pub async fn set_wake_events(&self, run_id: &str, wake_event_ids: &[String]) -> Result<()> {
        let ids_json =
            serde_json::to_string(wake_event_ids).context("failed to serialize wake event ids")?;
        sqlx::query("UPDATE autonomy_runs SET wake_event_ids = ? WHERE id = ?")
            .bind(&ids_json)
            .bind(run_id)
            .execute(&self.pool)
            .await
            .context("failed to record autonomy run wake events")?;
        Ok(())
    }

    /// Mark a run completed with its summary and actions. Only applies while
    /// the run is still `running`, so a late tool call cannot overwrite a run
    /// the driver already finished.
    pub async fn complete_run(
        &self,
        run_id: &str,
        summary: &str,
        actions: &[AutonomyAction],
    ) -> Result<bool> {
        let actions_json =
            serde_json::to_string(actions).context("failed to serialize autonomy actions")?;
        let result = sqlx::query(
            "UPDATE autonomy_runs SET status = 'completed', summary = ?, actions = ?, \
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
             duration_secs = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
             WHERE id = ? AND status = 'running'",
        )
        .bind(summary)
        .bind(&actions_json)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .context("failed to complete autonomy run")?;

        Ok(result.rows_affected() > 0)
    }

    /// Complete a run only after its owned noninteractive workers and branches
    /// are terminal.
    /// `allow_active_children` is used only after the configured run deadline;
    /// it preserves `run_id` attribution without cancelling those workers.
    pub async fn complete_run_if_children_settled(
        &self,
        run_id: &str,
        summary: &str,
        actions: &[AutonomyAction],
        allow_active_children: bool,
    ) -> Result<bool> {
        let actions_json =
            serde_json::to_string(actions).context("failed to serialize autonomy actions")?;
        let result = sqlx::query(
            "UPDATE autonomy_runs SET status = 'completed', summary = ?, actions = ?, \
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
              duration_secs = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
              WHERE id = ? AND status = 'running' AND (? OR NOT EXISTS (\
                  SELECT 1 FROM worker_runs \
                  WHERE run_id = ? AND interactive = FALSE \
                  AND lifecycle NOT IN ('succeeded', 'partial', 'cancelled', 'timed_out', 'blocked', 'failed')\
              ) AND NOT EXISTS (\
                  SELECT 1 FROM branch_runs \
                  WHERE run_id = ? AND status = 'running'\
              ))",
        )
        .bind(summary)
        .bind(&actions_json)
        .bind(run_id)
        .bind(allow_active_children)
        .bind(run_id)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .context("failed to complete settled autonomy run")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn has_active_owned_children(&self, run_id: &str) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT (\
                SELECT COUNT(*) FROM worker_runs \
                WHERE run_id = ? AND interactive = FALSE \
                AND lifecycle NOT IN ('succeeded', 'partial', 'cancelled', 'timed_out', 'blocked', 'failed')\
            ) + (\
                SELECT COUNT(*) FROM branch_runs \
                WHERE run_id = ? AND status = 'running'\
            )",
        )
        .bind(run_id)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count active autonomy children")?;
        Ok(count > 0)
    }

    /// Finish a run with a terminal status (timeout / failed) and an optional
    /// summary. Only applies while the run is still `running`.
    pub async fn finish_run_status(
        &self,
        run_id: &str,
        status: AutonomyRunStatus,
        summary: Option<&str>,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE autonomy_runs SET status = ?, summary = COALESCE(?, summary), \
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
             duration_secs = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
             WHERE id = ? AND status = 'running'",
        )
        .bind(status.as_str())
        .bind(summary)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .context("failed to finish autonomy run")?;

        Ok(result.rows_affected() > 0)
    }

    /// The most recent runs, newest first.
    pub async fn recent(&self, count: u32) -> Result<Vec<AutonomyRun>> {
        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM autonomy_runs ORDER BY started_at DESC, id DESC LIMIT ?"
        ))
        .bind(i64::from(count).clamp(1, 100))
        .fetch_all(&self.pool)
        .await
        .context("failed to list recent autonomy runs")?;

        rows.into_iter().map(run_from_row).collect()
    }

    /// Whether a run is currently in flight. Running rows older than
    /// `stale_after_secs` are treated as dead (crashed drivers) and marked
    /// failed so they cannot block future runs forever.
    pub async fn has_active_run(&self, stale_after_secs: u64) -> Result<bool> {
        let marked = sqlx::query(
            "UPDATE autonomy_runs SET status = 'failed', \
             summary = COALESCE(summary, 'run marked dead after exceeding its timeout'), \
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
             duration_secs = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
             WHERE status = 'running' \
             AND started_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
        )
        .bind(format!("-{stale_after_secs} seconds"))
        .execute(&self.pool)
        .await
        .context("failed to reap stale autonomy runs")?;

        if marked.rows_affected() > 0 {
            tracing::warn!(
                reaped = marked.rows_affected(),
                "marked stale running autonomy runs as failed"
            );
        }

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM autonomy_runs WHERE status = 'running'")
                .fetch_one(&self.pool)
                .await
                .context("failed to count running autonomy runs")?;

        Ok(count > 0)
    }

    /// Close running rows left behind by a previous process before the
    /// resident supervisor starts accepting heartbeats.
    pub async fn reconcile_running_runs(&self, summary: &str) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE autonomy_runs SET status = 'failed', summary = COALESCE(summary, ?), \
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
             duration_secs = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
             WHERE status = 'running'",
        )
        .bind(summary)
        .execute(&self.pool)
        .await
        .context("failed to reconcile interrupted autonomy runs")?;
        Ok(result.rows_affected())
    }

    /// When the most recent run started, if any. Tracked from the table (not
    /// in-memory) so restarts keep the interval anchored.
    pub async fn last_run_started_at(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let value: Option<String> = sqlx::query_scalar("SELECT MAX(started_at) FROM autonomy_runs")
            .fetch_one(&self.pool)
            .await
            .context("failed to read last autonomy run start")?;

        Ok(value.as_deref().and_then(parse_run_timestamp))
    }

    pub async fn run_is_active(&self, run_id: &str) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM autonomy_runs WHERE id = ? AND status = 'running'",
        )
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to read autonomy run status")?;
        Ok(count > 0)
    }
}

/// Parse the store's `strftime('%Y-%m-%dT%H:%M:%fZ')` timestamps.
pub(crate) fn parse_run_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

/// Column list used by all SELECT queries. Kept in sync with `run_from_row`.
const SELECT_COLUMNS: &str = "SELECT id, started_at, finished_at, duration_secs, status, \
     summary, actions, wake_event_ids";

fn run_from_row(row: sqlx::sqlite::SqliteRow) -> Result<AutonomyRun> {
    let status_value: String = row
        .try_get("status")
        .context("failed to read autonomy run status")?;
    let status = AutonomyRunStatus::parse(&status_value)
        .with_context(|| format!("invalid autonomy run status in database: {status_value}"))?;

    let actions_text: String = row.try_get("actions").unwrap_or_else(|_| "[]".to_string());
    let wake_event_ids_text: String = row
        .try_get("wake_event_ids")
        .unwrap_or_else(|_| "[]".to_string());

    Ok(AutonomyRun {
        id: row
            .try_get("id")
            .context("failed to read autonomy run id")?,
        started_at: row
            .try_get("started_at")
            .context("failed to read autonomy run started_at")?,
        finished_at: row.try_get("finished_at").ok().flatten(),
        duration_secs: row.try_get("duration_secs").ok().flatten(),
        status,
        summary: row.try_get("summary").ok().flatten(),
        actions: serde_json::from_str(&actions_text).unwrap_or_default(),
        wake_event_ids: serde_json::from_str(&wake_event_ids_text).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn store() -> AutonomyRunStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        AutonomyRunStore::new(pool)
    }

    fn action(kind: &str, task_number: Option<i64>, detail: &str) -> AutonomyAction {
        AutonomyAction {
            kind: kind.to_string(),
            task_number,
            detail: detail.to_string(),
        }
    }

    #[tokio::test]
    async fn begin_complete_and_recent_round_trip() {
        let store = store().await;

        let run_id = store.begin_run().await.expect("begin");
        assert!(store.has_active_run(3600).await.expect("active"));

        store
            .set_wake_events(&run_id, &["wake-1".to_string(), "wake-2".to_string()])
            .await
            .expect("wake events");

        let completed = store
            .complete_run(
                &run_id,
                "Enriched task #4 with findings.",
                &[action("enriched", Some(4), "added investigation comment")],
            )
            .await
            .expect("complete");
        assert!(completed);
        assert!(!store.has_active_run(3600).await.expect("active"));

        let recent = store.recent(5).await.expect("recent");
        assert_eq!(recent.len(), 1);
        let run = &recent[0];
        assert_eq!(run.id, run_id);
        assert_eq!(run.status, AutonomyRunStatus::Completed);
        assert_eq!(
            run.summary.as_deref(),
            Some("Enriched task #4 with findings.")
        );
        assert_eq!(run.actions.len(), 1);
        assert_eq!(run.actions[0].kind, "enriched");
        assert_eq!(run.actions[0].task_number, Some(4));
        assert_eq!(run.wake_event_ids, vec!["wake-1", "wake-2"]);
        assert!(run.finished_at.is_some());
        assert!(run.duration_secs.is_some());
    }

    #[tokio::test]
    async fn complete_only_applies_to_running_rows() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");

        assert!(
            store
                .finish_run_status(&run_id, AutonomyRunStatus::Timeout, Some("timed out"))
                .await
                .expect("finish")
        );
        // A late completion call after the driver finished the run is a no-op.
        assert!(
            !store
                .complete_run(&run_id, "late summary", &[])
                .await
                .expect("late complete")
        );

        let recent = store.recent(1).await.expect("recent");
        assert_eq!(recent[0].status, AutonomyRunStatus::Timeout);
        assert_eq!(recent[0].summary.as_deref(), Some("timed out"));
    }

    async fn add_worker(store: &AutonomyRunStore, run_id: &str, worker_id: &str, lifecycle: &str) {
        sqlx::query(
            "INSERT INTO worker_runs (id, task, worker_type, agent_id, interactive, lifecycle, status, run_id) \
             VALUES (?, 'task', 'builtin', 'agent', FALSE, ?, 'running', ?)",
        )
        .bind(worker_id)
        .bind(lifecycle)
        .bind(run_id)
        .execute(&store.pool)
        .await
        .expect("add worker");
    }

    async fn add_branch(store: &AutonomyRunStore, run_id: &str, branch_id: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO channels (id, platform, display_name) \
             VALUES ('autonomy', 'system', 'Autonomy')",
        )
        .execute(&store.pool)
        .await
        .expect("add channel");
        sqlx::query(
            "INSERT INTO branch_runs (id, channel_id, description, input, status, profile, max_turns, run_id) \
             VALUES (?, 'autonomy', 'branch', 'prompt', 'running', 'default', 10, ?)",
        )
        .bind(branch_id)
        .bind(run_id)
        .execute(&store.pool)
        .await
        .expect("add branch");
    }

    #[tokio::test]
    async fn active_owned_child_prevents_completion() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");
        add_worker(&store, &run_id, "worker-1", "running").await;

        assert!(
            store
                .has_active_owned_children(&run_id)
                .await
                .expect("active children")
        );
        assert!(
            !store
                .complete_run_if_children_settled(&run_id, "summary", &[], false)
                .await
                .expect("complete")
        );

        let recent = store.recent(1).await.expect("recent");
        assert_eq!(recent[0].status, AutonomyRunStatus::Running);
    }

    #[tokio::test]
    async fn active_owned_branch_prevents_completion() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");
        add_branch(&store, &run_id, "branch-1").await;

        assert!(
            store
                .has_active_owned_children(&run_id)
                .await
                .expect("active children")
        );
        assert!(
            !store
                .complete_run_if_children_settled(&run_id, "summary", &[], false)
                .await
                .expect("complete")
        );

        sqlx::query(
            "UPDATE branch_runs SET status = 'done', completed_at = CURRENT_TIMESTAMP \
             WHERE id = 'branch-1' AND status = 'running'",
        )
        .execute(&store.pool)
        .await
        .expect("complete branch");

        assert!(
            store
                .complete_run_if_children_settled(&run_id, "summary", &[], false)
                .await
                .expect("complete after branch")
        );
    }

    #[tokio::test]
    async fn child_completion_race_commits_summary_once() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");
        add_worker(&store, &run_id, "worker-1", "completing").await;

        assert!(
            !store
                .complete_run_if_children_settled(&run_id, "summary", &[], false)
                .await
                .expect("complete while child completes")
        );

        sqlx::query(
            "UPDATE worker_runs SET lifecycle = 'succeeded', status = 'done', completed_at = CURRENT_TIMESTAMP \
             WHERE id = 'worker-1' AND lifecycle = 'completing'",
        )
        .execute(&store.pool)
        .await
        .expect("complete child");

        assert!(
            store
                .complete_run_if_children_settled(&run_id, "summary", &[], false)
                .await
                .expect("complete after child")
        );
        assert!(
            !store
                .complete_run_if_children_settled(&run_id, "duplicate", &[], false)
                .await
                .expect("duplicate completion")
        );

        let recent = store.recent(1).await.expect("recent");
        assert_eq!(recent[0].status, AutonomyRunStatus::Completed);
        assert_eq!(recent[0].summary.as_deref(), Some("summary"));
    }

    #[tokio::test]
    async fn stale_running_rows_are_reaped() {
        let store = store().await;
        let run_id = store.begin_run().await.expect("begin");

        // A fresh running row within the stale window counts as active.
        assert!(store.has_active_run(3600).await.expect("active"));

        // Backdate the row past the stale window; the next check reaps it.
        sqlx::query("UPDATE autonomy_runs SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours') WHERE id = ?")
            .bind(&run_id)
            .execute(&store.pool)
            .await
            .expect("backdate");
        assert!(!store.has_active_run(3600).await.expect("active"));

        let recent = store.recent(1).await.expect("recent");
        assert_eq!(recent[0].status, AutonomyRunStatus::Failed);
        assert!(recent[0].summary.as_deref().unwrap_or("").contains("dead"));
    }

    #[tokio::test]
    async fn try_begin_run_is_single_flight() {
        let store = store().await;
        let first = store.try_begin_run().await.expect("first admission");
        let second = store.try_begin_run().await.expect("second admission");

        assert!(first.is_some());
        assert!(second.is_none());
        assert_eq!(
            store
                .recent(10)
                .await
                .expect("runs")
                .into_iter()
                .filter(|run| run.status == AutonomyRunStatus::Running)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn startup_reconciliation_closes_only_running_epochs() {
        let store = store().await;
        let interrupted = store.begin_run().await.expect("interrupted run");
        let completed = store.begin_run().await.expect("completed run");
        store
            .complete_run(&completed, "already complete", &[])
            .await
            .expect("complete run");

        assert_eq!(
            store
                .reconcile_running_runs("interrupted by restart")
                .await
                .expect("reconcile"),
            1
        );
        let runs = store.recent(10).await.expect("runs");
        let interrupted = runs.iter().find(|run| run.id == interrupted).unwrap();
        let completed = runs.iter().find(|run| run.id == completed).unwrap();
        assert_eq!(interrupted.status, AutonomyRunStatus::Failed);
        assert_eq!(
            interrupted.summary.as_deref(),
            Some("interrupted by restart")
        );
        assert_eq!(completed.status, AutonomyRunStatus::Completed);
        assert_eq!(completed.summary.as_deref(), Some("already complete"));
    }

    #[tokio::test]
    async fn last_run_started_at_tracks_newest_run() {
        let store = store().await;
        assert!(store.last_run_started_at().await.expect("empty").is_none());

        let run_id = store.begin_run().await.expect("begin");
        store
            .complete_run(&run_id, "done", &[])
            .await
            .expect("complete");

        let last = store
            .last_run_started_at()
            .await
            .expect("query")
            .expect("timestamp");
        assert!((chrono::Utc::now() - last).num_seconds().abs() < 60);
    }
}
