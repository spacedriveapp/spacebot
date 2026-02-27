//! Cortex: System-level observer and memory bulletin generator.
//!
//! The cortex's primary responsibility is generating the **memory bulletin** — a
//! periodically refreshed, LLM-curated summary of the agent's current knowledge.
//! This bulletin is injected into every channel's system prompt, giving all
//! conversations ambient awareness of who the user is, what's been decided,
//! what happened recently, and what's going on.
//!
//! The cortex also observes system-wide activity via signals for future use in
//! health monitoring and memory consolidation.

use crate::agent::worker::Worker;
use crate::error::Result;
use crate::hooks::CortexHook;
use crate::llm::SpacebotModel;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort};
use crate::memory::types::{Association, MemoryType, RelationType};
use crate::tasks::{TaskStatus, UpdateTaskInput};
use crate::{AgentDeps, ProcessEvent, ProcessType};

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use serde::Serialize;
use sqlx::{Row as _, SqlitePool};

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

fn update_warmup_status<F>(deps: &AgentDeps, update: F)
where
    F: FnOnce(&mut crate::config::WarmupStatus),
{
    let mut status = deps.runtime_config.warmup_status.load().as_ref().clone();
    update(&mut status);
    deps.runtime_config.warmup_status.store(Arc::new(status));
}

fn bulletin_age_secs(last_refresh_unix_ms: Option<i64>) -> Option<u64> {
    let now = chrono::Utc::now().timestamp_millis();
    last_refresh_unix_ms.map(|refresh_ms| {
        if now > refresh_ms {
            ((now - refresh_ms) / 1000) as u64
        } else {
            0
        }
    })
}

fn should_execute_warmup(warmup_config: crate::config::WarmupConfig, force: bool) -> bool {
    warmup_config.enabled || force
}

fn should_generate_bulletin_from_bulletin_loop(
    warmup_config: crate::config::WarmupConfig,
    status: &crate::config::WarmupStatus,
) -> bool {
    // If warmup is disabled, bulletin_loop remains the source of truth.
    if !warmup_config.enabled {
        return true;
    }

    let age_secs = bulletin_age_secs(status.last_refresh_unix_ms).or(status.bulletin_age_secs);

    let Some(age_secs) = age_secs else {
        // No recorded bulletin refresh yet — let bulletin loop generate one.
        return true;
    };

    // Warmup loop already refreshes bulletin on this cadence. If the cached
    // bulletin is still fresher than warmup cadence, skip duplicate synthesis.
    age_secs >= warmup_config.refresh_secs.max(1)
}

fn has_completed_initial_warmup(status: &crate::config::WarmupStatus) -> bool {
    status.last_refresh_unix_ms.is_some()
        && matches!(status.state, crate::config::WarmupState::Warm)
}

fn apply_cancelled_warmup_status(
    status: &mut crate::config::WarmupStatus,
    reason: &str,
    force: bool,
) -> bool {
    if !matches!(status.state, crate::config::WarmupState::Warming) {
        return false;
    }

    status.state = crate::config::WarmupState::Degraded;
    status.last_error = Some(format!(
        "warmup cancelled before completion (reason: {reason}, forced: {force})"
    ));
    status.bulletin_age_secs = bulletin_age_secs(status.last_refresh_unix_ms);
    true
}

struct WarmupRunGuard<'a> {
    deps: &'a AgentDeps,
    reason: &'a str,
    force: bool,
    committed: bool,
}

impl<'a> WarmupRunGuard<'a> {
    fn new(deps: &'a AgentDeps, reason: &'a str, force: bool) -> Self {
        Self {
            deps,
            reason,
            force,
            committed: false,
        }
    }

    fn mark_committed(&mut self) {
        self.committed = true;
    }
}

impl Drop for WarmupRunGuard<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        update_warmup_status(self.deps, |status| {
            if apply_cancelled_warmup_status(status, self.reason, self.force) {
                tracing::warn!(
                    reason = self.reason,
                    forced = self.force,
                    "warmup run ended without terminal status; demoted state to degraded"
                );
            }
        });
    }
}

async fn maybe_generate_bulletin_under_lock<F, Fut>(
    warmup_lock: &tokio::sync::Mutex<()>,
    warmup_config: &arc_swap::ArcSwap<crate::config::WarmupConfig>,
    warmup_status: &arc_swap::ArcSwap<crate::config::WarmupStatus>,
    generate: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let _warmup_guard = warmup_lock.lock().await;
    let warmup_config = **warmup_config.load();
    let status = warmup_status.load().as_ref().clone();
    let age_secs = bulletin_age_secs(status.last_refresh_unix_ms).or(status.bulletin_age_secs);
    let refresh_secs = warmup_config.refresh_secs.max(1);

    if should_generate_bulletin_from_bulletin_loop(warmup_config, &status) {
        generate().await
    } else {
        tracing::debug!(
            warmup_enabled = warmup_config.enabled,
            age_secs = ?age_secs,
            refresh_secs,
            "skipping bulletin loop generation because warmup bulletin is fresh"
        );
        true
    }
}

/// The cortex observes system-wide activity and maintains the memory bulletin.
pub struct Cortex {
    pub deps: AgentDeps,
    pub hook: CortexHook,
    /// Recent activity signals (rolling window).
    pub signal_buffer: Arc<RwLock<Vec<Signal>>>,
    /// System prompt loaded from prompts/CORTEX.md.
    pub system_prompt: String,
}

/// A high-level activity signal (not raw conversation).
#[derive(Debug, Clone)]
pub enum Signal {
    /// Channel started.
    ChannelStarted { channel_id: String },
    /// Channel ended.
    ChannelEnded { channel_id: String },
    /// Memory was saved.
    MemorySaved {
        memory_type: String,
        content_summary: String,
        importance: f32,
    },
    /// Worker completed.
    WorkerCompleted {
        task_summary: String,
        result_summary: String,
    },
    /// Compaction occurred.
    Compaction {
        channel_id: String,
        turns_compacted: i64,
    },
    /// Error occurred.
    Error {
        component: String,
        error_summary: String,
    },
}

