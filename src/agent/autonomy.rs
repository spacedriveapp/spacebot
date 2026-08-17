//! The autonomy channel: the agent's process for self-directed work.
//!
//! One resident channel receives interval heartbeats and durable wake events,
//! surveys task state, enriches and proposes work according to the configured
//! [`AutonomyLevel`], executes user-approved tasks at level `act`, and records
//! durable run epochs via `autonomy_complete`. See
//! `docs/design-docs/autonomy.md` and `docs/design-docs/wakes.md`.

use crate::agent::channel::{Channel, ChannelKind};
use crate::config::{AutonomyConfig, AutonomyLevel};
use crate::conversation::settings::{DelegationMode, ResolvedConversationSettings};
use crate::prompts::engine::{AutonomyRunHistoryView, AutonomyWakeEventView};
use crate::tasks::{Task, TaskListFilter, TaskStatus};
use crate::wakes::{AutonomyRunStatus, AutonomyRunStore};
use crate::{AgentDeps, InboundMessage, MessageContent, RoutedResponse};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, watch};

/// Conversation id (and channel id) for the autonomy channel. One per agent.
pub const AUTONOMY_CONVERSATION_ID: &str = "autonomy";

/// Retention window for consumed wake events, pruned after each run.
const WAKE_EVENT_RETENTION_DAYS: u32 = 30;

/// Maximum pending wake events pulled into a single run's context.
const WAKE_EVENT_BATCH_LIMIT: i64 = 200;

/// Retry budget for the completion contract — the same budget the
/// memory-persistence contract uses.
pub const AUTONOMY_CONTRACT_MAX_RETRIES: usize =
    crate::hooks::SpacebotHook::MEMORY_PERSISTENCE_CONTRACT_MAX_RETRIES;

/// Fallback summary recorded when a run ends without calling `autonomy_complete`.
pub const AUTONOMY_FALLBACK_SUMMARY: &str = "run ended without summary";

/// Shared state between the run driver, the channel, and the
/// `autonomy_complete` tool for a single autonomy run.
#[derive(Debug, Clone)]
pub struct AutonomyRunHandle {
    pub run_id: String,
    pub generation: u64,
    pub store: Arc<AutonomyRunStore>,
    completed: Arc<AtomicBool>,
    state: Arc<Mutex<AutonomyRunState>>,
    changed: Arc<Notify>,
}

#[derive(Debug, Default)]
struct AutonomyRunState {
    finish_request: Option<AutonomyFinishRequest>,
    active_children: HashSet<AutonomyChild>,
    quiescent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutonomyChild {
    Branch(crate::BranchId),
    WorkerOperation {
        worker_id: crate::WorkerId,
        operation_id: crate::agent::process_control::WorkerOperationId,
    },
}

#[derive(Debug, Clone)]
pub struct AutonomyFinishRequest {
    pub summary: String,
    pub actions: Vec<crate::wakes::AutonomyAction>,
}

impl AutonomyRunHandle {
    pub fn new(run_id: String, generation: u64, store: Arc<AutonomyRunStore>) -> Self {
        Self {
            run_id,
            generation,
            store,
            completed: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(AutonomyRunState::default())),
            changed: Arc::new(Notify::new()),
        }
    }

    /// Record that `autonomy_complete` was called for this run.
    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Release);
    }

    pub fn completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    /// Store the first finish request so duplicate tool calls cannot replace
    /// the summary that will be committed after child workers settle.
    pub fn request_finish(
        &self,
        request: AutonomyFinishRequest,
    ) -> std::result::Result<bool, usize> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finish_request.is_some() {
            return Ok(false);
        }
        if !state.active_children.is_empty() {
            return Err(state.active_children.len());
        }
        state.finish_request = Some(request);
        drop(state);
        self.changed.notify_one();
        Ok(true)
    }

    pub fn finish_requested(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.finish_request.is_some()
    }

    pub fn finish_request(&self) -> Option<AutonomyFinishRequest> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.finish_request.clone()
    }

    /// Register work before it can race the run's finish request.
    pub fn register_child(&self, child: AutonomyChild) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finish_request.is_some() {
            return false;
        }
        state.quiescent = false;
        state.active_children.insert(child)
    }

    pub fn settle_child(&self, child: AutonomyChild) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.active_children.remove(&child) {
            drop(state);
            self.changed.notify_one();
        }
    }

    pub fn has_active_children(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !state.active_children.is_empty()
    }

    pub fn active_children(&self) -> Vec<AutonomyChild> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_children.iter().copied().collect()
    }

    pub fn owns_child(&self, child: AutonomyChild) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_children.contains(&child)
    }

    pub fn mark_quiescent(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.quiescent {
            state.quiescent = true;
            drop(state);
            self.changed.notify_one();
        }
    }

    pub fn is_quiescent(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.quiescent
    }

    pub async fn changed(&self) {
        self.changed.notified().await;
    }
}

#[derive(Debug, Clone, Default)]
pub struct AutonomyRunSlot {
    state: Arc<Mutex<AutonomyRunSlotState>>,
}

#[derive(Debug, Default)]
struct AutonomyRunSlotState {
    generation: u64,
    current: Option<AutonomyRunHandle>,
}

impl AutonomyRunSlot {
    pub fn begin(&self, run_id: String, store: Arc<AutonomyRunStore>) -> AutonomyRunHandle {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation = state.generation.saturating_add(1);
        let handle = AutonomyRunHandle::new(run_id, state.generation, store);
        state.current = Some(handle.clone());
        handle
    }

