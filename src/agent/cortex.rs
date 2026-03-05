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

use crate::agent::channel_dispatch::{WorkerCompletionError, map_worker_completion_result};
use crate::agent::worker::Worker;
use crate::error::Result;
use crate::hooks::CortexHook;
use crate::llm::SpacebotModel;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort};
use crate::memory::types::{Association, MemoryType, RelationType};
use crate::tasks::{TaskStatus, UpdateTaskInput};
use crate::{
    AgentDeps, AgentId, BranchId, ChannelId, ProcessEvent, ProcessId, ProcessType, WorkerId,
};

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt, TypedPrompt};
use serde::Serialize;
use sqlx::{Row as _, SqlitePool};

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, broadcast};

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

const SIGNAL_BUFFER_CAPACITY: usize = 100;
const BULLETIN_REFRESH_FAILURE_BACKOFF_BASE_SECS: u64 = 30;
const BULLETIN_REFRESH_FAILURE_BACKOFF_MAX_SECS: u64 = 600;
const BULLETIN_REFRESH_CIRCUIT_OPEN_THRESHOLD: u32 = 3;
const BULLETIN_REFRESH_CIRCUIT_OPEN_SECS: u64 = 1800;

fn bulletin_refresh_failure_backoff(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(5);
    let multiplier = 1_u64 << exponent;
    let seconds = BULLETIN_REFRESH_FAILURE_BACKOFF_BASE_SECS
        .saturating_mul(multiplier)
        .min(BULLETIN_REFRESH_FAILURE_BACKOFF_MAX_SECS);
    Duration::from_secs(seconds)
}

fn record_bulletin_refresh_failure(
    bulletin_refresh_failures: &mut u32,
    bulletin_refresh_circuit_open: &mut bool,
    next_bulletin_refresh_allowed_at: &mut Instant,
    now: Instant,
) -> (Duration, bool) {
    *bulletin_refresh_failures = bulletin_refresh_failures.saturating_add(1);
    let backoff = bulletin_refresh_failure_backoff(*bulletin_refresh_failures);
    *next_bulletin_refresh_allowed_at = now + backoff;

    let mut circuit_opened = false;
    if *bulletin_refresh_failures >= BULLETIN_REFRESH_CIRCUIT_OPEN_THRESHOLD {
        if !*bulletin_refresh_circuit_open {
            *bulletin_refresh_circuit_open = true;
            circuit_opened = true;
        }
        let circuit_cooldown = Duration::from_secs(BULLETIN_REFRESH_CIRCUIT_OPEN_SECS);
        let circuit_recovery_at = now + circuit_cooldown;
        if circuit_recovery_at > *next_bulletin_refresh_allowed_at {
            *next_bulletin_refresh_allowed_at = circuit_recovery_at;
        }
    }

    (backoff, circuit_opened)
}

fn maybe_close_bulletin_refresh_circuit(
    bulletin_refresh_failures: &mut u32,
    bulletin_refresh_circuit_open: &mut bool,
    next_bulletin_refresh_allowed_at: &mut Instant,
    now: Instant,
) -> bool {
    if !*bulletin_refresh_circuit_open || now < *next_bulletin_refresh_allowed_at {
        return false;
    }

    *bulletin_refresh_failures = 0;
    *bulletin_refresh_circuit_open = false;
    *next_bulletin_refresh_allowed_at = now;
    true
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
) -> BulletinRefreshOutcome
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
        if generate().await {
            BulletinRefreshOutcome::Generated
        } else {
            BulletinRefreshOutcome::Failed
        }
    } else {
        tracing::debug!(
            warmup_enabled = warmup_config.enabled,
            age_secs = ?age_secs,
            refresh_secs,
            "skipping bulletin loop generation because warmup bulletin is fresh"
        );
        BulletinRefreshOutcome::SkippedFresh
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulletinRefreshOutcome {
    Generated,
    SkippedFresh,
    Failed,
}

impl BulletinRefreshOutcome {
    fn is_success(self) -> bool {
        !matches!(self, Self::Failed)
    }

    fn generated(self) -> bool {
        matches!(self, Self::Generated)
    }
}

/// The cortex observes system-wide activity and maintains the memory bulletin.
pub struct Cortex {
    pub deps: AgentDeps,
    pub hook: CortexHook,
    /// Recent activity signals (rolling window).
    pub signal_buffer: Arc<RwLock<VecDeque<Signal>>>,
    /// System prompt loaded from prompts/CORTEX.md.
    pub system_prompt: String,
}

/// A high-level activity signal (not raw conversation).
#[derive(Debug, Clone)]
pub enum Signal {
    /// Branch started.
    BranchStarted {
        branch_id: BranchId,
        channel_id: ChannelId,
        description: String,
    },
    /// Branch produced a result.
    BranchResult {
        branch_id: BranchId,
        channel_id: ChannelId,
        conclusion: String,
    },
    /// Worker started.
    WorkerStarted {
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        task_summary: String,
        worker_type: String,
    },
    /// Worker status update.
    WorkerStatus {
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        status: String,
    },
    /// Worker completed.
    WorkerCompleted {
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        success: bool,
        result_summary: String,
    },
    /// Tool execution started.
    ToolStarted {
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        tool_name: String,
    },
    /// Tool execution completed.
    ToolCompleted {
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        tool_name: String,
        result_summary: String,
    },
    /// Memory was saved.
    MemorySaved {
        memory_id: String,
        channel_id: Option<ChannelId>,
        memory_type: MemoryType,
        content_summary: String,
        importance: f32,
    },
    /// Compaction threshold was reached.
    CompactionTriggered {
        channel_id: ChannelId,
        threshold_reached: f32,
    },
    /// Generic status update.
    StatusUpdate {
        process_id: ProcessId,
        status: String,
    },
    /// Worker requested a permission decision.
    WorkerPermission {
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        permission_id: String,
        description: String,
    },
    /// Worker asked one or more questions.
    WorkerQuestion {
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        question_id: String,
        question_count: usize,
    },
    /// Agent sent a linked message.
    AgentMessageSent {
        from_agent_id: AgentId,
        to_agent_id: AgentId,
        channel_id: ChannelId,
    },
    /// Agent received a linked message.
    AgentMessageReceived {
        from_agent_id: AgentId,
        to_agent_id: AgentId,
        channel_id: ChannelId,
    },
    /// Task lifecycle update.
    TaskUpdated {
        task_number: i64,
        status: String,
        action: String,
    },
    /// Streaming text delta emitted by a process.
    TextDelta {
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        text_summary: String,
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
            signal_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(SIGNAL_BUFFER_CAPACITY))),
            system_prompt: system_prompt.into(),
        }
    }

    /// Process a process event and extract signals.
    pub async fn observe(&self, event: ProcessEvent) {
        let signal = signal_from_event(event);
        let buffer_len = {
            let mut buffer = self.signal_buffer.write().await;
            push_signal_into_buffer(&mut buffer, signal);
            buffer.len()
        };

        tracing::trace!(buffer_len, "cortex received signal");
    }

    /// Run periodic consolidation (future: health monitoring, memory maintenance).
    pub async fn run_consolidation(&self) -> Result<()> {
        tracing::debug!("cortex running consolidation");
        Ok(())
    }
}

