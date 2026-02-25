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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    #[test]
    fn migration_versions_are_unique() {
        let migrations_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let entries = std::fs::read_dir(&migrations_dir).expect("read migrations directory");

        let mut seen_versions = HashSet::new();
        for entry in entries {
            let entry = entry.expect("read migration directory entry");
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("sql") {
                continue;
            }

            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let (version, _) = file_name
                .split_once('_')
                .expect("migration filename should contain version prefix");
            assert!(
                !version.is_empty(),
                "migration version should not be empty: {file_name}"
            );
            assert!(
                version.chars().all(|character| character.is_ascii_digit()),
                "migration version should be numeric: {file_name}"
            );
            assert!(
                seen_versions.insert(version.to_string()),
                "duplicate migration version detected: {version} ({file_name})"
            );
        }
        assert!(
            !seen_versions.is_empty(),
            "no migrations found in migrations/"
        );
    }
}
