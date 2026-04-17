//! LadybugDB connection management.
//!
//! LadybugDB is an embedded graph database with Cypher query support. We use
//! it as the sole storage backend for all code graph data. Each project gets
//! its own database directory at `.spacebot/codegraph/<project_id>/lbug/`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use super::schema;

/// Truncate a `&str` to at most `max` bytes without splitting a multi-byte
/// UTF-8 character. Used for log-line previews of Cypher statements that
/// may contain emoji or other non-ASCII text — a naive `&s[..max]` panics
/// when `max` lands inside a code point.
fn truncate_for_log(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Path to the plaintext schema-version sidecar file that lives next to
/// the LadybugDB. Persisting the version outside of LadybugDB lets us
/// detect stale schemas without having to call `Database::new()` first —
/// which is critical because old on-disk formats can segfault the native
/// open path, and `spawn_blocking` cannot catch segfaults.
fn sidecar_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("version")
}

async fn read_sidecar_version(db_path: &Path) -> Option<u32> {
    tokio::fs::read_to_string(sidecar_path(db_path))
        .await
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

async fn write_sidecar_version(db_path: &Path, version: u32) {
    let path = sidecar_path(db_path);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&path, version.to_string()).await {
        tracing::warn!(
            path = %path.display(),
            err = %e,
            "failed to write schema version sidecar (non-fatal)"
        );
    }
}

async fn remove_sidecar_version(db_path: &Path) {
    let _ = tokio::fs::remove_file(sidecar_path(db_path)).await;
}

