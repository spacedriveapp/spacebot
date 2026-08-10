//! Append-only task comments, enrichment cadence, and autonomous claiming.
//!
//! Comments live in the same instance database as tasks, so every write that
//! has to move together — insert a comment and stamp `last_enriched_at`,
//! delete a task and its comments — happens in one transaction.

use crate::error::Result;
use crate::tasks::store::{SELECT_COLUMNS, Task, TaskStatus, TaskStore, task_from_row};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, sqlite::SqliteRow};

/// Longest comment body accepted, in bytes. Comments are synthesised findings,
/// not transcripts — worker output stays behind the `worker_id` link.
pub const MAX_COMMENT_BODY_BYTES: usize = 4000;

/// Shortest comment body accepted, in characters after trimming.
pub const MIN_COMMENT_BODY_CHARS: usize = 4;

/// Hard ceiling on rows returned by a single comment list call.
pub const MAX_COMMENT_PAGE: i64 = 200;

/// How many enrichment candidates a single selection query scans.
const ENRICHMENT_SCAN_LIMIT: i64 = 500;

/// Who wrote a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskCommentAuthor {
    /// An agent process — the autonomy channel or a branch.
    Agent,
    /// A human, via the API or interface.
    User,
    /// A worker reporting on its own run.
    Worker,
}

impl TaskCommentAuthor {
    pub const ALL: [TaskCommentAuthor; 3] = [
        TaskCommentAuthor::Agent,
        TaskCommentAuthor::User,
        TaskCommentAuthor::Worker,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskCommentAuthor::Agent => "agent",
            TaskCommentAuthor::User => "user",
            TaskCommentAuthor::Worker => "worker",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(TaskCommentAuthor::Agent),
            "user" => Some(TaskCommentAuthor::User),
            "worker" => Some(TaskCommentAuthor::Worker),
            _ => None,
        }
    }
}

impl std::fmt::Display for TaskCommentAuthor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskComment {
    /// Monotonic sequence number. Stable pagination cursor.
    pub seq: i64,
    pub id: String,
    pub task_id: String,
    pub author_type: TaskCommentAuthor,
    pub author_id: Option<String>,
    pub body: String,
    /// Worker run this comment summarises, when it summarises one.
    pub worker_id: Option<String>,
    pub metadata: Value,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct CreateTaskCommentInput {
    pub task_number: i64,
    pub author_type: TaskCommentAuthor,
    pub author_id: Option<String>,
    pub body: String,
    pub worker_id: Option<String>,
    pub metadata: Value,
    /// Stamp `tasks.last_enriched_at` in the same transaction. Set for agent
    /// and worker enrichment; user comments never move the cadence clock,
    /// because a user weighing in is what pulls a task back into selection.
    pub mark_enriched: bool,
}

/// Why a task surfaced in the enrichment selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentReason {
    /// A user commented after the last enrichment pass.
    UserEngaged,
    /// Never enriched.
    NeverEnriched,
    /// Enriched, but not recently.
    Stale,
}

impl EnrichmentReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EnrichmentReason::UserEngaged => "user_engaged",
            EnrichmentReason::NeverEnriched => "never_enriched",
            EnrichmentReason::Stale => "stale",
        }
    }
}

impl std::fmt::Display for EnrichmentReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct EnrichmentCandidate {
    pub task: Task,
    pub reason: EnrichmentReason,
}

/// Inputs to the enrichment selection query.
#[derive(Debug, Clone)]
pub struct EnrichmentSelection<'a> {
    pub agent_id: &'a str,
    /// Whether unassigned tasks are visible to this agent.
    pub claim_unowned: bool,
    pub statuses: &'a [TaskStatus],
    /// Start of the most recent autonomy run. Tasks enriched at or after this
    /// point are held back unless a user has commented since.
    pub last_run_started_at: Option<String>,
    /// Tasks enriched before this timestamp count as stale.
    pub stale_before: String,
    pub limit: i64,
}

/// Outcome of an atomic claim attempt.
#[derive(Debug, Clone)]
pub enum TaskClaim {
    /// This call moved the task from unassigned to the requesting agent.
    Claimed(Box<Task>),
    /// The task was already assigned to the requesting agent.
    AlreadyOwned(Box<Task>),
    /// Another agent holds the task. The caller skips it.
    OwnedByOther {
        agent_id: String,
    },
    NotFound,
}

