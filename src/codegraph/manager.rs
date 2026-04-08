//! Central manager for the Code Graph system.
//!
//! Owns all per-project graph databases, pipeline handles, and watchers.
//! Instance-level singleton shared across all agents via `ApiState` and
//! `AgentDeps`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::{RwLock, broadcast, mpsc};

use super::db::{CodeGraphDb, SharedCodeGraphDb};
use super::events::CodeGraphEvent;
use super::pipeline;
use super::pipeline::incremental;
use super::types::{CodeGraphConfig, IndexStatus, RegisteredProject};
use super::watcher::{self, ChangeBatch, WatcherHandle};
use crate::llm::LlmManager;
use crate::memory::EmbeddingModel;

/// Broadcast capacity for code graph events.
const EVENT_BUS_CAPACITY: usize = 256;

/// Buffer capacity for the per-project incremental change channel.
/// The watcher debounces so bursts are already batched; a small capacity
/// is plenty.
const CHANGE_CHANNEL_CAPACITY: usize = 8;

/// Inner state that can be shared with spawned tasks via `Arc`.
struct Inner {
    base_path: PathBuf,
    databases: RwLock<HashMap<String, SharedCodeGraphDb>>,
    watchers: RwLock<HashMap<String, WatcherHandle>>,
    registry: RwLock<HashMap<String, RegisteredProject>>,
    event_tx: broadcast::Sender<CodeGraphEvent>,
    config: RwLock<Arc<CodeGraphConfig>>,
    /// Optional Wave 6 integrations — populated by `set_llm_manager` /
    /// `set_embedding_model` from `main.rs` once those services exist. The
    /// pipeline reads them lazily, so the code graph can still index
    /// projects before (or without) LLM/embedding services being available.
    llm_manager: RwLock<Option<Arc<LlmManager>>>,
    embedding_model: RwLock<Option<Arc<EmbeddingModel>>>,
}

/// Central manager for all code graph operations.
///
/// Instance-level singleton: all agents share access to the same graphs.
pub struct CodeGraphManager {
    inner: Arc<Inner>,
}

impl CodeGraphManager {
    /// Create a new `CodeGraphManager` rooted at the given base path.
    ///
    /// The base path is typically `~/.spacebot/` — code graph data will be
    /// stored under `<base_path>/codegraph/`.
    pub fn new(base_path: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(EVENT_BUS_CAPACITY);

        Self {
            inner: Arc::new(Inner {
                base_path,
                databases: RwLock::new(HashMap::new()),
                watchers: RwLock::new(HashMap::new()),
                registry: RwLock::new(HashMap::new()),
                event_tx,
                config: RwLock::new(Arc::new(CodeGraphConfig::default())),
                llm_manager: RwLock::new(None),
                embedding_model: RwLock::new(None),
            }),
        }
    }

    /// Wire in the shared LLM manager so Phase 9 enrichment can call out
    /// to a model. Called once from `main.rs` after both managers exist.
    pub async fn set_llm_manager(&self, llm: Arc<LlmManager>) {
        *self.inner.llm_manager.write().await = Some(llm);
    }

    /// Wire in the shared fastembed model so the Wave 6 embeddings phase
    /// can generate vectors for code symbols. Called once from `main.rs`.
    pub async fn set_embedding_model(&self, embedder: Arc<EmbeddingModel>) {
        *self.inner.embedding_model.write().await = Some(embedder);
    }

    /// Accessor for the base path so helpers that work on the codegraph
    /// directory layout don't have to duplicate construction logic.
    pub fn base_path(&self) -> &std::path::Path {
        &self.inner.base_path
    }

    /// Load the project registry from disk on startup.
    pub async fn load_registry(&self) -> Result<()> {
        let registry_path = self
            .inner
            .base_path
            .join("codegraph")
            .join("registry.json");

        if !registry_path.exists() {
            return Ok(());
        }

        let data = tokio::fs::read_to_string(&registry_path)
            .await
            .with_context(|| format!("reading registry at {}", registry_path.display()))?;

        let projects: Vec<RegisteredProject> =
            serde_json::from_str(&data).with_context(|| "parsing registry.json")?;

        let mut registry = self.inner.registry.write().await;
        for project in projects {
            registry.insert(project.project_id.clone(), project);
        }

        tracing::info!(
            count = registry.len(),
            "loaded code graph project registry"
        );

        Ok(())
    }

