//! Cortex: system-level observer and memory writer.
//!
//! The cortex observes system-wide activity via signals, commits observation
//! memories, elevates tasks, runs memory maintenance, and synthesizes the
//! agent profile. It writes to the memory store like any other process; the
//! channel prompt renders that store directly, so the cortex never authors
//! prompt content at read time.

use crate::error::Result;
use crate::hooks::CortexHook;
use crate::llm::SpacebotModel;
use crate::memory::maintenance as memory_maintenance;
use crate::memory::types::{Association, MemoryType, RelationType};
use crate::{
    AgentDeps, AgentId, BranchId, ChannelId, ProcessEvent, ProcessId, ProcessType, WorkerId,
};

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt, TypedPrompt};
use serde::Serialize;
use sqlx::{Row as _, SqlitePool};

use std::collections::{HashMap, VecDeque};
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

fn refresh_age_secs(last_refresh_unix_ms: Option<i64>) -> Option<u64> {
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

const SIGNAL_BUFFER_CAPACITY: usize = 100;
const MAINTENANCE_CIRCUIT_OPEN_THRESHOLD: usize = 3;
const MAINTENANCE_CIRCUIT_OPEN_SECS: u64 = 1800;
const MAINTENANCE_TASK_TIMEOUT_MIN_SECS: u64 = 300;
const MAINTENANCE_TASK_TIMEOUT_MAX_SECS: u64 = 3_600;
const MAINTENANCE_TASK_TIMEOUT_MULTIPLIER: u64 = 6;
const MAINTENANCE_TASK_CANCEL_GRACE_SECS: u64 = 30;

fn record_maintenance_failure(
    maintenance_consecutive_failures: &mut usize,
    maintenance_disabled_at: &mut Option<Instant>,
    now: Instant,
) -> bool {
    *maintenance_consecutive_failures = maintenance_consecutive_failures.saturating_add(1);
    if *maintenance_consecutive_failures >= MAINTENANCE_CIRCUIT_OPEN_THRESHOLD
        && maintenance_disabled_at.is_none()
    {
        *maintenance_disabled_at = Some(now);
        return true;
    }
    false
}

fn maybe_close_maintenance_circuit(
    maintenance_consecutive_failures: &mut usize,
    maintenance_disabled_at: &mut Option<Instant>,
    now: Instant,
) -> bool {
    let Some(disabled_at) = *maintenance_disabled_at else {
        return false;
    };
    if now.duration_since(disabled_at) < Duration::from_secs(MAINTENANCE_CIRCUIT_OPEN_SECS) {
        return false;
    }

    *maintenance_consecutive_failures = 0;
    *maintenance_disabled_at = None;
    true
}

fn maintenance_task_timeout(maintenance_interval_secs: u64) -> Duration {
    let interval_secs = maintenance_interval_secs.max(1);
    let derived_secs = interval_secs.saturating_mul(MAINTENANCE_TASK_TIMEOUT_MULTIPLIER);
    let bounded_secs = derived_secs.clamp(
        MAINTENANCE_TASK_TIMEOUT_MIN_SECS,
        MAINTENANCE_TASK_TIMEOUT_MAX_SECS,
    );
    Duration::from_secs(bounded_secs)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum MaintenanceTimeoutAction {
    None,
    RequestCancel,
    ForceAbort,
}

fn maintenance_timeout_action(
    now: Instant,
    started_at: Instant,
    timeout: Duration,
    cancel_requested_at: Option<Instant>,
    forced_abort_issued: bool,
) -> MaintenanceTimeoutAction {
    if now.duration_since(started_at) < timeout {
        return MaintenanceTimeoutAction::None;
    }

    if cancel_requested_at.is_none() {
        return MaintenanceTimeoutAction::RequestCancel;
    }

    if forced_abort_issued {
        return MaintenanceTimeoutAction::None;
    }

    if now.duration_since(cancel_requested_at.unwrap())
        >= Duration::from_secs(MAINTENANCE_TASK_CANCEL_GRACE_SECS)
    {
        return MaintenanceTimeoutAction::ForceAbort;
    }

    MaintenanceTimeoutAction::None
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
    status.refresh_age_secs = refresh_age_secs(status.last_refresh_unix_ms);
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

fn maybe_spawn_synthesis_task(
    task: &mut Option<tokio::task::JoinHandle<anyhow::Result<bool>>>,
    backoff: &SynthesisTaskBackoff,
    task_name: &'static str,
    now: Instant,
    spawn: impl FnOnce() -> tokio::task::JoinHandle<anyhow::Result<bool>>,
) -> bool {
    if task.is_some() {
        return false;
    }

    if !backoff.can_spawn(now) {
        tracing::debug!(
            task = task_name,
            failure_count = backoff.failure_count,
            "cortex synthesis task scheduling skipped during backoff"
        );
        return false;
    }

    *task = Some(spawn());
    true
}

fn spawn_intraday_synthesis_task(
    deps: AgentDeps,
    logger: CortexLogger,
) -> tokio::task::JoinHandle<anyhow::Result<bool>> {
    tokio::spawn(async move { maybe_synthesize_intraday_batch(&deps, &logger).await })
}

fn spawn_daily_synthesis_task(
    deps: AgentDeps,
    logger: CortexLogger,
) -> tokio::task::JoinHandle<anyhow::Result<bool>> {
    tokio::spawn(async move { maybe_synthesize_daily_summary(&deps, &logger).await })
}

const SYNTHESIS_TASK_BACKOFF_INITIAL_SECS: u64 = 30;
const SYNTHESIS_TASK_BACKOFF_MAX_SECS: u64 = 5 * 60;

#[derive(Debug, Clone)]
struct SynthesisTaskBackoff {
    failure_count: u32,
    next_allowed_instant: Instant,
}

impl SynthesisTaskBackoff {
    fn new(now: Instant) -> Self {
        Self {
            failure_count: 0,
            next_allowed_instant: now,
        }
    }

    fn can_spawn(&self, now: Instant) -> bool {
        now >= self.next_allowed_instant
    }

    fn record_success(&mut self, now: Instant) {
        self.failure_count = 0;
        self.next_allowed_instant = now;
    }

    fn record_failure(&mut self, now: Instant) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.next_allowed_instant = now + synthesis_task_backoff_delay(self.failure_count);
    }
}

fn synthesis_task_backoff_delay(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(10);
    let multiplier = 1_u64 << exponent;
    let seconds = SYNTHESIS_TASK_BACKOFF_INITIAL_SECS
        .saturating_mul(multiplier)
        .min(SYNTHESIS_TASK_BACKOFF_MAX_SECS);

    Duration::from_secs(seconds)
}

async fn collect_synthesis_task(
    task: &mut Option<tokio::task::JoinHandle<anyhow::Result<bool>>>,
    task_name: &'static str,
    backoff: &mut SynthesisTaskBackoff,
    now: Instant,
) {
    let Some(handle) = task.as_ref() else {
        return;
    };

    if !handle.is_finished() {
        return;
    }

    let Some(handle) = task.take() else {
        return;
    };

    match handle.await {
        Ok(Ok(true)) => {
            backoff.record_success(now);
            tracing::debug!(task = task_name, "cortex synthesis task completed");
        }
        Ok(Ok(false)) => {
            backoff.record_success(now);
            tracing::trace!(task = task_name, "cortex synthesis task skipped");
        }
        Ok(Err(error)) => {
            backoff.record_failure(now);
            tracing::warn!(
                %error,
                task = task_name,
                failure_count = backoff.failure_count,
                "cortex synthesis task failed"
            );
        }
        Err(error) if error.is_cancelled() => {
            backoff.record_failure(now);
            tracing::debug!(
                %error,
                task = task_name,
                failure_count = backoff.failure_count,
                "cortex synthesis task cancelled"
            );
        }
        Err(error) if error.is_panic() => {
            backoff.record_failure(now);
            tracing::warn!(
                %error,
                task = task_name,
                failure_count = backoff.failure_count,
                "cortex synthesis task panicked"
            );
        }
        Err(error) => {
            backoff.record_failure(now);
            tracing::warn!(
                %error,
                task = task_name,
                failure_count = backoff.failure_count,
                "cortex synthesis task failed"
            );
        }
    }
}

#[derive(Debug, Clone, Default)]
struct BreakerState {
    failure_count: u32,
    tripped: bool,
}

#[derive(Debug, Clone)]
struct BreakerTripEvent {
    key: String,
    failure_count: u32,
}

#[derive(Debug, Default)]
struct HealthRuntimeState {
    breaker_state: HashMap<String, BreakerState>,
    pending_breaker_trip_events: Vec<BreakerTripEvent>,
}

impl HealthRuntimeState {
    fn track_tool_completed(&mut self, tool_name: &str, result: &str, threshold: u8) {
        let Some(structured_success) = parse_structured_success_flag(result) else {
            return;
        };

        self.update_breaker(
            format!("tool:{tool_name}"),
            !structured_success,
            threshold.max(1),
        );
    }

    fn update_breaker(&mut self, key: String, failure: bool, threshold: u8) {
        let state = self.breaker_state.entry(key.clone()).or_default();
        if failure {
            state.failure_count = state.failure_count.saturating_add(1);
            if !state.tripped && state.failure_count >= threshold as u32 {
                state.tripped = true;
                self.pending_breaker_trip_events.push(BreakerTripEvent {
                    key,
                    failure_count: state.failure_count,
                });
            }
            return;
        }

        state.failure_count = 0;
        state.tripped = false;
    }
}

fn parse_structured_success_flag(result: &str) -> Option<bool> {
    let trimmed = result.trim();
    if !trimmed.starts_with('{') || trimmed.len() > 16_384 {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let object = value.as_object()?;
    if let Some(success) = object.get("success").and_then(|value| value.as_bool()) {
        return Some(success);
    }
    object.get("ok").and_then(|value| value.as_bool())
}

/// The cortex observes system-wide activity and writes to the memory store.
pub struct Cortex {
    pub deps: AgentDeps,
    pub hook: CortexHook,
    /// Recent activity signals (rolling window).
    pub signal_buffer: Arc<RwLock<VecDeque<Signal>>>,
    /// Runtime supervision state for timeout enforcement and breaker signals.
    health_runtime_state: Arc<RwLock<HealthRuntimeState>>,
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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
    /// Optional notification store for emitting dashboard inbox entries.
    notification_store: Option<std::sync::Arc<crate::notifications::NotificationStore>>,
    /// Agent id, recorded in notifications for filtering.
    agent_id: Option<String>,
}

// TODO: re-enable once notifications have proper action_url
// const NOTIFY_EVENT_TYPES: &[&str] = &["circuit_breaker_tripped", "worker_killed"];

impl CortexLogger {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            notification_store: None,
            agent_id: None,
        }
    }

    /// Attach a notification store so high-signal events surface in the inbox.
    pub fn with_notifications(
        mut self,
        store: std::sync::Arc<crate::notifications::NotificationStore>,
        agent_id: String,
    ) -> Self {
        self.notification_store = Some(store);
        self.agent_id = Some(agent_id);
        self
    }

    /// Log a cortex action. Fire-and-forget.
    pub fn log(&self, event_type: &str, summary: &str, details: Option<serde_json::Value>) {
        let pool = self.pool.clone();
        let id = uuid::Uuid::new_v4().to_string();

        // TODO: re-enable once action_url points somewhere useful
        // let should_notify =
        //     self.notification_store.is_some() && NOTIFY_EVENT_TYPES.contains(&event_type);
        // let notif_data = should_notify.then(|| {
        //     (
        //         self.notification_store.clone().unwrap(),
        //         self.agent_id.clone(),
        //         summary.to_string(),
        //         details.clone(),
        //         id.clone(),
        //     )
        // });

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

        // TODO: re-enable once action_url points somewhere useful
        // if let Some((store, agent_id, title, details, entity_id)) = notif_data {
        //     tokio::spawn(async move {
        //         let n = crate::notifications::NewNotification {
        //             kind: crate::notifications::NotificationKind::CortexObservation,
        //             severity: crate::notifications::NotificationSeverity::Warn,
        //             title,
        //             body: details.as_ref().map(|d| d.to_string()),
        //             agent_id,
        //             related_entity_type: Some("cortex_event".to_string()),
        //             related_entity_id: Some(entity_id),
        //             action_url: None,
        //             metadata: None,
        //         };
        //         if let Err(error) = store.insert(n).await {
        //             tracing::warn!(%error, "failed to insert cortex observation notification");
        //         }
        //     });
        // }
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
            health_runtime_state: Arc::new(RwLock::new(HealthRuntimeState::default())),
            system_prompt: system_prompt.into(),
        }
    }

    /// Process a process event and extract signals.
    pub async fn observe(&self, event: ProcessEvent) {
        self.observe_health_event(&event).await;

        let Some(signal) = signal_from_event(event) else {
            return;
        };
        let buffer_len = {
            let mut buffer = self.signal_buffer.write().await;
            push_signal_into_buffer(&mut buffer, signal);
            buffer.len()
        };

        tracing::trace!(buffer_len, "cortex received signal");
    }

    async fn observe_health_event(&self, event: &ProcessEvent) {
        let threshold = self
            .deps
            .runtime_config
            .cortex
            .load()
            .circuit_breaker_threshold;
        let mut state = self.health_runtime_state.write().await;

        if let ProcessEvent::ToolCompleted {
            tool_name, result, ..
        } = event
        {
            state.track_tool_completed(tool_name, result, threshold);
        }
    }

    /// Run one health tick and emit pending circuit-breaker observations.
    pub async fn run_health_tick(&self, logger: &CortexLogger) -> Result<()> {
        let cortex_config = **self.deps.runtime_config.cortex.load();
        let pruned_dead_channels = self
            .deps
            .process_control_registry
            .prune_dead_channels()
            .await;

        let pending_breaker_trips = {
            let mut state = self.health_runtime_state.write().await;
            std::mem::take(&mut state.pending_breaker_trip_events)
        };

        for trip in pending_breaker_trips {
            logger.log(
                "circuit_breaker_tripped",
                &format!("Circuit breaker tripped for {}", trip.key),
                Some(serde_json::json!({
                    "key": trip.key,
                    "failure_count": trip.failure_count,
                    "threshold": cortex_config.circuit_breaker_threshold,
                    "action_taken": "observe_only",
                })),
            );
        }

        if pruned_dead_channels > 0 {
            logger.log(
                "health_check",
                &format!(
                    "Cortex health check pruned {} dead channels",
                    pruned_dead_channels
                ),
                Some(serde_json::json!({
                    "pruned_dead_channels": pruned_dead_channels,
                })),
            );
        }

        Ok(())
    }
}