/// A persisted cortex action record.
#[derive(Debug, Clone, Serialize)]
pub struct CortexEvent {
    pub id: String,
    pub event_type: String,
    pub summary: String,
    pub details: Option<serde_json::Value>,
    pub created_at: String,
}

/// Persists cortex actions to SQLite for audit and UI display.
///
/// All writes are fire-and-forget — they spawn a tokio task and return
/// immediately so the cortex never blocks on a DB write.
#[derive(Debug, Clone)]
pub struct CortexLogger {
    pool: SqlitePool,
}

impl CortexLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Log a cortex action. Fire-and-forget.
    pub fn log(&self, event_type: &str, summary: &str, details: Option<serde_json::Value>) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let event_type = event_type.to_string();
        let summary = summary.to_string();
        let details_json = details.map(|d| d.to_string());

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO cortex_events (id, event_type, summary, details) VALUES (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&event_type)
            .bind(&summary)
            .bind(&details_json)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, "failed to persist cortex event");
            }
        });
    }

    /// Load cortex events with optional type filter, newest first.
    pub async fn load_events(
        &self,
        limit: i64,
        offset: i64,
        event_type: Option<&str>,
    ) -> std::result::Result<Vec<CortexEvent>, sqlx::Error> {
        let rows = if let Some(event_type) = event_type {
            sqlx::query_as::<_, CortexEventRow>(
                "SELECT id, event_type, summary, details, created_at FROM cortex_events \
                 WHERE event_type = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(event_type)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, CortexEventRow>(
                "SELECT id, event_type, summary, details, created_at FROM cortex_events \
                 ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows.into_iter().map(|row| row.into_event()).collect())
    }

    /// Count cortex events with optional type filter.
    pub async fn count_events(
        &self,
        event_type: Option<&str>,
    ) -> std::result::Result<i64, sqlx::Error> {
        let count: (i64,) = if let Some(event_type) = event_type {
            sqlx::query_as("SELECT COUNT(*) FROM cortex_events WHERE event_type = ?")
                .bind(event_type)
                .fetch_one(&self.pool)
                .await?
        } else {
            sqlx::query_as("SELECT COUNT(*) FROM cortex_events")
                .fetch_one(&self.pool)
                .await?
        };

        Ok(count.0)
    }
}

/// Internal row type for SQLite query mapping.
#[derive(sqlx::FromRow)]
struct CortexEventRow {
    id: String,
    event_type: String,
    summary: String,
    details: Option<String>,
    created_at: chrono::NaiveDateTime,
}

impl CortexEventRow {
    fn into_event(self) -> CortexEvent {
        CortexEvent {
            id: self.id,
            event_type: self.event_type,
            summary: self.summary,
            details: self.details.and_then(|d| serde_json::from_str(&d).ok()),
            created_at: self.created_at.and_utc().to_rfc3339(),
        }
    }
}

impl Cortex {
    /// Create a new cortex.
    pub fn new(deps: AgentDeps, system_prompt: impl Into<String>) -> Self {
        let hook = CortexHook::new();

        Self {
            deps,
            hook,
            signal_buffer: Arc::new(RwLock::new(Vec::with_capacity(100))),
            system_prompt: system_prompt.into(),
        }
    }

    /// Process a process event and extract signals.
    pub async fn observe(&self, event: ProcessEvent) {
        let signal = match &event {
            ProcessEvent::MemorySaved { memory_id, .. } => Some(Signal::MemorySaved {
                memory_type: "unknown".into(),
                content_summary: format!("memory {}", memory_id),
                importance: 0.5,
            }),
            ProcessEvent::WorkerComplete { result, .. } => Some(Signal::WorkerCompleted {
                task_summary: "completed task".into(),
                result_summary: result.lines().next().unwrap_or("done").into(),
            }),
            ProcessEvent::CompactionTriggered {
                channel_id,
                threshold_reached,
                ..
            } => Some(Signal::Compaction {
                channel_id: channel_id.to_string(),
                turns_compacted: (*threshold_reached * 100.0) as i64,
            }),
            _ => None,
        };

        if let Some(signal) = signal {
            let mut buffer = self.signal_buffer.write().await;
            buffer.push(signal);

            if buffer.len() > 100 {
                buffer.remove(0);
            }

            tracing::debug!("cortex received signal, buffer size: {}", buffer.len());
        }
    }

    /// Run periodic consolidation (future: health monitoring, memory maintenance).
    pub async fn run_consolidation(&self) -> Result<()> {
        tracing::info!("cortex running consolidation");
        Ok(())
    }
}

/// Spawn the cortex bulletin loop for an agent.
///
/// Runs bulletin/profile maintenance on a configurable interval.
///
/// When warmup is enabled, warmup is the primary bulletin refresher and this
/// loop skips duplicate bulletin synthesis while the cached bulletin is fresh.
/// When warmup is disabled (or stale), this loop generates the bulletin.
pub fn spawn_bulletin_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_bulletin_loop(&deps, &logger).await {
            tracing::error!(%error, "cortex bulletin loop exited with error");
        }
    })
}

/// Spawn the warmup loop for an agent.
///
/// Warmup runs asynchronously and never blocks channel responsiveness.
pub fn spawn_warmup_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!("warmup loop started");
        let mut completed_initial_pass =
            has_completed_initial_warmup(deps.runtime_config.warmup_status.load().as_ref());

        loop {
            let warmup_config = **deps.runtime_config.warmup.load();

            if !warmup_config.enabled {
                update_warmup_status(&deps, |status| {
                    status.state = crate::config::WarmupState::Cold;
                    status.bulletin_age_secs = bulletin_age_secs(status.last_refresh_unix_ms);
                });
                tokio::time::sleep(Duration::from_secs(10)).await;
                completed_initial_pass = false;
                continue;
            }

            if !completed_initial_pass {
                completed_initial_pass =
                    has_completed_initial_warmup(deps.runtime_config.warmup_status.load().as_ref());
            }

            let sleep_secs = if completed_initial_pass {
                warmup_config.refresh_secs.max(1)
            } else {
                warmup_config.startup_delay_secs.max(1)
            };
            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

            if !completed_initial_pass {
                completed_initial_pass =
                    has_completed_initial_warmup(deps.runtime_config.warmup_status.load().as_ref());
                if completed_initial_pass {
                    continue;
                }
            }

            let reason = if completed_initial_pass {
                "scheduled"
            } else {
                "startup"
            };
            run_warmup_once(&deps, &logger, reason, false).await;
            completed_initial_pass = true;
        }
    })
}

