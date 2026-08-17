//! Agent-scoped runtime control registry for channels and workers.

use crate::agent::channel::WeakChannelControlHandle;
use crate::agent::worker::WorkerTranscriptSnapshot;
use crate::{BranchId, ChannelId, ProcessId, WorkerId};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct WorkerRegistrationId(u64);

impl WorkerRegistrationId {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for WorkerRegistrationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct WorkerOperationId(uuid::Uuid);

impl WorkerOperationId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for WorkerOperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkerOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerCallbackContext {
    pub worker_id: WorkerId,
    pub registration_id: WorkerRegistrationId,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRuntimeState {
    Starting,
    Running,
    WaitingForInput,
    Cancelling,
    Completing,
}

impl WorkerRuntimeState {
    fn can_transition_to(self, target: Self) -> bool {
        use WorkerRuntimeState::{Cancelling, Completing, Running, Starting, WaitingForInput};

        self == target
            || matches!(
                (self, target),
                (Starting, Running | Cancelling | Completing)
                    | (Running, WaitingForInput | Cancelling | Completing)
                    | (WaitingForInput, Running | Cancelling | Completing)
                    | (Cancelling, Completing)
            )
    }
}

impl fmt::Display for WorkerRuntimeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::Cancelling => "cancelling",
            Self::Completing => "completing",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerBackend {
    Builtin,
    OpenCode,
}

impl fmt::Display for WorkerBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin => formatter.write_str("builtin"),
            Self::OpenCode => formatter.write_str("opencode"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerRequester {
    Channel {
        channel_id: ChannelId,
    },
    Branch {
        channel_id: ChannelId,
        branch_id: BranchId,
    },
    CortexChat {
        thread_id: String,
    },
    Autonomy {
        run_id: String,
    },
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerResultTarget {
    Channel { channel_id: ChannelId },
    CortexChat { thread_id: String },
    None,
}

impl fmt::Display for WorkerResultTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel { channel_id } => write!(formatter, "channel:{channel_id}"),
            Self::CortexChat { thread_id } => write!(formatter, "cortex_chat:{thread_id}"),
            Self::None => formatter.write_str("none"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerOperationContext {
    pub operation_id: WorkerOperationId,
    pub requester: WorkerRequester,
    pub result_target: WorkerResultTarget,
    pub autonomy_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerFollowUp {
    pub operation: WorkerOperationContext,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerProvenance {
    pub origin_channel_id: Option<ChannelId>,
    pub origin_branch_id: Option<BranchId>,
    pub task: String,
    pub task_id: Option<String>,
    pub autonomy_run_id: Option<String>,
    pub spawning_process: ProcessId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSnapshot {
    pub worker_id: WorkerId,
    pub registration_id: WorkerRegistrationId,
    pub provenance: WorkerProvenance,
    pub backend: WorkerBackend,
    pub interactive: bool,
    pub routable: bool,
    pub state: WorkerRuntimeState,
    pub status: String,
    pub tool_calls: usize,
    pub active_operation: Option<WorkerOperationContext>,
    pub last_completed_operation_id: Option<WorkerOperationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMutationResult {
    Applied,
    NotFound,
    StaleRegistration,
    InvalidState,
    StaleOperation,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkerRegistryError {
    #[error("can't reserve worker: admission is closed")]
    AdmissionClosed,
    #[error("can't reserve worker {worker_id}: worker is already reserved or registered")]
    DuplicateWorker { worker_id: WorkerId },
    #[error("can't reserve worker: task is already owned by worker {existing_worker_id}")]
    DuplicateTask { existing_worker_id: WorkerId },
    #[error("can't reserve worker: channel {channel_id} has reached its limit of {max_workers}")]
    OriginChannelQuotaReached {
        channel_id: ChannelId,
        max_workers: usize,
    },
    #[error("can't register worker {worker_id}: worker is already registered")]
    DuplicateRegistration { worker_id: WorkerId },
    #[error("can't register worker {worker_id}: reservation is no longer owned")]
    ReservationNotOwned { worker_id: WorkerId },
    #[error("can't route worker {worker_id}: worker was not found")]
    WorkerNotFound { worker_id: WorkerId },
    #[error("can't route worker {worker_id}: worker is {state:?}")]
    WorkerBusy {
        worker_id: WorkerId,
        state: WorkerRuntimeState,
    },
    #[error("can't route worker {worker_id}: worker has no follow-up input")]
    FollowUpUnavailable { worker_id: WorkerId },
    #[error("can't inject worker {worker_id}: worker does not support running injection")]
    InjectionUnavailable { worker_id: WorkerId },
}

#[derive(Debug)]
pub struct WorkerReservation {
    worker_id: WorkerId,
    registration_id: WorkerRegistrationId,
    origin_channel_id: ChannelId,
    normalized_task: String,
}

impl WorkerReservation {
    pub fn callback_context(&self) -> WorkerCallbackContext {
        WorkerCallbackContext {
            worker_id: self.worker_id,
            registration_id: self.registration_id,
        }
    }
}

#[derive(Debug)]
pub struct WorkerAdmissionToken {
    worker_id: WorkerId,
    registration_id: WorkerRegistrationId,
}

impl WorkerAdmissionToken {
    pub fn callback_context(&self) -> WorkerCallbackContext {
        WorkerCallbackContext {
            worker_id: self.worker_id,
            registration_id: self.registration_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlActionResult {
    Cancelled,
    NotFound,
    AlreadyTerminal,
    Conflict,
}

#[derive(Clone)]
struct ChannelControlEntry {
    handle: WeakChannelControlHandle,
    registration_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerAdmissionPhase {
    Reserved,
    Registered,
}

struct WorkerAdmissionOwner {
    registration_id: WorkerRegistrationId,
    origin_channel_id: ChannelId,
    normalized_task: String,
    phase: WorkerAdmissionPhase,
}

#[derive(Default)]
struct WorkerAdmissions {
    closed: bool,
    owners: HashMap<WorkerId, WorkerAdmissionOwner>,
    task_owners: HashMap<String, WorkerId>,
    origin_channel_counts: HashMap<ChannelId, usize>,
}

impl WorkerAdmissions {
    fn release_if_matches(
        &mut self,
        worker_id: WorkerId,
        registration_id: WorkerRegistrationId,
    ) -> bool {
        let Some(owner) = self.owners.get(&worker_id) else {
            return false;
        };
        if owner.registration_id != registration_id {
            return false;
        }

        let owner = self
            .owners
            .remove(&worker_id)
            .expect("worker admission owner was checked above");
        if self.task_owners.get(&owner.normalized_task) == Some(&worker_id) {
            self.task_owners.remove(&owner.normalized_task);
        }
        if let Some(count) = self.origin_channel_counts.get_mut(&owner.origin_channel_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.origin_channel_counts.remove(&owner.origin_channel_id);
            }
        }
        true
    }
}

struct LiveWorkerState {
    state: WorkerRuntimeState,
    status: String,
    tool_calls: usize,
    active_operation: Option<WorkerOperationContext>,
    last_completed_operation_id: Option<WorkerOperationId>,
}

struct LiveWorkerEntry {
    worker_id: WorkerId,
    registration_id: WorkerRegistrationId,
    provenance: WorkerProvenance,
    backend: WorkerBackend,
    interactive: bool,
    live: RwLock<LiveWorkerState>,
    control: WorkerRuntimeControl,
}

pub type OpenCodeCancellationState =
    Arc<Mutex<Option<crate::opencode::worker::OpenCodeCancellationSession>>>;

/// Admission bucket for workers with no origin channel. Detached workers share
/// one quota rather than each inventing a scope name.
pub const DETACHED_WORKER_ADMISSION_SCOPE: &str = "detached";

/// Cancellation channel payload. `None` until cancellation is requested, then
/// the reason the requester gave, which the supervisor records on the worker's
/// terminal outcome.
pub type WorkerCancelSignal = Option<Arc<str>>;

/// Render a cancellation reason as the worker's terminal result text.
pub fn worker_cancellation_result(reason: Option<&str>) -> String {
    let reason = reason
        .map(|reason| crate::summarize_first_non_empty_line(reason, crate::EVENT_SUMMARY_MAX_CHARS))
        .unwrap_or_default();
    if reason.is_empty() {
        "Worker cancelled.".to_string()
    } else {
        format!("Worker cancelled: {reason}")
    }
}

pub struct WorkerRuntimeControl {
    supervisor_handle: Mutex<Option<JoinHandle<()>>>,
    execution_abort_handle: Mutex<Option<AbortHandle>>,
    cancel_tx: watch::Sender<WorkerCancelSignal>,
    terminal_notify: Arc<Notify>,
    transcript_snapshot: WorkerTranscriptSnapshot,
    opencode_cancellation: Option<OpenCodeCancellationState>,
    input_tx: Option<mpsc::Sender<WorkerFollowUp>>,
    injection_tx: Option<mpsc::Sender<String>>,
    process_run_logger: Option<crate::conversation::ProcessRunLogger>,
}

impl WorkerRuntimeControl {
    pub fn new(
        transcript_snapshot: WorkerTranscriptSnapshot,
        opencode_cancellation: Option<OpenCodeCancellationState>,
        input_tx: Option<mpsc::Sender<WorkerFollowUp>>,
        injection_tx: Option<mpsc::Sender<String>>,
        process_run_logger: Option<crate::conversation::ProcessRunLogger>,
    ) -> (Self, watch::Receiver<WorkerCancelSignal>, Arc<Notify>) {
        let (cancel_tx, cancel_rx) = watch::channel(None);
        let terminal_notify = Arc::new(Notify::new());
        (
            Self {
                supervisor_handle: Mutex::new(None),
                execution_abort_handle: Mutex::new(None),
                cancel_tx,
                terminal_notify: terminal_notify.clone(),
                transcript_snapshot,
                opencode_cancellation,
                input_tx,
                injection_tx,
                process_run_logger,
            },
            cancel_rx,
            terminal_notify,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerRouteResult {
    Routed {
        operation: WorkerOperationContext,
    },
    Injected,
    Busy {
        state: WorkerRuntimeState,
    },
    WaitUntilIdle,
    /// A durable terminal worker. Carries the outcome so a caller learns how the
    /// work ended instead of retrying a worker that can never accept input.
    Terminal {
        lifecycle: crate::conversation::WorkerLifecycle,
        result: Option<String>,
    },
    /// A durable nonterminal worker with no live controls in this agent.
    Unavailable {
        lifecycle: crate::conversation::WorkerLifecycle,
    },
    NotFound,
}

enum ChannelLookupResult {
    Found(crate::agent::channel::ChannelControlHandle),
    Stale(u64),
    Missing,
}

pub struct ProcessControlRegistry {
    channels: RwLock<HashMap<ChannelId, ChannelControlEntry>>,
    workers: RwLock<HashMap<WorkerId, Arc<LiveWorkerEntry>>>,
    admissions: Mutex<WorkerAdmissions>,
    next_channel_registration: AtomicU64,
    next_worker_registration: AtomicU64,
}

impl Default for ProcessControlRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessControlRegistry {
    pub fn new() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
            workers: RwLock::new(HashMap::new()),
            admissions: Mutex::new(WorkerAdmissions::default()),
            next_channel_registration: AtomicU64::new(1),
            next_worker_registration: AtomicU64::new(1),
        }
    }

    pub async fn reserve_worker(
        &self,
        worker_id: WorkerId,
        provenance: &WorkerProvenance,
        max_workers_per_origin_channel: usize,
    ) -> Result<WorkerReservation, WorkerRegistryError> {
        let admission_scope = provenance
            .origin_channel_id
            .clone()
            .unwrap_or_else(|| Arc::from(DETACHED_WORKER_ADMISSION_SCOPE));
        self.reserve_worker_in_scope(
            worker_id,
            provenance,
            admission_scope,
            max_workers_per_origin_channel,
        )
        .await
    }

    pub async fn reserve_worker_in_scope(
        &self,
        worker_id: WorkerId,
        provenance: &WorkerProvenance,
        admission_scope: ChannelId,
        max_workers_per_origin_channel: usize,
    ) -> Result<WorkerReservation, WorkerRegistryError> {
        let normalized_task = normalize_worker_task(&provenance.task);
        let mut admissions = self.admissions.lock().await;
        if admissions.closed {
            return Err(WorkerRegistryError::AdmissionClosed);
        }
        if admissions.owners.contains_key(&worker_id) {
            return Err(WorkerRegistryError::DuplicateWorker { worker_id });
        }
        if let Some(existing_worker_id) = admissions.task_owners.get(&normalized_task) {
            return Err(WorkerRegistryError::DuplicateTask {
                existing_worker_id: *existing_worker_id,
            });
        }
        let active_count = admissions
            .origin_channel_counts
            .get(&admission_scope)
            .copied()
            .unwrap_or_default();
        if active_count >= max_workers_per_origin_channel {
            return Err(WorkerRegistryError::OriginChannelQuotaReached {
                channel_id: admission_scope,
                max_workers: max_workers_per_origin_channel,
            });
        }

        let registration_id =
            WorkerRegistrationId(self.next_worker_registration.fetch_add(1, Ordering::AcqRel));
        admissions
            .task_owners
            .insert(normalized_task.clone(), worker_id);
        *admissions
            .origin_channel_counts
            .entry(admission_scope.clone())
            .or_default() += 1;
        admissions.owners.insert(
            worker_id,
            WorkerAdmissionOwner {
                registration_id,
                origin_channel_id: admission_scope.clone(),
                normalized_task: normalized_task.clone(),
                phase: WorkerAdmissionPhase::Reserved,
            },
        );

        Ok(WorkerReservation {
            worker_id,
            registration_id,
            origin_channel_id: admission_scope,
            normalized_task,
        })
    }

    pub async fn release_worker_reservation(&self, reservation: WorkerReservation) -> bool {
        let mut admissions = self.admissions.lock().await;
        let is_reserved = admissions
            .owners
            .get(&reservation.worker_id)
            .is_some_and(|owner| {
                owner.registration_id == reservation.registration_id
                    && owner.phase == WorkerAdmissionPhase::Reserved
                    && owner.origin_channel_id == reservation.origin_channel_id
                    && owner.normalized_task == reservation.normalized_task
            });
        is_reserved
            && admissions.release_if_matches(reservation.worker_id, reservation.registration_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn register_new_worker(
        &self,
        reservation: WorkerReservation,
        provenance: WorkerProvenance,
        backend: WorkerBackend,
        interactive: bool,
        initial_operation: WorkerOperationContext,
        status: impl Into<String>,
        control: WorkerRuntimeControl,
    ) -> Result<WorkerAdmissionToken, WorkerRegistryError> {
        self.register_worker(
            reservation,
            provenance,
            backend,
            interactive,
            WorkerRuntimeState::Starting,
            Some(initial_operation),
            status.into(),
            0,
            control,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn register_restored_worker(
        &self,
        reservation: WorkerReservation,
        provenance: WorkerProvenance,
        backend: WorkerBackend,
        interactive: bool,
        status: impl Into<String>,
        tool_calls: usize,
        control: WorkerRuntimeControl,
    ) -> Result<WorkerAdmissionToken, WorkerRegistryError> {
        self.register_worker(
            reservation,
            provenance,
            backend,
            interactive,
            WorkerRuntimeState::WaitingForInput,
            None,
            status.into(),
            tool_calls,
            control,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn register_worker(
        &self,
        reservation: WorkerReservation,
        provenance: WorkerProvenance,
        backend: WorkerBackend,
        interactive: bool,
        state: WorkerRuntimeState,
        active_operation: Option<WorkerOperationContext>,
        status: String,
        tool_calls: usize,
        control: WorkerRuntimeControl,
    ) -> Result<WorkerAdmissionToken, WorkerRegistryError> {
        let worker_id = reservation.worker_id;
        let registration_id = reservation.registration_id;
        let mut workers = self.workers.write().await;
        if workers.contains_key(&worker_id) {
            drop(workers);
            self.release_worker_reservation(reservation).await;
            return Err(WorkerRegistryError::DuplicateRegistration { worker_id });
        }

        let mut admissions = self.admissions.lock().await;
        if admissions.closed {
            admissions.release_if_matches(worker_id, registration_id);
            return Err(WorkerRegistryError::AdmissionClosed);
        }
        let reservation_is_owned = admissions.owners.get(&worker_id).is_some_and(|owner| {
            owner.registration_id == registration_id
                && owner.phase == WorkerAdmissionPhase::Reserved
                && owner.origin_channel_id == reservation.origin_channel_id
                && owner.normalized_task == reservation.normalized_task
                && owner.normalized_task == normalize_worker_task(&provenance.task)
        });
        if !reservation_is_owned {
            return Err(WorkerRegistryError::ReservationNotOwned { worker_id });
        }
        admissions
            .owners
            .get_mut(&worker_id)
            .expect("worker reservation was checked above")
            .phase = WorkerAdmissionPhase::Registered;

        workers.insert(
            worker_id,
            Arc::new(LiveWorkerEntry {
                worker_id,
                registration_id,
                provenance,
                backend,
                interactive,
                live: RwLock::new(LiveWorkerState {
                    state,
                    status,
                    tool_calls,
                    active_operation,
                    last_completed_operation_id: None,
                }),
                control,
            }),
        );
        Ok(WorkerAdmissionToken {
            worker_id,
            registration_id,
        })
    }

    pub async fn close_admission(&self) -> bool {
        let mut admissions = self.admissions.lock().await;
        let was_open = !admissions.closed;
        admissions.closed = true;
        let reservations = admissions
            .owners
            .iter()
            .filter_map(|(worker_id, owner)| {
                (owner.phase == WorkerAdmissionPhase::Reserved)
                    .then_some((*worker_id, owner.registration_id))
            })
            .collect::<Vec<_>>();
        for (worker_id, registration_id) in reservations {
            admissions.release_if_matches(worker_id, registration_id);
        }
        was_open
    }

    pub async fn install_task_handle(
        &self,
        callback: WorkerCallbackContext,
        handle: JoinHandle<()>,
    ) -> Result<(), JoinHandle<()>> {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return Err(handle);
        };
        let mut supervisor_handle = entry.control.supervisor_handle.lock().await;
        if supervisor_handle.is_some() {
            return Err(handle);
        }
        *supervisor_handle = Some(handle);
        Ok(())
    }

    pub async fn install_execution_abort_handle(
        &self,
        callback: WorkerCallbackContext,
        handle: AbortHandle,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let live = entry.live.read().await;
        if !matches!(
            live.state,
            WorkerRuntimeState::Running | WorkerRuntimeState::WaitingForInput
        ) {
            return WorkerMutationResult::InvalidState;
        }
        let mut execution_handle = entry.control.execution_abort_handle.lock().await;
        if execution_handle.is_some() {
            return WorkerMutationResult::InvalidState;
        }
        *execution_handle = Some(handle);
        WorkerMutationResult::Applied
    }

    pub async fn clear_execution_abort_handle(&self, callback: WorkerCallbackContext) {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return;
        };
        entry.control.execution_abort_handle.lock().await.take();
    }

    pub async fn deliver_claimed_follow_up(
        &self,
        callback: WorkerCallbackContext,
        follow_up: WorkerFollowUp,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        {
            let live = entry.live.read().await;
            if live.state != WorkerRuntimeState::Running
                || live
                    .active_operation
                    .as_ref()
                    .map(|operation| operation.operation_id)
                    != Some(follow_up.operation.operation_id)
            {
                return WorkerMutationResult::StaleOperation;
            }
        }
        let Some(input_tx) = entry.control.input_tx.clone() else {
            self.rollback_failed_follow_up(callback, follow_up.operation.operation_id)
                .await;
            return WorkerMutationResult::InvalidState;
        };
        if input_tx.send(follow_up.clone()).await.is_err() {
            self.rollback_failed_follow_up(callback, follow_up.operation.operation_id)
                .await;
            return WorkerMutationResult::NotFound;
        }
        WorkerMutationResult::Applied
    }

    pub async fn inject_running(&self, worker_id: WorkerId, message: String) -> WorkerRouteResult {
        let Some(entry) = self.workers.read().await.get(&worker_id).cloned() else {
            return WorkerRouteResult::NotFound;
        };
        let live = entry.live.read().await;
        if live.state != WorkerRuntimeState::Running {
            return WorkerRouteResult::Busy { state: live.state };
        }
        let Some(injection_tx) = entry.control.injection_tx.clone() else {
            return WorkerRouteResult::WaitUntilIdle;
        };
        drop(live);
        if injection_tx.send(message).await.is_ok() {
            WorkerRouteResult::Injected
        } else {
            WorkerRouteResult::NotFound
        }
    }

    pub async fn cancel_worker_runtime(
        &self,
        worker_id: WorkerId,
        reason: &str,
        grace: std::time::Duration,
    ) -> ControlActionResult {
        let Some(callback) = self.worker_callback_context(worker_id).await else {
            return ControlActionResult::NotFound;
        };
        self.cancel_worker_callback(callback, reason, grace).await
    }

    /// The reason recorded against a pending cancellation, when one has been
    /// requested for this exact registration.
    pub async fn worker_cancellation_reason(
        &self,
        callback: WorkerCallbackContext,
    ) -> Option<Arc<str>> {
        let entry = self.worker_entry_for_callback(callback).await?;
        // No await between the borrow and the clone, so the watch guard never
        // has to be held across a suspension point.
        entry.control.cancel_tx.borrow().clone()
    }

    async fn cancel_worker_callback(
        &self,
        callback: WorkerCallbackContext,
        reason: &str,
        grace: std::time::Duration,
    ) -> ControlActionResult {
        let worker_id = callback.worker_id;
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return ControlActionResult::AlreadyTerminal;
        };
        {
            let mut live = entry.live.write().await;
            let durable_lifecycle = match live.state {
                WorkerRuntimeState::Starting => None,
                WorkerRuntimeState::Running => Some(crate::conversation::WorkerLifecycle::Running),
                WorkerRuntimeState::WaitingForInput => {
                    Some(crate::conversation::WorkerLifecycle::WaitingForInput)
                }
                WorkerRuntimeState::Cancelling => return ControlActionResult::Cancelled,
                WorkerRuntimeState::Completing => return ControlActionResult::AlreadyTerminal,
            };
            if let Some(expected) = durable_lifecycle {
                let Some(run_logger) = &entry.control.process_run_logger else {
                    tracing::error!(%worker_id, "worker runtime has no durable cancellation control");
                    return ControlActionResult::Conflict;
                };
                match run_logger
                    .transition_worker(
                        worker_id,
                        expected,
                        crate::conversation::WorkerLifecycle::Cancelling,
                    )
                    .await
                {
                    Ok(crate::conversation::WorkerTransitionResult::Applied { .. })
                    | Ok(crate::conversation::WorkerTransitionResult::Conflict {
                        current: crate::conversation::WorkerLifecycle::Cancelling,
                    }) => {}
                    Ok(crate::conversation::WorkerTransitionResult::Conflict {
                        current: crate::conversation::WorkerLifecycle::Completing,
                    }) => {
                        live.state = WorkerRuntimeState::Completing;
                        return ControlActionResult::AlreadyTerminal;
                    }
                    Ok(crate::conversation::WorkerTransitionResult::Conflict { current })
                        if current.is_terminal() =>
                    {
                        live.state = WorkerRuntimeState::Completing;
                        return ControlActionResult::AlreadyTerminal;
                    }
                    Ok(crate::conversation::WorkerTransitionResult::Conflict { current }) => {
                        tracing::warn!(%worker_id, lifecycle = current.as_str(), "worker cancellation conflicted with durable lifecycle");
                        return ControlActionResult::Conflict;
                    }
                    Ok(crate::conversation::WorkerTransitionResult::NotFound) => {
                        tracing::warn!(%worker_id, "worker cancellation found no durable row");
                        return ControlActionResult::Conflict;
                    }
                    Err(error) => {
                        tracing::warn!(%error, %worker_id, "failed to claim durable worker cancellation");
                        return ControlActionResult::Conflict;
                    }
                }
            }
            live.state = WorkerRuntimeState::Cancelling;
        }
        if let Some(cancellation) = &entry.control.opencode_cancellation
            && let Some(session) = cancellation.lock().await.clone()
            && let Ok(Err(error)) = tokio::time::timeout(grace, async {
                session
                    .server
                    .lock()
                    .await
                    .abort_session(&session.session_id)
                    .await
            })
            .await
        {
            tracing::warn!(%error, %worker_id, "failed to abort OpenCode session");
        }
        let terminal = entry.control.terminal_notify.notified();
        tokio::pin!(terminal);
        entry
            .control
            .cancel_tx
            .send_replace(Some(Arc::from(reason)));
        if tokio::time::timeout(grace, &mut terminal).await.is_err()
            && let Some(handle) = entry.control.execution_abort_handle.lock().await.as_ref()
        {
            terminal.set(entry.control.terminal_notify.notified());
            handle.abort();
            let _ = tokio::time::timeout(grace, &mut terminal).await;
        }
        ControlActionResult::Cancelled
    }

    /// Cancel workers spawned by one channel and release reservations that did
    /// not reach registration. Callback-scoped cancellation cannot affect a
    /// replacement registration that reused the same worker ID.
    pub async fn cancel_workers_by_origin_channel(
        &self,
        channel_id: &ChannelId,
        reason: &str,
        grace: std::time::Duration,
    ) -> usize {
        let callbacks = self
            .workers
            .read()
            .await
            .values()
            .filter(|entry| entry.provenance.origin_channel_id.as_ref() == Some(channel_id))
            .map(|entry| WorkerCallbackContext {
                worker_id: entry.worker_id,
                registration_id: entry.registration_id,
            })
            .collect::<Vec<_>>();

        for callback in &callbacks {
            self.cancel_worker_callback(*callback, reason, grace).await;
        }

        let mut admissions = self.admissions.lock().await;
        let reservations = admissions
            .owners
            .iter()
            .filter_map(|(worker_id, owner)| {
                (owner.phase == WorkerAdmissionPhase::Reserved
                    && owner.origin_channel_id == *channel_id)
                    .then_some((*worker_id, owner.registration_id))
            })
            .collect::<Vec<_>>();
        for (worker_id, registration_id) in reservations {
            admissions.release_if_matches(worker_id, registration_id);
        }

        callbacks.len()
    }

    pub async fn transcript_snapshot(
        &self,
        callback: WorkerCallbackContext,
    ) -> Option<WorkerTranscriptSnapshot> {
        self.worker_entry_for_callback(callback)
            .await
            .map(|entry| entry.control.transcript_snapshot.clone())
    }

    pub async fn drain_workers(&self, reason: &str, grace: std::time::Duration) {
        self.close_admission().await;
        let worker_ids = self
            .workers
            .read()
            .await
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for worker_id in worker_ids {
            self.cancel_worker_runtime(worker_id, reason, grace).await;
        }
    }

    pub async fn detach_workers(&self) {
        self.close_admission().await;
        let entries = self
            .workers
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for entry in entries {
            if let Some(handle) = entry.control.supervisor_handle.lock().await.as_ref() {
                handle.abort();
            }
            self.remove_worker_if_registration_matches(WorkerCallbackContext {
                worker_id: entry.worker_id,
                registration_id: entry.registration_id,
            })
            .await;
        }
    }

    pub async fn worker_callback_context(
        &self,
        worker_id: WorkerId,
    ) -> Option<WorkerCallbackContext> {
        self.workers
            .read()
            .await
            .get(&worker_id)
            .map(|entry| WorkerCallbackContext {
                worker_id,
                registration_id: entry.registration_id,
            })
    }

    pub async fn worker_snapshot(&self, worker_id: WorkerId) -> Option<WorkerSnapshot> {
        let entry = self.workers.read().await.get(&worker_id).cloned()?;
        Some(snapshot_worker(&entry).await)
    }

    pub async fn worker_snapshot_for_callback(
        &self,
        callback: WorkerCallbackContext,
    ) -> Option<WorkerSnapshot> {
        let entry = self.worker_entry_for_callback(callback).await?;
        Some(snapshot_worker(&entry).await)
    }

    pub async fn list_worker_snapshots(&self) -> Vec<WorkerSnapshot> {
        let mut entries = self
            .workers
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| entry.worker_id);

        let mut snapshots = Vec::with_capacity(entries.len());
        for entry in entries {
            snapshots.push(snapshot_worker(&entry).await);
        }
        snapshots
    }

    pub async fn update_worker_state(
        &self,
        callback: WorkerCallbackContext,
        target: WorkerRuntimeState,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let mut live = entry.live.write().await;
        if !live.state.can_transition_to(target)
            || (target == WorkerRuntimeState::WaitingForInput && live.active_operation.is_some())
        {
            return WorkerMutationResult::InvalidState;
        }
        live.state = target;
        WorkerMutationResult::Applied
    }

    pub async fn update_worker_status(
        &self,
        callback: WorkerCallbackContext,
        status: impl Into<String>,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let mut live = entry.live.write().await;
        if matches!(
            live.state,
            WorkerRuntimeState::Cancelling | WorkerRuntimeState::Completing
        ) {
            return WorkerMutationResult::InvalidState;
        }
        live.status = status.into();
        WorkerMutationResult::Applied
    }

    pub async fn increment_worker_tool_calls(
        &self,
        callback: WorkerCallbackContext,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let mut live = entry.live.write().await;
        if matches!(
            live.state,
            WorkerRuntimeState::Cancelling | WorkerRuntimeState::Completing
        ) {
            return WorkerMutationResult::InvalidState;
        }
        live.tool_calls = live.tool_calls.saturating_add(1);
        WorkerMutationResult::Applied
    }

    pub async fn claim_worker_outcome_status(
        &self,
        callback: WorkerCallbackContext,
        run_logger: &crate::conversation::ProcessRunLogger,
        status: impl Into<String>,
    ) -> crate::Result<WorkerMutationResult> {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return Ok(self.missing_worker_mutation_result(callback).await);
        };
        let mut live = entry.live.write().await;
        if live.state != WorkerRuntimeState::Running {
            return Ok(WorkerMutationResult::InvalidState);
        }
        match run_logger
            .claim_worker_completion(
                callback.worker_id,
                crate::conversation::WorkerLifecycle::Running,
            )
            .await?
        {
            crate::conversation::WorkerTransitionResult::Applied { .. }
            | crate::conversation::WorkerTransitionResult::Conflict {
                current: crate::conversation::WorkerLifecycle::Completing,
            } => {
                live.state = WorkerRuntimeState::Completing;
                live.status = status.into();
                Ok(WorkerMutationResult::Applied)
            }
            crate::conversation::WorkerTransitionResult::Conflict { .. } => {
                Ok(WorkerMutationResult::InvalidState)
            }
            crate::conversation::WorkerTransitionResult::NotFound => {
                Ok(WorkerMutationResult::NotFound)
            }
        }
    }

    pub async fn worker_is_in_state(
        &self,
        callback: WorkerCallbackContext,
        expected: WorkerRuntimeState,
    ) -> bool {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return false;
        };
        entry.live.read().await.state == expected
    }

    pub async fn run_if_worker_state(
        &self,
        callback: WorkerCallbackContext,
        expected: WorkerRuntimeState,
        action: impl FnOnce(),
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let live = entry.live.write().await;
        if live.state != expected {
            return WorkerMutationResult::InvalidState;
        }
        action();
        WorkerMutationResult::Applied
    }

    pub async fn complete_worker_operation(
        &self,
        callback: WorkerCallbackContext,
        operation_id: WorkerOperationId,
        status: impl Into<String>,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let mut live = entry.live.write().await;
        if live.state != WorkerRuntimeState::Running
            || live
                .active_operation
                .as_ref()
                .is_none_or(|operation| operation.operation_id != operation_id)
        {
            return WorkerMutationResult::StaleOperation;
        }
        live.state = WorkerRuntimeState::WaitingForInput;
        live.status = status.into();
        live.active_operation = None;
        live.last_completed_operation_id = Some(operation_id);
        WorkerMutationResult::Applied
    }

    pub async fn persist_opencode_session(
        &self,
        callback: WorkerCallbackContext,
        run_logger: &crate::conversation::ProcessRunLogger,
        session_id: &str,
        port: u16,
    ) -> crate::Result<WorkerMutationResult> {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return Ok(self.missing_worker_mutation_result(callback).await);
        };
        let live = entry.live.read().await;
        if matches!(
            live.state,
            WorkerRuntimeState::Cancelling | WorkerRuntimeState::Completing
        ) {
            return Ok(WorkerMutationResult::InvalidState);
        }
        if run_logger
            .update_opencode_metadata(callback.worker_id, session_id, port)
            .await?
        {
            Ok(WorkerMutationResult::Applied)
        } else {
            Ok(WorkerMutationResult::NotFound)
        }
    }

    pub async fn claim_idle_follow_up(
        &self,
        worker_id: WorkerId,
        requester: WorkerRequester,
        result_target: WorkerResultTarget,
        autonomy_run_id: Option<String>,
        message: impl Into<String>,
    ) -> Result<WorkerFollowUp, WorkerRegistryError> {
        let Some(entry) = self.workers.read().await.get(&worker_id).cloned() else {
            return Err(WorkerRegistryError::WorkerNotFound { worker_id });
        };
        let mut live = entry.live.write().await;
        if live.state != WorkerRuntimeState::WaitingForInput || live.active_operation.is_some() {
            return Err(WorkerRegistryError::WorkerBusy {
                worker_id,
                state: live.state,
            });
        }

        let operation = WorkerOperationContext {
            operation_id: WorkerOperationId::new(),
            requester,
            result_target,
            autonomy_run_id,
        };
        live.state = WorkerRuntimeState::Running;
        live.status = "processing follow-up".to_string();
        live.active_operation = Some(operation.clone());
        Ok(WorkerFollowUp {
            operation,
            message: message.into(),
        })
    }

    pub async fn rollback_failed_follow_up(
        &self,
        callback: WorkerCallbackContext,
        operation_id: WorkerOperationId,
    ) -> WorkerMutationResult {
        let Some(entry) = self.worker_entry_for_callback(callback).await else {
            return self.missing_worker_mutation_result(callback).await;
        };
        let mut live = entry.live.write().await;
        if live.state != WorkerRuntimeState::Running
            || live
                .active_operation
                .as_ref()
                .is_none_or(|operation| operation.operation_id != operation_id)
        {
            return WorkerMutationResult::StaleOperation;
        }
        live.state = WorkerRuntimeState::WaitingForInput;
        live.active_operation = None;
        WorkerMutationResult::Applied
    }

    pub async fn remove_worker_if_registration_matches(
        &self,
        callback: WorkerCallbackContext,
    ) -> bool {
        let mut workers = self.workers.write().await;
        let should_remove = workers
            .get(&callback.worker_id)
            .is_some_and(|entry| entry.registration_id == callback.registration_id);
        if !should_remove {
            return false;
        }
        workers.remove(&callback.worker_id);
        drop(workers);

        self.admissions
            .lock()
            .await
            .release_if_matches(callback.worker_id, callback.registration_id);
        true
    }

    async fn worker_entry_for_callback(
        &self,
        callback: WorkerCallbackContext,
    ) -> Option<Arc<LiveWorkerEntry>> {
        self.workers
            .read()
            .await
            .get(&callback.worker_id)
            .filter(|entry| entry.registration_id == callback.registration_id)
            .cloned()
    }

    async fn missing_worker_mutation_result(
        &self,
        callback: WorkerCallbackContext,
    ) -> WorkerMutationResult {
        if self.workers.read().await.contains_key(&callback.worker_id) {
            WorkerMutationResult::StaleRegistration
        } else {
            WorkerMutationResult::NotFound
        }
    }

    pub async fn register_channel(
        &self,
        channel_id: ChannelId,
        handle: WeakChannelControlHandle,
    ) -> u64 {
        let registration_id = self
            .next_channel_registration
            .fetch_add(1, Ordering::AcqRel);
        self.channels.write().await.insert(
            channel_id,
            ChannelControlEntry {
                handle,
                registration_id,
            },
        );
        registration_id
    }

    pub async fn unregister_channel(&self, channel_id: &ChannelId, registration_id: u64) -> bool {
        let mut channels = self.channels.write().await;
        let should_remove = channels
            .get(channel_id)
            .is_some_and(|entry| entry.registration_id == registration_id);
        if should_remove {
            channels.remove(channel_id);
        }
        should_remove
    }

    pub async fn prune_dead_channels(&self) -> usize {
        let mut channels = self.channels.write().await;
        let before = channels.len();
        channels.retain(|_, entry| entry.handle.upgrade().is_some());
        before.saturating_sub(channels.len())
    }

    /// Live control handle for a channel, when one is running. Prunes a
    /// stale registration on the way.
    pub async fn channel_handle(
        &self,
        channel_id: &ChannelId,
    ) -> Option<crate::agent::channel::ChannelControlHandle> {
        match self.lookup_channel_handle(channel_id).await {
            ChannelLookupResult::Found(handle) => Some(handle),
            ChannelLookupResult::Stale(registration_id) => {
                self.remove_stale_channel_if_matches(channel_id, registration_id)
                    .await;
                None
            }
            ChannelLookupResult::Missing => None,
        }
    }

    async fn lookup_channel_handle(&self, channel_id: &ChannelId) -> ChannelLookupResult {
        let handle_entry = {
            let channels = self.channels.read().await;
            let Some(handle_entry) = channels.get(channel_id).cloned() else {
                return ChannelLookupResult::Missing;
            };

            handle_entry
        };

        match handle_entry.handle.upgrade() {
            Some(handle) => ChannelLookupResult::Found(handle),
            None => ChannelLookupResult::Stale(handle_entry.registration_id),
        }
    }

    pub async fn cancel_channel_branch(
        &self,
        channel_id: &ChannelId,
        branch_id: BranchId,
        reason: &str,
    ) -> ControlActionResult {
        for _ in 0..2 {
            match self.lookup_channel_handle(channel_id).await {
                ChannelLookupResult::Found(handle) => {
                    return handle.cancel_branch_with_reason(branch_id, reason).await;
                }
                ChannelLookupResult::Stale(registration_id) => {
                    self.remove_stale_channel_if_matches(channel_id, registration_id)
                        .await;
                }
                ChannelLookupResult::Missing => return ControlActionResult::NotFound,
            }
        }
        ControlActionResult::NotFound
    }

    async fn remove_stale_channel_if_matches(
        &self,
        channel_id: &ChannelId,
        expected_registration_id: u64,
    ) -> bool {
        let mut channels = self.channels.write().await;
        let should_remove = channels
            .get(channel_id)
            .is_some_and(|current| current.registration_id == expected_registration_id);

        if should_remove {
            channels.remove(channel_id);
        }

        should_remove
    }
}

fn normalize_worker_task(task: &str) -> String {
    task.trim()
        .strip_prefix("[opencode] ")
        .unwrap_or(task.trim())
        .trim()
        .to_string()
}

pub(crate) fn operation_result_or_marker(result: String, backend: WorkerBackend) -> String {
    if !result.trim().is_empty() {
        return result;
    }
    match backend {
        WorkerBackend::Builtin => {
            "Worker operation completed without a textual result.".to_string()
        }
        WorkerBackend::OpenCode => {
            "OpenCode operation completed without a textual result.".to_string()
        }
    }
}

async fn snapshot_worker(entry: &LiveWorkerEntry) -> WorkerSnapshot {
    let live = entry.live.read().await;
    let routable = match live.state {
        WorkerRuntimeState::Running => entry.control.injection_tx.is_some(),
        WorkerRuntimeState::WaitingForInput => entry.control.input_tx.is_some(),
        WorkerRuntimeState::Starting
        | WorkerRuntimeState::Cancelling
        | WorkerRuntimeState::Completing => false,
    };
    WorkerSnapshot {
        worker_id: entry.worker_id,
        registration_id: entry.registration_id,
        provenance: entry.provenance.clone(),
        backend: entry.backend,
        interactive: entry.interactive,
        routable,
        state: live.state,
        status: live.status.clone(),
        tool_calls: live.tool_calls,
        active_operation: live.active_operation.clone(),
        last_completed_operation_id: live.last_completed_operation_id,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ControlActionResult, ProcessControlRegistry, WorkerBackend, WorkerMutationResult,
        WorkerOperationContext, WorkerOperationId, WorkerProvenance, WorkerRegistryError,
        WorkerRequester, WorkerResultTarget, WorkerRuntimeControl, WorkerRuntimeState,
    };
    use crate::ProcessId;
    use crate::agent::channel::WeakChannelControlHandle;

    use std::sync::Arc;

    fn worker_id(value: u128) -> crate::WorkerId {
        uuid::Uuid::from_u128(value)
    }

    fn provenance(worker_id: crate::WorkerId, channel_id: &str, task: &str) -> WorkerProvenance {
        WorkerProvenance {
            origin_channel_id: Some(Arc::from(channel_id)),
            origin_branch_id: None,
            task: task.to_string(),
            task_id: None,
            autonomy_run_id: None,
            spawning_process: ProcessId::Worker(worker_id),
        }
    }

    fn operation(channel_id: &str) -> WorkerOperationContext {
        WorkerOperationContext {
            operation_id: WorkerOperationId::new(),
            requester: WorkerRequester::Channel {
                channel_id: Arc::from(channel_id),
            },
            result_target: WorkerResultTarget::Channel {
                channel_id: Arc::from(channel_id),
            },
            autonomy_run_id: None,
        }
    }

    fn control() -> WorkerRuntimeControl {
        WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
        )
        .0
    }

    async fn register_new_worker(
        registry: &ProcessControlRegistry,
        worker_id: crate::WorkerId,
        channel_id: &str,
        task: &str,
    ) -> (super::WorkerAdmissionToken, WorkerOperationContext) {
        let provenance = provenance(worker_id, channel_id, task);
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 4)
            .await
            .unwrap();
        let operation = operation(channel_id);
        let admission = registry
            .register_new_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                true,
                operation.clone(),
                "starting",
                control(),
            )
            .await
            .unwrap();
        (admission, operation)
    }

    async fn register_restored_worker(
        registry: &ProcessControlRegistry,
        worker_id: crate::WorkerId,
        channel_id: &str,
        task: &str,
    ) -> super::WorkerAdmissionToken {
        let provenance = provenance(worker_id, channel_id, task);
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 4)
            .await
            .unwrap();
        registry
            .register_restored_worker(
                reservation,
                provenance,
                WorkerBackend::OpenCode,
                true,
                "idle",
                0,
                control(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn duplicate_worker_registration_is_rejected() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(1);
        let provenance = provenance(worker_id, "channel-a", "compile project");
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 2)
            .await
            .unwrap();
        registry
            .register_new_worker(
                reservation,
                provenance.clone(),
                WorkerBackend::Builtin,
                false,
                operation("channel-a"),
                "starting",
                control(),
            )
            .await
            .unwrap();

        assert!(matches!(
            registry.reserve_worker(worker_id, &provenance, 2).await,
            Err(WorkerRegistryError::DuplicateWorker {
                worker_id: duplicate_worker_id,
            }) if duplicate_worker_id == worker_id
        ));
        assert_eq!(registry.list_worker_snapshots().await.len(), 1);
    }

    #[tokio::test]
    async fn restoration_requires_previous_registration_to_detach() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(2);
        let (old_admission, _) =
            register_new_worker(&registry, worker_id, "channel-a", "task-a").await;
        let old_callback = old_admission.callback_context();
        let restored_provenance = provenance(worker_id, "channel-a", "task-a");

        assert!(matches!(
            registry
                .reserve_worker(worker_id, &restored_provenance, 4)
                .await,
            Err(WorkerRegistryError::DuplicateWorker {
                worker_id: duplicate_worker_id,
            }) if duplicate_worker_id == worker_id
        ));
        assert!(
            registry
                .remove_worker_if_registration_matches(old_callback)
                .await
        );

        let restored = register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;
        let snapshot = registry.worker_snapshot(worker_id).await.unwrap();
        assert_ne!(
            old_callback.registration_id,
            restored.callback_context().registration_id
        );
        assert_eq!(snapshot.state, WorkerRuntimeState::WaitingForInput);
        assert!(snapshot.active_operation.is_none());
    }

    #[tokio::test]
    async fn restored_idle_worker_installs_execution_abort_handle_without_state_change() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(36);
        let admission = register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;
        let execution = tokio::spawn(std::future::pending::<()>());

        assert_eq!(
            registry
                .install_execution_abort_handle(
                    admission.callback_context(),
                    execution.abort_handle(),
                )
                .await,
            WorkerMutationResult::Applied
        );
        let snapshot = registry.worker_snapshot(worker_id).await.unwrap();
        assert_eq!(snapshot.state, WorkerRuntimeState::WaitingForInput);
        assert!(snapshot.active_operation.is_none());
        execution.abort();
    }

    #[tokio::test]
    async fn stale_worker_callbacks_cannot_mutate_replacement_registration() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(3);
        let (old_admission, old_operation) =
            register_new_worker(&registry, worker_id, "channel-a", "task-a").await;
        let stale_callback = old_admission.callback_context();
        assert_eq!(
            registry
                .update_worker_state(stale_callback, WorkerRuntimeState::Running)
                .await,
            WorkerMutationResult::Applied
        );
        assert!(
            registry
                .remove_worker_if_registration_matches(stale_callback)
                .await
        );
        let replacement =
            register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;

        assert!(
            !registry
                .remove_worker_if_registration_matches(stale_callback)
                .await
        );
        assert_eq!(
            registry
                .update_worker_status(stale_callback, "stale status")
                .await,
            WorkerMutationResult::StaleRegistration
        );
        assert_eq!(
            registry
                .complete_worker_operation(
                    stale_callback,
                    old_operation.operation_id,
                    "stale idle",
                )
                .await,
            WorkerMutationResult::StaleRegistration
        );

        let snapshot = registry.worker_snapshot(worker_id).await.unwrap();
        assert_eq!(
            snapshot.registration_id,
            replacement.callback_context().registration_id
        );
        assert_eq!(snapshot.status, "idle");
        assert_eq!(snapshot.state, WorkerRuntimeState::WaitingForInput);
        assert_eq!(snapshot.tool_calls, 0);
        assert!(snapshot.last_completed_operation_id.is_none());
        assert_eq!(
            registry.increment_worker_tool_calls(stale_callback).await,
            WorkerMutationResult::StaleRegistration
        );
    }

    #[tokio::test]
    async fn stale_operation_callbacks_cannot_settle_current_operation() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(4);
        let admission = register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;
        let follow_up = registry
            .claim_idle_follow_up(
                worker_id,
                WorkerRequester::System,
                WorkerResultTarget::None,
                None,
                "continue",
            )
            .await
            .unwrap();
        let stale_operation_id = WorkerOperationId::new();

        assert_eq!(
            registry
                .complete_worker_operation(
                    admission.callback_context(),
                    stale_operation_id,
                    "idle",
                )
                .await,
            WorkerMutationResult::StaleOperation
        );
        assert_eq!(
            registry
                .complete_worker_operation(
                    admission.callback_context(),
                    follow_up.operation.operation_id,
                    "idle",
                )
                .await,
            WorkerMutationResult::Applied
        );
    }

    #[tokio::test]
    async fn tool_call_counts_are_registration_fenced_in_snapshots() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(37);
        let worker_provenance = provenance(worker_id, "channel-a", "task-a");
        let reservation = registry
            .reserve_worker(worker_id, &worker_provenance, 4)
            .await
            .unwrap();
        let admission = registry
            .register_restored_worker(
                reservation,
                worker_provenance,
                WorkerBackend::Builtin,
                true,
                "idle",
                4,
                control(),
            )
            .await
            .unwrap();

        assert_eq!(
            registry
                .worker_snapshot(worker_id)
                .await
                .unwrap()
                .tool_calls,
            4
        );
        assert_eq!(
            registry
                .increment_worker_tool_calls(admission.callback_context())
                .await,
            WorkerMutationResult::Applied
        );
        assert_eq!(
            registry
                .worker_snapshot(worker_id)
                .await
                .unwrap()
                .tool_calls,
            5
        );
    }

    #[tokio::test]
    async fn normalized_tasks_are_exclusive_across_origin_channels() {
        let registry = ProcessControlRegistry::new();
        let first_worker_id = worker_id(5);
        let second_worker_id = worker_id(6);
        let first = provenance(first_worker_id, "channel-a", "compile project");
        let second = provenance(
            second_worker_id,
            "channel-b",
            "  [opencode] compile project  ",
        );
        let _reservation = registry
            .reserve_worker(first_worker_id, &first, 1)
            .await
            .unwrap();

        assert!(matches!(
            registry.reserve_worker(second_worker_id, &second, 1).await,
            Err(WorkerRegistryError::DuplicateTask {
                existing_worker_id,
            }) if existing_worker_id == first_worker_id
        ));
    }

    #[tokio::test]
    async fn origin_channel_quotas_are_independent() {
        let registry = ProcessControlRegistry::new();
        let first = provenance(worker_id(7), "channel-a", "task-a");
        let same_channel = provenance(worker_id(8), "channel-a", "task-b");
        let other_channel = provenance(worker_id(9), "channel-b", "task-c");
        let _first_reservation = registry
            .reserve_worker(worker_id(7), &first, 1)
            .await
            .unwrap();

        assert!(matches!(
            registry
                .reserve_worker(worker_id(8), &same_channel, 1)
                .await,
            Err(WorkerRegistryError::OriginChannelQuotaReached {
                channel_id,
                max_workers: 1,
            }) if channel_id.as_ref() == "channel-a"
        ));
        assert!(
            registry
                .reserve_worker(worker_id(9), &other_channel, 1)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn releasing_reservation_cleans_task_and_quota_ownership() {
        let registry = ProcessControlRegistry::new();
        let first = provenance(worker_id(10), "channel-a", "task-a");
        let replacement = provenance(worker_id(11), "channel-a", "[opencode] task-a");
        let reservation = registry
            .reserve_worker(worker_id(10), &first, 1)
            .await
            .unwrap();

        assert!(registry.release_worker_reservation(reservation).await);
        assert!(
            registry
                .reserve_worker(worker_id(11), &replacement, 1)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn concurrent_idle_follow_ups_claim_worker_once() {
        let registry = Arc::new(ProcessControlRegistry::new());
        let worker_id = worker_id(12);
        let _admission =
            register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let mut claims = Vec::new();
        for message in ["first", "second"] {
            let registry = registry.clone();
            let barrier = barrier.clone();
            claims.push(tokio::spawn(async move {
                barrier.wait().await;
                registry
                    .claim_idle_follow_up(
                        worker_id,
                        WorkerRequester::System,
                        WorkerResultTarget::None,
                        None,
                        message,
                    )
                    .await
            }));
        }
        barrier.wait().await;

        let first = claims.remove(0).await.unwrap();
        let second = claims.remove(0).await.unwrap();
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert!(matches!(
            first.as_ref().err().or_else(|| second.as_ref().err()),
            Some(WorkerRegistryError::WorkerBusy {
                state: WorkerRuntimeState::Running,
                ..
            })
        ));
        let snapshot = registry.worker_snapshot(worker_id).await.unwrap();
        assert_eq!(snapshot.state, WorkerRuntimeState::Running);
        assert!(snapshot.active_operation.is_some());
    }

    #[tokio::test]
    async fn cross_channel_follow_up_keeps_origin_and_targets_requester() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(14);
        let provenance = provenance(worker_id, "origin-channel", "task-a");
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 4)
            .await
            .unwrap();
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(1);
        let (control, _cancel_rx, _notify) = WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            Some(input_tx),
            None,
            None,
        );
        let admission = registry
            .register_restored_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                true,
                "idle",
                0,
                control,
            )
            .await
            .unwrap();
        let follow_up = registry
            .claim_idle_follow_up(
                worker_id,
                WorkerRequester::Channel {
                    channel_id: Arc::from("requesting-channel"),
                },
                WorkerResultTarget::Channel {
                    channel_id: Arc::from("requesting-channel"),
                },
                None,
                "continue",
            )
            .await
            .unwrap();
        assert_eq!(
            registry
                .deliver_claimed_follow_up(admission.callback_context(), follow_up.clone())
                .await,
            WorkerMutationResult::Applied
        );

        let delivered = input_rx.recv().await.unwrap();
        assert_eq!(delivered.operation, follow_up.operation);
        assert_eq!(
            registry
                .worker_snapshot(worker_id)
                .await
                .unwrap()
                .provenance
                .origin_channel_id
                .as_deref(),
            Some("origin-channel")
        );
        assert!(matches!(
            delivered.operation.result_target,
            WorkerResultTarget::Channel { channel_id }
                if channel_id.as_ref() == "requesting-channel"
        ));
    }

    #[tokio::test]
    async fn cancellation_backstop_does_not_abort_supervisor_or_remove_registration() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(15);
        let provenance = provenance(worker_id, "channel-a", "task-a");
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 4)
            .await
            .unwrap();
        let operation = operation("channel-a");
        let (control, _cancel_rx, _notify) = WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
        );
        let admission = registry
            .register_new_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                false,
                operation,
                "starting",
                control,
            )
            .await
            .unwrap();
        let handle = tokio::spawn(std::future::pending::<()>());
        registry
            .install_task_handle(admission.callback_context(), handle)
            .await
            .unwrap();

        assert_eq!(
            registry
                .cancel_worker_runtime(
                    worker_id,
                    "test cancel",
                    std::time::Duration::from_millis(1)
                )
                .await,
            ControlActionResult::Cancelled
        );
        let snapshot = registry.worker_snapshot(worker_id).await.unwrap();
        assert_eq!(snapshot.state, WorkerRuntimeState::Cancelling);
    }