fn summarize_signal_text(value: &str) -> String {
    crate::summarize_first_non_empty_line(value, crate::EVENT_SUMMARY_MAX_CHARS)
}

fn signal_from_event(event: ProcessEvent) -> Option<Signal> {
    Some(match event {
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
        ProcessEvent::WorkerIdle {
            worker_id,
            channel_id,
            ..
        } => Signal::WorkerStatus {
            worker_id,
            channel_id,
            status: "idle".to_string(),
        },
        // UI-only events — no cortex signal needed. Chronicle checkpoints are
        // durable and reachable through the timeline and the chronicle tool,
        // so they do not also need a slot in the signal buffer.
        ProcessEvent::ChannelSystemMessage { .. }
        | ProcessEvent::ChannelAssistantMessage { .. }
        | ProcessEvent::CompactionStarted { .. }
        | ProcessEvent::CompactionCompleted { .. }
        | ProcessEvent::ChronicleCheckpoint { .. }
        | ProcessEvent::OpenCodeSessionCreated { .. }
        | ProcessEvent::OpenCodePartUpdated { .. }
        | ProcessEvent::WorkerOperationResult { .. }
        | ProcessEvent::ProcessText { .. }
        | ProcessEvent::CortexChatUpdate { .. }
        | ProcessEvent::SettingsUpdated { .. }
        | ProcessEvent::ToolOutput { .. }
        | ProcessEvent::ReasoningDelta { .. } => return None,
    })
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
/// Profile refresh happens inside this tick loop.
pub fn spawn_cortex_loop(deps: AgentDeps, logger: CortexLogger) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let prompt_engine = deps.runtime_config.prompts.load();
        let routing = deps.runtime_config.routing.load();
        let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
        let tool_use_enforcement = deps.runtime_config.tool_use_enforcement.load();
        let system_prompt = match prompt_engine.render_static("cortex") {
            Ok(prompt) => match prompt_engine.maybe_append_tool_use_enforcement(
                prompt.clone(),
                tool_use_enforcement.as_ref(),
                &model_name,
            ) {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::warn!(%error, "failed to append tool-use enforcement, using base cortex prompt");
                    prompt
                }
            },
            Err(error) => {
                tracing::warn!(%error, "failed to render cortex prompt, using empty preamble");
                String::new()
            }
        };
        drop(prompt_engine);

        let cortex = Cortex::new(deps.clone(), system_prompt);
        let mut event_rx = deps.event_tx.subscribe();
        let mut memory_event_rx = deps.memory_event_tx.subscribe();
        let mut tool_output_rx = deps.tool_output_tx.subscribe();
        if let Err(error) = run_cortex_loop(
            &cortex,
            &logger,
            &mut event_rx,
            &mut memory_event_rx,
            &mut tool_output_rx,
        )
        .await
        {
            tracing::error!(%error, "cortex loop exited with error");
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
                    status.refresh_age_secs = refresh_age_secs(status.last_refresh_unix_ms);
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
            status.refresh_age_secs = refresh_age_secs(status.last_refresh_unix_ms);
        });
        return;
    }

    update_warmup_status(deps, |status| {
        status.state = crate::config::WarmupState::Warming;
        status.last_error = None;
        status.refresh_age_secs = refresh_age_secs(status.last_refresh_unix_ms);
    });
    let mut terminal_state_guard = WarmupRunGuard::new(deps, reason, force);

    let mut errors = Vec::new();
    let mut embedding_ready = false;

    // Warmup is LLM-free: it loads the local embedding model so first-recall
    // latency lands here instead of on a user turn. Knowledge reaches the
    // channel prompt through the deterministic store render.
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

    let now_ms = chrono::Utc::now().timestamp_millis();
    if errors.is_empty() {
        update_warmup_status(deps, |status| {
            status.state = crate::config::WarmupState::Warm;
            status.embedding_ready = embedding_ready || status.embedding_ready;
            status.last_refresh_unix_ms = Some(now_ms);
            status.last_error = None;
            status.refresh_age_secs = Some(0);
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
            status.refresh_age_secs = refresh_age_secs(status.last_refresh_unix_ms);
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

async fn run_cortex_loop(
    cortex: &Cortex,
    logger: &CortexLogger,
    event_rx: &mut broadcast::Receiver<ProcessEvent>,
    memory_event_rx: &mut broadcast::Receiver<ProcessEvent>,
    tool_output_rx: &mut broadcast::Receiver<ProcessEvent>,
) -> anyhow::Result<()> {
    tracing::info!("cortex loop started");

    const LAG_WARNING_INTERVAL_SECS: u64 = 30;

    // Generate an initial profile at startup.
    generate_profile(&cortex.deps, logger).await;
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
    let mut lagged_since_last_warning_tool_output: u64 = 0;
    let mut last_lag_warning_tool_output: Option<Instant> = None;
    let mut memory_event_stream_open = true;
    let mut tool_output_stream_open = true;
    let mut maintenance_task: Option<
        tokio::task::JoinHandle<crate::error::Result<memory_maintenance::MaintenanceReport>>,
    > = None;
    let mut maintenance_task_started_at: Option<Instant> = None;
    let mut maintenance_task_cancel_tx: Option<tokio::sync::watch::Sender<bool>> = None;
    let mut maintenance_task_cancel_requested_at: Option<Instant> = None;
    let mut maintenance_task_forced_abort_issued = false;
    let mut maintenance_consecutive_failures: usize = 0;
    let mut maintenance_disabled_at: Option<Instant> = None;
    let mut last_maintenance = Instant::now();
    let mut intraday_synthesis_task: Option<tokio::task::JoinHandle<anyhow::Result<bool>>> = None;
    let mut daily_synthesis_task: Option<tokio::task::JoinHandle<anyhow::Result<bool>>> = None;
    let mut intraday_synthesis_backoff = SynthesisTaskBackoff::new(Instant::now());
    let mut daily_synthesis_backoff = SynthesisTaskBackoff::new(Instant::now());

    loop {
        tokio::select! {
            biased;
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
                        if let Some(task) = intraday_synthesis_task.take() {
                            task.abort();
                        }
                        if let Some(task) = daily_synthesis_task.take() {
                            task.abort();
                        }
                        if let Some(task) = maintenance_task.take() {
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
                        if let Some(task) = intraday_synthesis_task.take() {
                            task.abort();
                        }
                        if let Some(task) = daily_synthesis_task.take() {
                            task.abort();
                        }
                        if let Some(task) = maintenance_task.take() {
                            task.abort();
                        }
                        return Ok(());
                    }
                    CortexReceiverOutcome::DisableStream => {
                        memory_event_stream_open = false;
                    }
                }
            },
            event = tool_output_rx.recv(), if tool_output_stream_open => {
                match handle_cortex_receiver_result(
                    event,
                    "tool_output",
                    ReceiverClosedBehavior::DisableStream,
                    &mut lagged_since_last_warning_tool_output,
                    &mut last_lag_warning_tool_output,
                    LAG_WARNING_INTERVAL_SECS,
                ) {
                    CortexReceiverOutcome::Observe(event) => cortex.observe(event).await,
                    CortexReceiverOutcome::Lagged { dropped } => {
                        #[cfg(feature = "metrics")]
                        crate::telemetry::Metrics::global()
                            .event_receiver_lagged_events_total
                            .with_label_values(&[&*cortex.deps.agent_id, "cortex_tool_output"])
                            .inc_by(dropped);
                        #[cfg(not(feature = "metrics"))]
                        let _ = dropped;
                    }
                    CortexReceiverOutcome::StopLoop => unreachable!("tool output stream cannot stop cortex loop"),
                    CortexReceiverOutcome::DisableStream => {
                        tool_output_stream_open = false;
                    }
                }
            },
            _ = tick_timer.tick() => {
                if let Err(error) = cortex.run_health_tick(logger).await {
                    tracing::warn!(%error, "cortex health tick failed");
                }

                let cortex_config = **cortex.deps.runtime_config.cortex.load();
                let now = Instant::now();

                collect_synthesis_task(
                    &mut intraday_synthesis_task,
                    "intraday",
                    &mut intraday_synthesis_backoff,
                    now,
                )
                .await;
                collect_synthesis_task(
                    &mut daily_synthesis_task,
                    "daily",
                    &mut daily_synthesis_backoff,
                    now,
                )
                .await;

                if maintenance_task
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                    && let Some(task) = maintenance_task.take()
                {
                    maintenance_task_started_at = None;
                    maintenance_task_cancel_tx = None;
                    maintenance_task_cancel_requested_at = None;
                    maintenance_task_forced_abort_issued = false;
                    match task.await {
                        Ok(Ok(report)) => {
                            if maintenance_consecutive_failures > 0 || maintenance_disabled_at.is_some()
                            {
                                tracing::info!(
                                    previous_failures = maintenance_consecutive_failures,
                                    "cortex maintenance circuit reset after successful run"
                                );
                            }
                            maintenance_consecutive_failures = 0;
                            maintenance_disabled_at = None;
                            logger.log(
                                "maintenance_completed",
                                "Memory maintenance completed",
                                Some(serde_json::json!({
                                    "decayed": report.decayed,
                                    "pruned": report.pruned,
                                    "merged": report.merged,
                                })),
                            );
                        }
                        Ok(Err(error)) => {
                            let now = Instant::now();
                            let circuit_opened = record_maintenance_failure(
                                &mut maintenance_consecutive_failures,
                                &mut maintenance_disabled_at,
                                now,
                            );
                            tracing::warn!(%error, "cortex maintenance failed");
                            if circuit_opened {
                                tracing::warn!(
                                    failures = maintenance_consecutive_failures,
                                    cooldown_secs = MAINTENANCE_CIRCUIT_OPEN_SECS,
                                    "cortex maintenance circuit opened after consecutive failures"
                                );
                            }
                        }
                        Err(error) => {
                            let now = Instant::now();
                            let circuit_opened = record_maintenance_failure(
                                &mut maintenance_consecutive_failures,
                                &mut maintenance_disabled_at,
                                now,
                            );
                            if error.is_cancelled() {
                                tracing::warn!(
                                    %error,
                                    "cortex maintenance task was cancelled before completion"
                                );
                            } else if error.is_panic() {
                                tracing::warn!(%error, "cortex maintenance task panicked");
                            } else {
                                tracing::warn!(%error, "cortex maintenance task failed");
                            }
                            if circuit_opened {
                                tracing::warn!(
                                    failures = maintenance_consecutive_failures,
                                    cooldown_secs = MAINTENANCE_CIRCUIT_OPEN_SECS,
                                    "cortex maintenance circuit opened after task failures"
                                );
                            }
                        }
                    }
                }

                if let Some(started_at) = maintenance_task_started_at {
                    let timeout = maintenance_task_timeout(cortex_config.maintenance_interval_secs);
                    let action = maintenance_timeout_action(
                        now,
                        started_at,
                        timeout,
                        maintenance_task_cancel_requested_at,
                        maintenance_task_forced_abort_issued,
                    );
                    match action {
                        MaintenanceTimeoutAction::None => {}
                        MaintenanceTimeoutAction::RequestCancel => {
                            if let Some(cancel_tx) = maintenance_task_cancel_tx.as_ref() {
                                cancel_tx.send(true).ok();
                            }
                            maintenance_task_cancel_requested_at = Some(now);
                            tracing::warn!(
                                elapsed_secs = started_at.elapsed().as_secs(),
                                timeout_secs = timeout.as_secs(),
                                "cortex maintenance task timed out; requesting graceful cancel"
                            );
                            logger.log(
                                "maintenance_timeout",
                                "Memory maintenance timeout requested",
                                Some(serde_json::json!({
                                    "elapsed_secs": started_at.elapsed().as_secs(),
                                    "timeout_secs": timeout.as_secs(),
                                    "maintenance_interval_secs": cortex_config.maintenance_interval_secs,
                                    "graceful_cancel": true,
                                })),
                            );
                        }
                        MaintenanceTimeoutAction::ForceAbort => {
                            if let Some(task) = maintenance_task.as_ref() {
                                task.abort();
                            }
                            maintenance_task_cancel_requested_at = Some(now);
                            maintenance_task_forced_abort_issued = true;
                            tracing::warn!(
                                elapsed_secs = started_at.elapsed().as_secs(),
                                timeout_secs = timeout.as_secs(),
                                grace_secs = MAINTENANCE_TASK_CANCEL_GRACE_SECS,
                                "cortex maintenance task did not stop gracefully; forcing abort"
                            );
                            logger.log(
                                "maintenance_timeout",
                                "Memory maintenance forced abort",
                                Some(serde_json::json!({
                                    "elapsed_secs": started_at.elapsed().as_secs(),
                                    "timeout_secs": timeout.as_secs(),
                                    "maintenance_interval_secs": cortex_config.maintenance_interval_secs,
                                    "forced_abort": true,
                                })),
                            );
                        }
                    }
                }

                let now = Instant::now();
                if maybe_close_maintenance_circuit(
                    &mut maintenance_consecutive_failures,
                    &mut maintenance_disabled_at,
                    now,
                ) {
                    tracing::info!("cortex maintenance circuit closed; retries re-enabled");
                }
                // The channel knowledge-context slot renders the store
                // directly — no change-driven regeneration exists anymore.

                if last_maintenance.elapsed() >= Duration::from_secs(
                    cortex_config.maintenance_interval_secs.max(1),
                ) {
                    if maintenance_task.is_none() && maintenance_disabled_at.is_none() {
                        maintenance_task_started_at = Some(Instant::now());
                        let maintenance_config = memory_maintenance::MaintenanceConfig {
                            prune_threshold: cortex_config.maintenance_prune_threshold,
                            decay_rate: cortex_config.maintenance_decay_rate,
                            min_age_days: cortex_config.maintenance_min_age_days,
                            merge_similarity_threshold: cortex_config
                                .maintenance_merge_similarity_threshold,
                        };
                        let memory_search = cortex.deps.memory_search.clone();
                        logger.log(
                            "maintenance_started",
                            "Memory maintenance started",
                            Some(serde_json::json!({
                                "decay_rate": maintenance_config.decay_rate,
                                "prune_threshold": maintenance_config.prune_threshold,
                                "min_age_days": maintenance_config.min_age_days,
                                "merge_similarity_threshold": maintenance_config.merge_similarity_threshold,
                            })),
                        );
                        let (maintenance_cancel_tx, maintenance_cancel_rx) =
                            tokio::sync::watch::channel(false);
                        maintenance_task = Some(tokio::spawn(async move {
                            memory_maintenance::run_maintenance_with_cancel(
                                memory_search.store(),
                                memory_search.embedding_table(),
                                memory_search.embedding_model_arc(),
                                &maintenance_config,
                                maintenance_cancel_rx,
                            )
                            .await
                        }));
                        maintenance_task_cancel_tx = Some(maintenance_cancel_tx);
                        maintenance_task_cancel_requested_at = None;
                        maintenance_task_forced_abort_issued = false;
                    } else if maintenance_disabled_at.is_some() {
                        tracing::debug!(
                            failures = maintenance_consecutive_failures,
                            cooldown_secs = MAINTENANCE_CIRCUIT_OPEN_SECS,
                            "maintenance scheduling skipped while maintenance circuit is open"
                        );
                    }

                    last_maintenance = Instant::now();
                }

                maybe_spawn_synthesis_task(
                    &mut intraday_synthesis_task,
                    &intraday_synthesis_backoff,
                    "intraday",
                    now,
                    || spawn_intraday_synthesis_task(cortex.deps.clone(), logger.clone()),
                );

                maybe_spawn_synthesis_task(
                    &mut daily_synthesis_task,
                    &daily_synthesis_backoff,
                    "daily",
                    now,
                    || spawn_daily_synthesis_task(cortex.deps.clone(), logger.clone()),
                );

                // Working memory: prune old events (cheap SQL, runs every tick but deletes nothing most of the time).
                let wm_config = **cortex.deps.runtime_config.working_memory.load();
                if let Err(error) = cortex.deps.working_memory.prune_old_events(wm_config.event_retention_days).await {
                    tracing::warn!(%error, "working memory event pruning failed");
                }

                crate::wakes::fire_due_schedule_wakes(&cortex.deps).await;

                // Autonomy: start a run when the interval has elapsed or wake
                // events are pending. The check is a few cheap SQL queries;
                // the run itself is spawned as a task so the tick never blocks.
                crate::agent::autonomy::maybe_run_autonomy(&cortex.deps).await;

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

// -- Intra-Day Synthesis + Daily Summaries --

/// Check and potentially synthesize a batch of recent working memory events.
///
/// Called on every cortex tick. The check is one cheap SQL query. LLM synthesis
/// only happens when the event count threshold or time fallback is reached.
pub async fn maybe_synthesize_intraday_batch(
    deps: &AgentDeps,
    logger: &CortexLogger,
) -> anyhow::Result<bool> {
    let wm = &deps.working_memory;
    let wm_config = **deps.runtime_config.working_memory.load();
    let today = wm.today();

    let last_end = wm.get_last_intraday_synthesis_end(&today).await?;
    let unsynthesized = wm.get_events_after(&today, last_end).await?;

    if unsynthesized.is_empty() {
        return Ok(false);
    }

    // Dual trigger: count-based OR time-based fallback.
    let count_trigger = unsynthesized.len() >= wm_config.intraday_batch_threshold;
    let time_trigger = if let Some(last) = last_end {
        let elapsed = (chrono::Utc::now() - last).num_seconds() as u64;
        elapsed >= wm_config.intraday_time_fallback_secs
    } else {
        // No previous synthesis — use time since first event.
        let first_event = &unsynthesized[0];
        let elapsed = (chrono::Utc::now() - first_event.timestamp).num_seconds() as u64;
        elapsed >= wm_config.intraday_time_fallback_secs
    };

    if !count_trigger && !time_trigger {
        return Ok(false);
    }

    // Build the event text for the LLM.
    let time_start = unsynthesized
        .first()
        .map(|e| e.timestamp)
        .unwrap_or_else(chrono::Utc::now);
    let time_end = unsynthesized
        .last()
        .map(|e| e.timestamp)
        .unwrap_or_else(chrono::Utc::now);
    let timezone = wm.timezone();
    let time_start_str = time_start
        .with_timezone(&timezone)
        .format("%H:%M")
        .to_string();
    let time_end_str = time_end
        .with_timezone(&timezone)
        .format("%H:%M")
        .to_string();

    let mut events_text = String::new();
    for event in &unsynthesized {
        let ts = event
            .timestamp
            .with_timezone(&timezone)
            .format("%H:%M")
            .to_string();
        let channel_label = event
            .channel_id
            .as_deref()
            .map(|c| format!(" [{c}]"))
            .unwrap_or_default();
        events_text.push_str(&format!(
            "[{ts}]{channel_label} {}: {}\n",
            event.event_type, event.summary
        ));
    }

    // Render the synthesis prompt.
    let prompt_engine = deps.runtime_config.prompts.load();
    let prompt = prompt_engine.render_intraday_synthesis(
        unsynthesized.len(),
        &time_start_str,
        &time_end_str,
        &events_text,
    )?;

    // Use a short one-shot LLM call — no tools, no hooks.
    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
    let usage_accumulator = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::llm::usage::UsageAccumulator::new(),
    ));
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone())
        .with_accumulator(usage_accumulator.clone())
        .with_debug(
            deps.prompt_records(),
            crate::llm::record::DebugContext {
                process: Some(crate::llm::record::ProcessRef {
                    kind: "cortex".to_string(),
                    id: None,
                    process_type: Some("intraday_synthesis".to_string()),
                    channel_id: None,
                }),
                trigger: Some(crate::llm::record::Trigger {
                    kind: "intraday_synthesis".to_string(),
                    ..Default::default()
                }),
                blocks: Vec::new(),
            },
        );

    let agent = AgentBuilder::new(model)
        .preamble("You are a concise narrative summarizer. Output only the summary paragraph, nothing else.")
        .hook(CortexHook::new())
        .build();

    let synthesis = agent.prompt(&prompt).await;
    let acc = usage_accumulator.lock().await;
    if let Err(e) = acc
        .flush(&deps.sqlite_pool, &deps.agent_id, "cortex", None)
        .await
    {
        tracing::warn!(error = %e, "failed to flush cortex token usage");
    }
    drop(acc);
    let synthesis = synthesis?;

    // Store the synthesis.
    wm.save_intraday_synthesis(
        &today,
        time_start,
        time_end,
        &synthesis,
        unsynthesized.len(),
    )
    .await?;

    tracing::info!(
        event_count = unsynthesized.len(),
        time_range = format!("{time_start_str}-{time_end_str}"),
        words = synthesis.split_whitespace().count(),
        trigger = if count_trigger {
            "count"
        } else {
            "time_fallback"
        },
        "intra-day synthesis completed"
    );

    logger.log(
        "intraday_synthesis",
        &format!(
            "Synthesized {} events ({time_start_str}-{time_end_str})",
            unsynthesized.len()
        ),
        Some(serde_json::json!({
            "event_count": unsynthesized.len(),
            "trigger": if count_trigger { "count" } else { "time_fallback" },
            "words": synthesis.split_whitespace().count(),
        })),
    );

    Ok(true)
}

/// Check and potentially synthesize yesterday's daily summary.
///
/// Called on every cortex tick. Idempotent — once a daily summary exists for
/// a given day, it is never regenerated. Uses intra-day synthesis paragraphs
/// (not raw events) as input, so the LLM call is small and cheap.
pub async fn maybe_synthesize_daily_summary(
    deps: &AgentDeps,
    logger: &CortexLogger,
) -> anyhow::Result<bool> {
    let wm = &deps.working_memory;
    let yesterday = wm.yesterday();

    // Idempotent check.
    if wm.has_daily_summary(&yesterday).await? {
        return Ok(false);
    }

    let intraday = wm.get_intraday_syntheses(&yesterday).await?;
    let raw_events = wm.get_events_for_day(&yesterday).await?;

    // No activity at all — save a minimal summary.
    if intraday.is_empty() && raw_events.is_empty() {
        wm.save_daily_summary(&yesterday, "No activity.", 0).await?;
        return Ok(true);
    }

    // Build input from intra-day synthesis paragraphs + any unsynthesized tail.
    let timezone = wm.timezone();
    let mut blocks_text = String::new();
    let mut total_events = 0i64;

    // Last timestamp covered by intra-day syntheses (if any).
    let mut last_synthesis_end = None;

    for synthesis in &intraday {
        let time_label = synthesis
            .time_range_start
            .with_timezone(&timezone)
            .format("%H:%M")
            .to_string();
        blocks_text.push_str(&format!("[{time_label}] {}\n\n", synthesis.summary));
        total_events += synthesis.event_count;
        let end = synthesis.time_range_end;
        last_synthesis_end = Some(
            last_synthesis_end.map_or(end, |prev: chrono::DateTime<chrono::Utc>| prev.max(end)),
        );
    }

    // Collect raw events not covered by any intra-day synthesis (the "tail").
    // This happens when events didn't hit the count/time trigger before midnight.
    let tail_events: Vec<_> = raw_events
        .iter()
        .filter(|event| match last_synthesis_end {
            Some(end) => event.timestamp > end,
            None => true, // No syntheses at all — all events are unsynthesized.
        })
        .collect();

    if !tail_events.is_empty() {
        if !blocks_text.is_empty() {
            blocks_text.push_str("Unsynthesized events from the rest of the day:\n");
        }
        for event in &tail_events {
            let ts = event
                .timestamp
                .with_timezone(&timezone)
                .format("%H:%M")
                .to_string();
            let channel_label = event
                .channel_id
                .as_deref()
                .map(|c| format!(" [{c}]"))
                .unwrap_or_default();
            blocks_text.push_str(&format!(
                "[{ts}]{channel_label} {}: {}\n",
                event.event_type, event.summary
            ));
        }
        blocks_text.push('\n');
        total_events += tail_events.len() as i64;
    }

    let wm_config = **deps.runtime_config.working_memory.load();
    let prompt_engine = deps.runtime_config.prompts.load();
    let prompt = prompt_engine.render_daily_summary(
        &yesterday,
        wm_config.daily_summary_max_words,
        &blocks_text,
    )?;

    // One-shot LLM call.
    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
    let usage_accumulator = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::llm::usage::UsageAccumulator::new(),
    ));
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone())
        .with_accumulator(usage_accumulator.clone())
        .with_debug(
            deps.prompt_records(),
            crate::llm::record::DebugContext {
                process: Some(crate::llm::record::ProcessRef {
                    kind: "cortex".to_string(),
                    id: None,
                    process_type: Some("daily_summary".to_string()),
                    channel_id: None,
                }),
                trigger: Some(crate::llm::record::Trigger {
                    kind: "daily_summary".to_string(),
                    ..Default::default()
                }),
                blocks: Vec::new(),
            },
        );

    let agent = AgentBuilder::new(model)
        .preamble("You are a daily activity summarizer. Output only the summary, nothing else.")
        .hook(CortexHook::new())
        .build();

    let summary = agent.prompt(&prompt).await;
    let acc = usage_accumulator.lock().await;
    if let Err(e) = acc
        .flush(&deps.sqlite_pool, &deps.agent_id, "cortex", None)
        .await
    {
        tracing::warn!(error = %e, "failed to flush cortex token usage");
    }
    drop(acc);
    let summary = summary?;

    wm.save_daily_summary(&yesterday, &summary, total_events)
        .await?;

    let tail_count = tail_events.len();

    tracing::info!(
        day = yesterday,
        intraday_blocks = intraday.len(),
        tail_events = tail_count,
        total_events,
        words = summary.split_whitespace().count(),
        "daily summary generated"
    );

    logger.log(
        "daily_summary",
        &format!(
            "Daily summary for {yesterday}: {total_events} events, {} blocks, {tail_count} tail",
            intraday.len()
        ),
        Some(serde_json::json!({
            "day": yesterday,
            "intraday_blocks": intraday.len(),
            "tail_events": tail_count,
            "total_events": total_events,
            "words": summary.split_whitespace().count(),
        })),
    );

    Ok(true)
}