/// Execute a single warmup pass.
///
/// This is used by the background warmup loop and the manual warmup API.
pub async fn run_warmup_once(deps: &AgentDeps, logger: &CortexLogger, reason: &str, force: bool) {
    let _warmup_guard = deps.runtime_config.warmup_lock.lock().await;
    let warmup_config = **deps.runtime_config.warmup.load();

    if !should_execute_warmup(warmup_config, force) {
        update_warmup_status(deps, |status| {
            status.state = crate::config::WarmupState::Cold;
            status.bulletin_age_secs = bulletin_age_secs(status.last_refresh_unix_ms);
        });
        return;
    }

    update_warmup_status(deps, |status| {
        status.state = crate::config::WarmupState::Warming;
        status.last_error = None;
        status.bulletin_age_secs = bulletin_age_secs(status.last_refresh_unix_ms);
    });
    let mut terminal_state_guard = WarmupRunGuard::new(deps, reason, force);

    let mut errors = Vec::new();
    let mut embedding_ready = false;

    if warmup_config.eager_embedding_load {
        match deps
            .memory_search
            .embedding_model_arc()
            .embed_one("warmup")
            .await
        {
            Ok(_) => embedding_ready = true,
            Err(error) => {
                errors.push(format!("embedding warmup failed: {error}"));
            }
        }
    }

    let bulletin_ok = generate_bulletin(deps, logger).await;
    if !bulletin_ok {
        errors.push("bulletin generation failed".to_string());
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    if errors.is_empty() {
        update_warmup_status(deps, |status| {
            status.state = crate::config::WarmupState::Warm;
            status.embedding_ready = embedding_ready || status.embedding_ready;
            status.last_refresh_unix_ms = Some(now_ms);
            status.last_error = None;
            status.bulletin_age_secs = Some(0);
        });
        terminal_state_guard.mark_committed();
        logger.log(
            "warmup_succeeded",
            "Warmup pass completed",
            Some(serde_json::json!({
                "reason": reason,
                "embedding_ready": embedding_ready,
                "forced": force,
            })),
        );
    } else {
        let last_error = errors.join("; ");
        update_warmup_status(deps, |status| {
            status.state = crate::config::WarmupState::Degraded;
            status.embedding_ready = embedding_ready || status.embedding_ready;
            status.last_error = Some(last_error.clone());
            status.bulletin_age_secs = bulletin_age_secs(status.last_refresh_unix_ms);
        });
        terminal_state_guard.mark_committed();
        logger.log(
            "warmup_failed",
            "Warmup pass failed",
            Some(serde_json::json!({
                "reason": reason,
                "errors": errors,
                "forced": force,
            })),
        );
    }
}

/// Trigger a forced warmup pass in the background from a dispatch path.
///
/// This helper never blocks the caller. It is intended for readiness guards on
/// worker/branch/cron dispatch when the system is cold or degraded.
pub fn trigger_forced_warmup(deps: AgentDeps, dispatch_type: &'static str) {
    tokio::spawn(async move {
        #[cfg(feature = "metrics")]
        let started = Instant::now();
        let logger = CortexLogger::new(deps.sqlite_pool.clone());
        let reason = format!("dispatch_{dispatch_type}");
        run_warmup_once(&deps, &logger, &reason, true).await;

        #[cfg(feature = "metrics")]
        if deps.runtime_config.ready_for_work() {
            crate::telemetry::Metrics::global()
                .warmup_recovery_latency_ms
                .with_label_values(&[&*deps.agent_id, dispatch_type])
                .observe(started.elapsed().as_secs_f64() * 1000.0);
        }
    });
}

async fn run_bulletin_loop(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    tracing::info!("cortex bulletin loop started");

    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_SECS: u64 = 15;

    // Run immediately on startup, with retries
    for attempt in 0..=MAX_RETRIES {
        let bulletin_ok = maybe_generate_bulletin_under_lock(
            deps.runtime_config.warmup_lock.as_ref(),
            &deps.runtime_config.warmup,
            &deps.runtime_config.warmup_status,
            || generate_bulletin(deps, logger),
        )
        .await;

        if bulletin_ok {
            break;
        }
        if attempt < MAX_RETRIES {
            tracing::info!(
                attempt = attempt + 1,
                max = MAX_RETRIES,
                "retrying bulletin generation in {RETRY_DELAY_SECS}s"
            );
            logger.log(
                "bulletin_failed",
                &format!(
                    "Bulletin generation failed, retrying (attempt {}/{})",
                    attempt + 1,
                    MAX_RETRIES
                ),
                Some(serde_json::json!({ "attempt": attempt + 1, "max_retries": MAX_RETRIES })),
            );
            tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
        }
    }

    // Generate initial profile after bulletin
    generate_profile(deps, logger).await;

    loop {
        let cortex_config = **deps.runtime_config.cortex.load();
        let interval = cortex_config.bulletin_interval_secs;

        tokio::time::sleep(Duration::from_secs(interval)).await;

        maybe_generate_bulletin_under_lock(
            deps.runtime_config.warmup_lock.as_ref(),
            &deps.runtime_config.warmup,
            &deps.runtime_config.warmup_status,
            || generate_bulletin(deps, logger),
        )
        .await;
        generate_profile(deps, logger).await;
    }
}

/// Bulletin sections: each defines a search mode + config, and how to label the
/// results when presenting them to the synthesis LLM.
struct BulletinSection {
    label: &'static str,
    mode: SearchMode,
    memory_type: Option<MemoryType>,
    sort_by: SearchSort,
    max_results: usize,
}

const BULLETIN_SECTIONS: &[BulletinSection] = &[
    BulletinSection {
        label: "Identity & Core Facts",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Identity),
        sort_by: SearchSort::Importance,
        max_results: 15,
    },
    BulletinSection {
        label: "Recent Memories",
        mode: SearchMode::Recent,
        memory_type: None,
        sort_by: SearchSort::Recent,
        max_results: 15,
    },
    BulletinSection {
        label: "Decisions",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Decision),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "High-Importance Context",
        mode: SearchMode::Important,
        memory_type: None,
        sort_by: SearchSort::Importance,
        max_results: 10,
    },
    BulletinSection {
        label: "Preferences & Patterns",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Preference),
        sort_by: SearchSort::Importance,
        max_results: 10,
    },
    BulletinSection {
        label: "Active Goals",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Goal),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "Recent Events",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Event),
        sort_by: SearchSort::Recent,
        max_results: 10,
    },
    BulletinSection {
        label: "Observations",
        mode: SearchMode::Typed,
        memory_type: Some(MemoryType::Observation),
        sort_by: SearchSort::Recent,
        max_results: 5,
    },
];