    #[tokio::test]
    async fn origin_cleanup_cancels_only_matching_workers_and_releases_reservations() {
        let registry = ProcessControlRegistry::new();
        let cron_worker_id = worker_id(31);
        let cron_provenance = provenance(cron_worker_id, "cron:job", "live-task");
        let cron_reservation = registry
            .reserve_worker(cron_worker_id, &cron_provenance, 4)
            .await
            .unwrap();
        let (cron_control, cron_cancel_rx, _notify) = WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
        );
        registry
            .register_new_worker(
                cron_reservation,
                cron_provenance,
                WorkerBackend::Builtin,
                false,
                operation("cron:job"),
                "starting",
                cron_control,
            )
            .await
            .unwrap();

        let other_worker_id = worker_id(32);
        register_new_worker(&registry, other_worker_id, "channel-a", "other-task").await;

        let reserved_worker_id = worker_id(33);
        let reserved_provenance = provenance(reserved_worker_id, "cron:job", "reserved-task");
        let reserved = registry
            .reserve_worker(reserved_worker_id, &reserved_provenance, 4)
            .await
            .unwrap();

        let cancelled = registry
            .cancel_workers_by_origin_channel(
                &Arc::from("cron:job"),
                "test cleanup",
                std::time::Duration::from_millis(1),
            )
            .await;