    /// Persist the registry to disk.
    async fn save_registry_inner(inner: &Inner) -> Result<()> {
        let registry_dir = inner.base_path.join("codegraph");
        tokio::fs::create_dir_all(&registry_dir).await?;

        let registry = inner.registry.read().await;
        let projects: Vec<&RegisteredProject> = registry.values().collect();
        let data = serde_json::to_string_pretty(&projects)?;

        let registry_path = registry_dir.join("registry.json");
        tokio::fs::write(&registry_path, data).await?;

        Ok(())
    }

    async fn save_registry(&self) -> Result<()> {
        Self::save_registry_inner(&self.inner).await
    }

    /// Subscribe to code graph events.
    pub fn subscribe(&self) -> broadcast::Receiver<CodeGraphEvent> {
        self.inner.event_tx.subscribe()
    }

    /// Get the event sender (for passing to pipeline and watcher).
    pub fn event_sender(&self) -> broadcast::Sender<CodeGraphEvent> {
        self.inner.event_tx.clone()
    }

    /// List all registered projects.
    pub async fn list_projects(&self) -> Vec<RegisteredProject> {
        self.inner
            .registry
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Get a specific project's info.
    pub async fn get_project(&self, project_id: &str) -> Option<RegisteredProject> {
        self.inner
            .registry
            .read()
            .await
            .get(project_id)
            .cloned()
    }

    /// Register and index a new project.
    pub async fn add_project(
        &self,
        project_id: String,
        name: String,
        root_path: PathBuf,
    ) -> Result<RegisteredProject> {
        {
            let registry = self.inner.registry.read().await;
            if registry.contains_key(&project_id) {
                bail!("project '{}' is already registered", project_id);
            }
        }

        let db = Arc::new(CodeGraphDb::open(&project_id, &self.inner.base_path).await?);

        let now = chrono::Utc::now();
        let project = RegisteredProject {
            project_id: project_id.clone(),
            name,
            root_path: root_path.clone(),
            status: IndexStatus::Pending,
            progress: None,
            error_message: None,
            last_index_stats: None,
            last_indexed_at: None,
            primary_language: None,
            schema_version: super::schema::SCHEMA_VERSION,
            created_at: now,
            updated_at: now,
        };

        {
            let mut registry = self.inner.registry.write().await;
            registry.insert(project_id.clone(), project.clone());
        }
        {
            let mut databases = self.inner.databases.write().await;
            databases.insert(project_id.clone(), db.clone());
        }

        self.save_registry().await?;

        let config = self.inner.config.read().await.clone();
        if config.auto_index_on_add {
            self.start_indexing(&project_id).await?;
        }

        Ok(project)
    }

    /// Ensure a database handle exists for a project, opening it if necessary.
    ///
    /// Projects loaded from the persisted registry at startup don't have their
    /// databases opened until they are actually needed (lazy open).
    async fn ensure_db(&self, project_id: &str) -> Result<SharedCodeGraphDb> {
        // Fast path: already open.
        {
            let databases = self.inner.databases.read().await;
            if let Some(db) = databases.get(project_id) {
                return Ok(db.clone());
            }
        }

        // Slow path: open the database and insert it.
        tracing::info!(project_id = %project_id, "lazy-opening LadybugDB for existing project");
        let db = Arc::new(CodeGraphDb::open(project_id, &self.inner.base_path).await?);
        let mut databases = self.inner.databases.write().await;
        // Double-check in case another task opened it concurrently.
        databases
            .entry(project_id.to_string())
            .or_insert(db.clone());
        Ok(databases.get(project_id).cloned().unwrap())
    }

    /// Start (or restart) the indexing pipeline for a project.
    pub async fn start_indexing(&self, project_id: &str) -> Result<()> {
        // Ensure the project is registered.
        {
            let registry = self.inner.registry.read().await;
            if !registry.contains_key(project_id) {
                bail!("project '{}' not registered", project_id);
            }
        }

        let db = self.ensure_db(project_id).await?;

        let root_path = {
            let registry = self.inner.registry.read().await;
            registry
                .get(project_id)
                .map(|p| p.root_path.clone())
                .with_context(|| format!("project '{}' not registered", project_id))?
        };

        let config = self.inner.config.read().await.clone();

        // Update status to indexing.
        {
            let mut registry = self.inner.registry.write().await;
            if let Some(project) = registry.get_mut(project_id) {
                project.status = IndexStatus::Indexing;
                project.error_message = None;
                project.updated_at = chrono::Utc::now();
            }
        }

        // Purge existing graph data so re-index starts fresh.
        // This runs before the pipeline spawns, keeping the pipeline itself instant.
        {
            db.ensure_schema().await?;
            let pid = project_id.replace('\\', "\\\\").replace('\'', "\\'");
            for label in super::schema::ALL_NODE_LABELS {
                db.execute(&format!(
                    "MATCH (n:{label}) WHERE n.project_id = '{pid}' DETACH DELETE n"
                )).await.ok();
            }
            tracing::debug!(project_id = %project_id, "purged stale graph data before indexing");
        }

        // Snapshot the optional Wave 6 services once per run so the pipeline
        // never has to take the RwLock again mid-phase.
        let llm_manager = self.inner.llm_manager.read().await.clone();
        let embedding_model = self.inner.embedding_model.read().await.clone();

        // Start the pipeline.
        let handle = pipeline::start_full_pipeline(
            project_id.to_string(),
            root_path.clone(),
            db.clone(),
            config.clone(),
            self.inner.event_tx.clone(),
            llm_manager,
            embedding_model,
        );

        // Spawn a progress-forwarder that reads from the watch channel and
        // updates the registry so polling API responses include live progress.
        let mut progress_rx = handle.progress_rx.clone();
        let progress_inner = self.inner.clone();
        let progress_project_id = project_id.to_string();
        tokio::spawn(async move {
            while progress_rx.changed().await.is_ok() {
                let progress = progress_rx.borrow().clone();
                let done = matches!(progress.phase, super::types::PipelinePhase::Complete)
                    && progress.phase_progress >= 1.0;
                {
                    let mut reg = progress_inner.registry.write().await;
                    if let Some(project) = reg.get_mut(&progress_project_id) {
                        project.progress = Some(progress);
                    }
                }
                if done {
                    break;
                }
            }
        });

        // Spawn a completion handler task.
        let project_id_owned = project_id.to_string();
        let inner = self.inner.clone();

        tokio::spawn(async move {
            match handle.wait().await {
                Ok(stats) => {
                    tracing::info!(
                        project_id = %project_id_owned,
                        nodes = stats.nodes_created,
                        edges = stats.edges_created,
                        "pipeline completed successfully"
                    );

                    // Update registry — keep progress visible so the
                    // frontend can display the "Complete" state before
                    // transitioning to the overview.
                    {
                        let mut reg = inner.registry.write().await;
                        if let Some(project) = reg.get_mut(&project_id_owned) {
                            project.status = IndexStatus::Indexed;
                            project.error_message = None;
                            project.progress = Some(super::types::PipelineProgress {
                                phase: super::types::PipelinePhase::Complete,
                                phase_progress: 1.0,
                                message: format!(
                                    "Indexing complete — {} nodes, {} edges",
                                    stats.nodes_created, stats.edges_created
                                ),
                                stats: stats.clone(),
                            });
                            project.last_index_stats = Some(stats);
                            project.last_indexed_at = Some(chrono::Utc::now());
                            project.updated_at = chrono::Utc::now();
                        }
                    }

                    // Clear the completion progress after 8 seconds so the
                    // frontend has time to poll and show the complete state.
                    let clear_inner = inner.clone();
                    let clear_pid = project_id_owned.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                        let mut reg = clear_inner.registry.write().await;
                        if let Some(project) = reg.get_mut(&clear_pid)
                            && project.status == IndexStatus::Indexed
                        {
                            project.progress = None;
                        }
                    });

                    // Persist registry.
                    if let Err(err) = Self::save_registry_inner(&inner).await {
                        tracing::warn!(%err, "failed to save registry after indexing");
                    }

                    // Start file watcher if enabled.
                    if config.real_time_watching {
                        Self::spawn_watcher_with_worker(
                            inner.clone(),
                            project_id_owned.clone(),
                            root_path,
                            db,
                            config,
                        )
                        .await;
                    }
                }
                Err(err) => {
                    let error_msg = format!("{err:#}");
                    tracing::error!(
                        project_id = %project_id_owned,
                        %err,
                        "pipeline failed"
                    );

                    let mut reg = inner.registry.write().await;
                    if let Some(project) = reg.get_mut(&project_id_owned) {
                        project.status = IndexStatus::Error;
                        project.error_message = Some(error_msg);
                        project.progress = None;
                        project.updated_at = chrono::Utc::now();
                    }

                    // Persist so the error survives a restart.
                    if let Err(e) = Self::save_registry_inner(&inner).await {
                        tracing::warn!(%e, "failed to save registry after pipeline error");
                    }
                }
            }
        });