/// Format a UTC instant the way the SQLite stores write timestamps, so
/// Rust-computed bounds compare lexicographically against stored values.
pub fn sqlite_timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Validate and normalise a comment body.
pub fn normalize_comment_body(body: &str) -> std::result::Result<String, String> {
    let trimmed = body.trim();
    if trimmed.chars().count() < MIN_COMMENT_BODY_CHARS {
        return Err(format!(
            "comment body must be at least {MIN_COMMENT_BODY_CHARS} characters"
        ));
    }
    if trimmed.len() > MAX_COMMENT_BODY_BYTES {
        return Err(format!(
            "comment body is {} bytes; the limit is {MAX_COMMENT_BODY_BYTES}. Summarise the finding and link the worker instead",
            trimmed.len()
        ));
    }
    Ok(trimmed.to_string())
}

impl TaskStore {
    /// Append a comment to a task, optionally stamping the enrichment clock.
    ///
    /// Fails when the task does not exist. The insert and the
    /// `last_enriched_at` update share one transaction so a crash can never
    /// leave a task marked enriched without the comment that enriched it.
    pub async fn add_comment(&self, input: CreateTaskCommentInput) -> Result<TaskComment> {
        let body = normalize_comment_body(&input.body).map_err(|message| {
            crate::error::Error::Other(anyhow::anyhow!("invalid comment: {message}"))
        })?;
        let metadata = match input.metadata {
            Value::Object(_) => input.metadata,
            _ => Value::Object(serde_json::Map::new()),
        };

        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open task comment transaction")?;

        let task_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM tasks WHERE task_number = ?")
                .bind(input.task_number)
                .fetch_optional(&mut *tx)
                .await
                .context("failed to resolve task for comment")?;
        let Some(task_id) = task_id else {
            return Err(anyhow::anyhow!("task #{} not found", input.task_number).into());
        };

        let comment_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_comments (id, task_id, author_type, author_id, body, worker_id, metadata) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&comment_id)
        .bind(&task_id)
        .bind(input.author_type.as_str())
        .bind(&input.author_id)
        .bind(&body)
        .bind(&input.worker_id)
        .bind(metadata.to_string())
        .execute(&mut *tx)
        .await
        .context("failed to insert task comment")?;

        if input.mark_enriched {
            sqlx::query(
                "UPDATE tasks SET last_enriched_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE id = ?",
            )
            .bind(&task_id)
            .execute(&mut *tx)
            .await
            .context("failed to stamp task enrichment time")?;
        }

        let row = sqlx::query(&format!(
            "{COMMENT_SELECT_COLUMNS} FROM task_comments WHERE id = ?"
        ))
        .bind(&comment_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to read inserted task comment")?;

        tx.commit()
            .await
            .context("failed to commit task comment transaction")?;

        comment_from_row(row)
    }

    /// List a task's comments oldest-first. `after_seq` resumes after a
    /// previously returned cursor.
    pub async fn list_comments(
        &self,
        task_number: i64,
        limit: i64,
        after_seq: Option<i64>,
    ) -> Result<Vec<TaskComment>> {
        let mut query = format!(
            "{COMMENT_SELECT_COLUMNS} FROM task_comments \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?)"
        );
        if after_seq.is_some() {
            query.push_str(" AND seq > ?");
        }
        query.push_str(" ORDER BY seq ASC LIMIT ?");

        let mut sql = sqlx::query(&query).bind(task_number);
        if let Some(cursor) = after_seq {
            sql = sql.bind(cursor);
        }
        let rows = sql
            .bind(limit.clamp(1, MAX_COMMENT_PAGE))
            .fetch_all(self.pool())
            .await
            .context("failed to list task comments")?;

        rows.into_iter().map(comment_from_row).collect()
    }

    /// The most recent `limit` comments on a task, returned oldest-first.
    ///
    /// Used by briefing paths that want the tail of the conversation on a
    /// task rather than the head.
    pub async fn recent_comments(&self, task_number: i64, limit: i64) -> Result<Vec<TaskComment>> {
        let rows = sqlx::query(&format!(
            "SELECT * FROM ({COMMENT_SELECT_COLUMNS} FROM task_comments \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             ORDER BY seq DESC LIMIT ?) ORDER BY seq ASC"
        ))
        .bind(task_number)
        .bind(limit.clamp(1, MAX_COMMENT_PAGE))
        .fetch_all(self.pool())
        .await
        .context("failed to list recent task comments")?;

        rows.into_iter().map(comment_from_row).collect()
    }