        assert_eq!(cancelled, 1);
        assert_eq!(
            cron_cancel_rx.borrow().as_deref(),
            Some("test cleanup"),
            "origin cleanup should record its cancellation reason"
        );
        assert_eq!(
            registry
                .worker_snapshot(cron_worker_id)
                .await
                .unwrap()
                .state,
            WorkerRuntimeState::Cancelling
        );
        assert_eq!(
            registry
                .worker_snapshot(other_worker_id)
                .await
                .unwrap()
                .state,
            WorkerRuntimeState::Starting
        );
        assert!(!registry.release_worker_reservation(reserved).await);

        let replacement_worker_id = worker_id(34);
        let replacement = provenance(replacement_worker_id, "cron:job", "reserved-task");
        assert!(
            registry
                .reserve_worker(replacement_worker_id, &replacement, 4)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn close_admission_rejects_new_reservations_idempotently() {
        let registry = ProcessControlRegistry::new();
        assert!(registry.close_admission().await);
        assert!(!registry.close_admission().await);

        let worker_id = worker_id(13);
        assert!(matches!(
            registry
                .reserve_worker(worker_id, &provenance(worker_id, "channel-a", "task-a"), 1,)
                .await,
            Err(WorkerRegistryError::AdmissionClosed)
        ));
    }

    #[tokio::test]
    async fn close_admission_fences_reservation_promotion() {
        let registry = Arc::new(ProcessControlRegistry::new());
        let worker_id = worker_id(16);
        let worker_provenance = provenance(worker_id, "channel-a", "task-a");
        let reservation = registry
            .reserve_worker(worker_id, &worker_provenance, 1)
            .await
            .unwrap();
        let workers_guard = registry.workers.write().await;
        let registering = {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry
                    .register_new_worker(
                        reservation,
                        worker_provenance,
                        WorkerBackend::Builtin,
                        false,
                        operation("channel-a"),
                        "starting",
                        control(),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(registry.close_admission().await);
        drop(workers_guard);

        assert!(matches!(
            registering.await.unwrap(),
            Err(WorkerRegistryError::AdmissionClosed)
        ));
        assert!(registry.worker_snapshot(worker_id).await.is_none());
        assert!(registry.admissions.lock().await.owners.is_empty());
    }

    #[tokio::test]
    async fn accepted_cancellation_racing_success_cannot_commit_success() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO channels (id, platform) VALUES ('channel-a', 'test')")
            .execute(&pool)
            .await
            .unwrap();
        let logger = crate::conversation::ProcessRunLogger::new(pool.clone());
        let registry = Arc::new(ProcessControlRegistry::new());
        let worker_id = worker_id(17);
        logger
            .log_worker_started(
                Some(&Arc::from("channel-a")),
                worker_id,
                "task-a",
                "builtin",
                &Arc::from("agent"),
                false,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let worker_provenance = provenance(worker_id, "channel-a", "task-a");
        let reservation = registry
            .reserve_worker(worker_id, &worker_provenance, 4)
            .await
            .unwrap();
        let operation = operation("channel-a");
        let (control, _cancel_rx, _notify) = WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            Some(logger.clone()),
        );
        let admission = registry
            .register_new_worker(
                reservation,
                worker_provenance,
                WorkerBackend::Builtin,
                false,
                operation,
                "starting",
                control,
            )
            .await
            .unwrap();
        let callback = admission.callback_context();
        assert_eq!(
            registry
                .update_worker_state(callback, WorkerRuntimeState::Running)
                .await,
            WorkerMutationResult::Applied
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let cancelling = {
            let registry = registry.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                registry
                    .cancel_worker_runtime(worker_id, "test cancel", std::time::Duration::ZERO)
                    .await
            })
        };
        let completing = {
            let barrier = barrier.clone();
            let logger = logger.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                crate::agent::channel_dispatch::commit_worker_outcome(
                    &logger,
                    worker_id,
                    crate::conversation::WorkerOutcomeKind::Succeeded,
                    "finished",
                    None,
                    crate::conversation::WorkerTerminalOwner::Worker,
                )
                .await
                .unwrap()
            })
        };
        barrier.wait().await;