    pub fn current(&self) -> Option<AutonomyRunHandle> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.current.clone()
    }

    pub fn clear_if_current(&self, generation: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.current.as_ref().map(|run| run.generation) != Some(generation) {
            return false;
        }
        state.current = None;
        true
    }
}

/// Cloneable doorbell installed in every [`AgentDeps`]. The bounded channel
/// coalesces repeated heartbeats while the resident supervisor is busy.
#[derive(Debug, Clone, Default)]
pub struct AutonomyControl {
    check_tx: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    stopped: Arc<AtomicBool>,
    stopped_notify: Arc<Notify>,
    preserve_idle_workers: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    transition: Arc<tokio::sync::Mutex<()>>,
}

impl AutonomyControl {
    fn attach(&self, check_tx: mpsc::Sender<()>, shutdown_tx: watch::Sender<bool>) {
        self.stopped.store(false, Ordering::Release);
        *self
            .check_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(check_tx);
        *self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(shutdown_tx);
    }

    pub fn request_check(&self) {
        let sender = self
            .check_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(sender) = sender {
            match sender.try_send(()) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(())) => {}
                Err(mpsc::error::TrySendError::Closed(())) => {
                    tracing::debug!("autonomy supervisor doorbell is closed");
                }
            }
        }
    }

    pub async fn lock_transition(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.transition.clone().lock_owned().await
    }

    pub fn activate(&self) {
        self.ready.store(true, Ordering::Release);
        self.request_check();
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn request_shutdown(&self, preserve_idle_workers: bool) {
        self.preserve_idle_workers
            .store(preserve_idle_workers, Ordering::Release);
        let sender = self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(sender) = sender {
            sender.send_replace(true);
        }
    }

    pub async fn shutdown_and_wait(&self) {
        self.request_shutdown(false);
        while !self.stopped.load(Ordering::Acquire) {
            let notified = self.stopped_notify.notified();
            if self.stopped.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }

    fn mark_stopped(&self) {
        self.stopped.store(true, Ordering::Release);
        self.stopped_notify.notify_waiters();
    }

    fn should_preserve_idle_workers(&self) -> bool {
        self.preserve_idle_workers.load(Ordering::Acquire)
    }
}

pub struct AutonomySupervisorHandle {
    control: AutonomyControl,
    task: tokio::task::JoinHandle<()>,
    channel_state: crate::agent::channel::ChannelState,
}

impl AutonomySupervisorHandle {
    pub fn channel_state(&self) -> crate::agent::channel::ChannelState {
        self.channel_state.clone()
    }

    pub async fn shutdown(self, preserve_idle_workers: bool) {
        self.control.request_shutdown(preserve_idle_workers);
        if let Err(error) = self.task.await
            && !error.is_cancelled()
        {
            tracing::warn!(%error, "autonomy supervisor failed during shutdown");
        }
    }
}

/// Whether an autonomy run is due right now.
///
/// Pure over its inputs so the decision is unit-testable: a run is due when
/// the level is on, the current hour is inside the active window, and either
/// unconsumed wake events are pending or the interval has elapsed since the
/// last run (a never-run agent is immediately due).
pub fn autonomy_run_due(
    level: AutonomyLevel,
    now: chrono::DateTime<chrono::Utc>,
    last_run_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pending_wake_events: i64,
    active_hours: Option<(u8, u8)>,
    current_hour: u8,
    interval_secs: u64,
) -> bool {
    if level == AutonomyLevel::Off {
        return false;
    }
    if let Some((start, end)) = active_hours
        && !crate::cron::scheduler::hour_in_active_window(current_hour, start, end)
    {
        return false;
    }
    if pending_wake_events > 0 {
        return true;
    }
    match last_run_started_at {
        None => true,
        Some(last) => {
            now.signed_duration_since(last) >= chrono::Duration::seconds(interval_secs as i64)
        }
    }
}

/// Whether a task belongs in this agent's autonomy context.
///
/// Assigned tasks are visible only to their assignee; unassigned tasks are
/// visible only when the agent claims unowned work.
pub fn task_visible_to_agent(task: &Task, agent_id: &str, claim_unowned: bool) -> bool {
    match task.assigned_agent_id.as_deref() {
        Some(assigned) => assigned == agent_id,
        None => claim_unowned,
    }
}

/// Ask the resident autonomy supervisor to inspect its durable wake state.
pub async fn maybe_run_autonomy(deps: &AgentDeps) {
    deps.autonomy_control.request_check();
}

/// Handle an external wake by checking whether an autonomy run is due.
pub async fn wake_one(deps: &AgentDeps) {
    deps.autonomy_control.request_check();
}

pub fn spawn_autonomy_supervisor(deps: AgentDeps) -> AutonomySupervisorHandle {
    let (check_tx, check_rx) = mpsc::channel(1);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    deps.autonomy_control
        .attach(check_tx.clone(), shutdown_tx.clone());

    let run_slot = AutonomyRunSlot::default();
    let channel_id: crate::ChannelId = Arc::from(AUTONOMY_CONVERSATION_ID);
    let (response_tx, response_rx) = tokio::sync::mpsc::channel::<RoutedResponse>(32);
    let event_rx = deps.event_tx.subscribe();
    let resolved_settings = ResolvedConversationSettings {
        delegation: DelegationMode::Direct,
        ..ResolvedConversationSettings::default()
    };
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
    let (channel, channel_tx) = Channel::new(
        channel_id,
        ChannelKind::Autonomy,
        deps.clone(),
        response_tx,
        event_rx,
        screenshot_dir,
        logs_dir,
        None,
        resolved_settings,
        None,
        Some(run_slot.clone()),
    );
    let channel_state = channel.state.clone();

    let control = deps.autonomy_control.clone();
    let task_control = control.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = run_autonomy_supervisor(
            deps,
            check_rx,
            shutdown_rx,
            run_slot,
            channel,
            channel_tx,
            response_rx,
        )
        .await
        {
            tracing::error!(%error, "autonomy supervisor stopped with an error");
        }
        task_control.mark_stopped();
    });
    AutonomySupervisorHandle {
        control,
        task,
        channel_state,
    }
}

