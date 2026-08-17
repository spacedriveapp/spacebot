//! Pending question store for the ask tool.
//!
//! Persists questions that the agent has asked the user so inbound interaction
//! clicks can be correlated back to the original question context. Restart-safe
//! by construction — questions survive process restarts.

use crate::error::Result;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Default TTL for pending questions (7 days).
pub const DEFAULT_QUESTION_TTL_DAYS: i64 = 7;

/// A single option for the ask tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A persisted pending question row.
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub question_id: String,
    pub agent_id: String,
    pub channel_id: String,
    pub question: String,
    pub options: Vec<AskOption>,
    pub multi_select: bool,
    pub message_ref: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub answer: Option<Vec<String>>,
}

/// Input for creating a pending question.
#[derive(Debug, Clone)]
pub struct NewQuestion {
    pub question_id: String,
    pub agent_id: String,
    pub channel_id: String,
    pub question: String,
    pub options: Vec<AskOption>,
    pub multi_select: bool,
    pub message_ref: Option<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct QuestionStore {
    pool: SqlitePool,
}

/// Timestamp matching SQLite's CURRENT_TIMESTAMP format so comparisons
/// against `datetime('now', …)` are consistent.
fn now_sqlite() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

impl QuestionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a new pending question.
    pub async fn insert(&self, question: &NewQuestion) -> Result<()> {
        let options_json = serde_json::to_string(&question.options)
            .context("failed to serialize question options")?;

        sqlx::query(
            r#"
            INSERT INTO pending_questions
                (question_id, agent_id, channel_id, question, options, multi_select, message_ref)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&question.question_id)
        .bind(&question.agent_id)
        .bind(&question.channel_id)
        .bind(&question.question)
        .bind(&options_json)
        .bind(question.multi_select as i64)
        .bind(&question.message_ref)
        .execute(&self.pool)
        .await
        .context("failed to insert pending question")?;

        // Prune expired questions in the background so the table does not
        // grow unbounded. Failure is non-fatal — the next write retries.
        let prune_pool = self.pool.clone();
        tokio::spawn(async move {
            if let Err(error) = QuestionStore::new(prune_pool)
                .prune_expired(DEFAULT_QUESTION_TTL_DAYS)
                .await
            {
                tracing::warn!(%error, "background prune of pending_questions failed");
            }
        });

        Ok(())
    }

    /// Look up a pending question by ID.
    pub async fn get(&self, question_id: &str) -> Result<Option<PendingQuestion>> {
        let row = sqlx::query(
            r#"
            SELECT question_id, agent_id, channel_id, question, options, multi_select,
                   message_ref, created_at, resolved_at, answer
            FROM pending_questions
            WHERE question_id = ?
            "#,
        )
        .bind(question_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch pending question")?;

        match row {
            Some(row) => Ok(Some(question_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Resolve a pending question with the given answer labels.
    /// Returns Ok(true) if the question was found and unresolved, Ok(false) if
    /// already resolved or not found.
    pub async fn resolve(&self, question_id: &str, answer: &[String]) -> Result<bool> {
        let answer_json = serde_json::to_string(answer).context("failed to serialize answer")?;
        let now = now_sqlite();

        let affected = sqlx::query(
            r#"
            UPDATE pending_questions
            SET resolved_at = ?, answer = ?
            WHERE question_id = ? AND resolved_at IS NULL
            "#,
        )
        .bind(&now)
        .bind(&answer_json)
        .bind(question_id)
        .execute(&self.pool)
        .await
        .context("failed to resolve pending question")?
        .rows_affected();

        Ok(affected > 0)
    }

    pub async fn delete_unresolved(&self, question_id: &str) -> Result<bool> {
        let affected = sqlx::query(
            "DELETE FROM pending_questions WHERE question_id = ? AND resolved_at IS NULL",
        )
        .bind(question_id)
        .execute(&self.pool)
        .await
        .context("failed to delete undelivered question")?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Prune resolved questions older than the TTL, and unanswered questions
    /// older than the TTL (expired). Returns the count of removed rows.
    pub async fn prune_expired(&self, ttl_days: i64) -> Result<u64> {
        let cutoff = format!("-{} days", ttl_days);

        let affected = sqlx::query(
            r#"
            DELETE FROM pending_questions
            WHERE (resolved_at IS NOT NULL AND resolved_at < datetime('now', ?))
               OR (resolved_at IS NULL AND created_at < datetime('now', ?))
            "#,
        )
        .bind(&cutoff)
        .bind(&cutoff)
        .execute(&self.pool)
        .await
        .context("failed to prune expired questions")?
        .rows_affected();

        Ok(affected)
    }
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn question_from_row(row: sqlx::sqlite::SqliteRow) -> Result<PendingQuestion> {
    let options_json: String = row
        .try_get("options")
        .context("failed to read question options")?;
    let options: Vec<AskOption> =
        serde_json::from_str(&options_json).context("failed to parse question options")?;

    let answer_json: Option<String> = row
        .try_get::<Option<String>, _>("answer")
        .context("failed to read answer")?;
    let answer = match answer_json {
        Some(json) => Some(serde_json::from_str(&json).context("failed to parse question answer")?),
        None => None,
    };

    Ok(PendingQuestion {
        question_id: row
            .try_get("question_id")
            .context("failed to read question_id")?,
        agent_id: row.try_get("agent_id").context("failed to read agent_id")?,
        channel_id: row
            .try_get("channel_id")
            .context("failed to read channel_id")?,
        question: row.try_get("question").context("failed to read question")?,
        options,
        multi_select: row
            .try_get::<i64, _>("multi_select")
            .context("failed to read multi_select")?
            != 0,
        message_ref: row
            .try_get::<Option<String>, _>("message_ref")
            .context("failed to read message_ref")?,
        created_at: row
            .try_get("created_at")
            .context("failed to read created_at")?,
        resolved_at: row
            .try_get::<Option<String>, _>("resolved_at")
            .context("failed to read resolved_at")?,
        answer,
    })
}