        let cancellation = cancelling.await.unwrap();
        completing.await.unwrap();
        let lifecycle = logger
            .read_worker_lifecycle(worker_id)
            .await
            .unwrap()
            .unwrap();
        if cancellation == ControlActionResult::Cancelled {
            assert_ne!(lifecycle, crate::conversation::WorkerLifecycle::Succeeded);
            assert!(matches!(
                lifecycle,
                crate::conversation::WorkerLifecycle::Cancelling
                    | crate::conversation::WorkerLifecycle::Cancelled
            ));
        } else {
            assert_eq!(cancellation, ControlActionResult::AlreadyTerminal);
            assert_eq!(lifecycle, crate::conversation::WorkerLifecycle::Succeeded);
        }
    }

    #[tokio::test]
    async fn stale_session_callback_cannot_update_replacement_worker_row() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let logger = crate::conversation::ProcessRunLogger::new(pool.clone());
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(18);
        logger
            .log_worker_started(
                None,
                worker_id,
                "task-a",
                "opencode",
                &Arc::from("agent"),
                true,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let old = register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;
        let stale_callback = old.callback_context();
        assert!(
            registry
                .remove_worker_if_registration_matches(stale_callback)
                .await
        );
        let replacement =
            register_restored_worker(&registry, worker_id, "channel-a", "task-a").await;

        assert_eq!(
            registry
                .persist_opencode_session(stale_callback, &logger, "stale-session", 1234)
                .await
                .unwrap(),
            WorkerMutationResult::StaleRegistration
        );
        assert_eq!(
            registry
                .worker_snapshot(worker_id)
                .await
                .unwrap()
                .registration_id,
            replacement.callback_context().registration_id
        );
        let metadata: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT opencode_session_id, opencode_port FROM worker_runs WHERE id = ?",
        )
        .bind(worker_id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(metadata, (None, None));
    }