struct ActiveEpoch {
    handle: AutonomyRunHandle,
    config: AutonomyConfig,
    wake_event_ids: Vec<String>,
    started_at: chrono::DateTime<chrono::Utc>,
    last_heartbeat: tokio::time::Instant,
}

async fn run_autonomy_supervisor(
    deps: AgentDeps,
    mut check_rx: mpsc::Receiver<()>,
    mut shutdown_rx: watch::Receiver<bool>,
    run_slot: AutonomyRunSlot,
    channel: Channel,
    channel_tx: mpsc::Sender<InboundMessage>,
    mut response_rx: mpsc::Receiver<RoutedResponse>,
) -> anyhow::Result<()> {
    loop {
        match deps
            .autonomy_run_store
            .reconcile_running_runs("run interrupted by process restart")
            .await
        {
            Ok(_) => break,
            Err(error) => {
                tracing::warn!(%error, "failed to reconcile autonomy runs; retrying");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    let response_drain = tokio::spawn(async move { while response_rx.recv().await.is_some() {} });
    let channel_control = channel.control_handle();
    let mut channel_handle = tokio::spawn(channel.run());
    let heartbeat_period = Duration::from_secs(60);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + heartbeat_period,
        heartbeat_period,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut active_epoch: Option<ActiveEpoch> = None;
    let mut exit_error: Option<anyhow::Error> = None;

    loop {
        let run_changed = async {
            match active_epoch.as_ref() {
                Some(epoch) => epoch.handle.changed().await,
                None => std::future::pending::<()>().await,
            }
        };
        let mut should_check = false;
        tokio::select! {
            _ = heartbeat.tick() => should_check = true,
            check = check_rx.recv() => {
                if check.is_none() {
                    break;
                }
                should_check = true;
            }
            _ = run_changed => {}
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            result = &mut channel_handle => {
                match result {
                    Ok(Ok(())) => exit_error = Some(anyhow::anyhow!("resident autonomy channel exited")),
                    Ok(Err(error)) => exit_error = Some(error.into()),
                    Err(error) => exit_error = Some(anyhow::anyhow!("resident autonomy channel task failed: {error}")),
                }
                break;
            }
        }

        if active_epoch
            .as_ref()
            .is_some_and(|epoch| epoch.handle.finish_requested() && epoch.handle.is_quiescent())
        {
            let epoch = active_epoch.as_ref().expect("epoch checked as present");
            match finish_epoch(&deps, &run_slot, epoch).await {
                Ok(true) => {
                    active_epoch = None;
                    should_check = true;
                }
                Ok(false) => {
                    tracing::warn!(run_id = %epoch.handle.run_id, "autonomy epoch is quiescent but durable children have not settled");
                }
                Err(error) => {
                    tracing::warn!(run_id = %epoch.handle.run_id, %error, "failed to finish autonomy epoch; will retry");
                }
            }
        }

        if should_check {
            if !deps.autonomy_control.is_ready() {
                continue;
            }
            match active_epoch.as_mut() {
                Some(epoch) if !epoch.handle.finish_requested() => {
                    if let Err(error) = send_heartbeat(&deps, &channel_tx, epoch).await {
                        tracing::warn!(run_id = %epoch.handle.run_id, %error, "failed to deliver autonomy heartbeat; will retry");
                    }
                }
                Some(_) => {}
                None => match start_epoch_if_due(&deps, &run_slot, &channel_tx).await {
                    Ok(epoch) => active_epoch = epoch,
                    Err(error) => {
                        tracing::warn!(%error, "failed to admit autonomy epoch; supervisor remains active");
                        if let Some(handle) = run_slot.current() {
                            let summary = format!("epoch admission failed: {error}");
                            if deps
                                .autonomy_run_store
                                .finish_run_status(
                                    &handle.run_id,
                                    AutonomyRunStatus::Failed,
                                    Some(&summary),
                                )
                                .await
                                .unwrap_or(false)
                            {
                                publish_terminal_summary(&deps, &handle.run_id, &summary);
                            }
                            run_slot.clear_if_current(handle.generation);
                        }
                    }
                },
            }
        }
    }

    let interrupted = active_epoch
        .as_ref()
        .map(|epoch| epoch.handle.clone())
        .or_else(|| run_slot.current());
    if deps.autonomy_control.should_preserve_idle_workers() {
        if let Some(handle) = &interrupted {
            for child in handle.active_children() {
                match child {
                    AutonomyChild::Branch(branch_id) => {
                        channel_control
                            .cancel_branch_with_reason(branch_id, "daemon restarting")
                            .await;
                    }
                    AutonomyChild::WorkerOperation { .. } => {}
                }
            }
        }
    } else {
        for child in interrupted
            .as_ref()
            .map(|handle| handle.active_children())
            .unwrap_or_default()
        {
            if let AutonomyChild::Branch(branch_id) = child {
                channel_control
                    .cancel_branch_with_reason(branch_id, "autonomy supervisor shutting down")
                    .await;
            }
        }
    }

    if let Some(handle) = interrupted {
        if deps
            .autonomy_run_store
            .finish_run_status(
                &handle.run_id,
                AutonomyRunStatus::Failed,
                Some("run interrupted while autonomy supervisor stopped"),
            )
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, run_id = %handle.run_id, "failed to terminalize interrupted autonomy epoch");
                false
            })
        {
            publish_terminal_summary(
                &deps,
                &handle.run_id,
                "run interrupted while autonomy supervisor stopped",
            );
        }
        run_slot.clear_if_current(handle.generation);
    }
    channel_handle.abort();
    if let Err(error) = channel_handle.await
        && !error.is_cancelled()
    {
        tracing::warn!(%error, "resident autonomy channel failed while stopping");
    }
    drop(channel_control);
    response_drain.abort();
    match exit_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn start_epoch_if_due(
    deps: &AgentDeps,
    run_slot: &AutonomyRunSlot,
    channel_tx: &mpsc::Sender<InboundMessage>,
) -> anyhow::Result<Option<ActiveEpoch>> {
    let _transition_guard = deps.autonomy_control.lock_transition().await;
    let raw_config = **deps.runtime_config.autonomy.load();
    let config = AutonomyConfig {
        level: raw_config.level.min(**deps.autonomy_ceiling.load()),
        ..raw_config
    };
    if config.level == AutonomyLevel::Off || deps.pause_reason().is_some() {
        return Ok(None);
    }
    let pending_count = deps.wake_event_store.pending_count().await?;
    let last_run_started_at = deps.autonomy_run_store.last_run_started_at().await?;
    let (current_hour, _) = crate::cron::scheduler::current_hour_and_timezone(&deps.runtime_config);
    if !autonomy_run_due(
        config.level,
        chrono::Utc::now(),
        last_run_started_at,
        pending_count,
        config.active_hours,
        current_hour,
        config.interval_secs,
    ) {
        return Ok(None);
    }
    let permit = match channel_tx.try_reserve() {
        Ok(permit) => permit,
        Err(mpsc::error::TrySendError::Full(_)) => return Ok(None),
        Err(mpsc::error::TrySendError::Closed(_)) => {
            anyhow::bail!("resident autonomy channel inbox is closed")
        }
    };
    let Some(run_id) = deps.autonomy_run_store.try_begin_run().await? else {
        return Ok(None);
    };
    let handle = run_slot.begin(run_id.clone(), deps.autonomy_run_store.clone());
    let events = deps
        .wake_event_store
        .pending(WAKE_EVENT_BATCH_LIMIT)
        .await?;
    let event_ids: Vec<String> = events.iter().map(|event| event.id.clone()).collect();
    let briefing = build_run_briefing(deps, &config, &events, "heartbeat", None).await?;
    commit_wake_claim(&deps.sqlite_pool, &run_id, &event_ids, &event_ids).await?;
    permit.send(autonomy_message(deps, briefing, handle.generation, true));
    tracing::info!(agent_id = %deps.agent_id, %run_id, generation = handle.generation, "autonomy epoch started");
    Ok(Some(ActiveEpoch {
        handle,
        config,
        wake_event_ids: event_ids,
        started_at: chrono::Utc::now(),
        last_heartbeat: tokio::time::Instant::now(),
    }))
}

async fn send_heartbeat(
    deps: &AgentDeps,
    channel_tx: &mpsc::Sender<InboundMessage>,
    epoch: &mut ActiveEpoch,
) -> anyhow::Result<()> {
    let _transition_guard = deps.autonomy_control.lock_transition().await;
    let config = **deps.runtime_config.autonomy.load();
    let Some(effective_level) = heartbeat_level(config.level, **deps.autonomy_ceiling.load())
    else {
        return Ok(());
    };
    let pending_count = deps.wake_event_store.pending_count().await?;
    if pending_count == 0
        && epoch.last_heartbeat.elapsed() < Duration::from_secs(config.interval_secs.max(1))
    {
        return Ok(());
    }
    let permit = match channel_tx.try_reserve() {
        Ok(permit) => permit,
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::debug!(run_id = %epoch.handle.run_id, "autonomy heartbeat coalesced behind queued channel work");
            return Ok(());
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            anyhow::bail!("resident autonomy channel inbox is closed");
        }
    };
    epoch.config = AutonomyConfig {
        level: effective_level,
        ..config
    };
    let events = deps
        .wake_event_store
        .pending(WAKE_EVENT_BATCH_LIMIT)
        .await?;
    let event_ids: Vec<String> = events.iter().map(|event| event.id.clone()).collect();
    let elapsed = chrono::Utc::now()
        .signed_duration_since(epoch.started_at)
        .num_seconds()
        .max(0) as u64;
    let briefing =
        build_run_briefing(deps, &epoch.config, &events, "heartbeat", Some(elapsed)).await?;
    let mut all_event_ids = epoch.wake_event_ids.clone();
    all_event_ids.extend(event_ids.iter().cloned());
    commit_wake_claim(
        &deps.sqlite_pool,
        &epoch.handle.run_id,
        &event_ids,
        &all_event_ids,
    )
    .await?;
    permit.send(autonomy_message(
        deps,
        briefing,
        epoch.handle.generation,
        false,
    ));
    epoch.wake_event_ids = all_event_ids;
    epoch.last_heartbeat = tokio::time::Instant::now();
    Ok(())
}