fn summarize_signal_text(value: &str) -> String {
    crate::summarize_first_non_empty_line(value, crate::EVENT_SUMMARY_MAX_CHARS)
}

fn signal_from_event(event: ProcessEvent) -> Signal {
    match event {
        ProcessEvent::BranchStarted {
            branch_id,
            channel_id,
            description,
            ..
        } => Signal::BranchStarted {
            branch_id,
            channel_id,
            description: summarize_signal_text(&description),
        },
        ProcessEvent::BranchResult {
            branch_id,
            channel_id,
            conclusion,
            ..
        } => Signal::BranchResult {
            branch_id,
            channel_id,
            conclusion: summarize_signal_text(&conclusion),
        },
        ProcessEvent::WorkerStarted {
            worker_id,
            channel_id,
            task,
            worker_type,
            ..
        } => Signal::WorkerStarted {
            worker_id,
            channel_id,
            task_summary: summarize_signal_text(&task),
            worker_type,
        },
        ProcessEvent::WorkerStatus {
            worker_id,
            channel_id,
            status,
            ..
        } => Signal::WorkerStatus {
            worker_id,
            channel_id,
            status: summarize_signal_text(&status),
        },
        ProcessEvent::WorkerComplete {
            worker_id,
            channel_id,
            result,
            success,
            ..
        } => Signal::WorkerCompleted {
            worker_id,
            channel_id,
            success,
            result_summary: summarize_signal_text(&result),
        },
        ProcessEvent::ToolStarted {
            process_id,
            channel_id,
            tool_name,
            ..
        } => Signal::ToolStarted {
            process_id,
            channel_id,
            tool_name,
        },
        ProcessEvent::ToolCompleted {
            process_id,
            channel_id,
            tool_name,
            result,
            ..
        } => Signal::ToolCompleted {
            process_id,
            channel_id,
            tool_name,
            result_summary: summarize_signal_text(&result),
        },
        ProcessEvent::MemorySaved {
            memory_id,
            channel_id,
            memory_type,
            importance,
            content_summary,
            ..
        } => Signal::MemorySaved {
            memory_id,
            channel_id,
            memory_type,
            content_summary,
            importance,
        },
        ProcessEvent::CompactionTriggered {
            channel_id,
            threshold_reached,
            ..
        } => Signal::CompactionTriggered {
            channel_id,
            threshold_reached,
        },
        ProcessEvent::StatusUpdate {
            process_id, status, ..
        } => Signal::StatusUpdate {
            process_id,
            status: summarize_signal_text(&status),
        },
        ProcessEvent::WorkerPermission {
            worker_id,
            channel_id,
            permission_id,
            description,
            ..
        } => Signal::WorkerPermission {
            worker_id,
            channel_id,
            permission_id,
            description: summarize_signal_text(&description),
        },
        ProcessEvent::WorkerQuestion {
            worker_id,
            channel_id,
            question_id,
            questions,
            ..
        } => Signal::WorkerQuestion {
            worker_id,
            channel_id,
            question_id,
            question_count: questions.len(),
        },
        ProcessEvent::AgentMessageSent {
            from_agent_id,
            to_agent_id,
            channel_id,
            ..
        } => Signal::AgentMessageSent {
            from_agent_id,
            to_agent_id,
            channel_id,
        },
        ProcessEvent::AgentMessageReceived {
            from_agent_id,
            to_agent_id,
            channel_id,
            ..
        } => Signal::AgentMessageReceived {
            from_agent_id,
            to_agent_id,
            channel_id,
        },
        ProcessEvent::TaskUpdated {
            task_number,
            status,
            action,
            ..
        } => Signal::TaskUpdated {
            task_number,
            status: summarize_signal_text(&status),
            action,
        },
        ProcessEvent::TextDelta {
            process_id,
            channel_id,
            text_delta,
            ..
        } => Signal::TextDelta {
            process_id,
            channel_id,
            text_summary: summarize_signal_text(&text_delta),
        },
    }
}