/// Gather raw memory data for each bulletin section by querying the store directly.
/// Returns formatted sections ready for LLM synthesis.
async fn gather_bulletin_sections(deps: &AgentDeps) -> String {
    let mut output = String::new();

    for section in BULLETIN_SECTIONS {
        let config = SearchConfig {
            mode: section.mode,
            memory_type: section.memory_type,
            sort_by: section.sort_by,
            max_results: section.max_results,
            ..Default::default()
        };

        let results = match deps.memory_search.search("", &config).await {
            Ok(results) => results,
            Err(error) => {
                tracing::warn!(
                    section = section.label,
                    %error,
                    "bulletin section query failed"
                );
                continue;
            }
        };

        if results.is_empty() {
            continue;
        }

        output.push_str(&format!("### {}\n\n", section.label));
        for result in &results {
            output.push_str(&format!(
                "- [{}] (importance: {:.1}) {}\n",
                result.memory.memory_type,
                result.memory.importance,
                result
                    .memory
                    .content
                    .lines()
                    .next()
                    .unwrap_or(&result.memory.content),
            ));
        }
        output.push('\n');
    }

    // Append active tasks (non-done) from the task store.
    match gather_active_tasks(deps).await {
        Ok(section) if !section.is_empty() => output.push_str(&section),
        Err(error) => {
            tracing::warn!(%error, "failed to gather active tasks for bulletin");
        }
        _ => {}
    }

    output
}

/// Query the task store for non-done tasks and format them as a bulletin section.
async fn gather_active_tasks(deps: &AgentDeps) -> anyhow::Result<String> {
    use crate::tasks::TaskStatus;

    let mut all_tasks = Vec::new();
    for status in &[
        TaskStatus::InProgress,
        TaskStatus::Ready,
        TaskStatus::Backlog,
        TaskStatus::PendingApproval,
    ] {
        let tasks = deps
            .task_store
            .list(&deps.agent_id, Some(*status), None, 20)
            .await?;
        all_tasks.extend(tasks);
    }

    if all_tasks.is_empty() {
        return Ok(String::new());
    }

    let mut output = String::from("### Active Tasks\n\n");
    for task in &all_tasks {
        let subtask_progress = if task.subtasks.is_empty() {
            String::new()
        } else {
            let done = task.subtasks.iter().filter(|s| s.completed).count();
            format!(" [{}/{}]", done, task.subtasks.len())
        };
        output.push_str(&format!(
            "- #{} [{}] ({}) {}{}\n",
            task.task_number, task.status, task.priority, task.title, subtask_progress,
        ));
    }
    output.push('\n');

    Ok(output)
}

/// Generate a memory bulletin and store it in RuntimeConfig.
///
/// Programmatically queries the memory store across multiple dimensions
/// (identity, recent, decisions, importance, preferences, goals, events,
/// observations), then asks an LLM to synthesize the raw results into a
/// concise briefing.
///
/// On failure, the previous bulletin is preserved (not blanked out).
/// Returns `true` if the bulletin was successfully generated.
#[tracing::instrument(skip(deps, logger), fields(agent_id = %deps.agent_id))]
pub async fn generate_bulletin(deps: &AgentDeps, logger: &CortexLogger) -> bool {
    tracing::info!("cortex generating memory bulletin");
    let started = Instant::now();

    // Phase 1: Programmatically gather raw memory sections (no LLM needed)
    let raw_sections = gather_bulletin_sections(deps).await;
    let section_count = raw_sections.matches("### ").count();

    if raw_sections.is_empty() {
        tracing::info!("no memories found, skipping bulletin synthesis");
        deps.runtime_config
            .memory_bulletin
            .store(Arc::new(String::new()));
        logger.log(
            "bulletin_generated",
            "Bulletin skipped: no memories in graph",
            Some(serde_json::json!({
                "word_count": 0,
                "sections": 0,
                "duration_ms": started.elapsed().as_millis() as u64,
                "skipped": true,
            })),
        );
        return true;
    }

    // Phase 2: LLM synthesis of raw sections into a cohesive bulletin
    let cortex_config = **deps.runtime_config.cortex.load();
    let prompt_engine = deps.runtime_config.prompts.load();
    let bulletin_prompt = match prompt_engine.render_static("cortex_bulletin") {
        Ok(p) => p,
        Err(error) => {
            tracing::error!(%error, "failed to render cortex bulletin prompt");
            return false;
        }
    };

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone());

    // No tools needed — the LLM just synthesizes the pre-gathered data
    let agent = AgentBuilder::new(model).preamble(&bulletin_prompt).build();

    let synthesis_prompt = match prompt_engine
        .render_system_cortex_synthesis(cortex_config.bulletin_max_words, &raw_sections)
    {
        Ok(p) => p,
        Err(error) => {
            tracing::error!(%error, "failed to render cortex synthesis prompt");
            return false;
        }
    };

    match agent.prompt(&synthesis_prompt).await {
        Ok(bulletin) => {
            let word_count = bulletin.split_whitespace().count();
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::info!(words = word_count, "cortex bulletin generated");
            deps.runtime_config
                .memory_bulletin
                .store(Arc::new(bulletin));
            let refresh_ms = chrono::Utc::now().timestamp_millis();
            update_warmup_status(deps, |status| {
                status.last_refresh_unix_ms = Some(refresh_ms);
                status.bulletin_age_secs = Some(0);
                if status.state != crate::config::WarmupState::Warming {
                    status.state = crate::config::WarmupState::Warm;
                    status.last_error = None;
                }
            });
            logger.log(
                "bulletin_generated",
                &format!("Bulletin generated: {word_count} words, {section_count} sections, {duration_ms}ms"),
                Some(serde_json::json!({
                    "word_count": word_count,
                    "sections": section_count,
                    "duration_ms": duration_ms,
                    "model": model_name,
                })),
            );
            true
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::error!(%error, "cortex bulletin synthesis failed, keeping previous bulletin");
            let error_message = error.to_string();
            update_warmup_status(deps, |status| {
                status.bulletin_age_secs = bulletin_age_secs(status.last_refresh_unix_ms);
                if status.state != crate::config::WarmupState::Warming {
                    status.state = crate::config::WarmupState::Degraded;
                    status.last_error =
                        Some(format!("bulletin generation failed: {error_message}"));
                }
            });
            logger.log(
                "bulletin_failed",
                &format!("Bulletin synthesis failed after {duration_ms}ms: {error}"),
                Some(serde_json::json!({
                    "error": error.to_string(),
                    "duration_ms": duration_ms,
                    "model": model_name,
                })),
            );
            false
        }
    }
}