// -- Agent Profile --

/// Persisted agent profile generated by the cortex.
#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
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
/// Uses the identity files as context, then asks an LLM to produce a display
/// name, status line, and short bio.
#[tracing::instrument(skip(deps, logger), fields(agent_id = %deps.agent_id))]
async fn generate_profile(deps: &AgentDeps, logger: &CortexLogger) {
    tracing::info!("cortex generating agent profile");
    let started = Instant::now();

    let prompt_engine = deps.runtime_config.prompts.load();
    let profile_prompt = match prompt_engine.render_static_segmented("cortex_profile") {
        Ok(p) => p,
        Err(error) => {
            tracing::warn!(%error, "failed to render cortex_profile prompt");
            return;
        }
    };

    let identity_context = {
        let rendered = deps.runtime_config.identity.load().render();
        if rendered.is_empty() {
            None
        } else {
            Some(rendered)
        }
    };

    let synthesis_prompt =
        match prompt_engine.render_system_profile_synthesis(identity_context.as_deref()) {
            Ok(p) => p,
            Err(error) => {
                tracing::warn!(%error, "failed to render profile synthesis prompt");
                return;
            }
        };

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
    let usage_accumulator = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::llm::usage::UsageAccumulator::new(),
    ));
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "cortex")
        .with_routing((**routing).clone())
        .with_accumulator(usage_accumulator.clone())
        .with_debug(
            deps.prompt_records(),
            crate::llm::record::DebugContext {
                process: Some(crate::llm::record::ProcessRef {
                    kind: "cortex".to_string(),
                    id: None,
                    process_type: Some("profile".to_string()),
                    channel_id: None,
                }),
                trigger: Some(crate::llm::record::Trigger {
                    kind: "profile".to_string(),
                    ..Default::default()
                }),
                blocks: profile_prompt.blocks.clone(),
            },
        );

    let agent = AgentBuilder::new(model)
        .preamble(&profile_prompt.text)
        .hook(CortexHook::new())
        .build();

    let result = agent
        .prompt_typed::<ProfileLlmResponse>(&synthesis_prompt)
        .await;
    let acc = usage_accumulator.lock().await;
    if let Err(e) = acc
        .flush(&deps.sqlite_pool, &deps.agent_id, "cortex", None)
        .await
    {
        tracing::warn!(error = %e, "failed to flush cortex token usage");
    }
    drop(acc);

    match result {
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

async fn run_association_loop(deps: &AgentDeps, logger: &CortexLogger) -> anyhow::Result<()> {
    tracing::info!("cortex association loop started");

    // Short delay on startup to let warmup and embeddings settle
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
        CortexReceiverOutcome, HealthRuntimeState, MAINTENANCE_TASK_CANCEL_GRACE_SECS,
        MaintenanceTimeoutAction, ReceiverClosedBehavior, Signal, SynthesisTaskBackoff,
        apply_cancelled_warmup_status, collect_synthesis_task, handle_cortex_receiver_result,
        has_completed_initial_warmup, maintenance_task_timeout, maintenance_timeout_action,
        maybe_spawn_synthesis_task, parse_structured_success_flag, push_signal_into_buffer,
        should_execute_warmup, signal_from_event, summarize_signal_text,
    };
    use crate::ProcessEvent;
    use crate::memory::MemoryType;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

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

    #[tokio::test]
    async fn working_memory_synthesis_task_is_single_flight() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
        let mut task: Option<tokio::task::JoinHandle<anyhow::Result<bool>>> = None;
        let now = Instant::now();
        let backoff = SynthesisTaskBackoff::new(now);

        let calls_for_first = Arc::clone(&calls);
        let release_rx_for_first = Arc::clone(&release_rx);
        assert!(maybe_spawn_synthesis_task(
            &mut task,
            &backoff,
            "intraday",
            now,
            move || {
                tokio::spawn(async move {
                    calls_for_first.fetch_add(1, Ordering::SeqCst);
                    let receiver = release_rx_for_first
                        .lock()
                        .await
                        .take()
                        .expect("release receiver should exist");
                    receiver.await.expect("release oneshot dropped");
                    Ok(true)
                })
            }
        ));

        let calls_for_second = Arc::clone(&calls);
        assert!(!maybe_spawn_synthesis_task(
            &mut task,
            &backoff,
            "intraday",
            now,
            move || {
                tokio::spawn(async move {
                    calls_for_second.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                })
            }
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first synthesis task should start");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release_tx.send(()).expect("release should send");
        task.take()
            .expect("task should exist")
            .await
            .expect("task should join")
            .expect("task should succeed");
    }

    #[tokio::test]
    async fn synthesis_task_failure_backs_off_before_respawn() {
        let calls = Arc::new(AtomicUsize::new(0));
        let now = Instant::now();
        let mut backoff = SynthesisTaskBackoff::new(now);
        let mut task: Option<tokio::task::JoinHandle<anyhow::Result<bool>>> = None;

        let calls_for_first = Arc::clone(&calls);
        assert!(maybe_spawn_synthesis_task(
            &mut task,
            &backoff,
            "intraday",
            now,
            move || {
                tokio::spawn(async move {
                    calls_for_first.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow::anyhow!("backend unavailable"))
                })
            }
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            while task.as_ref().is_some_and(|handle| !handle.is_finished()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed synthesis task should finish");
        collect_synthesis_task(&mut task, "intraday", &mut backoff, now).await;

        assert!(task.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(backoff.failure_count, 1);

        let retry_at = backoff.next_allowed_instant;
        let blocked_at = retry_at
            .checked_sub(Duration::from_millis(1))
            .expect("retry instant should be after current instant");
        let calls_for_blocked = Arc::clone(&calls);
        assert!(!maybe_spawn_synthesis_task(
            &mut task,
            &backoff,
            "intraday",
            blocked_at,
            move || {
                tokio::spawn(async move {
                    calls_for_blocked.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                })
            }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls_for_retry = Arc::clone(&calls);
        assert!(maybe_spawn_synthesis_task(
            &mut task,
            &backoff,
            "intraday",
            retry_at,
            move || {
                tokio::spawn(async move {
                    calls_for_retry.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                })
            }
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            while task.as_ref().is_some_and(|handle| !handle.is_finished()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retry synthesis task should finish");
        collect_synthesis_task(&mut task, "intraday", &mut backoff, retry_at).await;

        assert!(task.is_none());
        assert_eq!(backoff.failure_count, 0);
        assert_eq!(backoff.next_allowed_instant, retry_at);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
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

        let signal = signal_from_event(event).expect("MemorySaved should produce a signal");
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
                input: "actual prompt".to_string(),
                profile: "default".to_string(),
                model: "test-model".to_string(),
                max_turns: 10,
                reply_to_message_id: Some("message-1".to_string()),
            },
            ProcessEvent::BranchResult {
                agent_id: agent_id.clone(),
                branch_id,
                channel_id: channel_id.clone(),
                conclusion: "branch done".to_string(),
                status: "done".to_string(),
                transcript: None,
                tool_calls: 0,
            },
            ProcessEvent::WorkerStarted {
                agent_id: agent_id.clone(),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                channel_id: Some(channel_id.clone()),
                task: "do work".to_string(),
                worker_type: "shell".to_string(),
                interactive: false,
                directory: None,
            },
            ProcessEvent::WorkerStatus {
                agent_id: agent_id.clone(),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                channel_id: Some(channel_id.clone()),
                status: "running".to_string(),
            },
            ProcessEvent::WorkerComplete {
                agent_id: agent_id.clone(),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                active_operation: None,
                channel_id: Some(channel_id.clone()),
                result: "ok".to_string(),
                notify: false,
                success: true,
                outcome_kind: crate::conversation::WorkerOutcomeKind::Succeeded,
                outcome_version: 1,
                transcript_version: 0,
                terminal_owner: Some(crate::conversation::WorkerTerminalOwner::Worker),
            },
            ProcessEvent::ToolStarted {
                agent_id: agent_id.clone(),
                process_id: crate::ProcessId::Worker(worker_id),
                worker_registration_id: None,
                channel_id: Some(channel_id.clone()),
                call_id: "shell-call-1".to_string(),
                tool_name: "shell".to_string(),
                args: "echo hi".to_string(),
            },
            ProcessEvent::ToolCompleted {
                agent_id: agent_id.clone(),
                process_id: crate::ProcessId::Worker(worker_id),
                worker_registration_id: None,
                channel_id: Some(channel_id.clone()),
                call_id: "shell-call-1".to_string(),
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
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                interaction_target: crate::agent::process_control::WorkerResultTarget::Channel {
                    channel_id: channel_id.clone(),
                },
                channel_id: Some(channel_id.clone()),
                permission_id: "perm-1".to_string(),
                description: "allow network".to_string(),
                patterns: vec!["https://example.com".to_string()],
            },
            ProcessEvent::WorkerQuestion {
                agent_id: agent_id.clone(),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                interaction_target: crate::agent::process_control::WorkerResultTarget::Channel {
                    channel_id: channel_id.clone(),
                },
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
            ProcessEvent::WorkerIdle {
                agent_id: Arc::from("agent"),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                operation_id: crate::agent::process_control::WorkerOperationId::new(),
                channel_id: Some(channel_id.clone()),
            },
            ProcessEvent::OpenCodeSessionCreated {
                agent_id: Arc::from("agent"),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                channel_id: Some(channel_id.clone()),
                session_id: "session-1".to_string(),
                port: 19898,
            },
            ProcessEvent::OpenCodePartUpdated {
                agent_id: Arc::from("agent"),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                part: crate::opencode::types::OpenCodePart::Text {
                    id: "part-1".to_string(),
                    text: "hello".to_string(),
                },
            },
            ProcessEvent::WorkerOperationResult {
                agent_id: Arc::from("agent"),
                worker_id,
                worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
                operation_id: crate::agent::process_control::WorkerOperationId::new(),
                result_target: crate::agent::process_control::WorkerResultTarget::Channel {
                    channel_id: channel_id.clone(),
                },
                result: "initial result".to_string(),
            },
        ];

        for event in events {
            // Some events (OpenCode UI plumbing) return None — that's fine.
            let _signal: Option<Signal> = signal_from_event(event);
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
    fn parse_structured_success_flag_requires_json_object_bool() {
        assert_eq!(
            parse_structured_success_flag(r#"{"success":false}"#),
            Some(false)
        );
        assert_eq!(parse_structured_success_flag(r#"{"ok":true}"#), Some(true));
        assert_eq!(parse_structured_success_flag("plain text"), None);
        assert_eq!(
            parse_structured_success_flag(r#"{"success":"false"}"#),
            None
        );
    }

    #[test]
    fn maintenance_task_timeout_bounds() {
        assert_eq!(maintenance_task_timeout(1).as_secs(), 300);
        assert_eq!(maintenance_task_timeout(100).as_secs(), 600);
        assert_eq!(maintenance_task_timeout(600).as_secs(), 3_600);
        assert_eq!(maintenance_task_timeout(2_000).as_secs(), 3_600);
        assert_eq!(maintenance_task_timeout(0).as_secs(), 300);
    }

    #[test]
    fn maintenance_timeout_action_progresses_from_none_to_cancel_to_abort() {
        let now = Instant::now();
        let started_at = now - Duration::from_secs(1);
        let timeout = Duration::from_secs(3);
        let grace = Duration::from_secs(MAINTENANCE_TASK_CANCEL_GRACE_SECS);

        assert_eq!(
            maintenance_timeout_action(
                started_at + Duration::from_secs(1),
                started_at,
                timeout,
                None,
                false
            ),
            MaintenanceTimeoutAction::None
        );
        assert_eq!(
            maintenance_timeout_action(started_at + timeout, started_at, timeout, None, false),
            MaintenanceTimeoutAction::RequestCancel
        );
        assert_eq!(
            maintenance_timeout_action(
                started_at + timeout + grace,
                started_at,
                timeout,
                Some(started_at + timeout),
                false
            ),
            MaintenanceTimeoutAction::ForceAbort
        );
        assert_eq!(
            maintenance_timeout_action(
                started_at + timeout + grace + Duration::from_secs(1),
                started_at,
                timeout,
                Some(started_at + timeout),
                true
            ),
            MaintenanceTimeoutAction::None,
        );
    }

    #[test]
    fn breaker_trips_only_for_structured_failures_and_resets_on_success() {
        let mut state = HealthRuntimeState::default();
        state.track_tool_completed("shell", r#"{"success":false}"#, 2);
        assert!(state.pending_breaker_trip_events.is_empty());

        state.track_tool_completed("shell", r#"{"success":false}"#, 2);
        assert_eq!(state.pending_breaker_trip_events.len(), 1);
        assert_eq!(state.pending_breaker_trip_events[0].key, "tool:shell");

        state.track_tool_completed("shell", "command failed", 2);
        assert_eq!(state.pending_breaker_trip_events.len(), 1);

        state.track_tool_completed("shell", r#"{"success":true}"#, 2);
        let breaker = state
            .breaker_state
            .get("tool:shell")
            .expect("breaker state exists");
        assert_eq!(breaker.failure_count, 0);
        assert!(!breaker.tripped);
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