fn heartbeat_level(level: AutonomyLevel, ceiling: AutonomyLevel) -> Option<AutonomyLevel> {
    let effective = level.min(ceiling);
    (effective != AutonomyLevel::Off).then_some(effective)
}

async fn commit_wake_claim(
    pool: &sqlx::SqlitePool,
    run_id: &str,
    event_ids: &[String],
    all_event_ids: &[String],
) -> anyhow::Result<()> {
    if event_ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; event_ids.len()].join(", ");
    let mut transaction = pool.begin().await?;
    let query = format!(
        "UPDATE wake_events SET consumed_by = ? WHERE consumed_by IS NULL AND id IN ({placeholders})"
    );
    let mut consume = sqlx::query(&query).bind(run_id);
    for event_id in event_ids {
        consume = consume.bind(event_id);
    }
    let claimed = consume.execute(&mut *transaction).await?.rows_affected();
    anyhow::ensure!(
        claimed == event_ids.len() as u64,
        "wake event claim lost a concurrency race"
    );
    let run_updated = sqlx::query(
        "UPDATE autonomy_runs SET wake_event_ids = ? WHERE id = ? AND status = 'running'",
    )
    .bind(serde_json::to_string(all_event_ids)?)
    .bind(run_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    anyhow::ensure!(run_updated == 1, "autonomy run is no longer active");
    transaction.commit().await?;
    Ok(())
}