// -- Agent Profile --

/// Persisted agent profile generated by the cortex.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct AgentProfile {
    pub agent_id: String,
    pub display_name: Option<String>,
    pub status: Option<String>,
    pub bio: Option<String>,
    pub avatar_seed: Option<String>,
    pub generated_at: String,
    pub updated_at: String,
}

/// Load the current profile for an agent, if one exists.
pub async fn load_profile(pool: &SqlitePool, agent_id: &str) -> Option<AgentProfile> {
    sqlx::query_as::<_, AgentProfileRow>(
        "SELECT agent_id, display_name, status, bio, avatar_seed, generated_at, updated_at FROM agent_profile WHERE agent_id = ?",
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|row| row.into_profile())
}

#[derive(sqlx::FromRow)]
struct AgentProfileRow {
    agent_id: String,
    display_name: Option<String>,
    status: Option<String>,
    bio: Option<String>,
    avatar_seed: Option<String>,
    generated_at: chrono::NaiveDateTime,
    updated_at: chrono::NaiveDateTime,
}

impl AgentProfileRow {
    fn into_profile(self) -> AgentProfile {
        AgentProfile {
            agent_id: self.agent_id,
            display_name: self.display_name,
            status: self.status,
            bio: self.bio,
            avatar_seed: self.avatar_seed,
            generated_at: self.generated_at.and_utc().to_rfc3339(),
            updated_at: self.updated_at.and_utc().to_rfc3339(),
        }
    }
}

/// LLM response shape for profile generation.
#[derive(serde::Deserialize)]
struct ProfileLlmResponse {
    display_name: Option<String>,
    status: Option<String>,
    bio: Option<String>,
}

/// Generate an agent profile card and persist it to SQLite.
///
/// Uses the current memory bulletin and identity files as context, then asks
/// an LLM to produce a display name, status line, and short bio.
#[tracing::instrument(skip(deps, logger), fields(agent_id = %deps.agent_id))]
async fn generate_profile(deps: &AgentDeps, logger: &CortexLogger) {
    tracing::info!("cortex generating agent profile");
    let started = Instant::now();

    let prompt_engine = deps.runtime_config.prompts.load();
    let profile_prompt = match prompt_engine.render_static("cortex_profile") {
        Ok(p) => p,
        Err(error) => {
            tracing::warn!(%error, "failed to render cortex_profile prompt");
            return;
        }
    };

    // Gather context: identity + current bulletin
    let identity_context = {
        let rendered = deps.runtime_config.identity.load().render();
        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    };
    let memory_bulletin = {
        let bulletin = deps.runtime_config.memory_bulletin.load();
        if bulletin.is_empty() {
            None
        } else {
            Some(bulletin.as_ref().clone())
        }
    };

    let synthesis_prompt = match prompt_engine
        .render_system_profile_synthesis(identity_context.as_deref(), memory_bulletin.as_deref())
    {
        Ok(p) => p,
        Err(error) => {
            tracing::warn!(%error, "failed to render profile synthesis prompt");
            return;
        }
    };

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone());

    let agent = AgentBuilder::new(model).preamble(&profile_prompt).build();

    match agent.prompt(&synthesis_prompt).await {
        Ok(response) => {
            // Strip markdown code fences if the LLM wraps the JSON
            let cleaned = response
                .trim()
                .trim_start_matches("```json")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim();

            match serde_json::from_str::<ProfileLlmResponse>(cleaned) {
                Ok(profile_data) => {
                    let duration_ms = started.elapsed().as_millis() as u64;
                    let agent_id = &deps.agent_id;

                    // Use the agent ID as a stable avatar seed
                    let avatar_seed = agent_id.to_string();

                    if let Err(error) = sqlx::query(
                        "INSERT INTO agent_profile (agent_id, display_name, status, bio, avatar_seed, generated_at, updated_at) \
                         VALUES (?, ?, ?, ?, ?, datetime('now'), datetime('now')) \
                         ON CONFLICT(agent_id) DO UPDATE SET \
                         display_name = excluded.display_name, \
                         status = excluded.status, \
                         bio = excluded.bio, \
                         avatar_seed = excluded.avatar_seed, \
                         updated_at = datetime('now')",
                    )
                    .bind(agent_id.as_ref())
                    .bind(&profile_data.display_name)
                    .bind(&profile_data.status)
                    .bind(&profile_data.bio)
                    .bind(&avatar_seed)
                    .execute(&deps.sqlite_pool)
                    .await
                    {
                        tracing::warn!(%error, "failed to persist agent profile");
                        return;
                    }

                    tracing::info!(
                        display_name = ?profile_data.display_name,
                        status = ?profile_data.status,
                        duration_ms,
                        "agent profile generated"
                    );
                    logger.log(
                        "profile_generated",
                        &format!(
                            "Profile generated: {} — \"{}\" ({duration_ms}ms)",
                            profile_data.display_name.as_deref().unwrap_or("unnamed"),
                            profile_data.status.as_deref().unwrap_or("no status"),
                        ),
                        Some(serde_json::json!({
                            "display_name": profile_data.display_name,
                            "status": profile_data.status,
                            "duration_ms": duration_ms,
                            "model": model_name,
                        })),
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, raw = %cleaned, "failed to parse profile LLM response as JSON");
                    logger.log(
                        "profile_failed",
                        &format!(
                            "Profile generation failed: could not parse LLM response — {error}"
                        ),
                        Some(serde_json::json!({
                            "error": error.to_string(),
                            "raw_response": cleaned,
                        })),
                    );
                }
            }
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            tracing::warn!(%error, "profile generation LLM call failed");
            logger.log(
                "profile_failed",
                &format!("Profile generation failed after {duration_ms}ms: {error}"),
                Some(serde_json::json!({
                    "error": error.to_string(),
                    "duration_ms": duration_ms,
                    "model": model_name,
                })),
            );
        }
    }
}

