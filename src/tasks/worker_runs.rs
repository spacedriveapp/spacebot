//! Every worker run attempted against a task.
//!
//! `tasks.worker_id` names the run executing right now and is overwritten by
//! the next spawn, so a task retried three times remembers only the last one.
//! That is enough to route a reply and not enough to decide anything: an
//! autonomous loop picking work off the board has to know what has already been
//! tried and how it ended before it spawns again, or it repeats failed work
//! forever.
//!
//! The reference to the worker is a bare id. Tasks live in the instance
//! database and `worker_runs` lives in the per-agent database, so the link
//! crosses a database boundary and no foreign key can enforce it. A run whose
//! worker row has been pruned still records that the attempt happened.

use crate::error::Result;
use crate::tasks::revisions::TaskAuthorKind;
use crate::tasks::store::TaskStore;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, sqlite::SqliteRow};

/// Hard ceiling on rows returned by a single attempt-history call.
pub const MAX_ATTEMPT_PAGE: i64 = 100;

/// How much of a run's result is kept on the attempt.
///
/// The full result lives on the worker record; this is the line the board and
/// the prompt context read, and a worker can return a great deal of text.
const MAX_ATTEMPT_SUMMARY_CHARS: usize = 280;

/// How a worker run ended, from the task's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskAttemptOutcome {
    Succeeded,
    /// Reached its budget with real work delivered but the task unfinished.
    Partial,
    /// Stopped waiting on something outside its control.
    Blocked,
    Failed,
    Cancelled,
    TimedOut,
    /// The process died before the run reached a terminal state. Distinct from
    /// a failure: nothing was decided about the work itself.
    Interrupted,
}

impl TaskAttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "succeeded" => Some(Self::Succeeded),
            "partial" => Some(Self::Partial),
            "blocked" => Some(Self::Blocked),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "timed_out" => Some(Self::TimedOut),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    /// Whether this outcome means the work was actually delivered.
    pub fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// The worker's committed terminal kind is what an attempt records.
///
/// There is no `Interrupted` on the worker side: that outcome describes a run
/// with no terminal record at all, which is the one case this conversion cannot
/// be reached from.
impl From<crate::conversation::WorkerOutcomeKind> for TaskAttemptOutcome {
    fn from(kind: crate::conversation::WorkerOutcomeKind) -> Self {
        use crate::conversation::WorkerOutcomeKind as Kind;
        match kind {
            Kind::Succeeded => Self::Succeeded,
            Kind::Partial => Self::Partial,
            Kind::Blocked => Self::Blocked,
            Kind::Failed => Self::Failed,
            Kind::Cancelled => Self::Cancelled,
            Kind::TimedOut => Self::TimedOut,
        }
    }
}

impl std::fmt::Display for TaskAttemptOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// One worker run recorded against a task.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskAttempt {
    pub id: String,
    pub task_id: String,
    pub worker_id: String,
    /// 1 for the first run on this task.
    pub attempt: i64,
    pub author_type: TaskAuthorKind,
    pub author_id: Option<String>,
    pub agent_id: Option<String>,
    pub channel_id: Option<String>,
    pub started_at: String,
    /// `None` while the run is still live.
    pub outcome: Option<TaskAttemptOutcome>,
    pub outcome_summary: Option<String>,
    pub ended_at: Option<String>,
}