    /// Comment counts for every commented task, keyed by task id. One query,
    /// so a survey of many tasks does not fan out into one call per task.
    pub async fn comment_counts(&self) -> Result<std::collections::HashMap<String, i64>> {
        let rows =
            sqlx::query("SELECT task_id, COUNT(*) AS count FROM task_comments GROUP BY task_id")
                .fetch_all(self.pool())
                .await
                .context("failed to count comments per task")?;

        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("task_id")
                        .context("failed to read comment count task_id")?,
                    row.try_get("count")
                        .context("failed to read comment count")?,
                ))
            })
            .collect()
    }

    pub async fn count_comments(&self, task_number: i64) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM task_comments \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?)",
        )
        .bind(task_number)
        .fetch_one(self.pool())
        .await
        .context("failed to count task comments")
        .map_err(Into::into)
    }

    /// Atomically claim an unassigned task for `agent_id`.
    ///
    /// The guarded UPDATE is the whole race: exactly one caller sees a row
    /// affected, every other caller reads back the winner and skips.
    pub async fn claim_for_agent(&self, task_number: i64, agent_id: &str) -> Result<TaskClaim> {
        let mut tx = self
            .pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open task claim transaction")?;

        let claimed = sqlx::query(
            "UPDATE tasks SET assigned_agent_id = ?, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ? AND assigned_agent_id IS NULL",
        )
        .bind(agent_id)
        .bind(task_number)
        .execute(&mut *tx)
        .await
        .context("failed to claim task")?
        .rows_affected()
            > 0;

        let row = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE task_number = ?"
        ))
        .bind(task_number)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to read claimed task")?;

        tx.commit()
            .await
            .context("failed to commit task claim transaction")?;

        let Some(row) = row else {
            return Ok(TaskClaim::NotFound);
        };
        let task = task_from_row(row)?;

        if claimed {
            return Ok(TaskClaim::Claimed(Box::new(task)));
        }
        match task.assigned_agent_id.as_deref() {
            Some(owner) if owner == agent_id => Ok(TaskClaim::AlreadyOwned(Box::new(task))),
            Some(owner) => Ok(TaskClaim::OwnedByOther {
                agent_id: owner.to_string(),
            }),
            // The guarded UPDATE only misses an unassigned row when another
            // writer assigned and unassigned it between the two statements,
            // which the IMMEDIATE transaction rules out.
            None => Ok(TaskClaim::AlreadyOwned(Box::new(task))),
        }
    }

    /// Ordered enrichment candidates for an autonomy run.
    ///
    /// Priority, highest first: a user commented since the last enrichment,
    /// then never enriched, then stale. Tasks worked during the previous run
    /// are excluded unless a user has commented since — that is what stops a
    /// run from repeating the one before it.
    ///
    /// The user-comment comparison is inclusive of the enrichment timestamp.
    /// Both are millisecond-precision, so a comment landing in the same
    /// millisecond as the enrichment that preceded it would otherwise be
    /// swallowed. Erring toward one extra look at the task is the safe
    /// direction; erring toward dropping a user's input is not.
    pub async fn select_for_enrichment(
        &self,
        selection: EnrichmentSelection<'_>,
    ) -> Result<Vec<EnrichmentCandidate>> {
        if selection.statuses.is_empty() {
            return Ok(Vec::new());
        }

        let status_placeholders = vec!["?"; selection.statuses.len()].join(", ");
        let query = format!(
            "SELECT * FROM ( \
                 {SELECT_COLUMNS}, \
                 EXISTS ( \
                     SELECT 1 FROM task_comments c \
                     WHERE c.task_id = tasks.id AND c.author_type = 'user' \
                       AND (tasks.last_enriched_at IS NULL \
                            OR c.created_at >= tasks.last_enriched_at) \
                 ) AS user_engaged \
                 FROM tasks \
                 WHERE status IN ({status_placeholders}) \
                   AND (assigned_agent_id = ? OR (assigned_agent_id IS NULL AND ?)) \
                 LIMIT {ENRICHMENT_SCAN_LIMIT} \
             ) \
             WHERE user_engaged = 1 \
                OR last_enriched_at IS NULL \
                OR (last_enriched_at < ? AND (? IS NULL OR last_enriched_at <= ?)) \
             ORDER BY \
                 CASE WHEN user_engaged = 1 THEN 0 \
                      WHEN last_enriched_at IS NULL THEN 1 \
                      ELSE 2 END ASC, \
                 CASE priority \
                     WHEN 'critical' THEN 0 \
                     WHEN 'high' THEN 1 \
                     WHEN 'medium' THEN 2 \
                     WHEN 'low' THEN 3 \
                     ELSE 4 END ASC, \
                 COALESCE(last_enriched_at, '') ASC, \
                 task_number ASC \
             LIMIT ?"
        );

        let mut sql = sqlx::query(&query);
        for status in selection.statuses {
            sql = sql.bind(status.as_str());
        }
        let rows = sql
            .bind(selection.agent_id)
            .bind(selection.claim_unowned)
            .bind(&selection.stale_before)
            .bind(&selection.last_run_started_at)
            .bind(&selection.last_run_started_at)
            .bind(selection.limit.clamp(1, ENRICHMENT_SCAN_LIMIT))
            .fetch_all(self.pool())
            .await
            .context("failed to select enrichment candidates")?;

        rows.into_iter()
            .map(|row| {
                let user_engaged: bool = row
                    .try_get("user_engaged")
                    .context("failed to read enrichment engagement flag")?;
                let task = task_from_row(row)?;
                let reason = if user_engaged {
                    EnrichmentReason::UserEngaged
                } else if task.last_enriched_at.is_none() {
                    EnrichmentReason::NeverEnriched
                } else {
                    EnrichmentReason::Stale
                };
                Ok(EnrichmentCandidate { task, reason })
            })
            .collect()
    }
}