async fn finish_epoch(
    deps: &AgentDeps,
    run_slot: &AutonomyRunSlot,
    epoch: &ActiveEpoch,
) -> anyhow::Result<bool> {
    let Some(request) = epoch.handle.finish_request() else {
        anyhow::bail!("quiescent autonomy epoch has no finish request");
    };
    let recorded = deps
        .autonomy_run_store
        .complete_run_if_children_settled(
            &epoch.handle.run_id,
            &request.summary,
            &request.actions,
            false,
        )
        .await?;
    if recorded {
        epoch.handle.mark_completed();
        publish_terminal_summary(deps, &epoch.handle.run_id, &request.summary);
    } else if deps
        .autonomy_run_store
        .run_is_active(&epoch.handle.run_id)
        .await?
    {
        return Ok(false);
    }
    run_slot.clear_if_current(epoch.handle.generation);
    deps.wake_event_store
        .prune_consumed(WAKE_EVENT_RETENTION_DAYS)
        .await?;
    tracing::info!(run_id = %epoch.handle.run_id, generation = epoch.handle.generation, "autonomy epoch finished; channel remains resident");
    Ok(true)
}

/// The run table transition is the idempotency boundary. Only the caller that
/// changes a run from `running` may add its conclusion to the channel record.
fn publish_terminal_summary(deps: &AgentDeps, run_id: &str, summary: &str) {
    let channel_id: crate::ChannelId = Arc::from(AUTONOMY_CONVERSATION_ID);
    crate::conversation::ConversationLogger::new(deps.sqlite_pool.clone()).log_bot_message_with_id(
        &channel_id,
        &format!("autonomy-outcome:{run_id}"),
        summary,
    );
    if let Err(error) = deps
        .event_tx
        .send(crate::ProcessEvent::ChannelAssistantMessage {
            agent_id: deps.agent_id.clone(),
            channel_id,
            text: summary.to_string(),
        })
    {
        tracing::debug!(%error, "failed to emit autonomy outcome for live timeline");
    }
}

pub const AUTONOMY_GENERATION_KEY: &str = "autonomy_generation";
pub const AUTONOMY_EPOCH_START_KEY: &str = "autonomy_epoch_start";

fn autonomy_message(
    deps: &AgentDeps,
    text: String,
    generation: u64,
    epoch_start: bool,
) -> InboundMessage {
    let mut metadata = HashMap::new();
    metadata.insert(AUTONOMY_GENERATION_KEY.to_string(), generation.into());
    metadata.insert(AUTONOMY_EPOCH_START_KEY.to_string(), epoch_start.into());
    InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "system".into(),
        adapter: None,
        conversation_id: AUTONOMY_CONVERSATION_ID.to_string(),
        sender_id: "system".into(),
        agent_id: Some(deps.agent_id.clone()),
        content: MessageContent::Text(text),
        timestamp: chrono::Utc::now(),
        metadata,
        formatted_author: None,
    }
}

