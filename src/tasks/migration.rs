//! One-time migration of per-agent task data to the global task database.
//!
//! Scans each agent's per-agent `spacebot.db` for rows in the old `tasks`
//! table and inserts them into the global `tasks.db` with new globally unique
//! task numbers. Original per-agent task numbers are preserved in metadata
//! as `legacy_task_number`. Migration is idempotent — a marker file prevents
//! re-running on subsequent startups.

use anyhow::Context as _;
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};

use std::path::Path;

/// Marker file written to `{instance_dir}/data/` after successful migration.
const MIGRATION_MARKER: &str = ".tasks_migrated";

/// Migrate all per-agent tasks to the global task database.
///
/// This scans the `agents/` directory under `instance_dir` for agent data
/// directories containing `spacebot.db`. For each agent, it reads all rows
/// from the legacy `tasks` table and inserts them into the global store with
/// new globally unique task numbers.
///
/// The migration is idempotent: if the marker file exists, it returns
/// immediately. On success, the marker file is written.
pub async fn migrate_legacy_tasks(
    instance_dir: &Path,
    global_pool: &SqlitePool,
) -> anyhow::Result<()> {
    let data_dir = instance_dir.join("data");
    let marker_path = data_dir.join(MIGRATION_MARKER);

    if marker_path.exists() {
        tracing::debug!("global task migration already completed, skipping");
        return Ok(());
    }

    let agents_dir = instance_dir.join("agents");
    if !agents_dir.exists() {
        tracing::debug!("no agents directory found, skipping task migration");
        write_marker(&marker_path)?;
        return Ok(());
    }

    let mut total_migrated = 0u64;

    let entries = std::fs::read_dir(&agents_dir)
        .with_context(|| format!("failed to read agents directory: {}", agents_dir.display()))?;

    for entry in entries {
        let entry = entry.context("failed to read agents directory entry")?;
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }

        let agent_id = agent_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Try new per-agent name first, fall back to legacy.
        let new_db = agent_dir.join("data").join("agent.db");
        let legacy_db = agent_dir.join("data").join("spacebot.db");
        let db_path = if new_db.exists() {
            new_db
        } else if legacy_db.exists() {
            legacy_db
        } else {
            continue;
        };

        match migrate_agent_tasks(&agent_id, &db_path, global_pool).await {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(agent_id, count, "migrated legacy tasks to global database");
                }
                total_migrated += count;
            }
            Err(error) => {
                tracing::error!(
                    agent_id,
                    %error,
                    "failed to migrate tasks for agent — migration incomplete"
                );
                return Err(error);
            }
        }
    }

    write_marker(&marker_path)?;

    if total_migrated > 0 {
        tracing::info!(total_migrated, "legacy task migration complete");
    }

    Ok(())
}

/// Migrate tasks from a single agent's per-agent database to the global store.
async fn migrate_agent_tasks(
    agent_id: &str,
    db_path: &Path,
    global_pool: &SqlitePool,
) -> anyhow::Result<u64> {
    let url = format!("sqlite:{}?mode=ro", db_path.display());
    let agent_pool = SqlitePool::connect(&url)
        .await
        .with_context(|| format!("failed to connect to agent database: {}", db_path.display()))?;

    // Check whether the legacy tasks table exists (agent may predate the task feature).
    let table_exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'tasks'",
    )
    .fetch_one(&agent_pool)
    .await
    .unwrap_or(false);

    if !table_exists {
        agent_pool.close().await;
        return Ok(0);
    }

    let rows = sqlx::query(
        "SELECT id, agent_id, task_number, title, description, status, priority, \
         subtasks, metadata, source_memory_id, worker_id, created_by, \
         approved_at, approved_by, created_at, updated_at, completed_at \
         FROM tasks ORDER BY created_at ASC",
    )
    .fetch_all(&agent_pool)
    .await
    .context("failed to read legacy tasks")?;

    agent_pool.close().await;

    if rows.is_empty() {
        return Ok(0);
    }

    let mut migrated = 0u64;

    for row in &rows {
        let legacy_id: String = row.try_get("id").context("missing task id")?;

        // Skip if this task was already migrated (idempotent by legacy id).
        let already_exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM tasks WHERE id = ?")
                .bind(&legacy_id)
                .fetch_one(global_pool)
                .await
                .unwrap_or(false);

        if already_exists {
            continue;
        }

        let legacy_task_number: i64 = row.try_get("task_number").unwrap_or(0);
        let legacy_agent_id: String = row
            .try_get("agent_id")
            .unwrap_or_else(|_| agent_id.to_string());
        let title: String = row.try_get("title").context("missing task title")?;
        let description: Option<String> = row.try_get("description").ok();
        let status: String = row
            .try_get("status")
            .unwrap_or_else(|_| "backlog".to_string());
        let priority: String = row
            .try_get("priority")
            .unwrap_or_else(|_| "medium".to_string());
        let subtasks: String = row.try_get("subtasks").unwrap_or_else(|_| "[]".to_string());
        let metadata_raw: String = row.try_get("metadata").unwrap_or_else(|_| "{}".to_string());
        let source_memory_id: Option<String> = row.try_get("source_memory_id").ok();
        let worker_id: Option<String> =
            row.try_get::<Option<String>, _>("worker_id").ok().flatten();
        let created_by: String = row
            .try_get("created_by")
            .unwrap_or_else(|_| "unknown".to_string());
        let approved_at: Option<String> = read_optional_timestamp(row, "approved_at");
        let approved_by: Option<String> = row.try_get("approved_by").ok();
        let created_at: String = read_timestamp(row, "created_at");
        let updated_at: String = read_timestamp(row, "updated_at");
        let completed_at: Option<String> = read_optional_timestamp(row, "completed_at");

        // Determine owner/assigned from delegation metadata.
        let metadata: Value =
            serde_json::from_str(&metadata_raw).unwrap_or_else(|_| serde_json::json!({}));
        let delegating_agent = metadata
            .get("delegating_agent_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let owner_agent_id = delegating_agent.unwrap_or_else(|| legacy_agent_id.clone());
        let assigned_agent_id = legacy_agent_id.clone();

        // Merge legacy provenance into metadata.
        let enriched_metadata = enrich_metadata(metadata, &legacy_agent_id, legacy_task_number);

        // Allocate a new global task number inside a transaction.
        let mut tx = global_pool
            .begin()
            .await
            .context("failed to begin migration transaction")?;

        let global_number: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(task_number), 0) + 1 FROM tasks")
                .fetch_one(&mut *tx)
                .await
                .context("failed to allocate global task number")?;

        sqlx::query(
            "INSERT INTO tasks (
                id, task_number, title, description, status, priority,
                owner_agent_id, assigned_agent_id,
                subtasks, metadata, source_memory_id, worker_id,
                created_by, approved_at, approved_by,
                created_at, updated_at, completed_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&legacy_id)
        .bind(global_number)
        .bind(&title)
        .bind(&description)
        .bind(&status)
        .bind(&priority)
        .bind(&owner_agent_id)
        .bind(&assigned_agent_id)
        .bind(&subtasks)
        .bind(enriched_metadata.to_string())
        .bind(&source_memory_id)
        .bind(&worker_id)
        .bind(&created_by)
        .bind(&approved_at)
        .bind(&approved_by)
        .bind(&created_at)
        .bind(&updated_at)
        .bind(&completed_at)
        .execute(&mut *tx)
        .await
        .with_context(|| {
            format!(
                "failed to insert migrated task (agent={agent_id}, legacy_number={legacy_task_number})"
            )
        })?;

        tx.commit()
            .await
            .context("failed to commit migration transaction")?;

        migrated += 1;
    }

    Ok(migrated)
}