fn push_signal_into_buffer(buffer: &mut VecDeque<Signal>, signal: Signal) {
    if let Some(previous) = buffer.back_mut()
        && coalesce_signal(previous, &signal)
    {
        return;
    }

    buffer.push_back(signal);
    if buffer.len() > SIGNAL_BUFFER_CAPACITY {
        buffer.pop_front();
    }
}

fn coalesce_signal(previous: &mut Signal, next: &Signal) -> bool {
    match (previous, next) {
        (
            Signal::StatusUpdate {
                process_id: previous_process_id,
                status: previous_status,
            },
            Signal::StatusUpdate {
                process_id: next_process_id,
                status: next_status,
            },
        ) if previous_process_id == next_process_id => {
            *previous_status = next_status.clone();
            true
        }
        (
            Signal::WorkerStatus {
                worker_id: previous_worker_id,
                channel_id: previous_channel_id,
                status: previous_status,
            },
            Signal::WorkerStatus {
                worker_id: next_worker_id,
                channel_id: next_channel_id,
                status: next_status,
            },
        ) if previous_worker_id == next_worker_id && previous_channel_id == next_channel_id => {
            *previous_status = next_status.clone();
            true
        }
        (
            Signal::TaskUpdated {
                task_number: previous_task_number,
                status: previous_status,
                action: previous_action,
            },
            Signal::TaskUpdated {
                task_number: next_task_number,
                status: next_status,
                action: next_action,
            },
        ) if previous_task_number == next_task_number => {
            *previous_status = next_status.clone();
            *previous_action = next_action.clone();
            true
        }
        (
            Signal::TextDelta {
                process_id: previous_process_id,
                channel_id: previous_channel_id,
                text_summary: previous_text_summary,
            },
            Signal::TextDelta {
                process_id: next_process_id,
                channel_id: next_channel_id,
                text_summary: next_text_summary,
            },
        ) if previous_process_id == next_process_id && previous_channel_id == next_channel_id => {
            *previous_text_summary = next_text_summary.clone();
            true
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiverClosedBehavior {
    StopLoop,
    DisableStream,
}

#[derive(Debug, Clone)]
enum CortexReceiverOutcome {
    Observe(ProcessEvent),
    Lagged { dropped: u64 },
    StopLoop,
    DisableStream,
}

fn handle_cortex_receiver_result(
    result: std::result::Result<ProcessEvent, broadcast::error::RecvError>,
    receiver_name: &'static str,
    close_behavior: ReceiverClosedBehavior,
    lagged_since_last_warning: &mut u64,
    last_lag_warning: &mut Option<Instant>,
    warning_interval_secs: u64,
) -> CortexReceiverOutcome {
    match crate::classify_broadcast_recv_result(result) {
        crate::BroadcastRecvResult::Event(event) => CortexReceiverOutcome::Observe(event),
        crate::BroadcastRecvResult::Lagged(count) => {
            if let Some(dropped) = crate::drain_lag_warning_count(
                lagged_since_last_warning,
                last_lag_warning,
                count,
                Duration::from_secs(warning_interval_secs),
            ) {
                tracing::warn!(
                    receiver = receiver_name,
                    dropped,
                    "cortex event receiver lagged, dropping old events"
                );
            }
            CortexReceiverOutcome::Lagged { dropped: count }
        }
        crate::BroadcastRecvResult::Closed => match close_behavior {
            ReceiverClosedBehavior::StopLoop => {
                tracing::warn!(
                    receiver = receiver_name,
                    "cortex event bus closed, stopping cortex loop"
                );
                CortexReceiverOutcome::StopLoop
            }
            ReceiverClosedBehavior::DisableStream => {
                tracing::warn!(
                    receiver = receiver_name,
                    "cortex memory event bus closed, continuing without memory events"
                );
                CortexReceiverOutcome::DisableStream
            }
        },
    }
}

/// Spawn the cortex runtime loop for an agent.
///
/// The loop observes process events and runs periodic cortex maintenance ticks.
/// Bulletin generation and profile refresh happen inside this tick loop.
pub fn spawn_cortex_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let prompt_engine = deps.runtime_config.prompts.load();
        let system_prompt = match prompt_engine.render_static("cortex") {
            Ok(prompt) => prompt,
            Err(error) => {
                tracing::warn!(%error, "failed to render cortex prompt, using empty preamble");
                String::new()
            }
        };
        drop(prompt_engine);

        let cortex = Cortex::new(deps.clone(), system_prompt);
        let mut event_rx = deps.event_tx.subscribe();
        let mut memory_event_rx = deps.memory_event_tx.subscribe();
        if let Err(error) =
            run_cortex_loop(&cortex, &logger, &mut event_rx, &mut memory_event_rx).await
        {
            tracing::error!(%error, "cortex loop exited with error");
        }
    })
}

/// Backwards-compatible alias while callers migrate to `spawn_cortex_loop`.
pub fn spawn_bulletin_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    spawn_cortex_loop(deps, logger)
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

fn spawn_bulletin_refresh_task(
    deps: AgentDeps,
    logger: CortexLogger,
) -> tokio::task::JoinHandle<BulletinRefreshOutcome> {
    tokio::spawn(async move {
        let bulletin_outcome = maybe_generate_bulletin_under_lock(
            deps.runtime_config.warmup_lock.as_ref(),
            &deps.runtime_config.warmup,
            &deps.runtime_config.warmup_status,
            || generate_bulletin(&deps, &logger),
        )
        .await;
        if bulletin_outcome.generated() {
            generate_profile(&deps, &logger).await;
        }
        bulletin_outcome
    })
}