    #[tokio::test]
    async fn stale_registration_cannot_emit_state_guarded_event() {
        let registry = ProcessControlRegistry::new();
        let worker_id = worker_id(35);
        let (old_admission, _) =
            register_new_worker(&registry, worker_id, "channel-a", "task-a").await;
        let old_callback = old_admission.callback_context();
        registry
            .remove_worker_if_registration_matches(old_callback)
            .await;
        let (replacement_admission, _) =
            register_new_worker(&registry, worker_id, "channel-a", "task-a").await;
        let replacement_callback = replacement_admission.callback_context();
        registry
            .update_worker_state(replacement_callback, WorkerRuntimeState::Running)
            .await;
        let emitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let emitted_for_action = emitted.clone();

        let result = registry
            .run_if_worker_state(old_callback, WorkerRuntimeState::Running, move || {
                emitted_for_action.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await;

        assert_eq!(result, WorkerMutationResult::StaleRegistration);
        assert!(!emitted.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn empty_interactive_results_have_explicit_settlement_markers() {
        assert_eq!(
            super::operation_result_or_marker(String::new(), WorkerBackend::Builtin),
            "Worker operation completed without a textual result."
        );
        assert_eq!(
            super::operation_result_or_marker("  ".to_string(), WorkerBackend::OpenCode),
            "OpenCode operation completed without a textual result."
        );
    }

    #[tokio::test]
    async fn prune_dead_channels_removes_stale_entries() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("channel-1");
        let registration_id = registry
            .register_channel(
                channel_id.clone(),
                crate::agent::channel::WeakChannelControlHandle::dangling(),
            )
            .await;

        let pruned = registry.prune_dead_channels().await;

        assert_eq!(pruned, 1);
        assert!(
            !registry
                .unregister_channel(&channel_id, registration_id)
                .await
        );
    }

    #[tokio::test]
    async fn stale_channel_entry_cleanup_only_removes_matching_registration_id() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("channel-stale-race");
        let stale_handle = WeakChannelControlHandle::dangling();

        let stale_registration_id = registry
            .register_channel(channel_id.clone(), stale_handle)
            .await;

        let active_registration_id = registry
            .register_channel(channel_id.clone(), WeakChannelControlHandle::dangling())
            .await;

        assert!(
            !registry
                .remove_stale_channel_if_matches(&channel_id, stale_registration_id)
                .await
        );
        assert!(
            !registry
                .unregister_channel(&channel_id, stale_registration_id)
                .await
        );
        assert!(
            registry
                .unregister_channel(&channel_id, active_registration_id)
                .await
        );
    }

    #[tokio::test]
    async fn cancel_missing_entries_is_idempotent_not_found() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("missing-channel");
        let branch_id = uuid::Uuid::new_v4();

        assert_eq!(
            registry
                .cancel_channel_branch(&channel_id, branch_id, "test")
                .await,
            ControlActionResult::NotFound
        );
    }

    #[tokio::test]
    async fn cancel_stale_channel_entry_prunes_then_returns_not_found() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("stale-channel");

        let registration_id = registry
            .register_channel(channel_id.clone(), WeakChannelControlHandle::dangling())
            .await;

        assert!(registry.channel_handle(&channel_id).await.is_none());
        assert!(
            !registry
                .unregister_channel(&channel_id, registration_id)
                .await,
            "stale entry should be pruned during cancellation retry path"
        );
    }
}