        Ok(())
    }

    /// Remove a project and cascade-delete all its data.
    pub async fn remove_project(&self, project_id: &str) -> Result<()> {
        tracing::info!(project_id = %project_id, "cascade deleting project");

        // 1. Stop file watcher.
        {
            let mut watchers = self.inner.watchers.write().await;
            if let Some(wh) = watchers.remove(project_id) {
                wh.stop();
            }
        }

        // 2. Purge all graph data for this project while the DB is still open.
        {
            let db = self.ensure_db(project_id).await;
            if let Ok(db) = db {
                let pid = project_id.replace('\\', "\\\\").replace('\'', "\\'");
                for label in super::schema::ALL_NODE_LABELS {
                    db.execute(&format!(
                        "MATCH (n:{label}) WHERE n.project_id = '{pid}' DETACH DELETE n"
                    )).await.ok();
                }
                tracing::debug!(project_id = %project_id, "purged graph data");
            }
        }

        // 3. Remove DB handle from memory (releases the Arc).
        let db_handle = {
            let mut databases = self.inner.databases.write().await;
            databases.remove(project_id)
        };

        // 4. Destroy graph database files.
        // `destroy()` takes ownership so the inner lbug::Database handle is
        // dropped before file deletion (required on Windows to release locks).
        if let Some(db) = db_handle {
            match Arc::try_unwrap(db) {
                Ok(owned) => {
                    if let Err(err) = owned.destroy().await {
                        tracing::warn!(%err, "db.destroy() failed, will force-remove directory");
                    }
                }
                Err(arc) => {
                    // Other references still exist — drop ours and let step 4
                    // handle file removal after a delay.
                    let path = arc.db_path.clone();
                    drop(arc);
                    tracing::warn!(
                        path = %path.display(),
                        "other references to CodeGraphDb still exist, deferring cleanup"
                    );
                }
            }
        }

        // 5. Delete entire project metadata directory (lbug/ + any siblings).
        // This also cleans up if destroy() couldn't remove locked files.
        let meta_dir = self.inner.base_path.join("codegraph").join(project_id);
        if meta_dir.exists()
            && let Err(err) = tokio::fs::remove_dir_all(&meta_dir).await
        {
            tracing::warn!(path = %meta_dir.display(), %err, "failed to remove project directory, retrying after delay");
            // Brief delay to let file handles close, then retry.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            tokio::fs::remove_dir_all(&meta_dir).await.ok();
        }
        {
            let mut registry = self.inner.registry.write().await;
            registry.remove(project_id);
        }

        // 6. Persist registry.
        self.save_registry().await?;

        // 7. Fire project_removed event.
        let _ = self.inner.event_tx.send(CodeGraphEvent::ProjectRemoved {
            project_id: project_id.to_string(),
        });

        Ok(())
    }

    /// Get the current configuration.
    pub async fn config(&self) -> Arc<CodeGraphConfig> {
        self.inner.config.read().await.clone()
    }

    /// Update the configuration.
    pub async fn update_config(&self, new_config: CodeGraphConfig) {
        let mut config = self.inner.config.write().await;
        *config = Arc::new(new_config);
    }

    /// Get the database for a project.
    pub async fn get_db(&self, project_id: &str) -> Option<SharedCodeGraphDb> {
        self.inner.databases.read().await.get(project_id).cloned()
    }

    /// Restart watchers for all indexed projects on startup.
    pub async fn restart_watchers(&self) -> Result<()> {
        let config = self.inner.config.read().await.clone();
        if !config.real_time_watching {
            return Ok(());
        }

        let projects: Vec<(String, PathBuf)> = {
            let registry = self.inner.registry.read().await;
            registry
                .values()
                .filter(|p| p.status == IndexStatus::Indexed)
                .map(|p| (p.project_id.clone(), p.root_path.clone()))
                .collect()
        };

        for (project_id, root_path) in projects {
            let db = match self.ensure_db(&project_id).await {
                Ok(db) => db,
                Err(err) => {
                    tracing::warn!(project_id = %project_id, %err, "failed to open db for watcher");
                    continue;
                }
            };

            Self::spawn_watcher_with_worker(
                self.inner.clone(),
                project_id,
                root_path,
                db,
                config.clone(),
            )
            .await;
        }

        Ok(())
    }

    /// Start a file watcher for a project and pair it with an incremental
    /// worker task that drains the change channel. Both get registered so
    /// the watcher can be stopped via `WatcherHandle` on project removal.
    async fn spawn_watcher_with_worker(
        inner: Arc<Inner>,
        project_id: String,
        root_path: PathBuf,
        db: SharedCodeGraphDb,
        config: Arc<CodeGraphConfig>,
    ) {
        let (change_tx, change_rx) = mpsc::channel::<ChangeBatch>(CHANGE_CHANNEL_CAPACITY);

        match watcher::start_watcher(
            project_id.clone(),
            root_path.clone(),
            db.clone(),
            config.clone(),
            inner.event_tx.clone(),
            change_tx,
        ) {
            Ok(wh) => {
                let mut w = inner.watchers.write().await;
                w.insert(project_id.clone(), wh);
            }
            Err(err) => {
                tracing::error!(
                    project_id = %project_id,
                    %err,
                    "failed to start file watcher"
                );
                return;
            }
        }

        // Spawn the incremental consumer. It lives alongside the watcher and
        // exits when the channel is closed (watcher stopped / project removed).
        let worker_inner = inner.clone();
        tokio::spawn(run_incremental_worker(
            worker_inner,
            project_id,
            root_path,
            db,
            config,
            change_rx,
        ));
    }
}