async fn run_cortex_loop(
    cortex: &Cortex,
    logger: &CortexLogger,
    event_rx: &mut broadcast::Receiver<ProcessEvent>,
    memory_event_rx: &mut broadcast::Receiver<ProcessEvent>,
) -> anyhow::Result<()> {
    tracing::info!("cortex loop started");

    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_SECS: u64 = 15;
    const LAG_WARNING_INTERVAL_SECS: u64 = 30;

    // Run bulletin generation immediately on startup, with retries.
    for attempt in 0..=MAX_RETRIES {
        let bulletin_outcome = maybe_generate_bulletin_under_lock(
            cortex.deps.runtime_config.warmup_lock.as_ref(),
            &cortex.deps.runtime_config.warmup,
            &cortex.deps.runtime_config.warmup_status,
            || generate_bulletin(&cortex.deps, logger),
        )
        .await;

        if bulletin_outcome.is_success() {
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

    // Generate an initial profile after startup bulletin synthesis.
    generate_profile(&cortex.deps, logger).await;
    let mut last_bulletin_refresh = Instant::now();
    let mut tick_interval_secs = cortex
        .deps
        .runtime_config
        .cortex
        .load()
        .tick_interval_secs
        .max(1);
    let mut tick_period = Duration::from_secs(tick_interval_secs);
    let mut tick_timer =
        tokio::time::interval_at(tokio::time::Instant::now() + tick_period, tick_period);
    tick_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut lagged_since_last_warning_control: u64 = 0;
    let mut last_lag_warning_control: Option<Instant> = None;
    let mut lagged_since_last_warning_memory: u64 = 0;
    let mut last_lag_warning_memory: Option<Instant> = None;
    let mut memory_event_stream_open = true;
    let mut refresh_task: Option<tokio::task::JoinHandle<BulletinRefreshOutcome>> = None;
    let mut bulletin_refresh_failures: u32 = 0;
    let mut bulletin_refresh_circuit_open = false;
    let mut next_bulletin_refresh_allowed_at = Instant::now();

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                match handle_cortex_receiver_result(
                    event,
                    "control",
                    ReceiverClosedBehavior::StopLoop,
                    &mut lagged_since_last_warning_control,
                    &mut last_lag_warning_control,
                    LAG_WARNING_INTERVAL_SECS,
                ) {
                    CortexReceiverOutcome::Observe(event) => cortex.observe(event).await,
                    CortexReceiverOutcome::Lagged { dropped } => {
                        #[cfg(feature = "metrics")]
                        crate::telemetry::Metrics::global()
                            .event_receiver_lagged_events_total
                            .with_label_values(&[&*cortex.deps.agent_id, "cortex_control"])
                            .inc_by(dropped);
                        #[cfg(not(feature = "metrics"))]
                        let _ = dropped;
                    }
                    CortexReceiverOutcome::StopLoop => {
                        if let Some(task) = refresh_task.take() {
                            task.abort();
                        }
                        return Ok(());
                    }
                    CortexReceiverOutcome::DisableStream => unreachable!("control stream cannot disable itself"),
                }
            },
            event = memory_event_rx.recv(), if memory_event_stream_open => {
                match handle_cortex_receiver_result(
                    event,
                    "memory",
                    ReceiverClosedBehavior::DisableStream,
                    &mut lagged_since_last_warning_memory,
                    &mut last_lag_warning_memory,
                    LAG_WARNING_INTERVAL_SECS,
                ) {
                    CortexReceiverOutcome::Observe(event) => cortex.observe(event).await,
                    CortexReceiverOutcome::Lagged { dropped } => {
                        #[cfg(feature = "metrics")]
                        crate::telemetry::Metrics::global()
                            .event_receiver_lagged_events_total
                            .with_label_values(&[&*cortex.deps.agent_id, "cortex_memory"])
                            .inc_by(dropped);
                        #[cfg(not(feature = "metrics"))]
                        let _ = dropped;
                    }
                    CortexReceiverOutcome::StopLoop => {
                        if let Some(task) = refresh_task.take() {
                            task.abort();
                        }
                        return Ok(());
                    }
                    CortexReceiverOutcome::DisableStream => {
                        memory_event_stream_open = false;
                    }
                }
            },
            _ = tick_timer.tick() => {
                if let Err(error) = cortex.run_consolidation().await {
                    tracing::warn!(%error, "cortex consolidation tick failed");
                }

                if refresh_task
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                    && let Some(task) = refresh_task.take()
                {
                    match task.await {
                        Ok(outcome) => {
                            let now = Instant::now();
                            if outcome.is_success() {
                                last_bulletin_refresh = now;
                                bulletin_refresh_failures = 0;
                                bulletin_refresh_circuit_open = false;
                                next_bulletin_refresh_allowed_at = now;
                            } else {
                                let (backoff, circuit_opened) = record_bulletin_refresh_failure(
                                    &mut bulletin_refresh_failures,
                                    &mut bulletin_refresh_circuit_open,
                                    &mut next_bulletin_refresh_allowed_at,
                                    now,
                                );
                                if circuit_opened {
                                    let cooldown_secs =
                                        next_bulletin_refresh_allowed_at.duration_since(now).as_secs();
                                    tracing::warn!(
                                        failures = bulletin_refresh_failures,
                                        cooldown_secs,
                                        backoff_secs = backoff.as_secs(),
                                        "cortex bulletin refresh circuit opened after consecutive failures"
                                    );
                                } else {
                                    tracing::warn!(
                                        failures = bulletin_refresh_failures,
                                        backoff_secs = backoff.as_secs(),
                                        "cortex bulletin refresh failed; applying retry backoff"
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            let now = Instant::now();
                            let (backoff, circuit_opened) = record_bulletin_refresh_failure(
                                &mut bulletin_refresh_failures,
                                &mut bulletin_refresh_circuit_open,
                                &mut next_bulletin_refresh_allowed_at,
                                now,
                            );
                            if circuit_opened {
                                let cooldown_secs =
                                    next_bulletin_refresh_allowed_at.duration_since(now).as_secs();
                                tracing::warn!(
                                    %error,
                                    failures = bulletin_refresh_failures,
                                    cooldown_secs,
                                    backoff_secs = backoff.as_secs(),
                                    "cortex bulletin refresh circuit opened after task failure"
                                );
                            } else {
                                tracing::warn!(
                                    %error,
                                    failures = bulletin_refresh_failures,
                                    backoff_secs = backoff.as_secs(),
                                    "cortex bulletin refresh task failed"
                                );
                            }
                        }
                    }
                }

                let cortex_config = **cortex.deps.runtime_config.cortex.load();
                let bulletin_interval = Duration::from_secs(cortex_config.bulletin_interval_secs.max(1));
                let now = Instant::now();
                if maybe_close_bulletin_refresh_circuit(
                    &mut bulletin_refresh_failures,
                    &mut bulletin_refresh_circuit_open,
                    &mut next_bulletin_refresh_allowed_at,
                    now,
                ) {
                    tracing::info!("cortex bulletin refresh circuit closed; retries re-enabled");
                }
                if refresh_task.is_none()
                    && !bulletin_refresh_circuit_open
                    && last_bulletin_refresh.elapsed() >= bulletin_interval
                    && now >= next_bulletin_refresh_allowed_at
                {
                    refresh_task = Some(spawn_bulletin_refresh_task(
                        cortex.deps.clone(),
                        logger.clone(),
                    ));
                }

                let updated_tick_interval_secs = cortex_config.tick_interval_secs.max(1);
                if updated_tick_interval_secs != tick_interval_secs {
                    tick_interval_secs = updated_tick_interval_secs;
                    tick_period = Duration::from_secs(tick_interval_secs);
                    tick_timer = tokio::time::interval_at(
                        tokio::time::Instant::now() + tick_period,
                        tick_period,
                    );
                    tick_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                }
            }
        }
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
    let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone());

    // No tools needed — the LLM just synthesizes the pre-gathered data.
    // Attach CortexHook so observation/termination semantics stay consistent
    // with other process types.
    let agent = AgentBuilder::new(model)
        .preamble(&bulletin_prompt)
        .hook(CortexHook::new())
        .build();

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
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
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
    let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone());

    let agent = AgentBuilder::new(model)
        .preamble(&profile_prompt)
        .hook(CortexHook::new())
        .build();

    match agent
        .prompt_typed::<ProfileLlmResponse>(&synthesis_prompt)
        .await
    {
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
    let sandbox_enabled = deps.sandbox.mode_enabled();
    let sandbox_containment_active = deps.sandbox.containment_active();
    let sandbox_read_allowlist = deps.sandbox.prompt_read_allowlist();
    let sandbox_write_allowlist = deps.sandbox.prompt_write_allowlist();

    // Collect tool secret names so the worker template can list available credentials.
    let secrets_guard = deps.runtime_config.secrets.load();
    let tool_secret_names = match (*secrets_guard).as_ref() {
        Some(store) => store.tool_secret_names(),
        None => Vec::new(),
    };

    let worker_system_prompt = prompt_engine
        .render_worker_prompt(
            &deps.runtime_config.instance_dir.display().to_string(),
            &deps.runtime_config.workspace_dir.display().to_string(),
            sandbox_enabled,
            sandbox_containment_active,
            sandbox_read_allowlist,
            sandbox_write_allowlist,
            &tool_secret_names,
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
    let injection_tx = deps.injection_tx.clone();
    let links = deps.links.clone();
    let agent_names = deps.agent_names.clone();
    let sqlite_pool = deps.sqlite_pool.clone();
    let secrets_snapshot = deps.runtime_config.secrets.load().clone();
    let scan_mode = deps.secret_scan_mode();
    tokio::spawn(async move {
        // Helper closure: scrub both known secrets (Layer 1) and unknown leak
        // patterns (Layer 2) from text before it reaches channels or events.
        let scrub = |text: String| -> String {
            let scrubbed = if let Some(store) = secrets_snapshot.as_ref() {
                crate::secrets::scrub::scrub_with_store(&text, store)
            } else {
                text
            };
            scan_mode.maybe_scrub_leaks(scrubbed)
        };

        match worker.run().await {
            Ok(raw_result) => {
                let result_text = scrub(raw_result);
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

                // Handle delegated task completion: log to link channel and
                // notify the delegating agent's originating channel.
                notify_delegation_completion(
                    &task,
                    &result_text,
                    true,
                    &agent_id,
                    &links,
                    &agent_names,
                    &sqlite_pool,
                    &injection_tx,
                )
                .await;

                let (result, notify, success) = map_worker_completion_result(Ok(result_text));
                let _ = event_tx.send(ProcessEvent::WorkerComplete {
                    agent_id: Arc::from(agent_id.as_str()),
                    worker_id,
                    channel_id: None,
                    result,
                    notify,
                    success,
                });
            }
            Err(error) => {
                let (error_message, notify, success) = map_worker_completion_result(Err(
                    WorkerCompletionError::failed(scrub(error.to_string())),
                ));
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

                // Handle delegated task failure: log to link channel and
                // notify the delegating agent's originating channel.
                notify_delegation_completion(
                    &task,
                    &error_message,
                    success,
                    &agent_id,
                    &links,
                    &agent_names,
                    &sqlite_pool,
                    &injection_tx,
                )
                .await;

                let _ = event_tx.send(ProcessEvent::WorkerComplete {
                    agent_id: Arc::from(agent_id.as_str()),
                    worker_id,
                    channel_id: None,
                    result: error_message,
                    notify,
                    success,
                });
            }
        }
    });

    Ok(())
}

/// When a task with `metadata.delegating_agent_id` completes or fails, log the
/// result in the link channel between the two agents and inject a retrigger
/// system message into the delegating agent's originating channel so the user
/// gets notified.
#[allow(clippy::too_many_arguments)]
async fn notify_delegation_completion(
    task: &crate::tasks::Task,
    result_summary: &str,
    success: bool,
    executor_agent_id: &str,
    links: &arc_swap::ArcSwap<Vec<crate::links::AgentLink>>,
    agent_names: &std::collections::HashMap<String, String>,
    sqlite_pool: &sqlx::SqlitePool,
    injection_tx: &tokio::sync::mpsc::Sender<crate::ChannelInjection>,
) {
    // Check if this is a delegated task.
    let delegating_agent_id = task
        .metadata
        .get("delegating_agent_id")
        .and_then(|v| v.as_str());

    let Some(delegating_agent_id) = delegating_agent_id else {
        return; // Not a delegated task.
    };

    let originating_channel = task
        .metadata
        .get("originating_channel")
        .and_then(|v| v.as_str());

    let executor_display = agent_names
        .get(executor_agent_id)
        .cloned()
        .unwrap_or_else(|| executor_agent_id.to_string());

    let status_word = if success { "completed" } else { "failed" };
    let link_message = format!(
        "{executor_display} {status_word} task #{}: \"{}\"",
        task.task_number, task.title
    );

    // Log completion in the link channel on both sides.
    let all_links = links.load();
    if let Some(link) =
        crate::links::find_link_between(&all_links, executor_agent_id, delegating_agent_id)
    {
        let conversation_logger =
            crate::conversation::history::ConversationLogger::new(sqlite_pool.clone());
        let executor_link_channel = link.channel_id_for(executor_agent_id);
        let delegator_link_channel = link.channel_id_for(delegating_agent_id);
        conversation_logger.log_system_message(&executor_link_channel, &link_message);
        conversation_logger.log_system_message(&delegator_link_channel, &link_message);
    }

    // Inject a retrigger into the originating channel so the delegating agent
    // can relay the result to the user.
    let Some(originating_channel) = originating_channel else {
        tracing::info!(
            task_number = task.task_number,
            delegating_agent_id,
            "delegated task completed but no originating_channel in metadata, skipping retrigger"
        );
        return;
    };

    // Truncate very long results for the notification message.
    let truncated_result = if result_summary.len() > 500 {
        let boundary = result_summary.floor_char_boundary(500);
        format!("{}... [truncated]", &result_summary[..boundary])
    } else {
        result_summary.to_string()
    };

    let notification_text = format!(
        "[System] Delegated task #{} {status_word} by {executor_display}: \"{}\"\n\nResult: {truncated_result}",
        task.task_number, task.title,
    );

    let injection = crate::ChannelInjection {
        conversation_id: originating_channel.to_string(),
        agent_id: delegating_agent_id.to_string(),
        message: crate::InboundMessage {
            id: uuid::Uuid::new_v4().to_string(),
            source: "system".into(),
            adapter: None,
            conversation_id: originating_channel.to_string(),
            sender_id: "system".into(),
            agent_id: Some(delegating_agent_id.to_string().into()),
            content: crate::MessageContent::Text(notification_text),
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            formatted_author: None,
        },
    };

    if let Err(error) = injection_tx.send(injection).await {
        tracing::warn!(
            %error,
            task_number = task.task_number,
            originating_channel,
            delegating_agent_id,
            "failed to inject delegation completion retrigger"
        );
    } else {
        tracing::info!(
            task_number = task.task_number,
            originating_channel,
            delegating_agent_id,
            executor_agent_id,
            success,
            "injected delegation completion retrigger"
        );
    }
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
        BULLETIN_REFRESH_CIRCUIT_OPEN_SECS, BULLETIN_REFRESH_CIRCUIT_OPEN_THRESHOLD,
        BulletinRefreshOutcome, CortexReceiverOutcome, ReceiverClosedBehavior, Signal,
        apply_cancelled_warmup_status, handle_cortex_receiver_result, has_completed_initial_warmup,
        maybe_close_bulletin_refresh_circuit, maybe_generate_bulletin_under_lock,
        push_signal_into_buffer, record_bulletin_refresh_failure, should_execute_warmup,
        should_generate_bulletin_from_bulletin_loop, signal_from_event, summarize_signal_text,
    };
    use crate::ProcessEvent;
    use crate::memory::MemoryType;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

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
        assert_eq!(result, BulletinRefreshOutcome::SkippedFresh);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn summarize_signal_text_uses_first_non_empty_line() {
        let text = "\n\nfirst line\nsecond line";
        assert_eq!(summarize_signal_text(text), "first line");
    }

    #[test]
    fn summarize_signal_text_truncates_long_text() {
        let text = "a".repeat(200);
        let summary = summarize_signal_text(&text);
        assert_eq!(summary.chars().count(), crate::EVENT_SUMMARY_MAX_CHARS);
    }

    #[test]
    fn signal_from_event_maps_memory_saved_values() {
        let event = ProcessEvent::MemorySaved {
            agent_id: Arc::from("agent"),
            memory_id: "mem-1".to_string(),
            channel_id: Some(Arc::from("channel-1")),
            memory_type: MemoryType::Decision,
            importance: 0.92,
            content_summary: "persisted decision".to_string(),
        };

        let signal = signal_from_event(event);
        match signal {
            Signal::MemorySaved {
                memory_id,
                channel_id,
                memory_type,
                content_summary,
                importance,
            } => {
                assert_eq!(memory_id, "mem-1");
                assert_eq!(channel_id.as_deref(), Some("channel-1"));
                assert_eq!(memory_type, MemoryType::Decision);
                assert_eq!(content_summary, "persisted decision");
                assert_eq!(importance, 0.92);
            }
            _ => panic!("expected memory-saved signal"),
        }
    }

    #[test]
    fn signal_from_event_handles_every_process_event_variant() {
        let agent_id: crate::AgentId = Arc::from("agent");
        let channel_id: crate::ChannelId = Arc::from("channel");
        let worker_id = uuid::Uuid::new_v4();
        let branch_id = uuid::Uuid::new_v4();

        let events = vec![
            ProcessEvent::BranchStarted {
                agent_id: agent_id.clone(),
                branch_id,
                channel_id: channel_id.clone(),
                description: "branch start".to_string(),
                reply_to_message_id: Some("message-1".to_string()),
            },
            ProcessEvent::BranchResult {
                agent_id: agent_id.clone(),
                branch_id,
                channel_id: channel_id.clone(),
                conclusion: "branch done".to_string(),
            },
            ProcessEvent::WorkerStarted {
                agent_id: agent_id.clone(),
                worker_id,
                channel_id: Some(channel_id.clone()),
                task: "do work".to_string(),
                worker_type: "shell".to_string(),
            },
            ProcessEvent::WorkerStatus {
                agent_id: agent_id.clone(),
                worker_id,
                channel_id: Some(channel_id.clone()),
                status: "running".to_string(),
            },
            ProcessEvent::WorkerComplete {
                agent_id: agent_id.clone(),
                worker_id,
                channel_id: Some(channel_id.clone()),
                result: "ok".to_string(),
                notify: false,
                success: true,
            },
            ProcessEvent::ToolStarted {
                agent_id: agent_id.clone(),
                process_id: crate::ProcessId::Worker(worker_id),
                channel_id: Some(channel_id.clone()),
                tool_name: "shell".to_string(),
                args: "echo hi".to_string(),
            },
            ProcessEvent::ToolCompleted {
                agent_id: agent_id.clone(),
                process_id: crate::ProcessId::Worker(worker_id),
                channel_id: Some(channel_id.clone()),
                tool_name: "shell".to_string(),
                result: "done".to_string(),
            },
            ProcessEvent::MemorySaved {
                agent_id: agent_id.clone(),
                memory_id: "memory-1".to_string(),
                channel_id: Some(channel_id.clone()),
                memory_type: MemoryType::Fact,
                importance: 0.6,
                content_summary: "saved memory".to_string(),
            },
            ProcessEvent::CompactionTriggered {
                agent_id: agent_id.clone(),
                channel_id: channel_id.clone(),
                threshold_reached: 0.86,
            },
            ProcessEvent::StatusUpdate {
                agent_id: agent_id.clone(),
                process_id: crate::ProcessId::Worker(worker_id),
                status: "active".to_string(),
            },
            ProcessEvent::WorkerPermission {
                agent_id: agent_id.clone(),
                worker_id,
                channel_id: Some(channel_id.clone()),
                permission_id: "perm-1".to_string(),
                description: "allow network".to_string(),
                patterns: vec!["https://example.com".to_string()],
            },
            ProcessEvent::WorkerQuestion {
                agent_id: agent_id.clone(),
                worker_id,
                channel_id: Some(channel_id.clone()),
                question_id: "q-1".to_string(),
                questions: vec![],
            },
            ProcessEvent::AgentMessageSent {
                from_agent_id: agent_id.clone(),
                to_agent_id: Arc::from("agent-2"),
                link_id: "link-1".to_string(),
                channel_id: channel_id.clone(),
            },
            ProcessEvent::AgentMessageReceived {
                from_agent_id: Arc::from("agent-2"),
                to_agent_id: agent_id,
                link_id: "link-1".to_string(),
                channel_id: channel_id.clone(),
            },
            ProcessEvent::TaskUpdated {
                agent_id: Arc::from("agent"),
                task_number: 7,
                status: "created".to_string(),
                action: "created".to_string(),
            },
            ProcessEvent::TextDelta {
                agent_id: Arc::from("agent"),
                process_id: crate::ProcessId::Worker(worker_id),
                channel_id: Some(channel_id.clone()),
                text_delta: "he".to_string(),
                aggregated_text: "hello".to_string(),
            },
        ];

        for event in events {
            let _signal = signal_from_event(event);
        }
    }

    #[test]
    fn push_signal_into_buffer_coalesces_status_updates_for_same_process() {
        let mut buffer = VecDeque::new();
        let process_id = crate::ProcessId::Worker(uuid::Uuid::new_v4());

        push_signal_into_buffer(
            &mut buffer,
            Signal::StatusUpdate {
                process_id: process_id.clone(),
                status: "running".to_string(),
            },
        );
        push_signal_into_buffer(
            &mut buffer,
            Signal::StatusUpdate {
                process_id,
                status: "done".to_string(),
            },
        );

        assert_eq!(buffer.len(), 1);
        match buffer.back() {
            Some(Signal::StatusUpdate { status, .. }) => assert_eq!(status, "done"),
            _ => panic!("expected status-update signal"),
        }
    }

    #[test]
    fn push_signal_into_buffer_keeps_distinct_status_updates() {
        let mut buffer = VecDeque::new();

        push_signal_into_buffer(
            &mut buffer,
            Signal::StatusUpdate {
                process_id: crate::ProcessId::Worker(uuid::Uuid::new_v4()),
                status: "running".to_string(),
            },
        );
        push_signal_into_buffer(
            &mut buffer,
            Signal::StatusUpdate {
                process_id: crate::ProcessId::Worker(uuid::Uuid::new_v4()),
                status: "running".to_string(),
            },
        );

        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn memory_receiver_closed_disables_stream_without_stopping_loop() {
        let mut lagged_since_last_warning = 0;
        let mut last_lag_warning = None;

        let outcome = handle_cortex_receiver_result(
            Err(tokio::sync::broadcast::error::RecvError::Closed),
            "memory",
            ReceiverClosedBehavior::DisableStream,
            &mut lagged_since_last_warning,
            &mut last_lag_warning,
            30,
        );

        assert!(matches!(outcome, CortexReceiverOutcome::DisableStream));
    }

    #[test]
    fn memory_receiver_lagged_continues_loop_and_tracks_drop_count() {
        let mut lagged_since_last_warning = 0;
        let mut last_lag_warning = Some(Instant::now());

        let outcome = handle_cortex_receiver_result(
            Err(tokio::sync::broadcast::error::RecvError::Lagged(7)),
            "memory",
            ReceiverClosedBehavior::DisableStream,
            &mut lagged_since_last_warning,
            &mut last_lag_warning,
            30,
        );

        assert!(matches!(
            outcome,
            CortexReceiverOutcome::Lagged { dropped: 7 }
        ));
        assert_eq!(lagged_since_last_warning, 7);
    }

    #[test]
    fn bulletin_refresh_failure_opens_circuit_at_threshold() {
        let mut failures = 0_u32;
        let mut circuit_open = false;
        let mut next_allowed_at = Instant::now();
        let now = Instant::now();

        let (_, opened_first) = record_bulletin_refresh_failure(
            &mut failures,
            &mut circuit_open,
            &mut next_allowed_at,
            now,
        );
        assert!(!opened_first);
        assert!(!circuit_open);

        let (_, opened_second) = record_bulletin_refresh_failure(
            &mut failures,
            &mut circuit_open,
            &mut next_allowed_at,
            now,
        );
        assert!(!opened_second);
        assert!(!circuit_open);

        let (_, opened_third) = record_bulletin_refresh_failure(
            &mut failures,
            &mut circuit_open,
            &mut next_allowed_at,
            now,
        );
        assert!(opened_third);
        assert!(circuit_open);
        assert_eq!(failures, BULLETIN_REFRESH_CIRCUIT_OPEN_THRESHOLD);
        assert!(
            next_allowed_at
                >= now + std::time::Duration::from_secs(BULLETIN_REFRESH_CIRCUIT_OPEN_SECS),
            "circuit-open cooldown should dominate retry window"
        );
    }

    #[test]
    fn bulletin_refresh_circuit_closes_after_cooldown() {
        let mut failures = BULLETIN_REFRESH_CIRCUIT_OPEN_THRESHOLD;
        let mut circuit_open = true;
        let now = Instant::now();
        let mut next_allowed_at = now + std::time::Duration::from_millis(5);

        let closed_early = maybe_close_bulletin_refresh_circuit(
            &mut failures,
            &mut circuit_open,
            &mut next_allowed_at,
            now,
        );
        assert!(!closed_early);
        assert!(circuit_open);

        let closed = maybe_close_bulletin_refresh_circuit(
            &mut failures,
            &mut circuit_open,
            &mut next_allowed_at,
            now + std::time::Duration::from_millis(10),
        );
        assert!(closed);
        assert!(!circuit_open);
        assert_eq!(failures, 0);
    }

    #[tokio::test]
    async fn run_cortex_loop_tick_not_starved_by_events() {
        use std::time::Duration;

        const TEST_DURATION: Duration = Duration::from_millis(750);
        const TICK_PERIOD: Duration = Duration::from_millis(25);
        const MAX_DROPPED_EVENTS_BUDGET: u64 = 512;

        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<ProcessEvent>(1024);
        let event_tx_for_sender = event_tx.clone();
        let mut tick_timer =
            tokio::time::interval_at(tokio::time::Instant::now() + TICK_PERIOD, TICK_PERIOD);
        tick_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let sender = tokio::spawn(async move {
            let agent_id: crate::AgentId = Arc::from("agent");
            let process_id = crate::ProcessId::Worker(uuid::Uuid::new_v4());
            let deadline = tokio::time::Instant::now() + TEST_DURATION;
            while tokio::time::Instant::now() < deadline {
                for _ in 0..8 {
                    let _ = event_tx_for_sender.send(ProcessEvent::StatusUpdate {
                        agent_id: agent_id.clone(),
                        process_id: process_id.clone(),
                        status: "busy".to_string(),
                    });
                }
                tokio::task::yield_now().await;
            }
        });

        let deadline = tokio::time::Instant::now() + TEST_DURATION + Duration::from_millis(250);
        let mut tick_count = 0_u64;
        let mut lagged_dropped_events = 0_u64;
        let mut receiver_closed = false;

        while tokio::time::Instant::now() < deadline {
            tokio::select! {
                _ = tick_timer.tick() => {
                    tick_count = tick_count.saturating_add(1);
                }
                event = event_rx.recv() => {
                    match event {
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            lagged_dropped_events = lagged_dropped_events.saturating_add(skipped);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            receiver_closed = true;
                            break;
                        }
                    }
                }
            }
        }

        sender.await.expect("sender task should complete");
        drop(event_tx);

        assert!(
            !receiver_closed,
            "receiver should not close while load test sender is active"
        );
        assert!(
            tick_count >= (TEST_DURATION.as_millis() / TICK_PERIOD.as_millis() / 4) as u64,
            "periodic tick should continue firing under sustained event load"
        );
        assert!(
            lagged_dropped_events <= MAX_DROPPED_EVENTS_BUDGET,
            "lagged dropped events exceeded budget: {} > {}",
            lagged_dropped_events,
            MAX_DROPPED_EVENTS_BUDGET
        );
    }
}