/// Assemble the run briefing rendered from `autonomy_channel.md.j2`.
async fn build_run_briefing(
    deps: &AgentDeps,
    config: &AutonomyConfig,
    wake_events: &[crate::wakes::WakeEvent],
    trigger_reason: &str,
    elapsed_secs: Option<u64>,
) -> anyhow::Result<String> {
    let agent_name = deps
        .agent_names
        .get(deps.agent_id.as_ref())
        .cloned()
        .unwrap_or_else(|| deps.agent_id.to_string());

    // Wake definitions supply each event's name and instructions.
    // Instructions apply only within the wake's min_level; events from a wake
    // above the current level are rendered as observations. An event whose
    // definition is gone falls back to its wake id.
    let wake_defs: HashMap<String, crate::wakes::WakeDef> = if wake_events.is_empty() {
        HashMap::new()
    } else {
        deps.wake_def_store
            .list()
            .await?
            .into_iter()
            .map(|def| (def.id.clone(), def))
            .collect()
    };

    let wake_event_views: Vec<AutonomyWakeEventView> = wake_events
        .iter()
        .map(|event| {
            let def = wake_defs.get(&event.wake_id);
            AutonomyWakeEventView {
                wake_id: event.wake_id.clone(),
                name: def
                    .map(|def| def.name.clone())
                    .unwrap_or_else(|| event.wake_id.clone()),
                instructions: def
                    .filter(|def| def.min_level <= config.level)
                    .map(|def| def.instructions.clone()),
                gated: def.is_some_and(|def| def.min_level > config.level),
                fired_at: event.fired_at.clone(),
                delivery_count: event.delivery_count,
                payload: compact_payload(&event.payload),
            }
        })
        .collect();

    let run_history_views: Vec<AutonomyRunHistoryView> = deps
        .autonomy_run_store
        .recent(config.run_history_count.max(1))
        .await?
        .into_iter()
        .filter(|run| run.status != AutonomyRunStatus::Running)
        .map(|run| AutonomyRunHistoryView {
            started_at: run.started_at,
            status: run.status.as_str().to_string(),
            summary: run
                .summary
                .unwrap_or_else(|| "no summary recorded".to_string()),
            woken_by: run.wake_event_ids.len(),
        })
        .collect();

    let (task_state, has_tasks) = render_task_state(deps, config.claim_unowned).await?;
    let active_goals = crate::goals::render_active_goals_extended(&deps.goal_store).await?;
    let active_workers = render_active_workers(deps).await?;

    // Nothing to survey and no direction to work from. The run needs different
    // instructions, not a shorter version of the same ones.
    //
    // A wake event or a running worker is direction: the run has a reason to
    // exist and a bounded turn to spend on it, which cold-start discovery
    // would spend on the workspace instead.
    let instance_is_empty = !has_tasks
        && active_goals.is_empty()
        && wake_event_views.is_empty()
        && active_workers.is_none();

    let prompt_engine = deps.runtime_config.prompts.load();
    prompt_engine
        .render_autonomy_channel_prompt(
            &agent_name,
            config.level.as_str(),
            wake_event_views,
            run_history_views,
            &task_state,
            (!active_goals.is_empty()).then_some(active_goals.as_str()),
            active_workers.as_deref(),
            config.max_tasks_per_run,
            config.claim_unowned,
            instance_is_empty,
            trigger_reason,
            elapsed_secs,
        )
        .map_err(|error| anyhow::anyhow!("failed to render autonomy channel prompt: {error}"))
}

/// One-line JSON payload preview, truncated for prompt hygiene.
fn compact_payload(payload: &serde_json::Value) -> String {
    if payload.as_object().is_some_and(serde_json::Map::is_empty) {
        return String::new();
    }
    crate::tools::truncate_utf8_ellipsis(&payload.to_string(), 400)
}

/// Render the full task survey: pending_approval, ready, in_progress, backlog.
/// Returns the rendered survey and whether any task was visible in it.
async fn render_task_state(
    deps: &AgentDeps,
    claim_unowned: bool,
) -> anyhow::Result<(String, bool)> {
    let sections: [(TaskStatus, &str); 4] = [
        (
            TaskStatus::PendingApproval,
            "Pending approval (enrich these; never execute them)",
        ),
        (TaskStatus::Ready, "Ready (user-approved)"),
        (TaskStatus::InProgress, "In progress"),
        (TaskStatus::Backlog, "Backlog"),
    ];

    let mut output = String::new();
    let mut any = false;
    for (status, label) in sections {
        let tasks = deps
            .task_store
            .list(TaskListFilter {
                status: Some(status),
                limit: Some(200),
                ..Default::default()
            })
            .await?;
        let visible: Vec<Task> = tasks
            .into_iter()
            .filter(|task| task_visible_to_agent(task, &deps.agent_id, claim_unowned))
            .collect();
        if visible.is_empty() {
            continue;
        }

        any = true;

        // What has already been tried on these tasks, in one query. A run that
        // cannot see prior attempts repeats failed work and never escalates.
        let numbers: Vec<i64> = visible.iter().map(|task| task.task_number).collect();
        let attempts = deps
            .task_store
            .prior_attempt_summaries(&numbers)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "failed to load task attempt history for the board");
                std::collections::HashMap::new()
            });

        output.push_str(&format!("### {label}\n"));
        for task in visible {
            output.push_str(&render_task_line(
                &task,
                &deps.agent_id,
                attempts.get(&task.task_number).map(String::as_str),
            ));
        }
        output.push('\n');
    }

    if !any {
        output.push_str("No active tasks.\n");
    }
    Ok((output, any))
}