/// Column list used by all comment SELECT queries. Kept in sync with
/// [`comment_from_row`].
const COMMENT_SELECT_COLUMNS: &str =
    "SELECT seq, id, task_id, author_type, author_id, body, worker_id, metadata, created_at";

fn comment_from_row(row: SqliteRow) -> Result<TaskComment> {
    let author_value: String = row
        .try_get("author_type")
        .context("failed to read comment author_type")?;
    let author_type = TaskCommentAuthor::parse(&author_value)
        .with_context(|| format!("invalid comment author type in database: {author_value}"))?;
    let metadata_value: String = row.try_get("metadata").unwrap_or_else(|_| "{}".to_string());

    Ok(TaskComment {
        seq: row.try_get("seq").context("failed to read comment seq")?,
        id: row.try_get("id").context("failed to read comment id")?,
        task_id: row
            .try_get("task_id")
            .context("failed to read comment task_id")?,
        author_type,
        author_id: row
            .try_get::<Option<String>, _>("author_id")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        body: row.try_get("body").context("failed to read comment body")?,
        worker_id: row
            .try_get::<Option<String>, _>("worker_id")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        metadata: serde_json::from_str(&metadata_value)
            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
        created_at: row
            .try_get("created_at")
            .context("failed to read comment created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::store::{
        CreateTaskInput, TaskListFilter, TaskPriority, UpdateTaskInput, setup_test_store,
    };

    fn agent_comment(task_number: i64, body: &str) -> CreateTaskCommentInput {
        CreateTaskCommentInput {
            task_number,
            author_type: TaskCommentAuthor::Agent,
            author_id: Some("agent-test".to_string()),
            body: body.to_string(),
            worker_id: None,
            metadata: serde_json::json!({}),
            mark_enriched: true,
        }
    }

    fn user_comment(task_number: i64, body: &str) -> CreateTaskCommentInput {
        CreateTaskCommentInput {
            author_type: TaskCommentAuthor::User,
            author_id: Some("jamie".to_string()),
            mark_enriched: false,
            ..agent_comment(task_number, body)
        }
    }

    fn task_input(title: &str, assigned: Option<&str>, status: TaskStatus) -> CreateTaskInput {
        CreateTaskInput {
            owner_agent_id: "agent-test".to_string(),
            assigned_agent_id: assigned.map(str::to_string),
            title: title.to_string(),
            description: None,
            status,
            priority: TaskPriority::Medium,
            subtasks: Vec::new(),
            metadata: serde_json::json!({}),
            goal_id: None,
            source_memory_id: None,
            created_by: "autonomy".to_string(),
        }
    }

    #[tokio::test]
    async fn comments_are_chronological_and_paginate_by_seq() {
        let store = setup_test_store().await;
        let task = store
            .create(task_input(
                "commented",
                Some("agent-test"),
                TaskStatus::Ready,
            ))
            .await
            .expect("task should be created");

        for index in 0..5 {
            store
                .add_comment(agent_comment(
                    task.task_number,
                    &format!("finding number {index}"),
                ))
                .await
                .expect("comment should insert");
        }

        let first_page = store
            .list_comments(task.task_number, 2, None)
            .await
            .expect("list should succeed");
        assert_eq!(first_page.len(), 2);
        assert_eq!(first_page[0].body, "finding number 0");
        assert_eq!(first_page[1].body, "finding number 1");

        let second_page = store
            .list_comments(task.task_number, 2, Some(first_page[1].seq))
            .await
            .expect("list should succeed");
        assert_eq!(second_page.len(), 2);
        assert_eq!(second_page[0].body, "finding number 2");
        assert!(second_page[0].seq > first_page[1].seq);

        let tail = store
            .recent_comments(task.task_number, 2)
            .await
            .expect("recent should succeed");
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].body, "finding number 3");
        assert_eq!(tail[1].body, "finding number 4");

        assert_eq!(
            store
                .count_comments(task.task_number)
                .await
                .expect("count should succeed"),
            5
        );
    }

    #[tokio::test]
    async fn agent_comment_stamps_enrichment_and_user_comment_does_not() {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("cadence", Some("agent-test"), TaskStatus::Ready))
            .await
            .expect("task should be created");
        assert!(task.last_enriched_at.is_none());

        store
            .add_comment(user_comment(task.task_number, "please look at the retries"))
            .await
            .expect("user comment should insert");
        let after_user = store
            .get_by_number(task.task_number)
            .await
            .expect("load")
            .expect("task exists");
        assert!(
            after_user.last_enriched_at.is_none(),
            "user comments must not move the enrichment clock"
        );

        store
            .add_comment(agent_comment(task.task_number, "retries are exponential"))
            .await
            .expect("agent comment should insert");
        let after_agent = store
            .get_by_number(task.task_number)
            .await
            .expect("load")
            .expect("task exists");
        assert!(after_agent.last_enriched_at.is_some());
    }

    #[tokio::test]
    async fn rejects_missing_task_and_out_of_bounds_bodies() {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("bounds", Some("agent-test"), TaskStatus::Ready))
            .await
            .expect("task should be created");

        let missing = store
            .add_comment(agent_comment(9999, "a finding on a ghost"))
            .await
            .expect_err("missing task must fail");
        assert!(missing.to_string().contains("not found"));

        let too_short = store
            .add_comment(agent_comment(task.task_number, "  x "))
            .await
            .expect_err("short body must fail");
        assert!(too_short.to_string().contains("at least"));

        let too_long = store
            .add_comment(agent_comment(
                task.task_number,
                &"x".repeat(MAX_COMMENT_BODY_BYTES + 1),
            ))
            .await
            .expect_err("oversized body must fail");
        assert!(too_long.to_string().contains("limit is"));
    }

    #[tokio::test]
    async fn deleting_a_task_removes_its_comments() {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("doomed", Some("agent-test"), TaskStatus::Ready))
            .await
            .expect("task should be created");
        store
            .add_comment(agent_comment(task.task_number, "about to be orphaned"))
            .await
            .expect("comment should insert");

        assert!(
            store
                .delete(task.task_number)
                .await
                .expect("delete should succeed")
        );

        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_comments")
            .fetch_one(store.pool())
            .await
            .expect("count should succeed");
        assert_eq!(orphans, 0, "task deletion must cascade to comments");
    }

    #[tokio::test]
    async fn concurrent_claims_produce_one_winner() {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("unowned", None, TaskStatus::PendingApproval))
            .await
            .expect("task should be created");

        let first = store.clone();
        let second = store.clone();
        let number = task.task_number;
        let (left, right) = tokio::join!(
            tokio::spawn(async move { first.claim_for_agent(number, "agent-a").await }),
            tokio::spawn(async move { second.claim_for_agent(number, "agent-b").await }),
        );

        let outcomes = [
            left.expect("join").expect("claim should succeed"),
            right.expect("join").expect("claim should succeed"),
        ];
        let winners = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, TaskClaim::Claimed(_)))
            .count();
        let losers = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, TaskClaim::OwnedByOther { .. }))
            .count();
        assert_eq!(winners, 1, "exactly one claimant may win");
        assert_eq!(losers, 1, "the loser must get a clean skip");

        let claimed = store
            .get_by_number(number)
            .await
            .expect("load")
            .expect("task exists");
        assert!(claimed.assigned_agent_id.is_some());
    }

    #[tokio::test]
    async fn claim_is_idempotent_and_respects_existing_assignment() {
        let store = setup_test_store().await;
        let mine = store
            .create(task_input("mine", Some("agent-a"), TaskStatus::Ready))
            .await
            .expect("task should be created");
        let theirs = store
            .create(task_input("theirs", Some("agent-b"), TaskStatus::Ready))
            .await
            .expect("task should be created");

        assert!(matches!(
            store
                .claim_for_agent(mine.task_number, "agent-a")
                .await
                .expect("claim should succeed"),
            TaskClaim::AlreadyOwned(_)
        ));
        assert!(matches!(
            store
                .claim_for_agent(theirs.task_number, "agent-a")
                .await
                .expect("claim should succeed"),
            TaskClaim::OwnedByOther { .. }
        ));
        assert!(matches!(
            store
                .claim_for_agent(9999, "agent-a")
                .await
                .expect("claim should succeed"),
            TaskClaim::NotFound
        ));
    }

    fn selection<'a>(
        statuses: &'a [TaskStatus],
        last_run_started_at: Option<String>,
        stale_before: String,
    ) -> EnrichmentSelection<'a> {
        EnrichmentSelection {
            agent_id: "agent-test",
            claim_unowned: true,
            statuses,
            last_run_started_at,
            stale_before,
            limit: 10,
        }
    }

    #[tokio::test]
    async fn selection_orders_user_engaged_then_never_enriched_then_stale() {
        let store = setup_test_store().await;
        let statuses = [TaskStatus::PendingApproval];

        let engaged = store
            .create(task_input(
                "engaged",
                Some("agent-test"),
                TaskStatus::PendingApproval,
            ))
            .await
            .expect("create");
        let fresh = store
            .create(task_input(
                "never enriched",
                Some("agent-test"),
                TaskStatus::PendingApproval,
            ))
            .await
            .expect("create");
        let stale = store
            .create(task_input(
                "stale",
                Some("agent-test"),
                TaskStatus::PendingApproval,
            ))
            .await
            .expect("create");

        // Enrich two tasks in the distant past, then have a user comment on
        // one of them afterwards.
        for number in [engaged.task_number, stale.task_number] {
            sqlx::query("UPDATE tasks SET last_enriched_at = ? WHERE task_number = ?")
                .bind("2020-01-01T00:00:00.000Z")
                .bind(number)
                .execute(store.pool())
                .await
                .expect("backdate enrichment");
        }
        store
            .add_comment(user_comment(engaged.task_number, "one more thing"))
            .await
            .expect("user comment");

        let candidates = store
            .select_for_enrichment(selection(
                &statuses,
                Some("2026-01-01T00:00:00.000Z".to_string()),
                "2025-01-01T00:00:00.000Z".to_string(),
            ))
            .await
            .expect("selection should succeed");

        let ordered: Vec<(i64, EnrichmentReason)> = candidates
            .iter()
            .map(|candidate| (candidate.task.task_number, candidate.reason))
            .collect();
        assert_eq!(
            ordered,
            vec![
                (engaged.task_number, EnrichmentReason::UserEngaged),
                (fresh.task_number, EnrichmentReason::NeverEnriched),
                (stale.task_number, EnrichmentReason::Stale),
            ]
        );
    }

    #[tokio::test]
    async fn selection_excludes_work_from_the_previous_run() {
        let store = setup_test_store().await;
        let statuses = [TaskStatus::PendingApproval];
        let task = store
            .create(task_input(
                "just worked",
                Some("agent-test"),
                TaskStatus::PendingApproval,
            ))
            .await
            .expect("create");

        store
            .add_comment(agent_comment(task.task_number, "enriched during this run"))
            .await
            .expect("agent comment");

        let last_run = Some("2020-01-01T00:00:00.000Z".to_string());
        let stale_before = "2020-01-01T00:00:00.000Z".to_string();
        let candidates = store
            .select_for_enrichment(selection(&statuses, last_run.clone(), stale_before.clone()))
            .await
            .expect("selection should succeed");
        assert!(
            candidates.is_empty(),
            "a task enriched after the last run start must not resurface"
        );

        // A user weighing in clears the exclusion.
        store
            .add_comment(user_comment(task.task_number, "actually, reconsider this"))
            .await
            .expect("user comment");
        let candidates = store
            .select_for_enrichment(selection(&statuses, last_run, stale_before))
            .await
            .expect("selection should succeed");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].reason, EnrichmentReason::UserEngaged);
    }

    #[tokio::test]
    async fn selection_respects_assignment_and_claim_unowned() {
        let store = setup_test_store().await;
        let statuses = [TaskStatus::PendingApproval];
        store
            .create(task_input(
                "someone else's",
                Some("agent-other"),
                TaskStatus::PendingApproval,
            ))
            .await
            .expect("create");
        let unowned = store
            .create(task_input("unowned", None, TaskStatus::PendingApproval))
            .await
            .expect("create");

        let stale_before = "2020-01-01T00:00:00.000Z".to_string();
        let with_claim = store
            .select_for_enrichment(selection(&statuses, None, stale_before.clone()))
            .await
            .expect("selection should succeed");
        assert_eq!(with_claim.len(), 1);
        assert_eq!(with_claim[0].task.task_number, unowned.task_number);

        let without_claim = store
            .select_for_enrichment(EnrichmentSelection {
                claim_unowned: false,
                ..selection(&statuses, None, stale_before)
            })
            .await
            .expect("selection should succeed");
        assert!(
            without_claim.is_empty(),
            "unowned tasks are invisible when claim_unowned is off"
        );
    }

    #[tokio::test]
    async fn claim_next_ready_can_take_unowned_work() {
        let store = setup_test_store().await;
        store
            .create(task_input("unowned ready", None, TaskStatus::Ready))
            .await
            .expect("create");

        assert!(
            store
                .claim_next_ready("agent-test", false)
                .await
                .expect("claim should succeed")
                .is_none(),
            "unowned work stays untouched when claim_unowned is off"
        );

        let claimed = store
            .claim_next_ready("agent-test", true)
            .await
            .expect("claim should succeed")
            .expect("unowned ready task should be claimed");
        assert_eq!(claimed.status, TaskStatus::InProgress);
        assert_eq!(claimed.assigned_agent_id.as_deref(), Some("agent-test"));

        let remaining = store
            .list(TaskListFilter {
                status: Some(TaskStatus::Ready),
                ..Default::default()
            })
            .await
            .expect("list should succeed");
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn goal_id_survives_create_and_reassignment() {
        let store = setup_test_store().await;
        let goal_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO goals (id, title) VALUES (?, 'Ship the enrichment loop')")
            .bind(&goal_id)
            .execute(store.pool())
            .await
            .expect("goal should insert");

        let task = store
            .create(CreateTaskInput {
                goal_id: Some(goal_id.clone()),
                ..task_input(
                    "goal linked",
                    Some("agent-test"),
                    TaskStatus::PendingApproval,
                )
            })
            .await
            .expect("create");
        assert_eq!(task.goal_id.as_deref(), Some(goal_id.as_str()));

        let updated = store
            .update(
                task.task_number,
                UpdateTaskInput {
                    assigned_agent_id: Some("agent-other".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task exists");
        assert_eq!(updated.goal_id.as_deref(), Some(goal_id.as_str()));
    }

    #[tokio::test]
    async fn create_rejects_an_unknown_goal_link() {
        let store = setup_test_store().await;
        let error = store
            .create(CreateTaskInput {
                goal_id: Some("no-such-goal".to_string()),
                ..task_input("dangling", Some("agent-test"), TaskStatus::PendingApproval)
            })
            .await
            .expect_err("a task cannot link to a goal that does not exist");
        assert!(error.to_string().contains("FOREIGN KEY"));
    }
}