/// Inject legacy provenance fields into task metadata.
fn enrich_metadata(mut metadata: Value, legacy_agent_id: &str, legacy_task_number: i64) -> Value {
    if let Value::Object(ref mut map) = metadata {
        map.insert(
            "legacy_agent_id".to_string(),
            Value::String(legacy_agent_id.to_string()),
        );
        map.insert(
            "legacy_task_number".to_string(),
            Value::Number(serde_json::Number::from(legacy_task_number)),
        );
    }
    metadata
}

/// Normalize a timestamp string to RFC 3339 format. Handles both
/// `YYYY-MM-DDTHH:MM:SSZ` (already correct) and SQLite's default
/// `YYYY-MM-DD HH:MM:SS` form.
fn normalize_timestamp(value: &str) -> String {
    // Already RFC 3339 — return as-is.
    if value.contains('T') {
        return value.to_string();
    }
    // Try parsing SQLite's `YYYY-MM-DD HH:MM:SS` form.
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return naive.and_utc().to_rfc3339();
    }
    // Unrecognized format — return as-is rather than silently dropping.
    value.to_string()
}

fn read_timestamp(row: &sqlx::sqlite::SqliteRow, column: &str) -> String {
    if let Ok(value) = row.try_get::<String, _>(column) {
        return normalize_timestamp(&value);
    }
    row.try_get::<chrono::NaiveDateTime, _>(column)
        .map(|v| v.and_utc().to_rfc3339())
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339())
}

fn read_optional_timestamp(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<String> {
    if let Ok(Some(value)) = row.try_get::<Option<String>, _>(column)
        && !value.is_empty()
    {
        return Some(normalize_timestamp(&value));
    }
    row.try_get::<Option<chrono::NaiveDateTime>, _>(column)
        .ok()
        .flatten()
        .map(|v| v.and_utc().to_rfc3339())
}

fn write_marker(path: &Path) -> anyhow::Result<()> {
    std::fs::write(path, "migrated")
        .with_context(|| format!("failed to write migration marker: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrich_metadata_adds_legacy_fields() {
        let metadata = serde_json::json!({"source": "github"});
        let enriched = enrich_metadata(metadata, "agent-a", 42);
        assert_eq!(enriched["legacy_agent_id"], "agent-a");
        assert_eq!(enriched["legacy_task_number"], 42);
        assert_eq!(enriched["source"], "github");
    }

    #[test]
    fn enrich_metadata_preserves_delegation_context() {
        let metadata = serde_json::json!({
            "delegating_agent_id": "agent-b",
            "originating_channel": "dm-123"
        });
        let enriched = enrich_metadata(metadata, "agent-a", 7);
        assert_eq!(enriched["delegating_agent_id"], "agent-b");
        assert_eq!(enriched["legacy_agent_id"], "agent-a");
        assert_eq!(enriched["legacy_task_number"], 7);
    }
}