impl TaskAttempt {
    /// Whether this run has not reached a terminal state.
    pub fn is_live(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// What to record when a run starts.
#[derive(Debug, Clone, Default)]
pub struct StartTaskAttempt {
    pub worker_id: String,
    pub author_type: TaskAuthorKind,
    pub author_id: Option<String>,
    pub agent_id: Option<String>,
    pub channel_id: Option<String>,
}

const ATTEMPT_COLUMNS: &str = "SELECT id, task_id, worker_id, attempt, author_type, author_id, \
     agent_id, channel_id, started_at, outcome_kind, outcome_summary, ended_at \
     FROM task_worker_runs";

fn attempt_from_row(row: &SqliteRow) -> Result<TaskAttempt> {
    let author_type: String = row.try_get("author_type").unwrap_or_default();
    let outcome_kind: Option<String> = row.try_get("outcome_kind").ok().flatten();

    Ok(TaskAttempt {
        id: row.try_get("id").context("attempt row missing id")?,
        task_id: row
            .try_get("task_id")
            .context("attempt row missing task_id")?,
        worker_id: row
            .try_get("worker_id")
            .context("attempt row missing worker_id")?,
        attempt: row
            .try_get("attempt")
            .context("attempt row missing attempt")?,
        author_type: TaskAuthorKind::parse(&author_type).unwrap_or(TaskAuthorKind::System),
        author_id: row.try_get("author_id").ok().flatten(),
        agent_id: row.try_get("agent_id").ok().flatten(),
        channel_id: row.try_get("channel_id").ok().flatten(),
        started_at: row
            .try_get("started_at")
            .context("attempt row missing started_at")?,
        outcome: outcome_kind.as_deref().and_then(TaskAttemptOutcome::parse),
        outcome_summary: row.try_get("outcome_summary").ok().flatten(),
        ended_at: row.try_get("ended_at").ok().flatten(),
    })
}

impl TaskStore {
    /// Record that a worker run has started against a task.
    ///
    /// The attempt number is allocated inside the transaction, so two spawns
    /// racing on the same task cannot both claim the same ordinal. Re-recording
    /// the same worker returns the existing row rather than a second attempt,
    /// which makes a retried bind idempotent.
    pub async fn start_task_attempt(
        &self,
        task_number: i64,
        input: StartTaskAttempt,
    ) -> Result<Option<TaskAttempt>> {
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open task attempt transaction")?;

        let task_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM tasks WHERE task_number = ?")
                .bind(task_number)
                .fetch_optional(&mut *tx)
                .await
                .context("failed to resolve task for attempt")?;

        let Some(task_id) = task_id else {
            tx.rollback()
                .await
                .context("failed to roll back task attempt transaction")?;
            return Ok(None);
        };

        let existing = sqlx::query(&format!(
            "{ATTEMPT_COLUMNS} WHERE task_id = ? AND worker_id = ?"
        ))
        .bind(&task_id)
        .bind(&input.worker_id)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to check for an existing attempt")?;

        if let Some(row) = existing {
            let attempt = attempt_from_row(&row)?;
            tx.commit()
                .await
                .context("failed to commit task attempt transaction")?;
            return Ok(Some(attempt));
        }

        let next_attempt: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(attempt), 0) + 1 FROM task_worker_runs WHERE task_id = ?",
        )
        .bind(&task_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to allocate an attempt number")?;

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_worker_runs \
             (id, task_id, worker_id, attempt, author_type, author_id, agent_id, channel_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&task_id)
        .bind(&input.worker_id)
        .bind(next_attempt)
        .bind(input.author_type.as_str())
        .bind(&input.author_id)
        .bind(&input.agent_id)
        .bind(&input.channel_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            // The live-attempt index is what settles two spawns racing on the
            // same task, so a unique violation here is a lost race rather than
            // a storage fault, and the caller has to be able to tell them apart.
            if matches!(&error, sqlx::Error::Database(db) if db.is_unique_violation()) {
                anyhow::anyhow!(
                    "task #{task_number} already has a live attempt — another spawn claimed it first"
                )
            } else {
                anyhow::Error::new(error).context("failed to record task attempt")
            }
        })?;

        let row = sqlx::query(&format!("{ATTEMPT_COLUMNS} WHERE id = ?"))
            .bind(&id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to reload the recorded attempt")?;
        let attempt = attempt_from_row(&row)?;

        tx.commit()
            .await
            .context("failed to commit task attempt transaction")?;
        Ok(Some(attempt))
    }

    /// Record how a run ended.
    ///
    /// Terminal state is written once: a second call for the same worker leaves
    /// the first outcome in place, so a duplicated completion cannot rewrite
    /// history. Returns whether this call was the one that closed the run.
    pub async fn finish_task_attempt(
        &self,
        worker_id: &str,
        outcome: TaskAttemptOutcome,
        summary: Option<&str>,
    ) -> Result<bool> {
        let summary: Option<String> = summary
            .map(|text| text.chars().take(MAX_ATTEMPT_SUMMARY_CHARS).collect())
            .filter(|text: &String| !text.is_empty());

        let affected = sqlx::query(
            "UPDATE task_worker_runs \
             SET outcome_kind = ?, outcome_summary = ?, \
                 ended_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE worker_id = ? AND ended_at IS NULL",
        )
        .bind(outcome.as_str())
        .bind(summary)
        .bind(worker_id)
        .execute(self.pool())
        .await
        .context("failed to record task attempt outcome")?
        .rows_affected();

        Ok(affected > 0)
    }

    /// The runs attempted against a task, newest first.
    pub async fn list_task_attempts(
        &self,
        task_number: i64,
        limit: i64,
    ) -> Result<Vec<TaskAttempt>> {
        let limit = limit.clamp(1, MAX_ATTEMPT_PAGE);
        let rows = sqlx::query(&format!(
            "{ATTEMPT_COLUMNS} WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             ORDER BY attempt DESC LIMIT ?"
        ))
        .bind(task_number)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .context("failed to list task attempts")?;

        rows.iter().map(attempt_from_row).collect()
    }

    /// Every run attempted against a task, newest first.
    ///
    /// Internal execution briefings use the complete history. User-facing list
    /// endpoints remain bounded through [`Self::list_task_attempts`].
    pub(crate) async fn all_task_attempts(&self, task_number: i64) -> Result<Vec<TaskAttempt>> {
        let rows = sqlx::query(&format!(
            "{ATTEMPT_COLUMNS} WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             ORDER BY attempt DESC"
        ))
        .bind(task_number)
        .fetch_all(self.pool())
        .await
        .context("failed to load complete task attempt history")?;

        rows.iter().map(attempt_from_row).collect()
    }

    /// Every attempt still open, across all tasks.
    ///
    /// Read at startup to recover runs whose worker reached a terminal state
    /// that the attempt never learned about, before the rest are swept.
    pub async fn live_attempts(&self) -> Result<Vec<TaskAttempt>> {
        let rows = sqlx::query(&format!(
            "{ATTEMPT_COLUMNS} WHERE ended_at IS NULL ORDER BY started_at"
        ))
        .fetch_all(self.pool())
        .await
        .context("failed to list live task attempts")?;

        rows.iter().map(attempt_from_row).collect()
    }

    /// Close attempts left live by a process that died.
    ///
    /// Workers run in-process, so every attempt still open at startup belongs
    /// to a run that no longer exists. Without this the spawn guard would see a
    /// live attempt forever and the task could never be worked again — a crash
    /// mid-run would permanently take that task off the board.
    ///
    /// Recorded as interrupted rather than failed: the process died, which says
    /// nothing about whether the work was going to succeed. Runs that did reach
    /// a terminal state are closed with it beforehand, from `live_attempts`, so
    /// this only reaches the ones nothing decided.
    pub async fn reconcile_interrupted_attempts(&self) -> Result<usize> {
        let affected = sqlx::query(
            "UPDATE task_worker_runs \
             SET outcome_kind = ?, \
                 outcome_summary = COALESCE(outcome_summary, ?), \
                 ended_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE ended_at IS NULL",
        )
        .bind(TaskAttemptOutcome::Interrupted.as_str())
        .bind("The process running this attempt exited before it finished.")
        .execute(self.pool())
        .await
        .context("failed to reconcile interrupted task attempts")?
        .rows_affected();

        Ok(affected as usize)
    }

    /// Prior-attempt lines for a set of tasks, keyed by task number.
    ///
    /// One query for the whole board. Rendering prompt context must not issue a
    /// query per task, and a task that has never been attempted is simply
    /// absent from the map rather than carrying an empty entry.
    pub async fn prior_attempt_summaries(
        &self,
        task_numbers: &[i64],
    ) -> Result<std::collections::HashMap<i64, String>> {
        if task_numbers.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let placeholders = std::iter::repeat_n("?", task_numbers.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT t.task_number AS task_number, r.id, r.task_id, r.worker_id, r.attempt, \
             r.author_type, r.author_id, r.agent_id, r.channel_id, r.started_at, \
             r.outcome_kind, r.outcome_summary, r.ended_at \
             FROM task_worker_runs r JOIN tasks t ON t.id = r.task_id \
             WHERE t.task_number IN ({placeholders}) ORDER BY r.attempt DESC"
        );

        let mut query = sqlx::query(&sql);
        for number in task_numbers {
            query = query.bind(number);
        }
        let rows = query
            .fetch_all(self.pool())
            .await
            .context("failed to load attempt history for the board")?;

        let mut grouped: std::collections::HashMap<i64, Vec<TaskAttempt>> =
            std::collections::HashMap::new();
        for row in &rows {
            let number: i64 = row
                .try_get("task_number")
                .context("attempt row missing task_number")?;
            grouped
                .entry(number)
                .or_default()
                .push(attempt_from_row(row)?);
        }

        Ok(grouped
            .into_iter()
            .filter_map(|(number, attempts)| {
                render_prior_attempts(&attempts).map(|line| (number, line))
            })
            .collect())
    }

    /// The task a worker was spawned for, if it was spawned for one.
    pub async fn task_number_for_worker(&self, worker_id: &str) -> Result<Option<i64>> {
        let number: Option<i64> = sqlx::query_scalar(
            "SELECT t.task_number FROM task_worker_runs r \
             JOIN tasks t ON t.id = r.task_id \
             WHERE r.worker_id = ?",
        )
        .bind(worker_id)
        .fetch_optional(self.pool())
        .await
        .context("failed to resolve the task for a worker")?;

        Ok(number)
    }

    /// The run currently executing against a task, if any.
    ///
    /// This is what makes a spawn guard task-scoped rather than channel-scoped:
    /// it sees a live run no matter which channel started it.
    pub async fn live_task_attempt(&self, task_number: i64) -> Result<Option<TaskAttempt>> {
        let row = sqlx::query(&format!(
            "{ATTEMPT_COLUMNS} WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             AND ended_at IS NULL ORDER BY attempt DESC LIMIT 1"
        ))
        .bind(task_number)
        .fetch_optional(self.pool())
        .await
        .context("failed to look for a live task attempt")?;

        row.as_ref().map(attempt_from_row).transpose()
    }
}

/// One line summarising what has already been tried on a task.
///
/// Rendered into prompt context so a spawn decision is made knowing the
/// history. Bounded on purpose: a heavily retried task must not crowd out the
/// rest of the board.
pub fn render_prior_attempts(attempts: &[TaskAttempt]) -> Option<String> {
    let finished: Vec<&TaskAttempt> = attempts.iter().filter(|a| !a.is_live()).collect();
    let live = attempts.iter().find(|a| a.is_live());

    if finished.is_empty() && live.is_none() {
        return None;
    }

    let mut parts = Vec::new();

    if !finished.is_empty() {
        let outcomes: Vec<String> = finished
            .iter()
            .take(3)
            .map(|attempt| {
                let outcome = attempt
                    .outcome
                    .map(|o| o.to_string())
                    .unwrap_or_else(|| "ended without an outcome".to_string());
                format!("#{} {}", attempt.attempt, outcome)
            })
            .collect();

        let plural = if finished.len() == 1 {
            "attempt"
        } else {
            "attempts"
        };
        parts.push(format!(
            "{} prior {plural} ({})",
            finished.len(),
            outcomes.join(", ")
        ));
    }

    if let Some(live) = live {
        parts.push(format!("attempt #{} is running now", live.attempt));
    }

    Some(parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::WorkerOutcomeKind;
    use crate::tasks::store::{CreateTaskInput, setup_test_store};

    fn task_input(title: &str) -> CreateTaskInput {
        CreateTaskInput {
            owner_agent_id: "main".to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    async fn store_with_task() -> (TaskStore, i64) {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("linkage"))
            .await
            .expect("task should be created");
        (store, task.task_number)
    }

    fn start(worker_id: &str) -> StartTaskAttempt {
        StartTaskAttempt {
            worker_id: worker_id.to_string(),
            author_type: TaskAuthorKind::Agent,
            author_id: Some("main".to_string()),
            agent_id: Some("main".to_string()),
            channel_id: Some("telegram:1".to_string()),
        }
    }

    /// The record this whole module exists for: a task run three times keeps
    /// all three, where `tasks.worker_id` would remember only the last.
    #[tokio::test]
    async fn every_attempt_is_kept_with_its_outcome() {
        let (store, number) = store_with_task().await;

        for (worker, outcome) in [
            ("worker-a", TaskAttemptOutcome::Failed),
            ("worker-b", TaskAttemptOutcome::TimedOut),
            ("worker-c", TaskAttemptOutcome::Succeeded),
        ] {
            store
                .start_task_attempt(number, start(worker))
                .await
                .expect("start should succeed")
                .expect("task exists");
            store
                .finish_task_attempt(worker, outcome, Some("summary"))
                .await
                .expect("finish should succeed");
        }

        let attempts = store
            .list_task_attempts(number, 10)
            .await
            .expect("history should load");

        assert_eq!(attempts.len(), 3);
        // Newest first.
        assert_eq!(attempts[0].attempt, 3);
        assert_eq!(attempts[0].worker_id, "worker-c");
        assert_eq!(attempts[0].outcome, Some(TaskAttemptOutcome::Succeeded));
        assert_eq!(attempts[2].attempt, 1);
        assert_eq!(attempts[2].outcome, Some(TaskAttemptOutcome::Failed));
        assert!(attempts.iter().all(|a| !a.is_live()));
    }

    /// The reverse lookup the API had no way to answer.
    #[tokio::test]
    async fn a_worker_resolves_back_to_its_task() {
        let (store, number) = store_with_task().await;
        store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");

        assert_eq!(
            store
                .task_number_for_worker("worker-1")
                .await
                .expect("lookup should succeed"),
            Some(number)
        );
        assert_eq!(
            store
                .task_number_for_worker("worker-unknown")
                .await
                .expect("lookup should succeed"),
            None
        );
    }

    /// A live run is visible regardless of which channel started it, which is
    /// what lets a spawn guard be task-scoped rather than channel-scoped.
    #[tokio::test]
    async fn a_live_attempt_is_visible_until_it_ends() {
        let (store, number) = store_with_task().await;
        assert!(
            store
                .live_task_attempt(number)
                .await
                .expect("lookup should succeed")
                .is_none()
        );

        let mut other_channel = start("worker-1");
        other_channel.channel_id = Some("discord:99".to_string());
        store
            .start_task_attempt(number, other_channel)
            .await
            .expect("start should succeed")
            .expect("task exists");

        let live = store
            .live_task_attempt(number)
            .await
            .expect("lookup should succeed")
            .expect("a run is live");
        assert_eq!(live.worker_id, "worker-1");
        assert_eq!(live.channel_id.as_deref(), Some("discord:99"));

        store
            .finish_task_attempt("worker-1", TaskAttemptOutcome::Succeeded, None)
            .await
            .expect("finish should succeed");
        assert!(
            store
                .live_task_attempt(number)
                .await
                .expect("lookup should succeed")
                .is_none()
        );
    }

    /// The spawn guard reads before it writes, so two channels can both find a
    /// task free. Storage is what settles it: the second worker is refused, and
    /// the task opens again once the first run ends.
    #[tokio::test]
    async fn only_one_run_can_be_live_on_a_task() {
        let (store, number) = store_with_task().await;

        store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");

        let raced = store
            .start_task_attempt(number, start("worker-2"))
            .await
            .expect_err("a second live run should be refused");
        assert!(
            raced.to_string().contains("already has a live attempt"),
            "unexpected error: {raced}"
        );
        assert_eq!(
            store
                .list_task_attempts(number, 10)
                .await
                .expect("history should load")
                .len(),
            1
        );

        store
            .finish_task_attempt("worker-1", TaskAttemptOutcome::Failed, None)
            .await
            .expect("finish should succeed");

        let retry = store
            .start_task_attempt(number, start("worker-2"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        assert_eq!(retry.attempt, 2);
    }

    /// Attempts carry worker ids and outcome text, so they must not outlive the
    /// task. Foreign-key enforcement is not guaranteed to be on, which is why
    /// the delete is explicit.
    #[tokio::test]
    async fn deleting_a_task_deletes_its_attempts() {
        let (store, number) = store_with_task().await;
        store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        store
            .finish_task_attempt("worker-1", TaskAttemptOutcome::Succeeded, Some("done"))
            .await
            .expect("finish should succeed");

        // The condition the explicit delete exists for: with enforcement off,
        // the cascade on the foreign key does nothing.
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(store.pool())
            .await
            .expect("pragma should apply");

        assert!(store.delete(number).await.expect("delete should succeed"));

        assert_eq!(
            store
                .task_number_for_worker("worker-1")
                .await
                .expect("lookup should succeed"),
            None
        );
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_worker_runs")
            .fetch_one(store.pool())
            .await
            .expect("count should succeed");
        assert_eq!(remaining, 0);
    }

    /// Re-binding the same worker must not invent a second attempt, so a
    /// retried bind after a transient failure stays idempotent.
    #[tokio::test]
    async fn re_recording_the_same_worker_reuses_its_attempt() {
        let (store, number) = store_with_task().await;

        let first = store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        let again = store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");

        assert_eq!(first.id, again.id);
        assert_eq!(again.attempt, 1);
        assert_eq!(
            store
                .list_task_attempts(number, 10)
                .await
                .expect("history should load")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn execution_briefing_reads_attempts_beyond_the_api_page_limit() {
        let (store, number) = store_with_task().await;
        let total = MAX_ATTEMPT_PAGE + 1;
        for index in 0..total {
            let worker_id = format!("worker-{index}");
            store
                .start_task_attempt(number, start(&worker_id))
                .await
                .expect("start should succeed")
                .expect("task exists");
            store
                .finish_task_attempt(&worker_id, TaskAttemptOutcome::Succeeded, Some("finished"))
                .await
                .expect("finish should succeed");
        }

        assert_eq!(
            store
                .list_task_attempts(number, total)
                .await
                .expect("bounded history should load")
                .len(),
            MAX_ATTEMPT_PAGE as usize
        );
        assert_eq!(
            store
                .all_task_attempts(number)
                .await
                .expect("complete history should load")
                .len(),
            total as usize
        );
    }

    /// A duplicated completion must not rewrite how the run ended.
    #[tokio::test]
    async fn terminal_state_is_written_once() {
        let (store, number) = store_with_task().await;
        store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");

        assert!(
            store
                .finish_task_attempt("worker-1", TaskAttemptOutcome::Succeeded, Some("done"))
                .await
                .expect("finish should succeed")
        );
        assert!(
            !store
                .finish_task_attempt("worker-1", TaskAttemptOutcome::Failed, Some("nope"))
                .await
                .expect("finish should succeed"),
            "a second completion must not close the run again"
        );

        let attempts = store
            .list_task_attempts(number, 10)
            .await
            .expect("history should load");
        assert_eq!(attempts[0].outcome, Some(TaskAttemptOutcome::Succeeded));
        assert_eq!(attempts[0].outcome_summary.as_deref(), Some("done"));
    }

    /// The two writes that close a run land in different databases, so a worker
    /// can commit its outcome and the attempt still be open. Startup recovers
    /// what the worker committed before the sweep runs, or a run that succeeded
    /// would be recorded as interrupted and an autonomous loop would retry work
    /// that was already delivered.
    #[tokio::test]
    async fn a_committed_outcome_is_recovered_before_the_sweep() {
        let (store, number) = store_with_task().await;
        let other = store
            .create(task_input("swept"))
            .await
            .expect("task should be created");
        store
            .start_task_attempt(number, start("worker-committed"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        store
            .start_task_attempt(other.task_number, start("worker-vanished"))
            .await
            .expect("start should succeed")
            .expect("task exists");

        let live = store
            .live_attempts()
            .await
            .expect("live attempts should load");
        assert_eq!(live.len(), 2);
        assert!(live.iter().all(|attempt| attempt.is_live()));

        // What the startup pass does for a run whose worker record has an
        // outcome; the other worker left nothing behind.
        store
            .finish_task_attempt(
                "worker-committed",
                WorkerOutcomeKind::Succeeded.into(),
                Some("shipped it"),
            )
            .await
            .expect("recovery should succeed");

        let swept = store
            .reconcile_interrupted_attempts()
            .await
            .expect("reconcile should succeed");
        assert_eq!(swept, 1, "only the undecided run is swept");

        let recovered = store
            .list_task_attempts(number, 10)
            .await
            .expect("history should load");
        assert_eq!(recovered[0].outcome, Some(TaskAttemptOutcome::Succeeded));
        assert_eq!(recovered[0].outcome_summary.as_deref(), Some("shipped it"));

        let interrupted = store
            .list_task_attempts(other.task_number, 10)
            .await
            .expect("history should load");
        assert_eq!(
            interrupted[0].outcome,
            Some(TaskAttemptOutcome::Interrupted)
        );
    }

    /// A worker can return a great deal of text and the board reads this line.
    #[tokio::test]
    async fn an_attempt_summary_is_bounded() {
        let (store, number) = store_with_task().await;
        store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        store
            .finish_task_attempt(
                "worker-1",
                TaskAttemptOutcome::Succeeded,
                Some(&"x".repeat(5_000)),
            )
            .await
            .expect("finish should succeed");

        let attempts = store
            .list_task_attempts(number, 10)
            .await
            .expect("history should load");
        assert_eq!(
            attempts[0]
                .outcome_summary
                .as_deref()
                .map(|summary| summary.chars().count()),
            Some(MAX_ATTEMPT_SUMMARY_CHARS)
        );
    }

    /// A crash mid-run must not take the task off the board for good.
    #[tokio::test]
    async fn a_restart_closes_a_live_attempt_and_unblocks_the_task() {
        let (store, number) = store_with_task().await;
        store
            .start_task_attempt(number, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        assert!(
            store
                .live_task_attempt(number)
                .await
                .expect("lookup should succeed")
                .is_some()
        );

        let closed = store
            .reconcile_interrupted_attempts()
            .await
            .expect("reconcile should succeed");

        assert_eq!(closed, 1);
        assert!(
            store
                .live_task_attempt(number)
                .await
                .expect("lookup should succeed")
                .is_none(),
            "the task must be spawnable again after a restart"
        );

        let attempts = store
            .list_task_attempts(number, 10)
            .await
            .expect("history should load");
        assert_eq!(attempts[0].outcome, Some(TaskAttemptOutcome::Interrupted));

        // A run that already ended keeps the outcome it recorded.
        assert_eq!(
            store
                .reconcile_interrupted_attempts()
                .await
                .expect("second reconcile"),
            0
        );
    }

    #[tokio::test]
    async fn an_attempt_on_a_missing_task_is_not_recorded() {
        let store = setup_test_store().await;
        assert!(
            store
                .start_task_attempt(4242, start("worker-1"))
                .await
                .expect("start should succeed")
                .is_none()
        );
    }

    /// The board renders in one query, and a task never attempted is absent
    /// rather than carrying an empty line.
    #[tokio::test]
    async fn board_summaries_cover_only_attempted_tasks() {
        let store = setup_test_store().await;
        let attempted = store
            .create(task_input("attempted"))
            .await
            .expect("task should be created")
            .task_number;
        let untouched = store
            .create(task_input("untouched"))
            .await
            .expect("task should be created")
            .task_number;

        store
            .start_task_attempt(attempted, start("worker-1"))
            .await
            .expect("start should succeed")
            .expect("task exists");
        store
            .finish_task_attempt("worker-1", TaskAttemptOutcome::Failed, None)
            .await
            .expect("finish should succeed");
        store
            .start_task_attempt(attempted, start("worker-2"))
            .await
            .expect("start should succeed")
            .expect("task exists");

        let summaries = store
            .prior_attempt_summaries(&[attempted, untouched])
            .await
            .expect("summaries should load");

        assert_eq!(summaries.len(), 1);
        let line = summaries
            .get(&attempted)
            .expect("attempted task summarised");
        assert!(line.contains("1 prior attempt"), "{line}");
        assert!(line.contains("#1 failed"), "{line}");
        assert!(line.contains("attempt #2 is running now"), "{line}");
        assert!(!summaries.contains_key(&untouched));
    }

    #[tokio::test]
    async fn board_summaries_are_empty_without_tasks() {
        let store = setup_test_store().await;
        assert!(
            store
                .prior_attempt_summaries(&[])
                .await
                .expect("summaries should load")
                .is_empty()
        );
    }

    #[test]
    fn prior_attempts_render_nothing_for_a_fresh_task() {
        assert_eq!(render_prior_attempts(&[]), None);
    }

    #[test]
    fn prior_attempts_name_the_outcomes_and_the_live_run() {
        let attempt = |n: i64, outcome: Option<TaskAttemptOutcome>, ended: bool| TaskAttempt {
            id: format!("id-{n}"),
            task_id: "task-1".to_string(),
            worker_id: format!("worker-{n}"),
            attempt: n,
            author_type: TaskAuthorKind::Agent,
            author_id: None,
            agent_id: None,
            channel_id: None,
            started_at: "2026-08-14T00:00:00Z".to_string(),
            outcome,
            outcome_summary: None,
            ended_at: ended.then(|| "2026-08-14T01:00:00Z".to_string()),
        };

        let rendered = render_prior_attempts(&[
            attempt(3, None, false),
            attempt(2, Some(TaskAttemptOutcome::TimedOut), true),
            attempt(1, Some(TaskAttemptOutcome::Failed), true),
        ])
        .expect("a task with history renders");

        assert!(rendered.contains("2 prior attempts"));
        assert!(rendered.contains("#2 timed_out"));
        assert!(rendered.contains("#1 failed"));
        assert!(rendered.contains("attempt #3 is running now"));
    }

    /// A heavily retried task must not crowd the board out of the prompt.
    #[test]
    fn prior_attempts_are_bounded() {
        let attempts: Vec<TaskAttempt> = (1..=20)
            .rev()
            .map(|n| TaskAttempt {
                id: format!("id-{n}"),
                task_id: "task-1".to_string(),
                worker_id: format!("worker-{n}"),
                attempt: n,
                author_type: TaskAuthorKind::Agent,
                author_id: None,
                agent_id: None,
                channel_id: None,
                started_at: "2026-08-14T00:00:00Z".to_string(),
                outcome: Some(TaskAttemptOutcome::Failed),
                outcome_summary: None,
                ended_at: Some("2026-08-14T01:00:00Z".to_string()),
            })
            .collect();

        let rendered = render_prior_attempts(&attempts).expect("renders");
        assert!(rendered.contains("20 prior attempts"));
        assert_eq!(rendered.matches('#').count(), 3, "only three are named");
    }
}