fn render_task_line(task: &Task, agent_id: &str, prior_attempts: Option<&str>) -> String {
    let ownership = match task.assigned_agent_id.as_deref() {
        Some(assigned) if assigned == agent_id => String::new(),
        Some(assigned) => format!(" (assigned to {assigned})"),
        None => " (unowned)".to_string(),
    };
    let mut line = format!(
        "- #{} [{}] {}{}",
        task.task_number,
        task.priority.as_str(),
        task.title,
        ownership
    );
    if let Some(description) = task
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        line.push_str(" — ");
        line.push_str(&crate::tools::truncate_utf8_ellipsis(
            &description.split_whitespace().collect::<Vec<_>>().join(" "),
            300,
        ));
    }
    // Surface the execution plan so runs execute tasks as configured instead
    // of re-deciding where and how the work happens.
    let plan = crate::tasks::ExecutionPlan::resolve(task, None);
    if !plan.is_empty() {
        line.push_str(&format!(" [{}]", plan.summary()));
    }
    // Surface ordering so runs don't re-derive the pipeline from prose.
    let blocked_by = task.blocked_by();
    if !blocked_by.is_empty() {
        let numbers: Vec<String> = blocked_by.iter().map(|n| format!("#{n}")).collect();
        line.push_str(&format!(" [blocked by {}]", numbers.join(", ")));
    }
    if let Some(parent) = task.stack_parent() {
        line.push_str(&format!(" [stacks on #{parent}]"));
    }
    // What has already been tried, so a run does not repeat failed work.
    if let Some(attempts) = prior_attempts {
        line.push_str(&format!(" [{attempts}]"));
    }
    line.push('\n');
    line
}

