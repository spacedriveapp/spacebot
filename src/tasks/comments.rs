//! Append-only task discussion.
//!
//! A task's description is its specification; the thread is how it got there.
//! Comments are never edited or deleted — a record that can be rewritten is not
//! a record — so refinement that happened over weeks stays readable long after
//! the description settled. Material edits to the specification itself are
//! tracked separately, as revisions.

use crate::error::{Result, TaskError};
use crate::tasks::revisions::TaskAuthorKind;

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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskComment {
    /// Monotonic sequence number. Stable pagination cursor.
    pub seq: i64,
    pub id: String,
    pub task_id: String,
    pub author_type: TaskAuthorKind,
    pub author_id: Option<String>,
    pub body: String,
    /// Worker run this comment reports on, when it reports on one.
    pub worker_id: Option<String>,
    pub metadata: Value,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct CreateTaskCommentInput {
    pub task_number: i64,
    pub author_type: TaskAuthorKind,
    pub author_id: Option<String>,
    pub body: String,
    pub worker_id: Option<String>,
    pub metadata: Value,
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

impl crate::tasks::TaskStore {
    /// Append a comment to a task. Fails when the task does not exist.
    pub async fn add_comment(&self, input: CreateTaskCommentInput) -> Result<TaskComment> {
        let body = normalize_comment_body(&input.body)
            .map_err(|message| TaskError::Invalid(format!("invalid comment: {message}")))?;
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
            return Err(TaskError::NotFound {
                task_number: input.task_number,
            }
            .into());
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
    /// Used by briefing paths that want the tail of the conversation on a task
    /// rather than the head.
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

    /// Complete discussion for an internal task execution briefing.
    pub(crate) async fn all_comments(&self, task_number: i64) -> Result<Vec<TaskComment>> {
        let rows = sqlx::query(&format!(
            "{COMMENT_SELECT_COLUMNS} FROM task_comments \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             ORDER BY seq ASC"
        ))
        .bind(task_number)
        .fetch_all(self.pool())
        .await
        .context("failed to load complete task discussion")?;

        rows.into_iter().map(comment_from_row).collect()
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
    let author_type = TaskAuthorKind::parse(&author_value)
        .with_context(|| format!("invalid comment author kind in database: {author_value}"))?;
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