/// Consume debounced change batches from the watcher and drive either an
/// incremental pipeline or a full re-index, depending on the percentage
/// of files that changed.
async fn run_incremental_worker(
    inner: Arc<Inner>,
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    initial_config: Arc<CodeGraphConfig>,
    mut change_rx: mpsc::Receiver<ChangeBatch>,
) {
    tracing::debug!(project_id = %project_id, "incremental worker started");

    while let Some(batch) = change_rx.recv().await {
        if batch.is_empty() {
            continue;
        }

        // Re-read config each batch so live settings updates take effect
        // on the next incremental run without needing to restart the watcher.
        let config = {
            let guard = inner.config.read().await;
            guard.clone()
        };

        // Count indexed files to decide between incremental and full.
        let total_files = match incremental::count_indexed_files(&db, &project_id).await {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(
                    project_id = %project_id,
                    %err,
                    "failed to count indexed files, falling back to incremental"
                );
                usize::MAX // treat as "huge project" — prefer incremental
            }
        };

        let full = incremental::should_full_reindex(batch.len(), total_files, &config);

        if full {
            tracing::info!(
                project_id = %project_id,
                changed = batch.len(),
                total = total_files,
                threshold_pct = config.re_index_threshold,
                "change set exceeds threshold, triggering full re-index"
            );

            // Fall back to the full pipeline. This reuses all the same
            // machinery start_indexing does, minus the registration dance.
            if let Err(err) = trigger_full_reindex(
                inner.clone(),
                project_id.clone(),
                root_path.clone(),
                db.clone(),
                config,
            )
            .await
            {
                tracing::error!(
                    project_id = %project_id,
                    %err,
                    "full re-index fallback failed"
                );
            }

            // Drain any further updates while the full pipeline runs so we
            // don't immediately re-trigger on unrelated changes.
            continue;
        }

        // Mark the project as indexing while the incremental runs so the
        // UI shows activity rather than the stale badge.
        {
            let mut reg = inner.registry.write().await;
            if let Some(project) = reg.get_mut(&project_id) {
                project.status = IndexStatus::Indexing;
                project.updated_at = chrono::Utc::now();
            }
        }

        match incremental::run_incremental_pipeline(
            &project_id,
            &root_path,
            batch,
            &db,
            &config,
            &inner.event_tx,
        )
        .await
        {
            Ok(stats) => {
                tracing::info!(
                    project_id = %project_id,
                    nodes = stats.nodes_created,
                    edges = stats.edges_created,
                    "incremental update applied"
                );

                let mut reg = inner.registry.write().await;
                if let Some(project) = reg.get_mut(&project_id) {
                    project.status = IndexStatus::Indexed;
                    project.last_indexed_at = Some(chrono::Utc::now());
                    project.updated_at = chrono::Utc::now();
                }
            }
            Err(err) => {
                tracing::error!(
                    project_id = %project_id,
                    %err,
                    "incremental pipeline failed"
                );

                let _ = inner.event_tx.send(CodeGraphEvent::GraphError {
                    project_id: project_id.clone(),
                    phase: None,
                    error: format!("{err:#}"),
                });

                let mut reg = inner.registry.write().await;
                if let Some(project) = reg.get_mut(&project_id) {
                    project.status = IndexStatus::Error;
                    project.error_message = Some(format!("{err:#}"));
                    project.updated_at = chrono::Utc::now();
                }
            }
        }
    }

    // Silence "unused" on the initial_config capture — we intentionally
    // keep it so the worker's config is seeded even if the global config
    // lock is contended at start-up.
    drop(initial_config);

    tracing::debug!(project_id = %project_id, "incremental worker exiting");
}

