//! LadybugDB connection management.
//!
//! LadybugDB is an embedded graph database with Cypher query support. We use
//! it as the sole storage backend for all code graph data. Each project gets
//! its own database directory at `.spacebot/codegraph/<project_id>/lbug/`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use super::schema;

/// The LadybugDB database handle, wrapped in an Arc for shared ownership.
/// `lbug::Database` is `Send + Sync`, and `Connection::new` is cheap,
/// so we create short-lived connections per operation rather than pooling.
#[derive(Debug)]
pub struct CodeGraphDb {
    /// Path to the database directory.
    pub db_path: PathBuf,
    /// Project ID this database belongs to.
    pub project_id: String,
    /// The underlying LadybugDB database handle.
    database: Arc<lbug::Database>,
    /// Whether the schema has been initialized.
    schema_initialized: std::sync::atomic::AtomicBool,
}

impl CodeGraphDb {
    /// Open or create a LadybugDB instance for a project.
    ///
    /// If the directory exists but is empty or corrupt (e.g. after a cascade
    /// delete), it is removed and recreated so LadybugDB can initialize fresh.
    pub async fn open(project_id: &str, base_path: &Path) -> Result<Self> {
        let db_path = base_path
            .join("codegraph")
            .join(project_id)
            .join("lbug");

        // If the directory exists but is empty or missing catalog files,
        // remove it so LadybugDB can initialize a fresh database.
        if db_path.exists() {
            let is_empty_or_corrupt = match tokio::fs::read_dir(&db_path).await {
                Ok(mut entries) => entries.next_entry().await.ok().flatten().is_none(),
                Err(_) => true,
            };
            if is_empty_or_corrupt {
                tracing::info!(
                    path = %db_path.display(),
                    "removing empty/corrupt LadybugDB directory before re-creation"
                );
                tokio::fs::remove_dir_all(&db_path).await.ok();
            }
        }

        // Ensure the *parent* directory exists so LadybugDB can create
        // its own `lbug/` directory with the correct catalog layout.
        // Do NOT create db_path itself — LadybugDB needs to do that.
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| {
                    format!("creating parent directory at {}", parent.display())
                })?;
        }

        let path_clone = db_path.clone();
        let database = tokio::task::spawn_blocking(move || {
            lbug::Database::new(&path_clone, lbug::SystemConfig::default())
        })
        .await
        .context("LadybugDB open task panicked")?
        .with_context(|| format!("opening LadybugDB at {}", db_path.display()))?;

        Ok(Self {
            db_path,
            project_id: project_id.to_string(),
            database: Arc::new(database),
            schema_initialized: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Initialize the graph schema if not already done.
    ///
    /// Checks a `_SchemaVersion` sentinel table in the DB. If the
    /// stored version doesn't match `SCHEMA_VERSION` in the code,
    /// all tables are dropped and recreated so new columns are
    /// available. This forces a re-index but avoids silent node
    /// creation failures from column mismatches.
    pub async fn ensure_schema(&self) -> Result<()> {
        if self
            .schema_initialized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }

        let needs_rebuild = self.check_schema_version().await;

        if needs_rebuild {
            tracing::info!(
                project_id = %self.project_id,
                expected = schema::SCHEMA_VERSION,
                "schema version mismatch — dropping and recreating all tables"
            );
            let drop_stmts = schema::schema_drop_ddl();
            let db = self.database.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn =
                    lbug::Connection::new(&db).context("creating connection for schema drop")?;
                for stmt in &drop_stmts {
                    if let Err(e) = conn.query(stmt) {
                        // "does not exist" is fine — the table may not
                        // have been created in a partially-initialized DB.
                        let msg = e.to_string();
                        if !msg.contains("does not exist")
                            && !msg.contains("not found")
                            && !msg.contains("not exist")
                        {
                            tracing::warn!(ddl = %stmt, err = %msg, "drop statement failed");
                        }
                    }
                }
                Ok(())
            })
            .await
            .context("schema drop task panicked")??;
        }

        tracing::info!(
            project_id = %self.project_id,
            path = %self.db_path.display(),
            "initializing LadybugDB schema"
        );

        let ddl_statements = schema::schema_ddl();
        let total = ddl_statements.len();

        let db = self.database.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn =
                lbug::Connection::new(&db).context("creating connection for schema init")?;

            let mut success = 0;
            let mut skipped = 0;
            for stmt in &ddl_statements {
                match conn.query(stmt) {
                    Ok(_) => success += 1,
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("already exists") {
                            skipped += 1;
                        } else {
                            tracing::warn!(ddl = %stmt, err = %msg, "DDL statement failed");
                            skipped += 1;
                        }
                    }
                }
            }

            tracing::info!(
                total,
                success,
                skipped,
                "schema DDL execution complete"
            );
            Ok(())
        })
        .await
        .context("schema init task panicked")??;

        self.schema_initialized
            .store(true, std::sync::atomic::Ordering::Release);

        Ok(())
    }

    /// Read the `_SchemaVersion` sentinel. Returns `true` when the DB
    /// needs a full rebuild (version missing or doesn't match code).
    async fn check_schema_version(&self) -> bool {
        let db = self.database.clone();
        let result = tokio::task::spawn_blocking(move || -> Option<u32> {
            let conn = lbug::Connection::new(&db).ok()?;
            let mut result = conn
                .query("MATCH (sv:_SchemaVersion) RETURN sv.version")
                .ok()?;
            let row: Option<Vec<lbug::Value>> = result.by_ref().next();
            match row?.first()? {
                lbug::Value::Int32(v) => Some(*v as u32),
                lbug::Value::Int64(v) => Some(*v as u32),
                _ => None,
            }
        })
        .await
        .ok()
        .flatten();

        match result {
            Some(v) if v == schema::SCHEMA_VERSION => false,
            Some(v) => {
                tracing::info!(
                    project_id = %self.project_id,
                    stored = v,
                    expected = schema::SCHEMA_VERSION,
                    "schema version stale"
                );
                true
            }
            None => {
                tracing::info!(
                    project_id = %self.project_id,
                    "no schema version sentinel found — assuming fresh DB"
                );
                false
            }
        }
    }

    /// Execute a single Cypher statement (DDL or DML), ignoring results.
    pub async fn execute(&self, cypher: &str) -> Result<()> {
        let db = self.database.clone();
        let cypher = cypher.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).context("creating connection")?;
            conn.query(&cypher)
                .with_context(|| format!("executing: {}", &cypher[..cypher.len().min(120)]))?;
            Ok(())
        })
        .await
        .context("execute task panicked")?
    }

    /// Execute a batch of Cypher statements in a single blocking call.
    ///
    /// More efficient than calling `execute` in a loop since it only creates
    /// one connection and avoids repeated `spawn_blocking` overhead.
    pub async fn execute_batch(&self, statements: Vec<String>) -> Result<BatchResult> {
        let db = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).context("creating connection for batch")?;
            let mut success = 0u64;
            let mut errors = 0u64;
            for stmt in &statements {
                match conn.query(stmt) {
                    Ok(_) => success += 1,
                    Err(e) => {
                        tracing::debug!(err = %e, stmt = %&stmt[..stmt.len().min(100)], "batch statement failed");
                        errors += 1;
                    }
                }
            }
            Ok(BatchResult { success, errors })
        })
        .await
        .context("batch execute task panicked")?
    }

    /// Execute a Cypher query and return rows as `Vec<Vec<lbug::Value>>`.
    pub async fn query(&self, cypher: &str) -> Result<Vec<Vec<lbug::Value>>> {
        let db = self.database.clone();
        let cypher = cypher.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).context("creating connection")?;
            let mut result = conn
                .query(&cypher)
                .with_context(|| format!("querying: {}", &cypher[..cypher.len().min(120)]))?;
            let rows: Vec<Vec<lbug::Value>> = result.by_ref().collect();
            Ok(rows)
        })
        .await
        .context("query task panicked")?
    }

    /// Execute a query and return a single i64 value (e.g. from RETURN id).
    pub async fn query_scalar_i64(&self, cypher: &str) -> Result<Option<i64>> {
        let rows = self.query(cypher).await?;
        if let Some(row) = rows.first()
            && let Some(val) = row.first()
        {
            return Ok(match val {
                lbug::Value::Int64(n) => Some(*n),
                lbug::Value::Int32(n) => Some(*n as i64),
                lbug::Value::Int16(n) => Some(*n as i64),
                _ => None,
            });
        }
        Ok(None)
    }

    /// Destroy the database files on disk (used during cascade delete).
    ///
    /// Takes ownership of `self` so the inner `lbug::Database` handle is
    /// dropped *before* we attempt to remove the directory. On Windows,
    /// open file handles prevent deletion, so the drop order matters.
    pub async fn destroy(self) -> Result<()> {
        let db_path = self.db_path.clone();
        // Explicitly drop the database handle to release file locks.
        drop(self);

        if db_path.exists() {
            tokio::fs::remove_dir_all(&db_path)
                .await
                .with_context(|| {
                    format!(
                        "removing LadybugDB directory at {}",
                        db_path.display()
                    )
                })?;
        }
        Ok(())
    }
}

/// Result of a batch execution.
#[derive(Debug)]
pub struct BatchResult {
    pub success: u64,
    pub errors: u64,
}

/// Wraps a `CodeGraphDb` behind an `Arc` for shared ownership.
pub type SharedCodeGraphDb = Arc<CodeGraphDb>;
