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