// -- Association loop --

/// Spawn the association loop for an agent.
///
/// Scans memories for embedding similarity and creates association edges
/// between related memories. On first run, backfills all existing memories.
/// Subsequent runs only process memories created since the last pass.
pub fn spawn_association_loop(
    deps: AgentDeps,
    logger: CortexLogger,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_association_loop(&deps, &logger).await {
            tracing::error!(%error, "cortex association loop exited with error");
        }
    })
}

/// Spawn a background loop that picks up ready tasks when idle.
pub fn spawn_ready_task_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_ready_task_loop(&deps, &logger).await {
            tracing::error!(%error, "cortex ready-task loop exited with error");
        }
    })
}

async fn run_ready_task_loop(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    tracing::info!("cortex ready-task loop started");

    // Let startup settle before first pickup attempt.
    tokio::time::sleep(Duration::from_secs(10)).await;

    loop {
        let interval = deps.runtime_config.cortex.load().tick_interval_secs;
        tokio::time::sleep(Duration::from_secs(interval.max(5))).await;

        if let Err(error) = pickup_one_ready_task(deps, logger).await {
            tracing::warn!(%error, "ready-task pickup pass failed");
        }
    }
}

async fn pickup_one_ready_task(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    let Some(task) = deps.task_store.claim_next_ready(&deps.agent_id).await? else {
        return Ok(());
    };

    logger.log(
        "task_pickup_started",
        &format!("Picked up ready task #{}", task.task_number),
        Some(serde_json::json!({
            "task_number": task.task_number,
            "title": task.title,
        })),
    );

    let prompt_engine = deps.runtime_config.prompts.load();
    let worker_system_prompt = prompt_engine
        .render_worker_prompt(
            &deps.runtime_config.instance_dir.display().to_string(),
            &deps.runtime_config.workspace_dir.display().to_string(),
        )
        .map_err(|error| anyhow::anyhow!("failed to render worker prompt: {error}"))?;

    let mut task_prompt = format!("Execute task #{}: {}", task.task_number, task.title);
    if let Some(description) = &task.description {
        task_prompt.push_str("\n\nDescription:\n");
        task_prompt.push_str(description);
    }
    if !task.subtasks.is_empty() {
        task_prompt.push_str("\n\nSubtasks:\n");
        for (index, subtask) in task.subtasks.iter().enumerate() {
            let marker = if subtask.completed { "[x]" } else { "[ ]" };
            task_prompt.push_str(&format!("{}. {} {}\n", index + 1, marker, subtask.title));
        }
    }

    let screenshot_dir = deps
        .runtime_config
        .workspace_dir
        .join(".spacebot")
        .join("screenshots");
    let logs_dir = deps
        .runtime_config
        .workspace_dir
        .join(".spacebot")
        .join("logs");
    if let Err(error) = std::fs::create_dir_all(&screenshot_dir) {
        tracing::warn!(%error, path = %screenshot_dir.display(), "failed to create screenshot directory");
    }
    if let Err(error) = std::fs::create_dir_all(&logs_dir) {
        tracing::warn!(%error, path = %logs_dir.display(), "failed to create logs directory");
    }

    let browser_config = (**deps.runtime_config.browser_config.load()).clone();
    let brave_search_key = (**deps.runtime_config.brave_search_key.load()).clone();
    let worker = Worker::new(
        None,
        task_prompt,
        worker_system_prompt,
        deps.clone(),
        browser_config,
        screenshot_dir,
        brave_search_key,
        logs_dir,
    );

    let worker_id = worker.id;
    deps.task_store
        .update(
            &deps.agent_id,
            task.task_number,
            UpdateTaskInput {
                worker_id: Some(worker_id.to_string()),
                ..Default::default()
            },
        )
        .await?;

    let _ = deps.event_tx.send(ProcessEvent::TaskUpdated {
        agent_id: deps.agent_id.clone(),
        task_number: task.task_number,
        status: "in_progress".to_string(),
        action: "updated".to_string(),
    });

    let task_description = format!("task #{}: {}", task.task_number, task.title);

    let _ = deps.event_tx.send(ProcessEvent::WorkerStarted {
        agent_id: deps.agent_id.clone(),
        worker_id,
        channel_id: None,
        task: task_description.clone(),
        worker_type: "task".to_string(),
    });

    // Log to worker_runs directly — task workers have no parent channel, so the
    // channel event handler won't persist them.
    let run_logger = crate::conversation::history::ProcessRunLogger::new(deps.sqlite_pool.clone());
    run_logger.log_worker_started(None, worker_id, &task_description, "task", &deps.agent_id);

    let task_store = deps.task_store.clone();
    let agent_id = deps.agent_id.to_string();
    let event_tx = deps.event_tx.clone();
    let logger = logger.clone();
    tokio::spawn(async move {
        match worker.run().await {
            Ok(result_text) => {
                let db_updated = task_store
                    .update(
                        &agent_id,
                        task.task_number,
                        UpdateTaskInput {
                            status: Some(TaskStatus::Done),
                            ..Default::default()
                        },
                    )
                    .await;

                if let Err(ref error) = db_updated {
                    tracing::warn!(%error, task_number = task.task_number, "failed to mark picked-up task done");
                }

                run_logger.log_worker_completed(worker_id, &result_text, true);

                // Only emit task SSE event if the DB write succeeded.
                if db_updated.is_ok() {
                    let _ = event_tx.send(ProcessEvent::TaskUpdated {
                        agent_id: Arc::from(agent_id.as_str()),
                        task_number: task.task_number,
                        status: "done".to_string(),
                        action: "updated".to_string(),
                    });
                }

                logger.log(
                    "task_pickup_completed",
                    &format!("Completed picked-up task #{}", task.task_number),
                    Some(serde_json::json!({
                        "task_number": task.task_number,
                        "worker_id": worker_id.to_string(),
                    })),
                );

                let _ = event_tx.send(ProcessEvent::WorkerComplete {
                    agent_id: Arc::from(agent_id.as_str()),
                    worker_id,
                    channel_id: None,
                    result: result_text,
                    notify: true,
                    success: true,
                });
            }
            Err(error) => {
                let error_message = format!("Worker failed: {error}");
                run_logger.log_worker_completed(worker_id, &error_message, false);

                let requeue_result = task_store
                    .update(
                        &agent_id,
                        task.task_number,
                        UpdateTaskInput {
                            status: Some(TaskStatus::Ready),
                            clear_worker_id: true,
                            ..Default::default()
                        },
                    )
                    .await;

                if let Err(ref update_error) = requeue_result {
                    tracing::warn!(%update_error, task_number = task.task_number, "failed to return task to ready after failure");
                }

                // Only emit task SSE event if the DB write succeeded.
                if requeue_result.is_ok() {
                    let _ = event_tx.send(ProcessEvent::TaskUpdated {
                        agent_id: Arc::from(agent_id.as_str()),
                        task_number: task.task_number,
                        status: "ready".to_string(),
                        action: "updated".to_string(),
                    });
                }

                logger.log(
                    "task_pickup_failed",
                    &format!("Picked-up task #{} failed: {error}", task.task_number),
                    Some(serde_json::json!({
                        "task_number": task.task_number,
                        "worker_id": worker_id.to_string(),
                        "error": error.to_string(),
                    })),
                );

                let _ = event_tx.send(ProcessEvent::WorkerComplete {
                    agent_id: Arc::from(agent_id.as_str()),
                    worker_id,
                    channel_id: None,
                    result: format!("Worker failed: {error}"),
                    notify: true,
                    success: false,
                });
            }
        }
    });

    Ok(())
}

