//! Database connection management and migrations.

use crate::error::{DbError, Result};
use anyhow::Context as _;
use sqlx::SqlitePool;
use std::path::Path;

/// Database connections bundle for per-agent databases.
pub struct Db {
    /// SQLite pool for relational data.
    pub sqlite: SqlitePool,

    /// LanceDB connection for vector storage.
    pub lance: lancedb::Connection,

    /// Redb database for key-value config.
    pub redb: Arc<redb::Database>,
}

use std::sync::Arc;

impl Db {
    /// Connect to all databases and run migrations.
    pub async fn connect(data_dir: &Path) -> Result<Self> {
        // SQLite
        let sqlite_url = format!("sqlite:{}?mode=rwc", data_dir.join("spacebot.db").display());
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

/// Instance-level database for cross-agent data (agent links, shared notes).
///
/// Separate from per-agent databases because links span agents and need
/// a shared home that both agents can reference.
pub struct InstanceDb {
    pub sqlite: SqlitePool,
}

impl InstanceDb {
    /// Connect to the instance database and run instance-level migrations.
    pub async fn connect(instance_dir: &Path) -> Result<Self> {
        let db_path = instance_dir.join("instance.db");
        let sqlite_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let sqlite = SqlitePool::connect(&sqlite_url)
            .await
            .with_context(|| format!("failed to connect to instance.db at {}", db_path.display()))?;

        sqlx::migrate!("./migrations_instance")
            .run(&sqlite)
            .await
            .with_context(|| "failed to run instance database migrations")?;

        Ok(Self { sqlite })
    }

    /// Close the instance database connection.
    pub async fn close(self) {
        self.sqlite.close().await;
    }
}
