//! Central manager for the Code Graph system.
//!
//! Owns all per-project graph databases, pipeline handles, and watchers.
//! Instance-level singleton shared across all agents via `ApiState` and
//! `AgentDeps`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::{RwLock, broadcast};

use super::db::{CodeGraphDb, SharedCodeGraphDb};
use super::events::CodeGraphEvent;
use super::pipeline;
use super::types::{CodeGraphConfig, IndexStatus, RegisteredProject};
use super::watcher::{self, WatcherHandle};

/// Broadcast capacity for code graph events.
const EVENT_BUS_CAPACITY: usize = 256;

/// Inner state that can be shared with spawned tasks via `Arc`.
struct Inner {
    base_path: PathBuf,
    databases: RwLock<HashMap<String, SharedCodeGraphDb>>,
    watchers: RwLock<HashMap<String, WatcherHandle>>,
    registry: RwLock<HashMap<String, RegisteredProject>>,
    event_tx: broadcast::Sender<CodeGraphEvent>,
    config: RwLock<Arc<CodeGraphConfig>>,
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
            }),
        }
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

    /// Start (or restart) the indexing pipeline for a project.
    pub async fn start_indexing(&self, project_id: &str) -> Result<()> {
        let db = {
            let databases = self.inner.databases.read().await;
            databases
                .get(project_id)
                .cloned()
                .with_context(|| format!("no database for project '{}'", project_id))?
        };

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
                project.updated_at = chrono::Utc::now();
            }
        }

        // Start the pipeline.
        let handle = pipeline::start_full_pipeline(
            project_id.to_string(),
            root_path.clone(),
            db.clone(),
            config.clone(),
            self.inner.event_tx.clone(),
        );

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

                    // Update registry.
                    {
                        let mut reg = inner.registry.write().await;
                        if let Some(project) = reg.get_mut(&project_id_owned) {
                            project.status = IndexStatus::Indexed;
                            project.last_index_stats = Some(stats);
                            project.last_indexed_at = Some(chrono::Utc::now());
                            project.updated_at = chrono::Utc::now();
                        }
                    }

                    // Persist registry.
                    if let Err(err) = Self::save_registry_inner(&inner).await {
                        tracing::warn!(%err, "failed to save registry after indexing");
                    }

                    // Start file watcher if enabled.
                    if config.real_time_watching {
                        match watcher::start_watcher(
                            project_id_owned.clone(),
                            root_path,
                            db,
                            config,
                            inner.event_tx.clone(),
                        ) {
                            Ok(wh) => {
                                let mut w = inner.watchers.write().await;
                                w.insert(project_id_owned, wh);
                            }
                            Err(err) => {
                                tracing::error!(
                                    %err,
                                    "failed to start file watcher"
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(
                        project_id = %project_id_owned,
                        %err,
                        "pipeline failed"
                    );

                    let mut reg = inner.registry.write().await;
                    if let Some(project) = reg.get_mut(&project_id_owned) {
                        project.status = IndexStatus::Error;
                        project.updated_at = chrono::Utc::now();
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

        // 2. Delete graph database files.
        {
            let databases = self.inner.databases.read().await;
            if let Some(db) = databases.get(project_id) {
                db.destroy().await?;
            }
        }

        // 3. Delete metadata directory.
        let meta_dir = self.inner.base_path.join("codegraph").join(project_id);
        if meta_dir.exists() {
            tokio::fs::remove_dir_all(&meta_dir).await.ok();
        }

        // 4. Remove from in-memory state.
        {
            let mut databases = self.inner.databases.write().await;
            databases.remove(project_id);
        }
        {
            let mut registry = self.inner.registry.write().await;
            registry.remove(project_id);
        }

        // 5. Persist registry.
        self.save_registry().await?;

        // 6. Fire project_removed event.
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
            let db = match self.get_db(&project_id).await {
                Some(db) => db,
                None => {
                    let db =
                        Arc::new(CodeGraphDb::open(&project_id, &self.inner.base_path).await?);
                    let mut databases = self.inner.databases.write().await;
                    databases.insert(project_id.clone(), db.clone());
                    db
                }
            };

            match watcher::start_watcher(
                project_id.clone(),
                root_path,
                db,
                config.clone(),
                self.inner.event_tx.clone(),
            ) {
                Ok(wh) => {
                    let mut watchers = self.inner.watchers.write().await;
                    watchers.insert(project_id, wh);
                }
                Err(err) => {
                    tracing::warn!(project_id = %project_id, %err, "failed to restart watcher");
                }
            }
        }

        Ok(())
    }
}

impl std::fmt::Debug for CodeGraphManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGraphManager")
            .field("base_path", &self.inner.base_path)
            .finish_non_exhaustive()
    }
}