async fn run_association_loop(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    tracing::info!("cortex association loop started");

    // Short delay on startup to let the bulletin and embeddings settle
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Backfill: process all existing memories on first run
    let backfill_count = run_association_pass(deps, logger, None).await;
    tracing::info!(
        associations_created = backfill_count,
        "association backfill complete"
    );

    let mut last_pass_at = chrono::Utc::now();

    loop {
        let cortex_config = **deps.runtime_config.cortex.load();
        let interval = cortex_config.association_interval_secs;

        tokio::time::sleep(Duration::from_secs(interval)).await;

        let since = Some(last_pass_at);
        last_pass_at = chrono::Utc::now();

        let count = run_association_pass(deps, logger, since).await;
        if count > 0 {
            tracing::info!(associations_created = count, "association pass complete");
        }
    }
}

/// Run a single association pass.
///
/// If `since` is None, processes all non-forgotten memories (backfill).
/// If `since` is Some, only processes memories created/updated after that time.
/// Returns the number of associations created.
async fn run_association_pass(
    deps: &AgentDeps,
    logger: &CortexLogger,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> usize {
    let cortex_config = **deps.runtime_config.cortex.load();
    let similarity_threshold = cortex_config.association_similarity_threshold;
    let updates_threshold = cortex_config.association_updates_threshold;
    let max_per_pass = cortex_config.association_max_per_pass;
    let is_backfill = since.is_none();

    let store = deps.memory_search.store();
    let embedding_table = deps.memory_search.embedding_table();

    // Get the memories to process
    let memories = match fetch_memories_for_association(&deps.sqlite_pool, since).await {
        Ok(memories) => memories,
        Err(error) => {
            tracing::warn!(%error, "failed to fetch memories for association pass");
            return 0;
        }
    };

    if memories.is_empty() {
        return 0;
    }

    let memory_count = memories.len();
    let mut created = 0_usize;

    for memory_id in &memories {
        if created >= max_per_pass {
            break;
        }

        // Find similar memories via embedding search
        let similar = match embedding_table
            .find_similar(memory_id, similarity_threshold, 10)
            .await
        {
            Ok(results) => results,
            Err(error) => {
                tracing::debug!(memory_id, %error, "similarity search failed for memory");
                continue;
            }
        };

        for (target_id, similarity) in similar {
            if created >= max_per_pass {
                break;
            }

            // Determine relation type based on similarity
            let relation_type = if similarity >= updates_threshold {
                RelationType::Updates
            } else {
                RelationType::RelatedTo
            };

            // Weight: map similarity range to 0.5-1.0
            let weight =
                0.5 + (similarity - similarity_threshold) / (1.0 - similarity_threshold) * 0.5;

            let association = Association::new(memory_id, &target_id, relation_type)
                .with_weight(weight.clamp(0.0, 1.0));

            if let Err(error) = store.create_association(&association).await {
                tracing::debug!(%error, "failed to create association");
                continue;
            }

            created += 1;
        }
    }

    if created > 0 {
        let summary = if is_backfill {
            format!("Backfill: created {created} associations from {memory_count} memories")
        } else {
            format!("Created {created} associations from {memory_count} new memories")
        };

        logger.log(
            "association_created",
            &summary,
            Some(serde_json::json!({
                "associations_created": created,
                "memories_processed": memory_count,
                "backfill": is_backfill,
                "similarity_threshold": similarity_threshold,
                "updates_threshold": updates_threshold,
            })),
        );
    }

    created
}

/// Fetch memory IDs to process for association.
/// If `since` is None, returns all non-forgotten memory IDs (backfill).
/// If `since` is Some, returns IDs of memories created or updated since that time.
async fn fetch_memories_for_association(
    pool: &SqlitePool,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> anyhow::Result<Vec<String>> {
    let rows = if let Some(since) = since {
        sqlx::query(
            "SELECT id FROM memories WHERE forgotten = 0 AND (created_at > ? OR updated_at > ?) ORDER BY created_at DESC",
        )
        .bind(since)
        .bind(since)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            "SELECT id FROM memories WHERE forgotten = 0 ORDER BY importance DESC, created_at DESC",
        )
        .fetch_all(pool)
        .await?
    };

    Ok(rows.iter().map(|row| row.get("id")).collect())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_cancelled_warmup_status, has_completed_initial_warmup,
        maybe_generate_bulletin_under_lock, should_execute_warmup,
        should_generate_bulletin_from_bulletin_loop,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn run_warmup_once_semantics_skip_when_disabled_without_force() {
        let warmup_config = crate::config::WarmupConfig {
            enabled: false,
            ..Default::default()
        };

        assert!(!should_execute_warmup(warmup_config, false));
    }

    #[test]
    fn run_warmup_once_semantics_force_overrides_disabled_config() {
        let warmup_config = crate::config::WarmupConfig {
            enabled: false,
            ..Default::default()
        };

        assert!(should_execute_warmup(warmup_config, true));
    }

    #[test]
    fn run_warmup_once_semantics_enabled_runs_without_force() {
        let warmup_config = crate::config::WarmupConfig {
            enabled: true,
            ..Default::default()
        };

        assert!(should_execute_warmup(warmup_config, false));
    }

    #[test]
    fn initial_warmup_completion_detected_when_status_has_refresh_timestamp() {
        let status = crate::config::WarmupStatus {
            state: crate::config::WarmupState::Warm,
            last_refresh_unix_ms: Some(1_700_000_000_000),
            ..Default::default()
        };

        assert!(has_completed_initial_warmup(&status));
    }

    #[test]
    fn initial_warmup_completion_not_detected_without_refresh_timestamp() {
        let status = crate::config::WarmupStatus::default();

        assert!(!has_completed_initial_warmup(&status));
    }

    #[test]
    fn initial_warmup_completion_not_detected_when_timestamp_exists_but_state_is_not_warm() {
        let status = crate::config::WarmupStatus {
            state: crate::config::WarmupState::Cold,
            last_refresh_unix_ms: Some(1_700_000_000_000),
            ..Default::default()
        };

        assert!(!has_completed_initial_warmup(&status));
    }

    #[test]
    fn cancelled_warmup_demotes_warming_state_to_degraded() {
        let mut status = crate::config::WarmupStatus {
            state: crate::config::WarmupState::Warming,
            ..Default::default()
        };

        let changed = apply_cancelled_warmup_status(&mut status, "startup", false);

        assert!(changed);
        assert_eq!(status.state, crate::config::WarmupState::Degraded);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("warmup cancelled before completion"))
        );
    }

    #[test]
    fn cancelled_warmup_does_not_override_terminal_state() {
        let mut status = crate::config::WarmupStatus {
            state: crate::config::WarmupState::Warm,
            last_refresh_unix_ms: Some(1_700_000_000_000),
            ..Default::default()
        };

        let changed = apply_cancelled_warmup_status(&mut status, "scheduled", false);

        assert!(!changed);
        assert_eq!(status.state, crate::config::WarmupState::Warm);
    }

    #[test]
    fn bulletin_loop_generation_runs_when_warmup_disabled() {
        let warmup_config = crate::config::WarmupConfig {
            enabled: false,
            ..Default::default()
        };
        let status = crate::config::WarmupStatus {
            bulletin_age_secs: Some(0),
            ..Default::default()
        };

        assert!(should_generate_bulletin_from_bulletin_loop(
            warmup_config,
            &status
        ));
    }

    #[test]
    fn bulletin_loop_generation_skips_when_warmup_enabled_and_fresh() {
        let warmup_config = crate::config::WarmupConfig {
            enabled: true,
            refresh_secs: 900,
            ..Default::default()
        };
        let status = crate::config::WarmupStatus {
            bulletin_age_secs: Some(10),
            ..Default::default()
        };

        assert!(!should_generate_bulletin_from_bulletin_loop(
            warmup_config,
            &status
        ));
    }

    #[test]
    fn bulletin_loop_generation_runs_when_warmup_enabled_and_stale() {
        let warmup_config = crate::config::WarmupConfig {
            enabled: true,
            refresh_secs: 900,
            ..Default::default()
        };
        let status = crate::config::WarmupStatus {
            bulletin_age_secs: Some(901),
            ..Default::default()
        };

        assert!(should_generate_bulletin_from_bulletin_loop(
            warmup_config,
            &status
        ));
    }

    #[tokio::test]
    async fn bulletin_loop_generation_lock_snapshot_skips_after_fresh_update() {
        let warmup_lock = Arc::new(tokio::sync::Mutex::new(()));
        let warmup_config = Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::config::WarmupConfig::default(),
        ));
        let warmup_status = Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::config::WarmupStatus {
                bulletin_age_secs: Some(901), // stale at first
                ..Default::default()
            },
        ));

        let calls = Arc::new(AtomicUsize::new(0));

        // Hold lock so we can update status before helper takes its snapshot.
        let guard = warmup_lock.as_ref().lock().await;

        let warmup_lock_for_task = Arc::clone(&warmup_lock);
        let warmup_config_for_task = Arc::clone(&warmup_config);
        let warmup_status_for_task = Arc::clone(&warmup_status);
        let calls_for_task = Arc::clone(&calls);
        let task = tokio::spawn(async move {
            maybe_generate_bulletin_under_lock(
                warmup_lock_for_task.as_ref(),
                warmup_config_for_task.as_ref(),
                warmup_status_for_task.as_ref(),
                || async {
                    calls_for_task.fetch_add(1, Ordering::SeqCst);
                    true
                },
            )
            .await
        });

        // Warmup refresh lands before lock is released; helper should observe
        // fresh status and skip generation.
        warmup_status.store(Arc::new(crate::config::WarmupStatus {
            bulletin_age_secs: Some(10),
            ..Default::default()
        }));
        drop(guard);

        let result = task.await.expect("task should join");
        assert!(result);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
