//! Database connection management and migrations.

use crate::error::{DbError, Result};
use anyhow::Context as _;
use sqlx::SqlitePool;
use std::path::Path;

/// Database connections bundle.
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