/// Trigger a full re-index as a fallback when the change set is too
/// large for an incremental update.
async fn trigger_full_reindex(
    inner: Arc<Inner>,
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    config: Arc<CodeGraphConfig>,
) -> Result<()> {
    // Flip status to indexing.
    {
        let mut reg = inner.registry.write().await;
        if let Some(project) = reg.get_mut(&project_id) {
            project.status = IndexStatus::Indexing;
            project.error_message = None;
            project.updated_at = chrono::Utc::now();
        }
    }

    // Purge graph data for this project — same logic as start_indexing.
    db.ensure_schema().await?;
    let pid_escaped = project_id.replace('\\', "\\\\").replace('\'', "\\'");
    for label in super::schema::ALL_NODE_LABELS {
        db.execute(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid_escaped}' DETACH DELETE n"
        ))
        .await
        .ok();
    }

    // Pull the Wave 6 services so the full-re-index fallback can still
    // enrich + embed on its way through.
    let llm_manager = inner.llm_manager.read().await.clone();
    let embedding_model = inner.embedding_model.read().await.clone();

    let handle = pipeline::start_full_pipeline(
        project_id.clone(),
        root_path,
        db,
        config,
        inner.event_tx.clone(),
        llm_manager,
        embedding_model,
    );

    match handle.wait().await {
        Ok(stats) => {
            let mut reg = inner.registry.write().await;
            if let Some(project) = reg.get_mut(&project_id) {
                project.status = IndexStatus::Indexed;
                project.error_message = None;
                project.last_index_stats = Some(stats);
                project.last_indexed_at = Some(chrono::Utc::now());
                project.updated_at = chrono::Utc::now();
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

impl std::fmt::Debug for CodeGraphManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGraphManager")
            .field("base_path", &self.inner.base_path)
            .finish_non_exhaustive()
    }
}