/// Remove a LadybugDB database at `path`, handling both the single-file
/// format (file + optional `.wal` sibling) and the directory-based format.
/// Retries up to 5 times with escalating delays because Windows holds file
/// handles briefly after native Kuzu code releases them.
async fn retry_remove_db(path: &Path) -> bool {
    let wal = path.with_extension("wal");
    const DELAYS_MS: &[u64] = &[100, 200, 400, 800];
    for attempt in 0..5 {
        if !path.exists() {
            tokio::fs::remove_file(&wal).await.ok();
            return true;
        }
        let is_file = tokio::fs::metadata(path)
            .await
            .is_ok_and(|m| m.is_file());

        let result = if is_file {
            let r = tokio::fs::remove_file(path).await;
            if r.is_ok() {
                tokio::fs::remove_file(&wal).await.ok();
            }
            r
        } else {
            let r = tokio::fs::remove_dir_all(path).await;
            if r.is_ok() {
                tokio::fs::remove_file(&wal).await.ok();
            }
            r
        };

        match result {
            Ok(()) => return true,
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    attempt,
                    err = %e,
                    "database removal failed, retrying"
                );
                if let Some(&delay) = DELAYS_MS.get(attempt) {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
            }
        }
    }
    !path.exists()
}

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
    /// Checks the schema version on an existing DB. If stale, the entire
    /// database directory is deleted and recreated from scratch. This
    /// avoids Cypher DROP operations that can crash LadybugDB's native
    /// code when FTS/vector extensions hold references to tables.
    pub async fn open(project_id: &str, base_path: &Path) -> Result<Self> {
        let db_path = base_path
            .join("codegraph")
            .join(project_id)
            .join("lbug");

        // Track whether we cleaned up stale data so we can skip the schema
        // version check on a freshly created database (no tables to nuke).
        let mut freshly_cleaned = false;

        // Pre-open sidecar check: if the on-disk schema version recorded
        // next to the DB doesn't match what this binary expects, nuke the
        // DB *before* calling Database::new(). Old formats (e.g. v5 vs v11)
        // can segfault the native open path, and spawn_blocking cannot
        // catch segfaults. The sidecar is missing on pre-existing installs;
        // in that case we fall through to the legacy _SchemaVersion query.
        if db_path.exists() {
            match read_sidecar_version(&db_path).await {
                Some(v) if v == schema::SCHEMA_VERSION => {
                    // DB is at the expected version per sidecar — safe to open.
                }
                Some(v) => {
                    tracing::info!(
                        project_id = %project_id,
                        stored = v,
                        expected = schema::SCHEMA_VERSION,
                        path = %db_path.display(),
                        "sidecar version mismatch — nuking DB before open to avoid native segfault"
                    );
                    if !retry_remove_db(&db_path).await {
                        bail!(
                            "cannot remove stale LadybugDB at {} — file locks held",
                            db_path.display()
                        );
                    }
                    remove_sidecar_version(&db_path).await;
                    freshly_cleaned = true;
                }
                None => {
                    // No sidecar on an existing DB: pre-existing install.
                    // The post-open _SchemaVersion check will catch stale
                    // schemas *if* Database::new() succeeds. If it segfaults,
                    // the user must manually delete the DB directory — we
                    // cannot recover from a native crash. Future opens after
                    // one successful init will have the sidecar.
                    tracing::debug!(
                        path = %db_path.display(),
                        "no version sidecar; falling back to post-open schema check"
                    );
                }
            }
        }

        // If an empty directory sits at db_path, remove it so LadybugDB
        // can initialize a fresh database. Files (single-file DB format)
        // are left for Database::new to open directly.
        if db_path.exists() {
            let is_dir = tokio::fs::metadata(&db_path)
                .await
                .is_ok_and(|m| m.is_dir());

            if is_dir {
                let is_empty_or_corrupt = match tokio::fs::read_dir(&db_path).await {
                    Ok(mut entries) => entries.next_entry().await.ok().flatten().is_none(),
                    Err(_) => true,
                };
                if is_empty_or_corrupt {
                    tracing::info!(
                        path = %db_path.display(),
                        "removing empty/corrupt LadybugDB directory before re-creation"
                    );
                    if !retry_remove_db(&db_path).await {
                        bail!(
                            "cannot remove corrupt LadybugDB directory at {} — \
                             another process may hold file locks",
                            db_path.display()
                        );
                    }
                    freshly_cleaned = true;
                }
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
        .context("LadybugDB open task panicked")?;

        // If the DB is corrupted (e.g. leftover WAL files from a previous
        // incomplete nuke on Windows), delete the directory and retry once.
        let database = match database {
            Ok(db) => db,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Corrupted") || msg.contains("wal") || msg.contains("WAL") {
                    tracing::warn!(
                        path = %db_path.display(),
                        err = %msg,
                        "corrupted database detected — nuking and retrying"
                    );
                    if !retry_remove_db(&db_path).await {
                        bail!(
                            "cannot remove corrupted LadybugDB at {} after retries",
                            db_path.display()
                        );
                    }
                    freshly_cleaned = true;
                    if let Some(parent) = db_path.parent() {
                        tokio::fs::create_dir_all(parent).await.ok();
                    }
                    let path_clone = db_path.clone();
                    tokio::task::spawn_blocking(move || {
                        lbug::Database::new(&path_clone, lbug::SystemConfig::default())
                    })
                    .await
                    .context("LadybugDB retry open task panicked")?
                    .with_context(|| format!("reopening LadybugDB at {} after nuke", db_path.display()))?
                } else {
                    return Err(e).with_context(|| format!("opening LadybugDB at {}", db_path.display()));
                }
            }
        };

        // Skip the version check when we just cleaned up stale data — the
        // DB is guaranteed fresh with no tables, so nuking would pointlessly
        // fight Windows file handles on a database we just created.
        let (database, needs_nuke) = if freshly_cleaned {
            (Arc::new(database), false)
        } else {
            Self::check_schema_version_static(Arc::new(database), project_id).await
        };

        let database = if needs_nuke {
            tracing::info!(
                project_id = %project_id,
                expected = schema::SCHEMA_VERSION,
                "schema stale — nuking database directory and starting fresh"
            );
            // Drop FTS/vector indexes before closing the database so Kuzu
            // releases internal file handles (critical for Windows deletion).
            Self::drop_extension_indexes_static(&database).await;
            drop(database);

            if !retry_remove_db(&db_path).await {
                bail!(
                    "cannot remove stale LadybugDB at {} — file handles still held",
                    db_path.display()
                );
            }
            if let Some(parent) = db_path.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }

            let path_clone = db_path.clone();
            let fresh = tokio::task::spawn_blocking(move || {
                lbug::Database::new(&path_clone, lbug::SystemConfig::default())
            })
            .await
            .context("LadybugDB reopen task panicked")?
            .with_context(|| format!("reopening LadybugDB at {}", db_path.display()))?;
            Arc::new(fresh)
        } else {
            database
        };

        // Persist the current version so the next open() can short-circuit
        // the Database::new() → _SchemaVersion query path entirely.
        write_sidecar_version(&db_path, schema::SCHEMA_VERSION).await;

        Ok(Self {
            db_path,
            project_id: project_id.to_string(),
            database,
            schema_initialized: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Initialize the graph schema if not already done.
    ///
    /// On a fresh DB (after nuke or first creation) this runs all CREATE
    /// statements. On an existing DB with matching schema version, it's
    /// a no-op thanks to IF NOT EXISTS.
    pub async fn ensure_schema(&self) -> Result<()> {
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

    /// Check schema version using a temporary Arc. Used during `open()`
    /// before the struct is constructed. The Arc is unwrapped afterward
    /// so ownership returns to the caller.
    async fn check_schema_version_static(database: Arc<lbug::Database>, project_id: &str) -> (Arc<lbug::Database>, bool) {
        let db = database.clone();
        let pid = project_id.to_string();

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

        let needs_rebuild = match result {
            Some(v) if v == schema::SCHEMA_VERSION => false,
            Some(v) => {
                tracing::info!(
                    project_id = %pid,
                    stored = v,
                    expected = schema::SCHEMA_VERSION,
                    "schema version stale"
                );
                true
            }
            None => true,
        };

        (database, needs_rebuild)
    }

    /// Best-effort cleanup of FTS and vector indexes before a directory nuke.
    /// Releasing these lets Kuzu close internal file handles so Windows
    /// `remove_dir_all` can succeed on the first attempt.
    async fn drop_extension_indexes_static(database: &Arc<lbug::Database>) {
        let db = database.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let conn = match lbug::Connection::new(&db) {
                Ok(c) => c,
                Err(_) => return,
            };
            let _ = conn.query("LOAD EXTENSION fts");
            for label in schema::ALL_NODE_LABELS {
                let idx = format!("{}_fts", label.to_lowercase());
                let _ = conn.query(&format!("CALL DROP_FTS_INDEX('{label}', '{idx}')"));
            }
        })
        .await;
    }

    /// Execute a single Cypher statement (DDL or DML), ignoring results.
    pub async fn execute(&self, cypher: &str) -> Result<()> {
        let db = self.database.clone();
        let cypher = cypher.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = lbug::Connection::new(&db).context("creating connection")?;
            conn.query(&cypher)
                .with_context(|| format!("executing: {}", truncate_for_log(&cypher, 120)))?;
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
                        tracing::debug!(err = %e, stmt = %truncate_for_log(stmt, 100), "batch statement failed");
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
                .with_context(|| format!("querying: {}", truncate_for_log(&cypher, 120)))?;
            let rows: Vec<Vec<lbug::Value>> = result.by_ref().collect();
            Ok(rows)
        })
        .await
        .context("query task panicked")?
    }

    /// Stream rows from a Cypher query without materializing them all.
    ///
    /// The native cursor is driven inside `spawn_blocking` — each row comes
    /// from a separate `hasNext()/getNext()` FFI pair. Rows are pushed
    /// through a bounded channel, so a slow consumer backpressures the
    /// producer, and dropping the stream (e.g. client disconnect) causes
    /// the next `blocking_send` to fail, breaking the loop and releasing
    /// the native cursor via `Drop`. Matches GitNexus's `streamQuery`
    /// pattern (`lbug-adapter.ts:690`) so LadybugDB never has to assemble
    /// a large result set in one shot — the `.collect()` in `query` is
    /// what segfaults the native layer on big graphs.
    pub fn query_stream(
        &self,
        cypher: String,
    ) -> impl futures::Stream<Item = Result<Vec<lbug::Value>>> + Send + 'static {
        let db = self.database.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<lbug::Value>>>(256);

        tokio::task::spawn_blocking(move || {
            let conn = match lbug::Connection::new(&db).context("creating connection") {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            };
            let mut result = match conn
                .query(&cypher)
                .with_context(|| format!("querying: {}", truncate_for_log(&cypher, 120)))
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            };
            while let Some(row) = result.next() {
                if tx.blocking_send(Ok(row)).is_err() {
                    break;
                }
            }
        });

        tokio_stream::wrappers::ReceiverStream::new(rx)
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

    /// Install and load the LadybugDB FTS extension. Safe to call
    /// multiple times; silently succeeds if already loaded. Returns
    /// `Ok(true)` when the extension is ready or `Ok(false)` if loading
    /// failed (e.g. Windows extension compatibility).
    pub async fn load_fts_extension(&self) -> Result<bool> {
        let db = self.database.clone();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let conn = lbug::Connection::new(&db)
                .context("creating connection for FTS extension")?;

            let install = conn.query("INSTALL fts");
            if let Err(e) = &install {
                let msg = e.to_string();
                if !msg.contains("already installed")
                    && !msg.contains("already exists")
                    && !msg.contains("already loaded")
                {
                    tracing::warn!(err = %msg, "FTS extension install failed");
                    return Ok(false);
                }
            }

            let load = conn.query("LOAD EXTENSION fts");
            if let Err(e) = &load {
                let msg = e.to_string();
                if !msg.contains("already loaded") {
                    tracing::warn!(err = %msg, "FTS extension load failed");
                    return Ok(false);
                }
            }

            tracing::debug!("FTS extension ready");
            Ok(true)
        })
        .await
        .context("FTS extension task panicked")?
    }

    /// Install and load the LadybugDB vector extension for HNSW
    /// similarity search. Same pattern as `load_fts_extension`.
    pub async fn load_vector_extension(&self) -> Result<bool> {
        let db = self.database.clone();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let conn = lbug::Connection::new(&db)
                .context("creating connection for vector extension")?;

            let install = conn.query("INSTALL vector");
            if let Err(e) = &install {
                let msg = e.to_string();
                if !msg.contains("already installed")
                    && !msg.contains("already exists")
                    && !msg.contains("already loaded")
                {
                    tracing::warn!(err = %msg, "vector extension install failed");
                    return Ok(false);
                }
            }

            let load = conn.query("LOAD EXTENSION vector");
            if let Err(e) = &load {
                let msg = e.to_string();
                if !msg.contains("already loaded") {
                    tracing::warn!(err = %msg, "vector extension load failed");
                    return Ok(false);
                }
            }

            tracing::debug!("vector extension ready");
            Ok(true)
        })
        .await
        .context("vector extension task panicked")?
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

/// Nuke a project's on-disk LadybugDB directory and version sidecar.
/// The caller must ensure no `lbug::Database` handle for this project is
/// still live in memory — Windows file locks will defeat the removal
/// otherwise. Safe to call on a project that was never indexed.
///
/// Re-index flows call this before `ensure_db` so the subsequent
/// `lbug::Database::new` always runs against an empty directory. That
/// eliminates the stale-data segfault risk that the open-path sidecar
/// check can miss (e.g. sidecar written correctly but native storage
/// corrupt in a way only the lbug binary can detect — too late, it has
/// already SIGSEGV'd).
pub async fn reset_project_db(project_id: &str, base_path: &Path) -> Result<()> {
    let db_path = base_path
        .join("codegraph")
        .join(project_id)
        .join("lbug");

    if db_path.exists() && !retry_remove_db(&db_path).await {
        bail!(
            "cannot remove LadybugDB at {} — file handles still held",
            db_path.display()
        );
    }
    remove_sidecar_version(&db_path).await;
    Ok(())
}
