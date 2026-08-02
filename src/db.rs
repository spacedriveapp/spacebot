//! Database connection management and migrations.

use crate::error::{DbError, Result};

use anyhow::Context as _;
use sqlx::SqlitePool;

use std::path::Path;
use std::sync::Arc;

/// Database connections bundle for per-agent databases.
pub struct Db {
    /// SQLite pool for relational data.
    pub sqlite: SqlitePool,

    /// LanceDB connection for vector storage.
    pub lance: lancedb::Connection,

    /// Redb database for key-value config.
    pub redb: Arc<redb::Database>,
}

impl Db {
    /// Connect to all databases and run migrations.
    pub async fn connect(data_dir: &Path) -> Result<Self> {
        // SQLite — per-agent agent.db. If an old spacebot.db exists from
        // before the rename, move it to agent.db.
        let agent_db = data_dir.join("agent.db");
        let legacy_db = data_dir.join("spacebot.db");
        if legacy_db.exists() && !agent_db.exists() {
            std::fs::rename(&legacy_db, &agent_db).with_context(|| {
                format!(
                    "failed to rename legacy per-agent DB {} -> {}",
                    legacy_db.display(),
                    agent_db.display()
                )
            })?;
        }
        let sqlite_url = format!("sqlite:{}?mode=rwc", agent_db.display());
        let sqlite = SqlitePool::connect(&sqlite_url)
            .await
            .with_context(|| "failed to connect to SQLite")?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&sqlite)
            .await
            .with_context(|| "failed to run database migrations")?;

        // LanceDB
        let lance_path = data_dir.join("lancedb");
        std::fs::create_dir_all(&lance_path).with_context(|| {
            format!(
                "failed to create LanceDB directory: {}",
                lance_path.display()
            )
        })?;

        let lance = lancedb::connect(lance_path.to_str().unwrap_or("./lancedb"))
            .execute()
            .await
            .map_err(|e| DbError::LanceConnect(e.to_string()))?;

        // Redb
        let redb_path = data_dir.join("config.redb");
        let redb = redb::Database::create(&redb_path)
            .with_context(|| format!("failed to create redb at: {}", redb_path.display()))?;

        Ok(Self {
            sqlite,
            lance,
            redb: Arc::new(redb),
        })
    }

    /// Close all database connections gracefully.
    pub async fn close(self) {
        self.sqlite.close().await;
        // LanceDB and redb close automatically when dropped
    }
}

/// Connect to the instance-level spacebot database and run its migrations.
///
/// The instance database lives at `{instance_dir}/data/spacebot.db` and holds
/// data shared across all agents: tasks, projects, repos, worktrees. This
/// replaces per-agent task and project tables.
///
/// If an old `tasks.db` exists from before the rename, it is moved to
/// `spacebot.db` first.
pub async fn connect_instance_db(data_dir: &Path) -> Result<SqlitePool> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data directory: {}", data_dir.display()))?;

    let db_path = data_dir.join("spacebot.db");
    let legacy_tasks_db = data_dir.join("tasks.db");
    if legacy_tasks_db.exists() && !db_path.exists() {
        std::fs::rename(&legacy_tasks_db, &db_path).with_context(|| {
            format!(
                "failed to rename legacy tasks.db -> spacebot.db at {}",
                data_dir.display()
            )
        })?;
    }
    let url = format!("sqlite:{}?mode=rwc", db_path.display());

    let pool = SqlitePool::connect(&url).await.with_context(|| {
        format!(
            "failed to connect to instance database: {}",
            db_path.display()
        )
    })?;

    sqlx::migrate!("./migrations/global")
        .run(&pool)
        .await
        .with_context(|| "failed to run instance database migrations")?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Foreign keys must actually be enforced.
    ///
    /// Nothing in this repo issues `PRAGMA foreign_keys`, which reads like the
    /// constraints are decorative — SQLite itself defaults the pragma off. They
    /// are not: sqlx sets `foreign_keys = ON` on every connection it opens
    /// (sqlx-sqlite `options/mod.rs`, default pragma map). This test pins that
    /// behaviour so an options change or a driver swap fails here rather than
    /// silently leaving dangling `project_id`s on tasks.
    #[tokio::test]
    async fn instance_db_enforces_task_project_foreign_keys() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pool = connect_instance_db(dir.path())
            .await
            .expect("instance db should connect and migrate");

        let enforced: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("read foreign_keys pragma");
        assert_eq!(enforced, 1, "foreign key enforcement must be on");

        sqlx::query(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'platform', '/tmp/p1')",
        )
        .execute(&pool)
        .await
        .expect("insert project");

        sqlx::query(
            "INSERT INTO tasks (id, task_number, title, owner_agent_id, assigned_agent_id, \
             created_by, project_id) VALUES ('t1', 1, 'bound', 'a', 'a', 'test', 'p1')",
        )
        .execute(&pool)
        .await
        .expect("insert bound task");

        // A binding to a project that does not exist must be rejected outright.
        let dangling = sqlx::query(
            "INSERT INTO tasks (id, task_number, title, owner_agent_id, assigned_agent_id, \
             created_by, project_id) VALUES ('t2', 2, 'dangling', 'a', 'a', 'test', 'nope')",
        )
        .execute(&pool)
        .await;
        assert!(
            dangling.is_err(),
            "a task must not be bindable to a project that does not exist"
        );

        // Deleting the project unbinds the task rather than destroying it.
        sqlx::query("DELETE FROM projects WHERE id = 'p1'")
            .execute(&pool)
            .await
            .expect("delete project");

        let (survived, project_id): (i64, Option<String>) =
            sqlx::query_as("SELECT task_number, project_id FROM tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .expect("task should survive its project");
        assert_eq!(survived, 1);
        assert!(
            project_id.is_none(),
            "ON DELETE SET NULL must unbind the task, not cascade the delete"
        );
    }
}