/// Render every nonterminal worker so heartbeats can tend retained interactive
/// sessions as well as actively running work.
async fn render_active_workers(deps: &AgentDeps) -> anyhow::Result<Option<String>> {
    let live_workers = deps.process_control_registry.list_worker_snapshots().await;
    let live_ids = live_workers
        .iter()
        .map(|worker| worker.worker_id.to_string())
        .collect::<HashSet<_>>();
    let logger = crate::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone());
    let (workers, _total) = logger
        .list_worker_runs(&deps.agent_id, 100, 0, None)
        .await?;
    let workers: Vec<_> = workers
        .into_iter()
        .filter(|worker| {
            !matches!(
                worker.lifecycle.as_str(),
                "succeeded" | "partial" | "cancelled" | "timed_out" | "blocked" | "failed"
            )
        })
        .collect();
    if workers.is_empty() && live_workers.is_empty() {
        return Ok(None);
    }

    let mut output = String::new();
    for worker in live_workers {
        let task_line = crate::summarize_first_non_empty_line(&worker.provenance.task, 160);
        output.push_str(&format!(
            "- {} [{}; {}; runtime attached{}] {}\n",
            worker.worker_id,
            worker.backend,
            worker.state,
            if worker.interactive {
                ", interactive"
            } else {
                ""
            },
            task_line,
        ));
    }
    for worker in workers {
        if live_ids.contains(&worker.id) {
            continue;
        }
        let task_line = crate::summarize_first_non_empty_line(&worker.task, 160);
        let ownership = worker
            .run_id
            .as_deref()
            .map(|run_id| format!(", originating epoch {run_id}"))
            .unwrap_or_default();
        let interaction = if worker.interactive {
            ", interactive"
        } else {
            ""
        };
        output.push_str(&format!(
            "- {} [{}; {}{}{}; unavailable] {}\n",
            worker.id, worker.worker_type, worker.lifecycle, interaction, ownership, task_line
        ));
    }
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    async fn run_store() -> Arc<AutonomyRunStore> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Arc::new(AutonomyRunStore::new(pool))
    }

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn utc(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn due_requires_level_on() {
        assert!(!autonomy_run_due(
            AutonomyLevel::Off,
            utc(10_000),
            None,
            5,
            None,
            12,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Observe,
            utc(10_000),
            None,
            0,
            None,
            12,
            1800
        ));
    }

    #[test]
    fn heartbeat_admission_stops_when_dial_or_ceiling_is_off() {
        assert_eq!(
            heartbeat_level(AutonomyLevel::Act, AutonomyLevel::Suggest),
            Some(AutonomyLevel::Suggest)
        );
        assert_eq!(
            heartbeat_level(AutonomyLevel::Off, AutonomyLevel::Act),
            None
        );
        assert_eq!(
            heartbeat_level(AutonomyLevel::Act, AutonomyLevel::Off),
            None
        );
    }

    #[tokio::test]
    async fn autonomy_transitions_serialize_with_admission() {
        let control = AutonomyControl::default();
        let guard = control.lock_transition().await;
        let contender = {
            let control = control.clone();
            tokio::spawn(async move { control.lock_transition().await })
        };

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!contender.is_finished());
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), contender)
            .await
            .expect("transition lock should become available")
            .expect("contender should not panic");
    }

    #[test]
    fn due_respects_active_hours() {
        // Window 8-22: hour 3 is outside even with pending events.
        assert!(!autonomy_run_due(
            AutonomyLevel::Act,
            utc(10_000),
            None,
            5,
            Some((8, 22)),
            3,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            utc(10_000),
            None,
            5,
            Some((8, 22)),
            9,
            1800
        ));
        // Midnight-wrapping window 22-6: hour 23 is inside.
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            utc(10_000),
            None,
            0,
            Some((22, 6)),
            23,
            1800
        ));
    }

    #[test]
    fn pending_wake_events_pull_the_run_forward() {
        let now = utc(10_000);
        let recent_run = Some(utc(9_900)); // 100s ago, interval 1800s
        assert!(!autonomy_run_due(
            AutonomyLevel::Suggest,
            now,
            recent_run,
            0,
            None,
            12,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Suggest,
            now,
            recent_run,
            1,
            None,
            12,
            1800
        ));
    }

    #[test]
    fn interval_elapse_makes_the_run_due() {
        let now = utc(10_000);
        assert!(!autonomy_run_due(
            AutonomyLevel::Act,
            now,
            Some(utc(10_000 - 1799)),
            0,
            None,
            12,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            now,
            Some(utc(10_000 - 1800)),
            0,
            None,
            12,
            1800
        ));
        // Never ran before: immediately due.
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            now,
            None,
            0,
            None,
            12,
            1800
        ));
    }

    fn task_with_assignment(assigned: Option<&str>) -> Task {
        Task {
            id: "task-1".to_string(),
            task_number: 1,
            title: "test".to_string(),
            description: None,
            status: TaskStatus::Ready,
            priority: crate::tasks::TaskPriority::Medium,
            owner_agent_id: "owner".to_string(),
            assigned_agent_id: assigned.map(str::to_string),
            subtasks: Vec::new(),
            metadata: serde_json::json!({}),
            goal_id: None,
            source_memory_id: None,
            worker_id: None,
            worker_type: None,
            project_id: None,
            repo_id: None,
            worktree_mode: None,
            worktree_id: None,
            required_skills: Vec::new(),
            depends_on: Vec::new(),
            revision: 1,
            created_by: "user".to_string(),
            approved_at: None,
            approved_by: None,
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
        }
    }

    #[test]
    fn task_visibility_follows_assignment_and_claim_flag() {
        let mine = task_with_assignment(Some("agent-a"));
        let theirs = task_with_assignment(Some("agent-b"));
        let unowned = task_with_assignment(None);

        assert!(task_visible_to_agent(&mine, "agent-a", false));
        assert!(!task_visible_to_agent(&theirs, "agent-a", true));
        assert!(task_visible_to_agent(&unowned, "agent-a", true));
        assert!(!task_visible_to_agent(&unowned, "agent-a", false));
    }

    #[tokio::test]
    async fn finish_waits_for_owned_children() {
        let store = run_store().await;
        let run_id = store.begin_run().await.unwrap();
        let handle = AutonomyRunHandle::new(run_id, 1, store);
        let worker_id = crate::WorkerId::new_v4();
        let child = AutonomyChild::WorkerOperation {
            worker_id,
            operation_id: crate::agent::process_control::WorkerOperationId::new(),
        };

        assert!(handle.register_child(child));
        assert_eq!(
            handle.request_finish(AutonomyFinishRequest {
                summary: "worker still active".to_string(),
                actions: Vec::new(),
            }),
            Err(1)
        );
        handle.settle_child(child);
        assert_eq!(
            handle.request_finish(AutonomyFinishRequest {
                summary: "worker result incorporated".to_string(),
                actions: Vec::new(),
            }),
            Ok(true)
        );
        assert!(!handle.register_child(AutonomyChild::WorkerOperation {
            worker_id: crate::WorkerId::new_v4(),
            operation_id: crate::agent::process_control::WorkerOperationId::new(),
        }));
    }

    #[tokio::test]
    async fn stale_worker_operation_cannot_settle_later_child() {
        let store = run_store().await;
        let run_id = store.begin_run().await.unwrap();
        let handle = AutonomyRunHandle::new(run_id, 1, store);
        let worker_id = crate::WorkerId::new_v4();
        let stale = AutonomyChild::WorkerOperation {
            worker_id,
            operation_id: crate::agent::process_control::WorkerOperationId::new(),
        };
        let current = AutonomyChild::WorkerOperation {
            worker_id,
            operation_id: crate::agent::process_control::WorkerOperationId::new(),
        };
        assert!(handle.register_child(current));

        handle.settle_child(stale);

        assert!(handle.owns_child(current));
        assert!(handle.has_active_children());
    }

    #[tokio::test]
    async fn stale_generation_cannot_clear_current_epoch() {
        let store = run_store().await;
        let slot = AutonomyRunSlot::default();
        let first = slot.begin("first".to_string(), store.clone());
        assert!(slot.clear_if_current(first.generation));
        let second = slot.begin("second".to_string(), store);

        assert!(!slot.clear_if_current(first.generation));
        assert_eq!(slot.current().unwrap().run_id, "second");
        assert!(slot.clear_if_current(second.generation));
        assert!(slot.current().is_none());
    }

    #[tokio::test]
    async fn autonomy_doorbell_coalesces_while_pending() {
        let control = AutonomyControl::default();
        let (check_tx, mut check_rx) = mpsc::channel(1);
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        control.attach(check_tx, shutdown_tx);

        assert!(!control.is_ready());
        control.activate();
        assert!(control.is_ready());
        control.request_check();

        assert_eq!(check_rx.recv().await, Some(()));
        assert!(check_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn wake_claim_rolls_back_when_epoch_is_not_active() {
        let pool = test_pool().await;
        let wake_store = crate::wakes::WakeEventStore::new(pool.clone());
        wake_store
            .enqueue("task.approved", "task:7", &serde_json::json!({"task": 7}))
            .await
            .unwrap();
        let event = wake_store.pending(1).await.unwrap().remove(0);

        assert!(
            commit_wake_claim(
                &pool,
                "missing-run",
                std::slice::from_ref(&event.id),
                std::slice::from_ref(&event.id),
            )
            .await
            .is_err()
        );
        assert_eq!(wake_store.pending_count().await.unwrap(), 1);
    }
}
