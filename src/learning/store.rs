//! LearningStore: CRUD operations against learning.db.

use super::LearningError;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

/// Wraps a dedicated SQLite connection pool for learning.db.
///
/// Separate from the main spacebot.db pool so high-frequency learning writes
/// don't contend with latency-sensitive memory reads.
pub struct LearningStore {
    pool: SqlitePool,
}

impl LearningStore {
    /// Connect to (or create) learning.db at the given path.
    ///
    /// Runs embedded migrations, enables WAL mode, and configures a small pool
    /// (one writer, one reader).
    pub async fn connect(path: &Path) -> Result<Arc<Self>, LearningError> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let options = SqliteConnectOptions::from_str(&url)
            .map_err(|error| LearningError::Engine(format!("invalid db path: {error}")))?
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;

        // Run embedded schema migrations.
        Self::run_migrations(&pool).await?;

        Ok(Arc::new(Self { pool }))
    }

    /// Run the embedded learning schema. Uses raw SQL rather than sqlx::migrate!
    /// because learning.db is a separate database file from the main migrations dir.
    async fn run_migrations(pool: &SqlitePool) -> Result<(), LearningError> {
        sqlx::raw_sql(SCHEMA_V1).execute(pool).await?;
        Ok(())
    }

    /// Write a key-value pair to the learning_state table (upsert).
    pub async fn set_state(&self, key: &str, value: impl Into<String>) -> Result<(), LearningError> {
        let value = value.into();
        sqlx::query(
            "INSERT INTO learning_state (key, value, updated_at) VALUES (?, ?, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(&value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read a value from the learning_state table.
    pub async fn get_state(&self, key: &str) -> Result<Option<String>, LearningError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM learning_state WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(value,)| value))
    }

    /// Log a learning event to the audit trail.
    pub async fn log_event(
        &self,
        event_type: &str,
        summary: &str,
        details: Option<&serde_json::Value>,
    ) -> Result<(), LearningError> {
        let id = uuid::Uuid::new_v4().to_string();
        let details_json = details.map(|d| d.to_string());
        sqlx::query(
            "INSERT INTO learning_events (id, event_type, summary, details, created_at) VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&id)
        .bind(event_type)
        .bind(summary)
        .bind(&details_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a metric data point.
    pub async fn record_metric(
        &self,
        metric_name: &str,
        metric_value: f64,
    ) -> Result<(), LearningError> {
        sqlx::query(
            "INSERT INTO metrics (metric_name, metric_value, recorded_at) VALUES (?, ?, datetime('now'))",
        )
        .bind(metric_name)
        .bind(metric_value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl std::fmt::Debug for LearningStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LearningStore").finish_non_exhaustive()
    }
}

/// Embedded schema for learning.db v1.
///
/// All tables use `IF NOT EXISTS` so re-running is safe. Later milestones add
/// tables via additional migration constants (SCHEMA_V2, etc.).
const SCHEMA_V1: &str = r#"
-- Episodes (Layer 1 foundation)
CREATE TABLE IF NOT EXISTS episodes (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    trace_id TEXT,
    channel_id TEXT,
    process_id TEXT NOT NULL,
    process_type TEXT NOT NULL,
    task TEXT NOT NULL,
    predicted_outcome TEXT,
    predicted_confidence REAL DEFAULT 0.0,
    actual_outcome TEXT,
    actual_confidence REAL,
    surprise_level REAL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    duration_secs REAL,
    phase TEXT,
    metadata TEXT
);
CREATE INDEX IF NOT EXISTS idx_episodes_agent ON episodes(agent_id, started_at);
CREATE INDEX IF NOT EXISTS idx_episodes_trace ON episodes(trace_id, started_at);
CREATE INDEX IF NOT EXISTS idx_episodes_outcome ON episodes(actual_outcome);

-- Steps (Layer 1 — step envelopes)
CREATE TABLE IF NOT EXISTS steps (
    id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    trace_id TEXT,
    tool_name TEXT,
    args_summary TEXT,
    intent TEXT,
    hypothesis TEXT,
    prediction TEXT,
    confidence_before REAL,
    alternatives TEXT,
    assumptions TEXT,
    result TEXT,
    evaluation TEXT,
    surprise_level REAL,
    confidence_after REAL,
    lesson TEXT,
    evidence_gathered INTEGER DEFAULT 0,
    progress_made INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_steps_episode ON steps(episode_id, created_at);
CREATE INDEX IF NOT EXISTS idx_steps_call_id ON steps(call_id);

-- Learning events log (audit trail)
CREATE TABLE IF NOT EXISTS learning_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    summary TEXT NOT NULL,
    details TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_learning_events_type ON learning_events(event_type, created_at);

-- System metrics (time-series)
CREATE TABLE IF NOT EXISTS metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    metric_name TEXT NOT NULL,
    metric_value REAL NOT NULL,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_metrics_name ON metrics(metric_name, recorded_at);

-- Learning engine state (KV for heartbeats/cursors)
CREATE TABLE IF NOT EXISTS learning_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;
