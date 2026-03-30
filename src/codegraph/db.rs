//! LadybugDB (KuzuDB fork) connection management.
//!
//! LadybugDB is an embedded graph database with Cypher query support. We use
//! it as the sole storage backend for all code graph data. Each project gets
//! its own database directory at `.spacebot/codegraph/<project_id>/lbug/`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use super::schema;

/// Maximum number of connections in the per-project pool.
const POOL_SIZE: usize = 5;

/// A connection pool to a single project's LadybugDB instance.
#[derive(Debug)]
pub struct CodeGraphDb {
    /// Path to the database directory.
    pub db_path: PathBuf,
    /// Project ID this database belongs to.
    pub project_id: String,
    /// Connection pool (Mutex-guarded for now; will upgrade to a proper pool
    /// once the kuzu Rust crate's connection model is confirmed).
    connections: Vec<Mutex<()>>,
    /// Whether the schema has been initialized.
    schema_initialized: std::sync::atomic::AtomicBool,
}

impl CodeGraphDb {
    /// Open or create a LadybugDB instance for a project.
    pub async fn open(project_id: &str, base_path: &Path) -> Result<Self> {
        let db_path = base_path
            .join("codegraph")
            .join(project_id)
            .join("lbug");

        // Ensure the directory exists.
        tokio::fs::create_dir_all(&db_path)
            .await
            .with_context(|| format!("creating LadybugDB directory at {}", db_path.display()))?;

        let connections = (0..POOL_SIZE).map(|_| Mutex::new(())).collect();

        let db = Self {
            db_path,
            project_id: project_id.to_string(),
            connections,
            schema_initialized: std::sync::atomic::AtomicBool::new(false),
        };

        Ok(db)
    }

    /// Initialize the graph schema if not already done.
    pub async fn ensure_schema(&self) -> Result<()> {
        if self
            .schema_initialized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }

        // Acquire the first connection slot for schema init.
        let _guard = self.connections[0].lock().await;

        // Double-check after acquiring lock.
        if self
            .schema_initialized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }

        tracing::info!(
            project_id = %self.project_id,
            path = %self.db_path.display(),
            "initializing LadybugDB schema"
        );

        // Schema initialization will be implemented when we integrate the
        // actual kuzu crate. For now, we just log and mark as initialized.
        // The schema DDL is defined in schema.rs.
        let _ddl = schema::schema_ddl();

        self.schema_initialized
            .store(true, std::sync::atomic::Ordering::Release);

        Ok(())
    }

    /// Destroy the database files on disk (used during cascade delete).
    pub async fn destroy(&self) -> Result<()> {
        if self.db_path.exists() {
            tokio::fs::remove_dir_all(&self.db_path)
                .await
                .with_context(|| {
                    format!(
                        "removing LadybugDB directory at {}",
                        self.db_path.display()
                    )
                })?;
        }
        Ok(())
    }
}

/// Wraps a `CodeGraphDb` behind an `Arc` for shared ownership.
pub type SharedCodeGraphDb = Arc<CodeGraphDb>;
