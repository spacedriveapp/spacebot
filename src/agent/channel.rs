//! Channel: User-facing conversation process.

use crate::agent::channel_attachments;
use crate::agent::channel_attachments::download_attachments;
use crate::agent::channel_dispatch::{WorkerTaskPreset, spawn_memory_persistence_branch};
use crate::agent::channel_history::{
    apply_history_after_turn, event_is_for_channel, extract_message_id,
    extract_reply_from_tool_syntax, format_batched_user_message, format_user_message,
    message_display_name, pop_retrigger_bridge_message,
};
use crate::agent::channel_prompt::{
    MAX_RETRIGGERS_PER_TURN, RETRIGGER_DEBOUNCE_MS, RETRIGGER_MAX_TURNS, TemporalContext,
};
use crate::agent::compactor::Compactor;
use crate::agent::process_control::ControlActionResult;
use crate::agent::status::StatusBlock;
use crate::agent::worker::Worker;
use crate::conversation::{ChannelStore, ConversationLogger, ProcessRunLogger};
use crate::error::{AgentError, Result};
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::{
    AgentDeps, BranchId, ChannelId, InboundMessage, OutboundResponse, ProcessEvent, ProcessId,
    ProcessType, WorkerId,
};
use rig::agent::AgentBuilder;
use rig::completion::CompletionModel;
use rig::message::UserContent;
use rig::one_or_many::OneOrMany;
use rig::tool::server::ToolServer;
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Weak};
use tokio::sync::broadcast;
use tokio::sync::{RwLock, mpsc};

/// A background process result waiting to be relayed to the user via retrigger.
///
/// Instead of injecting raw result text into history as a fake "User" message
/// (where it can be confused with prior results), pending results are accumulated
/// here and embedded directly into the retrigger message text. This gives the
/// LLM unambiguous, ID-tagged results to relay.
#[derive(Clone, Debug)]
struct PendingResult {
    /// "branch" or "worker"
    process_type: &'static str,
    /// The branch or worker ID (short UUID).
    process_id: String,
    /// The result/conclusion text from the process.
    result: String,
    /// Whether the process completed successfully.
    success: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalWorkerStatus {
    Success,
    Partial,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct RetrievalWorkerSource {
    source: String,
    tool: String,
    retrieval_mode: Option<String>,
    locator: String,
    title: String,
    snippet: String,
    why_it_matters: String,
}

#[derive(Debug, Deserialize)]
struct RetrievalWorkerPayload {
    status: RetrievalWorkerStatus,
    summary: String,
    #[serde(default)]
    sources: Vec<RetrievalWorkerSource>,
    #[serde(default)]
    gaps: Vec<String>,
}

const EVENT_LAG_WARNING_INTERVAL_SECS: u64 = 30;

async fn recv_channel_event(
    event_rx: &mut broadcast::Receiver<ProcessEvent>,
) -> crate::BroadcastRecvResult<ProcessEvent> {
    crate::classify_broadcast_recv_result(event_rx.recv().await)
}

fn should_process_event_for_channel(event: &ProcessEvent, channel_id: &ChannelId) -> bool {
    event_is_for_channel(event, channel_id)
}

fn should_flush_coalesce_buffer_for_event(event: &ProcessEvent) -> bool {
    matches!(
        event,
        ProcessEvent::BranchStarted { .. }
            | ProcessEvent::BranchResult { .. }
            | ProcessEvent::WorkerStarted { .. }
            | ProcessEvent::WorkerStatus { .. }
            | ProcessEvent::WorkerComplete { .. }
    )
}

/// Shared state that channel tools need to act on the channel.
///
/// Wrapped in Arc and passed to tools (branch, spawn_worker, route, cancel)
/// so they can create real Branch/Worker processes when the LLM invokes them.
#[derive(Clone)]
pub struct ChannelState {
    pub channel_id: ChannelId,
    pub history: Arc<RwLock<Vec<rig::message::Message>>>,
    pub active_branches: Arc<RwLock<HashMap<BranchId, tokio::task::JoinHandle<()>>>>,
    pub active_workers: Arc<RwLock<HashMap<WorkerId, Worker>>>,
    /// Tokio task handles for running workers, used for cancellation via abort().
    pub worker_handles: Arc<RwLock<HashMap<WorkerId, tokio::task::JoinHandle<()>>>>,
    /// Task-shaping metadata for workers so completion results can be mediated
    /// before they are fed back through the generic retrigger path.
    pub worker_task_presets: Arc<RwLock<HashMap<WorkerId, WorkerTaskPreset>>>,
    /// Input senders for interactive workers, keyed by worker ID.
    /// Used by the route tool to deliver follow-up messages.
    pub worker_inputs: Arc<RwLock<HashMap<WorkerId, tokio::sync::mpsc::Sender<String>>>>,
    /// Injection senders for all workers, keyed by worker ID.
    /// Used by the route tool to deliver addendum context to running workers
    /// without requiring the worker to be interactive.
    pub worker_injections: Arc<RwLock<HashMap<WorkerId, tokio::sync::mpsc::Sender<String>>>>,
    /// Task descriptions reserved for spawn. Prevents the TOCTOU race where
    /// two concurrent `spawn_worker` calls both pass `check_duplicate_task`
    /// before either registers in the status block. Reservations are
    /// claimed under a write lock before any async spawn work and released
    /// when the worker is registered in the status block or the spawn fails.
    pub reserved_tasks: Arc<RwLock<HashSet<String>>>,
    pub status_block: Arc<RwLock<StatusBlock>>,
    pub deps: AgentDeps,
    pub conversation_logger: ConversationLogger,
    pub process_run_logger: ProcessRunLogger,
    /// Discord message ID to reply to for work spawned in the current turn.
    pub reply_target_message_id: Arc<RwLock<Option<String>>>,
    pub channel_store: ChannelStore,
    pub screenshot_dir: std::path::PathBuf,
    pub logs_dir: std::path::PathBuf,
}

impl ChannelState {
    /// Cancel a running worker by aborting its tokio task and cleaning up state.
    /// Returns an error message if the worker is not found.
    pub async fn cancel_worker(&self, worker_id: WorkerId) -> std::result::Result<(), String> {
        self.cancel_worker_with_reason(worker_id, "cancelled by channel")
            .await
    }

    /// Cancel a running worker by aborting its tokio task and cleaning up state.
    /// Emits a synthetic terminal event so downstream consumers converge.
    pub async fn cancel_worker_with_reason(
        &self,
        worker_id: WorkerId,
        reason: &str,
    ) -> std::result::Result<(), String> {
        let removed = self
            .active_workers
            .write()
            .await
            .remove(&worker_id)
            .is_some();
        let handle = self.worker_handles.write().await.remove(&worker_id);
        let removed_input = self
            .worker_inputs
            .write()
            .await
            .remove(&worker_id)
            .is_some();
        self.worker_injections.write().await.remove(&worker_id);
        self.worker_task_presets.write().await.remove(&worker_id);
        let removed_status = self.status_block.write().await.remove_worker(worker_id);
        let should_emit = removed || handle.is_some();

        if !should_emit {
            if removed_input || removed_status {
                return Ok(());
            }
            return Err(format!("Worker {worker_id} not found"));
        }

        if let Some(handle) = handle {
            handle.abort();
        }

        let reason = crate::summarize_first_non_empty_line(reason, crate::EVENT_SUMMARY_MAX_CHARS);
        let result = if reason.is_empty() {
            "Worker cancelled.".to_string()
        } else {
            format!("Worker cancelled: {reason}")
        };

        self.process_run_logger
            .log_worker_completed(worker_id, &result, false);
        if let Err(error) = self.deps.event_tx.send(ProcessEvent::WorkerComplete {
            agent_id: self.deps.agent_id.clone(),
            worker_id,
            channel_id: Some(self.channel_id.clone()),
            result,
            notify: true,
            success: false,
        }) {
            tracing::warn!(
                %error,
                agent_id = %self.deps.agent_id,
                worker_id = %worker_id,
                channel_id = %self.channel_id,
                "failed to emit synthetic worker completion event"
            );
        }

        Ok(())
    }

    /// Cancel a running branch by aborting its tokio task.
    /// Returns an error message if the branch is not found.
    pub async fn cancel_branch(&self, branch_id: BranchId) -> std::result::Result<(), String> {
        self.cancel_branch_with_reason(branch_id, "cancelled by channel")
            .await
    }

    /// Cancel a running branch by aborting its tokio task.
    /// Emits a synthetic terminal result so channel state converges.
    pub async fn cancel_branch_with_reason(
        &self,
        branch_id: BranchId,
        reason: &str,
    ) -> std::result::Result<(), String> {
        let handle = self.active_branches.write().await.remove(&branch_id);
        let removed_status = self.status_block.write().await.remove_branch(branch_id);
        let Some(handle) = handle else {
            if removed_status {
                return Ok(());
            }
            return Err(format!("Branch {branch_id} not found"));
        };

        handle.abort();
        let reason = crate::summarize_first_non_empty_line(reason, crate::EVENT_SUMMARY_MAX_CHARS);
        let conclusion = if reason.is_empty() {
            "Branch cancelled.".to_string()
        } else {
            format!("Branch cancelled: {reason}")
        };
        self.process_run_logger
            .log_branch_completed(branch_id, &conclusion);
        if let Err(error) = self.deps.event_tx.send(ProcessEvent::BranchResult {
            agent_id: self.deps.agent_id.clone(),
            branch_id,
            channel_id: self.channel_id.clone(),
            conclusion,
        }) {
            tracing::warn!(
                %error,
                agent_id = %self.deps.agent_id,
                branch_id = %branch_id,
                channel_id = %self.channel_id,
                "failed to emit synthetic branch result event"
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ChannelControlHandle {
    inner: Arc<ChannelControlState>,
}

struct ChannelControlState {
    state: ChannelState,
}

#[derive(Clone)]
pub struct WeakChannelControlHandle {
    inner: Weak<ChannelControlState>,
}

impl ChannelControlHandle {
    pub fn new(state: ChannelState) -> Self {
        Self {
            inner: Arc::new(ChannelControlState { state }),
        }
    }

    pub fn downgrade(&self) -> WeakChannelControlHandle {
        WeakChannelControlHandle {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub async fn cancel_worker_with_reason(
        &self,
        worker_id: WorkerId,
        reason: &str,
    ) -> ControlActionResult {
        match self
            .inner
            .state
            .cancel_worker_with_reason(worker_id, reason)
            .await
        {
            Ok(()) => ControlActionResult::Cancelled,
            Err(_) => ControlActionResult::NotFound,
        }
    }

    pub async fn cancel_branch_with_reason(
        &self,
        branch_id: BranchId,
        reason: &str,
    ) -> ControlActionResult {
        match self
            .inner
            .state
            .cancel_branch_with_reason(branch_id, reason)
            .await
        {
            Ok(()) => ControlActionResult::Cancelled,
            Err(_) => ControlActionResult::NotFound,
        }
    }
}

impl WeakChannelControlHandle {
    pub fn dangling() -> Self {
        Self { inner: Weak::new() }
    }

    pub fn upgrade(&self) -> Option<ChannelControlHandle> {
        self.inner
            .upgrade()
            .map(|inner| ChannelControlHandle { inner })
    }
}

impl std::fmt::Debug for ChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelState")
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

fn mediate_worker_result_for_retrigger(
    result: &str,
    task_preset: WorkerTaskPreset,
    process_succeeded: bool,
) -> (String, bool) {
    match task_preset {
        WorkerTaskPreset::Default => (result.to_string(), process_succeeded),
        WorkerTaskPreset::Retrieval => {
            mediate_retrieval_result_for_retrigger(result, process_succeeded)
        }
    }
}

fn normalize_retrieval_payload(result: &str) -> Cow<'_, str> {
    let trimmed = result.trim();
    let Some(fenced) = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
    else {
        return Cow::Borrowed(trimmed);
    };

    let fenced = fenced.trim();
    let normalized = match fenced.split_once('\n') {
        Some((first_line, rest)) if first_line.trim().eq_ignore_ascii_case("json") => rest,
        _ => fenced,
    };

    Cow::Owned(normalized.trim().to_string())
}

fn mediate_retrieval_result_for_retrigger(result: &str, process_succeeded: bool) -> (String, bool) {
    let trimmed = result.trim();

    if !process_succeeded {
        if trimmed.is_empty() || crate::tools::should_block_user_visible_text(trimmed) {
            return (
                "Retrieval worker failed before it could return safe structured evidence. Retry the lookup or inspect the worker transcript."
                    .to_string(),
                false,
            );
        }

        return (
            format!(
                "Retrieval worker failed before it could return structured evidence.\n\nDetails:\n{}",
                crate::tools::truncate_utf8_ellipsis(trimmed, 800)
            ),
            false,
        );
    }

    let normalized = normalize_retrieval_payload(trimmed);
    let payload: RetrievalWorkerPayload = match serde_json::from_str(&normalized) {
        Ok(payload) => payload,
        Err(_) => {
            return (
                "Retrieval worker returned malformed structured output and the result could not be safely relayed. Retry the lookup or inspect the worker transcript."
                    .to_string(),
                false,
            )
        }
    };

    if payload.summary.trim().is_empty() {
        return (
            "Retrieval worker returned structured output without a usable summary. Retry the lookup or inspect the worker transcript."
                .to_string(),
            false,
        );
    }

    if matches!(payload.status, RetrievalWorkerStatus::Success) && payload.sources.is_empty() {
        return (
            "Retrieval worker claimed success but returned no supporting evidence. Retry the lookup or inspect the worker transcript."
                .to_string(),
            false,
        );
    }

    if !matches!(payload.status, RetrievalWorkerStatus::Success) && payload.gaps.is_empty() {
        return (
            "Retrieval worker returned an incomplete result without explaining the gap. Retry the lookup or inspect the worker transcript."
                .to_string(),
            false,
        );
    }

    let mut lines = vec![payload.summary.trim().to_string()];

    if !payload.sources.is_empty() {
        lines.push("Supporting evidence:".to_string());
        for source in payload.sources.iter().take(5) {
            let title = if source.title.trim().is_empty() {
                if source.locator.trim().is_empty() {
                    source.source.trim()
                } else {
                    source.locator.trim()
                }
            } else {
                source.title.trim()
            };

            let mut metadata = vec![source.source.trim().to_string()];
            if !source.tool.trim().is_empty() {
                metadata.push(format!("via {}", source.tool.trim()));
            }
            if let Some(mode) = source.retrieval_mode.as_deref()
                && !mode.trim().is_empty()
            {
                metadata.push(mode.trim().to_string());
            }
            if !source.locator.trim().is_empty() {
                metadata.push(source.locator.trim().to_string());
            }

            lines.push(format!("- {} ({})", title, metadata.join("; ")));

            if !source.snippet.trim().is_empty() {
                lines.push(format!(
                    "  {}",
                    crate::tools::truncate_utf8_ellipsis(source.snippet.trim(), 240)
                ));
            }
            if !source.why_it_matters.trim().is_empty() {
                lines.push(format!(
                    "  Why it matters: {}",
                    crate::tools::truncate_utf8_ellipsis(source.why_it_matters.trim(), 200)
                ));
            }
        }
    }

    if !payload.gaps.is_empty() {
        lines.push("Remaining gaps:".to_string());
        for gap in payload.gaps.iter().take(5) {
            if !gap.trim().is_empty() {
                lines.push(format!(
                    "- {}",
                    crate::tools::truncate_utf8_ellipsis(gap.trim(), 200)
                ));
            }
        }
    }

    let relayed_success = !matches!(payload.status, RetrievalWorkerStatus::Unavailable);
    (lines.join("\n"), relayed_success)
}

/// User-facing conversation process.
pub struct Channel {
    pub id: ChannelId,
    pub title: Option<String>,
    pub deps: AgentDeps,
    pub hook: SpacebotHook,
    pub state: ChannelState,
    /// Per-channel tool server (isolated from other channels).
    pub tool_server: rig::tool::server::ToolServerHandle,
    /// Input channel for receiving messages.
    pub message_rx: mpsc::Receiver<InboundMessage>,
    /// Event receiver for process events.
    pub event_rx: broadcast::Receiver<ProcessEvent>,
    /// Outbound response sender for the messaging layer.
    pub response_tx: mpsc::Sender<OutboundResponse>,
    /// Self-sender for re-triggering the channel after background process completion.
    pub self_tx: mpsc::Sender<InboundMessage>,
    /// Conversation ID from the first message (for synthetic re-trigger messages).
    pub conversation_id: Option<String>,
    /// Adapter source captured from the first non-system message.
    pub source_adapter: Option<String>,
    /// Conversation context (platform, channel name, server) captured from the first message.
    pub conversation_context: Option<String>,
    /// Context monitor that triggers background compaction.
    pub compactor: Compactor,
    /// Count of user messages since last memory persistence branch.
    message_count: usize,
    /// Branch IDs for silent memory persistence branches (results not injected into history).
    memory_persistence_branches: HashSet<BranchId>,
    /// Optional Discord reply target captured when each branch was started.
    branch_reply_targets: HashMap<BranchId, String>,
    /// Buffer for coalescing rapid-fire messages.
    coalesce_buffer: Vec<InboundMessage>,
    /// Deadline for flushing the coalesce buffer.
    coalesce_deadline: Option<tokio::time::Instant>,
    /// Number of retriggers fired since the last real user message.
    retrigger_count: usize,
    /// Whether a retrigger is pending (debounce window active).
    pending_retrigger: bool,
    /// Metadata for the pending retrigger (e.g. Discord reply target).
    pending_retrigger_metadata: HashMap<String, serde_json::Value>,
    /// Deadline for firing the pending retrigger (debounce timer).
    retrigger_deadline: Option<tokio::time::Instant>,
    /// Background process results waiting to be embedded in the next retrigger.
    /// Accumulated during the debounce window and drained when the retrigger fires.
    pending_results: Vec<PendingResult>,
    /// Optional send_agent_message tool (only when agent has active links).
    send_agent_message_tool: Option<crate::tools::SendAgentMessageTool>,
    /// Channel-local reply mode toggle.
    /// When true, suppress unsolicited replies unless explicitly invoked.
    listen_only_mode: bool,
    /// Session-scoped override used when persistence is unavailable/failed.
    listen_only_session_override: Option<bool>,
    /// Handle exposed to the supervision control plane.
    control_handle: ChannelControlHandle,
}

/// RAII guard that records `message_handling_duration_seconds` when dropped,
/// ensuring the metric is observed on every exit path (including early returns
/// and `?` error propagation).
#[cfg(feature = "metrics")]
struct MessageDurationGuard {
    agent_id: String,
    channel_type: String,
    start: std::time::Instant,
}

#[cfg(feature = "metrics")]
impl Drop for MessageDurationGuard {
    fn drop(&mut self) {
        crate::telemetry::Metrics::global()
            .message_handling_duration_seconds
            .with_label_values(&[&self.agent_id, &self.channel_type])
            .observe(self.start.elapsed().as_secs_f64());
    }
}

impl Channel {
    /// Create a new channel.
    ///
    /// All tunable config (prompts, routing, thresholds, browser, skills) is read
    /// from `deps.runtime_config` on each use, so changes propagate to running
    /// channels without restart.
    pub fn new(
        id: ChannelId,
        deps: AgentDeps,
        response_tx: mpsc::Sender<OutboundResponse>,
        event_rx: broadcast::Receiver<ProcessEvent>,
        screenshot_dir: std::path::PathBuf,
        logs_dir: std::path::PathBuf,
    ) -> (Self, mpsc::Sender<InboundMessage>) {
        let process_id = ProcessId::Channel(id.clone());
        let hook = SpacebotHook::new(
            deps.agent_id.clone(),
            process_id,
            ProcessType::Channel,
            Some(id.clone()),
            deps.event_tx.clone(),
        );
        let status_block = Arc::new(RwLock::new(StatusBlock::new()));
        let history = Arc::new(RwLock::new(Vec::new()));
        let active_branches = Arc::new(RwLock::new(HashMap::new()));
        let active_workers = Arc::new(RwLock::new(HashMap::new()));
        let (message_tx, message_rx) = mpsc::channel(64);

        let conversation_logger = ConversationLogger::new(deps.sqlite_pool.clone());
        let process_run_logger = ProcessRunLogger::new(deps.sqlite_pool.clone());
        let channel_store = ChannelStore::new(deps.sqlite_pool.clone());

        let compactor = Compactor::new(id.clone(), deps.clone(), history.clone());

        let state = ChannelState {
            channel_id: id.clone(),
            history: history.clone(),
            active_branches: active_branches.clone(),
            active_workers: active_workers.clone(),
            worker_handles: Arc::new(RwLock::new(HashMap::new())),
            worker_task_presets: Arc::new(RwLock::new(HashMap::new())),
            worker_inputs: Arc::new(RwLock::new(HashMap::new())),
            worker_injections: Arc::new(RwLock::new(HashMap::new())),
            reserved_tasks: Arc::new(RwLock::new(HashSet::new())),
            status_block: status_block.clone(),
            deps: deps.clone(),
            conversation_logger,
            process_run_logger,
            reply_target_message_id: Arc::new(RwLock::new(None)),
            channel_store: channel_store.clone(),
            screenshot_dir,
            logs_dir,
        };

        // Each channel gets its own isolated tool server to avoid races between
        // concurrent channels sharing per-turn add/remove cycles.
        let tool_server = ToolServer::new().run();

        // Construct the send_agent_message tool if this agent has links.
        let send_agent_message_tool = {
            let has_links =
                !crate::links::links_for_agent(&deps.links.load(), &deps.agent_id).is_empty();
            if has_links {
                Some(crate::tools::SendAgentMessageTool::new(
                    deps.agent_id.clone(),
                    deps.links.clone(),
                    deps.agent_names.clone(),
                    deps.task_store_registry.clone(),
                    ConversationLogger::new(deps.sqlite_pool.clone()),
                ))
            } else {
                None
            }
        };

        let self_tx = message_tx.clone();
        let resolved_listen_only_mode = deps.runtime_config.channel_config.load().listen_only_mode;
        let control_handle = ChannelControlHandle::new(state.clone());
        let channel = Self {
            id: id.clone(),
            title: None,
            deps,
            hook,
            state,
            tool_server,
            message_rx,
            event_rx,
            response_tx,
            self_tx,
            conversation_id: None,
            source_adapter: None,
            conversation_context: None,
            compactor,
            message_count: 0,
            memory_persistence_branches: HashSet::new(),
            branch_reply_targets: HashMap::new(),
            coalesce_buffer: Vec::new(),
            coalesce_deadline: None,
            retrigger_count: 0,
            pending_retrigger: false,
            pending_retrigger_metadata: HashMap::new(),
            retrigger_deadline: None,
            pending_results: Vec::new(),
            send_agent_message_tool,
            listen_only_mode: resolved_listen_only_mode,
            listen_only_session_override: None,
            control_handle,
        };

        (channel, message_tx)
    }

    /// Get the agent's display name (falls back to agent ID).
    fn agent_display_name(&self) -> &str {
        self.deps
            .agent_names
            .get(self.deps.agent_id.as_ref())
            .map(String::as_str)
            .unwrap_or(self.deps.agent_id.as_ref())
    }

    fn current_adapter(&self) -> Option<&str> {
        self.source_adapter
            .as_deref()
            .or_else(|| {
                self.conversation_id
                    .as_deref()
                    .and_then(|conversation_id| conversation_id.split(':').next())
            })
            .filter(|adapter| !adapter.is_empty())
    }

    fn sync_listen_only_mode_from_runtime(&mut self) {
        if let Some(override_mode) = self.listen_only_session_override {
            self.listen_only_mode = override_mode;
            return;
        }
        let runtime_default = self
            .deps
            .runtime_config
            .channel_config
            .load()
            .listen_only_mode;
        let explicit_listen_only = **self.deps.runtime_config.channel_listen_only_explicit.load();
        let settings_store = self
            .deps
            .runtime_config
            .settings
            .load()
            .as_ref()
            .as_ref()
            .cloned();
        self.listen_only_mode = if explicit_listen_only.is_some() {
            runtime_default
        } else if let Some(settings_store) = settings_store {
            match settings_store.channel_listen_only_mode_for(self.id.as_ref()) {
                Ok(Some(enabled)) => enabled,
                Ok(None) => runtime_default,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        channel_id = %self.id,
                        "failed to sync channel-scoped listen_only_mode setting"
                    );
                    runtime_default
                }
            }
        } else {
            runtime_default
        };
    }

    fn set_listen_only_mode(&mut self, enabled: bool) -> bool {
        let mut persisted = false;
        let settings_store = self
            .deps
            .runtime_config
            .settings
            .load()
            .as_ref()
            .as_ref()
            .cloned();
        if let Some(settings_store) = settings_store {
            match settings_store.set_channel_listen_only_mode_for(self.id.as_ref(), enabled) {
                Ok(()) => persisted = true,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        channel_id = %self.id,
                        listen_only_mode = enabled,
                        "failed to persist listen_only_mode setting"
                    );
                }
            }
        } else {
            tracing::warn!(
                channel_id = %self.id,
                listen_only_mode = enabled,
                "settings store unavailable; listen_only_mode is session-scoped"
            );
        }

        self.listen_only_mode = enabled;
        self.listen_only_session_override = if persisted { None } else { Some(enabled) };
        persisted
    }

    fn persist_inbound_user_message(
        &self,
        message: &InboundMessage,
        raw_text: &str,
        saved_attachments: Option<&[channel_attachments::SavedAttachmentMeta]>,
    ) {
        if message.source == "system" {
            return;
        }
        let sender_name = message
            .metadata
            .get("sender_display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&message.sender_id);

        // If attachments were saved, enrich the metadata with their info
        let metadata = if let Some(saved) = saved_attachments {
            let mut enriched = message.metadata.clone();
            if let Ok(attachments_json) = serde_json::to_value(saved) {
                enriched.insert("attachments".to_string(), attachments_json);
            }
            enriched
        } else {
            message.metadata.clone()
        };

        self.state.conversation_logger.log_user_message(
            &self.state.channel_id,
            sender_name,
            &message.sender_id,
            raw_text,
            &metadata,
        );
        self.state
            .channel_store
            .upsert(&message.conversation_id, &metadata);
    }

    fn suppress_plaintext_fallback(&self) -> bool {
        matches!(self.current_adapter(), Some("email"))
    }

    /// Return a handle that allows external supervision to cancel this channel's
    /// workers and branches without direct access to Channel internals.
    pub fn control_handle(&self) -> ChannelControlHandle {
        self.control_handle.clone()
    }

    fn rewrite_tool_routed_command_prompt(&self, raw_text: &str) -> Option<String> {
        match raw_text.trim() {
            "/tasks" => Some(
                "use channel tools to fetch my ready tasks (limit 10) and reply exactly with:\n\
                 - header: tasks (ready):\n\
                 - each line: - #<task_number> [<priority>] <title>\n\
                 if no tasks are ready, reply exactly: tasks (ready): none"
                    .to_string(),
            ),
            "/today" => Some(
                "use channel tools to build a local tasks snapshot and reply exactly in this format:\n\
                 - first line: today (local tasks snapshot):\n\
                 - section 1: in-progress tasks (up to 5), each line:   #<task_number> [<priority>] <title>\n\
                 - section 2: up next ready tasks (up to 5), each line:   #<task_number> [<priority>] <title>\n\
                 if a section is empty use:\n\
                 - in progress: none\n\
                 - up next (ready): none"
                    .to_string(),
            ),
            "/digest" => Some(
                "using available tools and channel context, generate a concise day digest from local 00:00 to now with exactly this order:\n\
                 1) top decisions\n\
                 2) key convo themes\n\
                 3) open loops\n\
                 keep it practical and concise; if there are no meaningful updates, reply exactly: no material updates today."
                    .to_string(),
            ),
            _ => None,
        }
    }

    fn compute_listen_mode_invocation(
        &self,
        message: &InboundMessage,
        raw_text: &str,
    ) -> (bool, bool, bool) {
        let text = raw_text.trim();
        let invoked_by_command = text.starts_with('/');
        let invoked_by_mention = match message.source.as_str() {
            "telegram" => {
                let text_lower = text.to_lowercase();
                message
                    .metadata
                    .get("telegram_bot_username")
                    .and_then(|v| v.as_str())
                    .map(|username| {
                        let mention = format!("@{}", username.to_lowercase());
                        text_lower.match_indices(&mention).any(|(start, _)| {
                            let end = start + mention.len();
                            let before_ok = start == 0
                                || text_lower[..start].chars().next_back().is_none_or(
                                    |character| {
                                        !(character.is_ascii_alphanumeric() || character == '_')
                                    },
                                );
                            let after_ok = end == text_lower.len()
                                || text_lower[end..].chars().next().is_none_or(|character| {
                                    !(character.is_ascii_alphanumeric() || character == '_')
                                });
                            before_ok && after_ok
                        })
                    })
                    .unwrap_or(false)
            }
            "discord" => message
                .metadata
                .get("discord_mentioned_bot")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "slack" => message
                .metadata
                .get("slack_mentions_or_replies_to_bot")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "twitch" => message
                .metadata
                .get("twitch_mentions_or_replies_to_bot")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            _ => false,
        };
        let invoked_by_reply = match message.source.as_str() {
            // Use bot-specific reply metadata; generic reply_to_is_bot can
            // match unrelated bots and cause false invokes.
            "discord" => message
                .metadata
                .get("discord_reply_to_bot")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "telegram" => {
                let reply_to_is_bot = message
                    .metadata
                    .get("reply_to_is_bot")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let bot_username = message
                    .metadata
                    .get("telegram_bot_username")
                    .and_then(|v| v.as_str())
                    .map(str::to_lowercase);
                let reply_username = message
                    .metadata
                    .get("reply_to_username")
                    .and_then(|v| v.as_str())
                    .map(str::to_lowercase);
                reply_to_is_bot
                    && reply_username
                        .zip(bot_username)
                        .is_some_and(|(reply, bot)| bot == reply)
            }
            _ => message
                .metadata
                .get("reply_to_is_bot")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };

        (invoked_by_command, invoked_by_mention, invoked_by_reply)
    }

    async fn send_builtin_text(&mut self, text: String, log_label: &str) {
        match self
            .response_tx
            .send(OutboundResponse::Text(text.clone()))
            .await
        {
            Ok(()) => {
                #[cfg(feature = "metrics")]
                {
                    let channel_type = self.current_adapter().unwrap_or("unknown");
                    crate::telemetry::Metrics::global()
                        .messages_sent_total
                        .with_label_values(&[&self.deps.agent_id, channel_type])
                        .inc();
                }
                self.state.conversation_logger.log_bot_message_with_name(
                    &self.state.channel_id,
                    &text,
                    Some(self.agent_display_name()),
                );
            }
            Err(error) => {
                #[cfg(feature = "metrics")]
                {
                    let channel_type = self.current_adapter().unwrap_or("unknown");
                    crate::telemetry::Metrics::global()
                        .channel_errors_total
                        .with_label_values(&[&self.deps.agent_id, channel_type, "send_failed"])
                        .inc();
                }
                tracing::error!(%error, channel_id = %self.id, %log_label, "failed to send built-in reply");
            }
        }
    }

    async fn try_handle_builtin_ops_commands(
        &mut self,
        raw_text: &str,
        message: &InboundMessage,
    ) -> Result<bool> {
        if message.source == "system" {
            return Ok(false);
        }
        let supported_source = matches!(
            message.source.as_str(),
            "telegram" | "discord" | "slack" | "twitch"
        );
        if !supported_source {
            return Ok(false);
        }

        let text = raw_text.trim();
        if !text.starts_with('/') {
            return Ok(false);
        }

        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let now_line = temporal_context.current_time_line();

        match text {
            "/status" => {
                let routing = self.deps.runtime_config.routing.load();
                let channel_model = routing.resolve(ProcessType::Channel, None).to_string();
                let branch_model = routing.resolve(ProcessType::Branch, None).to_string();
                let mode = if self.listen_only_mode {
                    "quiet"
                } else {
                    "active"
                };
                let adapter = self.current_adapter().unwrap_or("unknown");
                let body = format!(
                    "status\n\
                     - agent: {}\n\
                     - channel: {}\n\
                     - adapter: {}\n\
                     - mode: {} (quiet => only command/@mention/reply-to-bot)\n\
                     - channel model: {}\n\
                     - branch model: {}\n\
                     - time: {}",
                    self.deps.agent_id,
                    self.id,
                    adapter,
                    mode,
                    channel_model,
                    branch_model,
                    now_line
                );
                self.send_builtin_text(body, "status").await;
                return Ok(true);
            }
            "/quiet" => {
                let persisted = self.set_listen_only_mode(true);
                let body = if persisted {
                    "quiet mode enabled. i'll only reply to commands, @mentions, or replies to my message."
                        .to_string()
                } else {
                    "quiet mode enabled for this session, but persistence failed; it may revert after restart.".to_string()
                };
                self.send_builtin_text(body, "quiet").await;
                return Ok(true);
            }
            "/active" => {
                let persisted = self.set_listen_only_mode(false);
                let body = if persisted {
                    "active mode enabled. i'll respond normally in this chat.".to_string()
                } else {
                    "active mode enabled for this session, but persistence failed; it may revert after restart.".to_string()
                };
                self.send_builtin_text(body, "active").await;
                return Ok(true);
            }
            "/help" => {
                let lines = [
                    "commands:".to_string(),
                    "- /status: current mode, models, binding snapshot".to_string(),
                    "- /today: in-progress + ready task snapshot".to_string(),
                    "- /tasks: ready task list".to_string(),
                    "- /digest: one-shot day digest (00:00 -> now)".to_string(),
                    "- /quiet: listen-only mode".to_string(),
                    "- /active: normal reply mode".to_string(),
                    "- /agent-id: runtime agent id".to_string(),
                ];
                let body = lines.join("\n");
                self.send_builtin_text(body, "help").await;
                return Ok(true);
            }
            _ => {}
        }

        Ok(false)
    }

    /// Run the channel event loop.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!(channel_id = %self.id, "channel started");
        let mut lagged_events_since_warning: u64 = 0;
        let mut last_lag_warning: Option<std::time::Instant> = None;

        loop {
            // Compute next deadline from coalesce and retrigger timers
            let next_deadline = match (self.coalesce_deadline, self.retrigger_deadline) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            let sleep_duration = next_deadline
                .map(|deadline| {
                    let now = tokio::time::Instant::now();
                    if deadline > now {
                        deadline - now
                    } else {
                        std::time::Duration::from_millis(1)
                    }
                })
                .unwrap_or(std::time::Duration::from_secs(3600)); // Default long timeout if no deadline

            tokio::select! {
                Some(message) = self.message_rx.recv() => {
                    let config = self.deps.runtime_config.coalesce.load();
                    if self.should_coalesce(&message, &config) {
                        self.coalesce_buffer.push(message);
                        self.update_coalesce_deadline(&config).await;
                    } else {
                        // Flush any pending buffer before handling this message
                        if let Err(error) = self.flush_coalesce_buffer().await {
                            tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer");
                        }
                        if let Err(error) = self.handle_message(message).await {
                            tracing::error!(%error, channel_id = %self.id, "error handling message");
                        }
                    }
                }
                event = recv_channel_event(&mut self.event_rx) => {
                    match event {
                        crate::BroadcastRecvResult::Event(event) => {
                            if !should_process_event_for_channel(&event, &self.id) {
                                continue;
                            }
                            // Worker/branch lifecycle events bypass coalescing.
                            if should_flush_coalesce_buffer_for_event(&event)
                                && let Err(error) = self.flush_coalesce_buffer().await
                            {
                                tracing::error!(
                                    %error,
                                    channel_id = %self.id,
                                    "error flushing coalesce buffer"
                                );
                            }
                            if let Err(error) = self.handle_event(event).await {
                                tracing::error!(%error, channel_id = %self.id, "error handling event");
                            }
                        }
                        crate::BroadcastRecvResult::Lagged(skipped) => {
                            #[cfg(feature = "metrics")]
                            crate::telemetry::Metrics::global()
                                .event_receiver_lagged_events_total
                                .with_label_values(&[&*self.deps.agent_id, "channel_control"])
                                .inc_by(skipped);

                            if let Some(skipped) = crate::drain_lag_warning_count(
                                &mut lagged_events_since_warning,
                                &mut last_lag_warning,
                                skipped,
                                std::time::Duration::from_secs(
                                    EVENT_LAG_WARNING_INTERVAL_SECS,
                                ),
                            ) {
                                tracing::warn!(
                                    channel_id = %self.id,
                                    skipped,
                                    "channel event receiver lagged, dropping old events"
                                );
                            }
                        }
                        crate::BroadcastRecvResult::Closed => {
                            tracing::info!(channel_id = %self.id, "channel event bus closed, stopping channel");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(sleep_duration), if next_deadline.is_some() => {
                    let now = tokio::time::Instant::now();
                    // Check coalesce deadline
                    if self.coalesce_deadline.is_some_and(|d| d <= now)
                        && let Err(error) = self.flush_coalesce_buffer().await
                    {
                        tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer on deadline");
                    }
                    // Check retrigger deadline
                    if self.retrigger_deadline.is_some_and(|d| d <= now) {
                        self.flush_pending_retrigger().await;
                    }
                }
                else => break,
            }
        }

        // Flush any remaining buffer before shutting down
        if let Err(error) = self.flush_coalesce_buffer().await {
            tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer on shutdown");
        }

        tracing::info!(channel_id = %self.id, "channel stopped");
        Ok(())
    }

    /// Determine if a message should be coalesced (batched with other messages).
    ///
    /// Returns false for:
    /// - System re-trigger messages (always process immediately)
    /// - Messages when coalescing is disabled
    /// - Messages in DMs when multi_user_only is true
    fn should_coalesce(
        &self,
        message: &InboundMessage,
        config: &crate::config::CoalesceConfig,
    ) -> bool {
        if !config.enabled {
            return false;
        }
        if message.source == "system" {
            return false;
        }
        if config.multi_user_only && self.is_dm() {
            return false;
        }
        // Built-in slash commands should execute immediately and never be batched.
        let looks_like_command = match &message.content {
            crate::MessageContent::Text(text) => text.trim_start().starts_with('/'),
            crate::MessageContent::Media { text, .. } => text
                .as_deref()
                .is_some_and(|value| value.trim_start().starts_with('/')),
            crate::MessageContent::Interaction { .. } => false,
        };
        if looks_like_command {
            return false;
        }
        true
    }

    /// Check if this is a DM (direct message) conversation based on conversation_id.
    fn is_dm(&self) -> bool {
        // Check conversation_id pattern for DM indicators
        if let Some(ref conv_id) = self.conversation_id {
            conv_id.contains(":dm:")
                || conv_id.starts_with("discord:dm:")
                || conv_id.starts_with("slack:dm:")
        } else {
            // If no conversation_id set yet, default to not DM (safer)
            false
        }
    }

    /// Update the coalesce deadline based on buffer size and config.
    async fn update_coalesce_deadline(&mut self, config: &crate::config::CoalesceConfig) {
        let now = tokio::time::Instant::now();

        if let Some(first_message) = self.coalesce_buffer.first() {
            let elapsed_since_first =
                chrono::Utc::now().signed_duration_since(first_message.timestamp);
            let elapsed_millis = elapsed_since_first.num_milliseconds().max(0) as u64;

            let max_wait_ms = config.max_wait_ms;
            let debounce_ms = config.debounce_ms;

            // If we have enough messages to trigger coalescing (min_messages threshold)
            if self.coalesce_buffer.len() >= config.min_messages {
                // Cap at max_wait from the first message
                let remaining_wait_ms = max_wait_ms.saturating_sub(elapsed_millis);
                let max_deadline = now + std::time::Duration::from_millis(remaining_wait_ms);

                // If no deadline set yet, use debounce window
                // Otherwise, keep existing deadline (don't extend past max_wait)
                if self.coalesce_deadline.is_none() {
                    let new_deadline = now + std::time::Duration::from_millis(debounce_ms);
                    self.coalesce_deadline = Some(new_deadline.min(max_deadline));
                } else {
                    // Already have a deadline, cap it at max_wait
                    self.coalesce_deadline = self.coalesce_deadline.map(|d| d.min(max_deadline));
                }
            } else {
                // Not enough messages yet - set a short debounce window
                let new_deadline = now + std::time::Duration::from_millis(debounce_ms);
                self.coalesce_deadline = Some(new_deadline);
            }
        }
    }

    /// Flush the coalesce buffer by processing all buffered messages.
    ///
    /// If there's only one message, process it normally.
    /// If there are multiple messages, batch them into a single turn.
    async fn flush_coalesce_buffer(&mut self) -> Result<()> {
        if self.coalesce_buffer.is_empty() {
            return Ok(());
        }

        self.coalesce_deadline = None;

        let messages: Vec<InboundMessage> = std::mem::take(&mut self.coalesce_buffer);

        if messages.len() == 1 {
            // Single message - process normally
            let message = messages
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("empty iterator after length check"))?;
            self.handle_message(message).await
        } else {
            // Multiple messages - batch them
            self.handle_message_batch(messages).await
        }
    }

    /// Handle a batch of messages as a single LLM turn.
    ///
    /// Formats all messages with attribution and timestamps, persists each
    /// individually to conversation history, then presents them as one user turn
    /// with a coalesce hint telling the LLM this is a fast-moving conversation.
    #[tracing::instrument(skip(self, messages), fields(channel_id = %self.id, agent_id = %self.deps.agent_id, message_count = messages.len()))]
    async fn handle_message_batch(&mut self, messages: Vec<InboundMessage>) -> Result<()> {
        // Apply runtime-config updates immediately without requiring a restart.
        self.sync_listen_only_mode_from_runtime();

        let message_count = messages.len();
        let batch_start_timestamp = messages
            .iter()
            .map(|message| message.timestamp)
            .min()
            .unwrap_or_else(chrono::Utc::now);
        let batch_tail_timestamp = messages
            .iter()
            .map(|message| message.timestamp)
            .max()
            .unwrap_or(batch_start_timestamp);
        let elapsed = batch_tail_timestamp.signed_duration_since(batch_start_timestamp);
        let elapsed_secs = elapsed.num_milliseconds() as f64 / 1000.0;

        tracing::info!(
            channel_id = %self.id,
            message_count,
            elapsed_secs,
            "handling batched messages"
        );

        #[cfg(feature = "metrics")]
        let metrics_channel_type = messages
            .iter()
            .find(|m| m.source != "system")
            .map(|m| m.source.clone())
            .or_else(|| self.current_adapter().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        #[cfg(feature = "metrics")]
        let _duration_guard = MessageDurationGuard {
            agent_id: self.deps.agent_id.to_string(),
            channel_type: metrics_channel_type.clone(),
            start: std::time::Instant::now(),
        };

        // Increment messages_received_total for each non-system message in the batch
        #[cfg(feature = "metrics")]
        {
            let received_count = messages.iter().filter(|m| m.source != "system").count() as u64;
            if received_count > 0 {
                crate::telemetry::Metrics::global()
                    .messages_received_total
                    .with_label_values(&[&self.deps.agent_id, &metrics_channel_type])
                    .inc_by(received_count);
            }
        }

        // Count unique senders for the hint
        let unique_senders: std::collections::HashSet<_> =
            messages.iter().map(|m| &m.sender_id).collect();
        let unique_sender_count = unique_senders.len();

        // Track conversation_id from the first message
        if self.conversation_id.is_none()
            && let Some(first) = messages.first()
        {
            self.conversation_id = Some(first.conversation_id.clone());
        }

        if self.source_adapter.is_none()
            && let Some(first) = messages.first()
            && first.source != "system"
        {
            self.source_adapter = Some(first.source.clone());
        }

        // Capture conversation context from the first message
        if self.conversation_context.is_none()
            && let Some(first) = messages.first()
        {
            let prompt_engine = self.deps.runtime_config.prompts.load();
            let server_name = first
                .metadata
                .get(crate::metadata_keys::SERVER_NAME)
                .and_then(|v| v.as_str());
            let channel_name = first
                .metadata
                .get(crate::metadata_keys::CHANNEL_NAME)
                .and_then(|v| v.as_str());
            self.conversation_context = Some(prompt_engine.render_conversation_context(
                &first.source,
                server_name,
                channel_name,
            )?);
        }

        // Persist each message to conversation log (individual audit trail)
        let save_attachments_enabled = self
            .deps
            .runtime_config
            .channel_config
            .load()
            .save_attachments;
        let saved_dir = self.deps.runtime_config.saved_dir();

        // Entries: (formatted_text, attachments, optional saved bytes per attachment)
        let mut pending_batch_entries: Vec<(
            String,
            Vec<crate::Attachment>,
            Option<Vec<channel_attachments::SavedAttachmentWithBytes>>,
        )> = Vec::new();
        let mut conversation_id = String::new();
        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let mut batch_has_invoke = false;

        for message in &messages {
            if message.source != "system" {
                let sender_name = message
                    .metadata
                    .get("sender_display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&message.sender_id);

                let (raw_text, attachments) = match &message.content {
                    crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
                    crate::MessageContent::Media { text, attachments } => {
                        (text.clone().unwrap_or_default(), attachments.clone())
                    }
                    // Render interactions as their Display form so the LLM sees plain text.
                    crate::MessageContent::Interaction { .. } => {
                        (message.content.to_string(), Vec::new())
                    }
                };

                if self.listen_only_mode {
                    let (invoked_by_command, invoked_by_mention, invoked_by_reply) =
                        self.compute_listen_mode_invocation(message, &raw_text);
                    batch_has_invoke |=
                        invoked_by_command || invoked_by_mention || invoked_by_reply;
                }

                // Save attachments to disk when enabled
                let saved_data = if save_attachments_enabled && !attachments.is_empty() {
                    Some(
                        channel_attachments::save_channel_attachments(
                            &self.deps.sqlite_pool,
                            self.deps.llm_manager.http_client(),
                            self.state.channel_id.as_ref(),
                            &saved_dir,
                            &attachments,
                        )
                        .await,
                    )
                } else {
                    None
                };

                // Enrich metadata with saved attachment info
                let metadata = if let Some(ref data) = saved_data {
                    let metas: Vec<_> = data.iter().map(|(meta, _)| meta.clone()).collect();
                    let mut enriched = message.metadata.clone();
                    if let Ok(json) = serde_json::to_value(&metas) {
                        enriched.insert("attachments".to_string(), json);
                    }
                    enriched
                } else {
                    message.metadata.clone()
                };

                self.state.conversation_logger.log_user_message(
                    &self.state.channel_id,
                    sender_name,
                    &message.sender_id,
                    &raw_text,
                    &metadata,
                );
                self.state
                    .channel_store
                    .upsert(&message.conversation_id, &metadata);

                conversation_id = message.conversation_id.clone();

                // Include both absolute and relative time context.
                let relative_secs = batch_tail_timestamp
                    .signed_duration_since(message.timestamp)
                    .num_seconds()
                    .max(0);
                let relative_text = if relative_secs < 1 {
                    "just now".to_string()
                } else if relative_secs < 60 {
                    format!("{}s ago", relative_secs)
                } else {
                    format!("{}m ago", relative_secs / 60)
                };
                let absolute_timestamp = temporal_context.format_timestamp(message.timestamp);

                let display_name = message_display_name(message);

                let formatted_text = format_batched_user_message(
                    display_name,
                    &absolute_timestamp,
                    &relative_text,
                    &raw_text,
                );

                pending_batch_entries.push((formatted_text, attachments, saved_data));
            }
        }

        if self.listen_only_mode && !batch_has_invoke {
            tracing::debug!(
                channel_id = %self.id,
                message_count,
                "listen-first mode: suppressing unsolicited coalesced batch"
            );
            // Keep passive memory capture behavior aligned with single-message flow.
            self.message_count += message_count;
            self.check_memory_persistence().await;
            return Ok(());
        }

        let mut user_contents: Vec<UserContent> = Vec::new();
        for (formatted_text, attachments, saved_data) in pending_batch_entries {
            if !attachments.is_empty() {
                let attachment_content = if let Some(ref saved) = saved_data {
                    let mut content = Vec::new();
                    let mut unsaved = Vec::new();
                    for (index, attachment) in attachments.iter().enumerate() {
                        if let Some((_, bytes)) = saved.get(index) {
                            if attachment.mime_type.starts_with("audio/") {
                                unsaved.push(attachment.clone());
                            } else {
                                content.push(channel_attachments::content_from_bytes(
                                    bytes, attachment,
                                ));
                            }
                        } else {
                            unsaved.push(attachment.clone());
                        }
                    }
                    if !unsaved.is_empty() {
                        content.extend(download_attachments(&self.deps, &unsaved).await);
                    }
                    content
                } else {
                    download_attachments(&self.deps, &attachments).await
                };
                for content in attachment_content {
                    user_contents.push(content);
                }
            }
            user_contents.push(UserContent::text(formatted_text));
        }

        // Separate text and non-text (image/audio) content
        let mut text_parts = Vec::new();
        let mut attachment_parts = Vec::new();
        for content in user_contents {
            match content {
                UserContent::Text(t) => text_parts.push(t.text.clone()),
                other => attachment_parts.push(other),
            }
        }

        let combined_text = format!(
            "[{} messages arrived rapidly in this channel]\n\n{}",
            message_count,
            text_parts.join("\n")
        );

        // Build system prompt with coalesce hint
        let system_prompt = self
            .build_system_prompt_with_coalesce(message_count, elapsed_secs, unique_sender_count)
            .await?;

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = messages.iter().rev().find_map(extract_message_id);
        }

        // Run agent turn with any image/audio attachments preserved
        let (result, skip_flag, replied_flag, _) = self
            .run_agent_turn(
                &combined_text,
                &system_prompt,
                &conversation_id,
                attachment_parts,
                false, // not a retrigger
            )
            .await?;

        self.handle_agent_result(result, &skip_flag, &replied_flag, false)
            .await;
        // Check compaction
        if let Err(error) = self.compactor.check_and_compact().await {
            tracing::warn!(channel_id = %self.id, %error, "compaction check failed");
        }

        // Increment message counter for memory persistence
        self.message_count += message_count;
        self.check_memory_persistence().await;

        Ok(())
    }

    /// Build system prompt with coalesce hint for batched messages.
    async fn build_system_prompt_with_coalesce(
        &self,
        message_count: usize,
        elapsed_secs: f64,
        unique_senders: usize,
    ) -> Result<String> {
        let rc = &self.deps.runtime_config;
        let prompt_engine = rc.prompts.load();

        let identity_context = rc.identity.load().render();
        let memory_bulletin = rc.memory_bulletin.load();
        let skills = rc.skills.load();
        let skills_prompt = skills.render_channel_prompt(&prompt_engine)?;

        let browser_enabled = rc.browser_config.load().enabled;
        let web_search_enabled = rc.brave_search_key.load().is_some();
        let opencode_enabled = rc.opencode.load().enabled;
        let sandbox_enabled = self.deps.sandbox.containment_active();
        let mcp_tool_names = self.deps.mcp_manager.get_tool_names().await;
        let worker_capabilities = prompt_engine.render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
            &mcp_tool_names,
        )?;

        let temporal_context = TemporalContext::from_runtime(rc.as_ref());
        let current_time_line = temporal_context.current_time_line();
        let status_text = {
            let status = self.state.status_block.read().await;
            status.render_with_time_context(Some(&current_time_line))
        };

        // Render coalesce hint
        let elapsed_str = format!("{:.1}s", elapsed_secs);
        let coalesce_hint = prompt_engine
            .render_coalesce_hint(message_count, &elapsed_str, unique_senders)
            .ok();

        let available_channels = self.build_available_channels().await;

        let org_context = self.build_org_context(&prompt_engine);

        let adapter_prompt = self
            .current_adapter()
            .and_then(|adapter| prompt_engine.render_channel_adapter_prompt(adapter));

        let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };

        let project_context = self.build_project_context(&prompt_engine).await;

        prompt_engine.render_channel_prompt_with_links(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            empty_to_none(skills_prompt),
            worker_capabilities,
            self.conversation_context.clone(),
            empty_to_none(status_text),
            coalesce_hint,
            available_channels,
            sandbox_enabled,
            org_context,
            adapter_prompt,
            project_context,
        )
    }

    /// Handle an incoming message by running the channel's LLM agent loop.
    ///
    /// The LLM decides which tools to call: reply (to respond), branch (to think),
    /// spawn_worker (to delegate), route (to follow up with a worker), cancel, or
    /// memory_save. The tools act on the channel's shared state directly.
    #[tracing::instrument(skip(self, message), fields(channel_id = %self.id, agent_id = %self.deps.agent_id, message_id = %message.id))]
    async fn handle_message(&mut self, message: InboundMessage) -> Result<()> {
        // Apply runtime-config updates immediately without requiring a restart.
        self.sync_listen_only_mode_from_runtime();

        tracing::info!(
            channel_id = %self.id,
            message_id = %message.id,
            "handling message"
        );

        #[cfg(feature = "metrics")]
        let _duration_guard = {
            let channel_type = if message.source != "system" {
                message.source.clone()
            } else {
                self.current_adapter().unwrap_or("unknown").to_string()
            };
            MessageDurationGuard {
                agent_id: self.deps.agent_id.to_string(),
                channel_type,
                start: std::time::Instant::now(),
            }
        };

        // Increment messages_received_total for non-system messages
        #[cfg(feature = "metrics")]
        if message.source != "system" {
            crate::telemetry::Metrics::global()
                .messages_received_total
                .with_label_values(&[&self.deps.agent_id, &message.source])
                .inc();
        }

        // Track conversation_id for synthetic re-trigger messages
        if self.conversation_id.is_none() {
            self.conversation_id = Some(message.conversation_id.clone());
        }

        if self.source_adapter.is_none() && message.source != "system" {
            self.source_adapter = Some(message.source.clone());
        }

        let (raw_text, attachments) = match &message.content {
            crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
            crate::MessageContent::Media { text, attachments } => {
                (text.clone().unwrap_or_default(), attachments.clone())
            }
            // Render interactions as their Display form so the LLM sees plain text.
            crate::MessageContent::Interaction { .. } => (message.content.to_string(), Vec::new()),
        };

        // Save attachments to disk when enabled, capturing bytes for LLM reuse
        let save_attachments_enabled = self
            .deps
            .runtime_config
            .channel_config
            .load()
            .save_attachments;
        let saved_attachment_data = if save_attachments_enabled && !attachments.is_empty() {
            let saved_dir = self.deps.runtime_config.saved_dir();
            Some(
                channel_attachments::save_channel_attachments(
                    &self.deps.sqlite_pool,
                    self.deps.llm_manager.http_client(),
                    self.state.channel_id.as_ref(),
                    &saved_dir,
                    &attachments,
                )
                .await,
            )
        } else {
            None
        };

        let saved_metas: Option<Vec<_>> = saved_attachment_data
            .as_ref()
            .map(|data| data.iter().map(|(meta, _)| meta.clone()).collect());

        self.persist_inbound_user_message(&message, &raw_text, saved_metas.as_deref());

        // Deterministic built-in command: bypass model output drift for agent identity checks.
        if message.source != "system" && raw_text.trim() == "/agent-id" {
            self.send_builtin_text(self.deps.agent_id.to_string(), "agent-id")
                .await;
            return Ok(());
        }

        // Deterministic liveness ping for Telegram mentions.
        // This avoids model/provider flakiness for simple "you there?" style checks.
        if message.source == "telegram" {
            let text = raw_text.trim().to_lowercase();
            let (_, has_mention, _) = self.compute_listen_mode_invocation(&message, &raw_text);
            let looks_like_ping = text.contains("you here")
                || text.contains("ping")
                || text.ends_with(" yo")
                || text == "yo"
                || text.contains("alive")
                || text.contains("there?");

            if has_mention && looks_like_ping {
                self.send_builtin_text("yeah i'm here".to_string(), "telegram-ping")
                    .await;
                return Ok(());
            }
        }

        // Deterministic ping ack for Discord quiet-mode mentions/replies to avoid
        // flaky model behavior (e.g. skipping or over-formatting simple liveness checks).
        if message.source == "discord" && self.listen_only_mode {
            let text = raw_text.trim().to_lowercase();
            let (_, invoked_by_mention, invoked_by_reply) =
                self.compute_listen_mode_invocation(&message, &raw_text);
            let directed = invoked_by_mention || invoked_by_reply;
            let looks_like_ping = text.contains("you here")
                || text.contains("ping")
                || text.ends_with(" yo")
                || text == "yo"
                || text.contains("alive")
                || text.contains("there?");
            if directed && looks_like_ping {
                self.send_builtin_text("yeah i'm here".to_string(), "discord-ping")
                    .await;
                return Ok(());
            }
        }

        // Capture conversation context from the first message (platform, channel, server)
        if self.conversation_context.is_none() {
            let prompt_engine = self.deps.runtime_config.prompts.load();
            let server_name = message
                .metadata
                .get(crate::metadata_keys::SERVER_NAME)
                .and_then(|v| v.as_str());
            let channel_name = message
                .metadata
                .get(crate::metadata_keys::CHANNEL_NAME)
                .and_then(|v| v.as_str());
            self.conversation_context = Some(prompt_engine.render_conversation_context(
                &message.source,
                server_name,
                channel_name,
            )?);
        }

        if self
            .try_handle_builtin_ops_commands(&raw_text, &message)
            .await?
        {
            return Ok(());
        }

        let rewritten_text = if message.source == "system" {
            raw_text.clone()
        } else {
            self.rewrite_tool_routed_command_prompt(&raw_text)
                .unwrap_or_else(|| raw_text.clone())
        };

        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let message_timestamp = temporal_context.format_timestamp(message.timestamp);
        let user_text = format_user_message(&rewritten_text, &message, &message_timestamp);

        let mut invoked_by_command = false;
        let mut invoked_by_mention = false;
        let mut invoked_by_reply = false;

        // Listen-first guardrail:
        // ingest all messages, but only reply when explicitly invoked.
        if self.listen_only_mode && message.source != "system" {
            (invoked_by_command, invoked_by_mention, invoked_by_reply) =
                self.compute_listen_mode_invocation(&message, &raw_text);

            if !invoked_by_command && !invoked_by_mention && !invoked_by_reply {
                tracing::debug!(
                    channel_id = %self.id,
                    source = %message.source,
                    "listen-first mode: suppressing unsolicited reply"
                );
                // In quiet/listen-first mode we still want passive memory capture.
                // Count suppressed user messages so auto memory persistence branches
                // continue to run on interval without requiring explicit invokes.
                self.message_count += 1;
                self.check_memory_persistence().await;
                return Ok(());
            }
        }

        let system_prompt = self.build_system_prompt().await?;

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = extract_message_id(&message);
        }

        let is_retrigger = message.source == "system";
        let attachment_content = if !attachments.is_empty() {
            if let Some(ref saved_data) = saved_attachment_data {
                // Reuse already-downloaded bytes for images/text; audio still
                // needs transcription via the normal path so we fall through.
                let mut content = Vec::new();
                let mut unsaved_attachments = Vec::new();

                for (index, attachment) in attachments.iter().enumerate() {
                    if let Some((_, bytes)) = saved_data.get(index) {
                        // Audio attachments need transcription, not just bytes
                        if attachment.mime_type.starts_with("audio/") {
                            unsaved_attachments.push(attachment.clone());
                        } else {
                            content
                                .push(channel_attachments::content_from_bytes(bytes, attachment));
                        }
                    } else {
                        unsaved_attachments.push(attachment.clone());
                    }
                }

                // Process any attachments that weren't saved (or need transcription)
                if !unsaved_attachments.is_empty() {
                    let extra = download_attachments(&self.deps, &unsaved_attachments).await;
                    content.extend(extra);
                }
                content
            } else {
                download_attachments(&self.deps, &attachments).await
            }
        } else {
            Vec::new()
        };

        let (result, skip_flag, replied_flag, retrigger_reply_preserved) = self
            .run_agent_turn(
                &user_text,
                &system_prompt,
                &message.conversation_id,
                attachment_content,
                is_retrigger,
            )
            .await?;

        self.handle_agent_result(result, &skip_flag, &replied_flag, is_retrigger)
            .await;

        // Safety-net: in quiet mode, explicit mention/reply should never be dropped silently.
        if self.listen_only_mode
            && !is_retrigger
            && !invoked_by_command
            && (invoked_by_mention || invoked_by_reply)
            && skip_flag.load(std::sync::atomic::Ordering::Relaxed)
            && !replied_flag.load(std::sync::atomic::Ordering::Relaxed)
            && matches!(
                message.source.as_str(),
                "discord" | "telegram" | "slack" | "twitch"
            )
        {
            self.send_builtin_text(
                "yeah i'm here — tell me what you need.".to_string(),
                "quiet-mode-fallback",
            )
            .await;
        }

        // After retrigger turns, persist a fallback summary only when we don't
        // already have the LLM's actual relay text in history.
        //
        // PromptCancelled + reply tool is now handled in apply_history_after_turn:
        // it extracts the reply content from tool args and records that exact
        // assistant message (while dropping scaffolding). In that common success
        // path, we skip summary injection to avoid replacing user-visible wording
        // with raw worker output.
        //
        // If relay failed (replied=false), or if we couldn't extract a clean
        // reply content payload, this fallback preserves a compact background
        // result record for the next user turn.
        if is_retrigger {
            let replied = replied_flag.load(std::sync::atomic::Ordering::Relaxed);
            if replied && retrigger_reply_preserved {
                tracing::debug!(
                    channel_id = %self.id,
                    "skipping retrigger summary injection; relay reply already preserved"
                );
            } else {
                // Extract the result summaries from the metadata we attached in
                // flush_pending_retrigger, so we record only the substance (not
                // the retrigger instructions/template scaffolding).
                let summary = message
                    .metadata
                    .get("retrigger_result_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("[background work completed]");

                let record = if replied {
                    summary.to_string()
                } else {
                    tracing::warn!(
                        channel_id = %self.id,
                        "retrigger relay failed, preserving result in history for next turn"
                    );
                    format!(
                        "[background work completed but relay to user failed — include this in your next response]\n{summary}"
                    )
                };

                let mut history = self.state.history.write().await;
                // Replace the synthetic bridge message (if present) with the summary
                // to avoid consecutive assistant messages in history.
                let replaced = pop_retrigger_bridge_message(&mut history);
                tracing::debug!(
                    channel_id = %self.id,
                    replaced_bridge = replaced,
                    replied,
                    "injecting retrigger summary into history"
                );
                history.push(rig::message::Message::Assistant {
                    id: None,
                    content: OneOrMany::one(rig::message::AssistantContent::text(record)),
                });
            }

            // Mark the completed items as relayed in the status block so their
            // full result summaries stop appearing on subsequent turns. This
            // prevents the LLM from re-summarising stale worker/branch results.
            if replied
                && let Some(ids) = message
                    .metadata
                    .get("retrigger_process_ids")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            {
                let mut status = self.state.status_block.write().await;
                status.mark_relayed(&ids);
                tracing::debug!(
                    channel_id = %self.id,
                    count = ids.len(),
                    "marked retrigger results as relayed in status block"
                );
            }
        }

        // Check context size and trigger compaction if needed
        if let Err(error) = self.compactor.check_and_compact().await {
            tracing::warn!(channel_id = %self.id, %error, "compaction check failed");
        }

        // Increment message counter and spawn memory persistence branch if threshold reached
        if !is_retrigger {
            self.retrigger_count = 0;
            self.message_count += 1;
            self.check_memory_persistence().await;
        }

        Ok(())
    }

    /// Build the rendered available channels fragment for cross-channel awareness.
    async fn build_available_channels(&self) -> Option<String> {
        self.deps.messaging_manager.as_ref()?;

        let channels = match self.state.channel_store.list_active().await {
            Ok(channels) => channels,
            Err(error) => {
                tracing::warn!(%error, "failed to list channels for system prompt");
                return None;
            }
        };

        // Filter out the current channel and cron channels
        let entries: Vec<crate::prompts::engine::ChannelEntry> = channels
            .into_iter()
            .filter(|channel| {
                channel.id.as_str() != self.id.as_ref()
                    && channel.platform != "cron"
                    && channel.platform != "webhook"
            })
            .map(|channel| crate::prompts::engine::ChannelEntry {
                name: channel.display_name.unwrap_or_else(|| channel.id.clone()),
                platform: channel.platform,
                id: channel.id,
            })
            .collect();

        if entries.is_empty() {
            return None;
        }

        let prompt_engine = self.deps.runtime_config.prompts.load();
        prompt_engine.render_available_channels(entries).ok()
    }

    /// Build org context showing the agent's position in the communication hierarchy.
    fn build_org_context(&self, prompt_engine: &crate::prompts::PromptEngine) -> Option<String> {
        let agent_id = self.deps.agent_id.as_ref();
        let all_links = self.deps.links.load();
        let links = crate::links::links_for_agent(&all_links, agent_id);

        if links.is_empty() {
            return None;
        }

        // Build a lookup map for humans so we can surface display names,
        // roles, and descriptions in the org context prompt.
        let all_humans = self.deps.humans.load();
        let humans_by_id: std::collections::HashMap<&str, &crate::config::HumanDef> =
            all_humans.iter().map(|h| (h.id.as_str(), h)).collect();

        let mut superiors = Vec::new();
        let mut subordinates = Vec::new();
        let mut peers = Vec::new();

        for link in &links {
            let is_from = link.from_agent_id == agent_id;
            let other_id = if is_from {
                &link.to_agent_id
            } else {
                &link.from_agent_id
            };

            let is_human = humans_by_id.contains_key(other_id.as_str());

            let (name, role, description) = if let Some(human) = humans_by_id.get(other_id.as_str())
            {
                // Human node — use display_name, role, and description from HumanDef
                let name = human
                    .display_name
                    .clone()
                    .unwrap_or_else(|| other_id.clone());
                (name, human.role.clone(), human.description.clone())
            } else {
                // Agent node — use agent display name, no role/description
                let name = self
                    .deps
                    .agent_names
                    .get(other_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| other_id.clone());
                (name, None, None)
            };

            let info = crate::prompts::engine::LinkedAgent {
                name,
                id: other_id.clone(),
                is_human,
                role,
                description,
            };

            match link.kind {
                crate::links::LinkKind::Hierarchical => {
                    // from is above to: if we're `from`, the other is our subordinate
                    if is_from {
                        subordinates.push(info);
                    } else {
                        superiors.push(info);
                    }
                }
                crate::links::LinkKind::Peer => peers.push(info),
            }
        }

        if superiors.is_empty() && subordinates.is_empty() && peers.is_empty() {
            return None;
        }

        let org_context = crate::prompts::engine::OrgContext {
            superiors,
            subordinates,
            peers,
        };

        prompt_engine.render_org_context(org_context).ok()
    }

    /// Build pre-rendered project context for prompt injection.
    ///
    /// Fetches all active projects with their repos and worktrees, converts them
    /// to prompt-friendly structs, and renders via the projects_context template.
    /// Returns `None` if no projects exist or if rendering fails.
    async fn build_project_context(
        &self,
        prompt_engine: &crate::prompts::engine::PromptEngine,
    ) -> Option<String> {
        use crate::prompts::engine::{ProjectContext, ProjectRepoContext, ProjectWorktreeContext};

        let store = &self.deps.project_store;
        let projects = match store
            .list_projects(
                &self.deps.agent_id,
                Some(crate::projects::ProjectStatus::Active),
            )
            .await
        {
            Ok(projects) => projects,
            Err(error) => {
                tracing::warn!(%error, "failed to load projects for prompt injection");
                return None;
            }
        };

        if projects.is_empty() {
            return None;
        }

        let mut contexts = Vec::with_capacity(projects.len());
        for project in &projects {
            let repos = match store.list_repos(&project.id).await {
                Ok(repos) => repos,
                Err(error) => {
                    tracing::warn!(%error, project_id = %project.id, "failed to load repos for project");
                    Vec::new()
                }
            };

            let worktrees = match store.list_worktrees_with_repos(&project.id).await {
                Ok(worktrees) => worktrees,
                Err(error) => {
                    tracing::warn!(%error, project_id = %project.id, "failed to load worktrees for project");
                    Vec::new()
                }
            };

            contexts.push(ProjectContext {
                name: project.name.clone(),
                root_path: project.root_path.clone(),
                description: if project.description.is_empty() {
                    None
                } else {
                    Some(project.description.clone())
                },
                tags: project.tags.clone(),
                repos: repos
                    .into_iter()
                    .map(|repo| ProjectRepoContext {
                        name: repo.name.clone(),
                        path: repo.path.clone(),
                        default_branch: repo.default_branch.clone(),
                        remote_url: if repo.remote_url.is_empty() {
                            None
                        } else {
                            Some(repo.remote_url.clone())
                        },
                    })
                    .collect(),
                worktrees: worktrees
                    .into_iter()
                    .map(|worktree_with_repo| ProjectWorktreeContext {
                        name: worktree_with_repo.worktree.name.clone(),
                        path: worktree_with_repo.worktree.path.clone(),
                        branch: worktree_with_repo.worktree.branch.clone(),
                        repo_name: worktree_with_repo.repo_name.clone(),
                    })
                    .collect(),
            });
        }

        match prompt_engine.render_projects_context(contexts) {
            Ok(rendered) => {
                let rendered = rendered.trim().to_string();
                if rendered.is_empty() {
                    None
                } else {
                    Some(rendered)
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to render projects context");
                None
            }
        }
    }

    /// Assemble the full system prompt using the PromptEngine.
    async fn build_system_prompt(&self) -> crate::error::Result<String> {
        let rc = &self.deps.runtime_config;
        let prompt_engine = rc.prompts.load();

        let identity_context = rc.identity.load().render();
        let memory_bulletin = rc.memory_bulletin.load();
        let skills = rc.skills.load();
        let skills_prompt = skills.render_channel_prompt(&prompt_engine)?;

        let browser_enabled = rc.browser_config.load().enabled;
        let web_search_enabled = rc.brave_search_key.load().is_some();
        let opencode_enabled = rc.opencode.load().enabled;
        let sandbox_enabled = self.deps.sandbox.containment_active();
        let mcp_tool_names = self.deps.mcp_manager.get_tool_names().await;
        let worker_capabilities = prompt_engine.render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
            &mcp_tool_names,
        )?;

        let temporal_context = TemporalContext::from_runtime(rc.as_ref());
        let current_time_line = temporal_context.current_time_line();
        let status_text = {
            let status = self.state.status_block.read().await;
            status.render_with_time_context(Some(&current_time_line))
        };

        let available_channels = self.build_available_channels().await;

        let org_context = self.build_org_context(&prompt_engine);

        let adapter_prompt = self
            .current_adapter()
            .and_then(|adapter| prompt_engine.render_channel_adapter_prompt(adapter));

        let project_context = self.build_project_context(&prompt_engine).await;

        let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };

        prompt_engine.render_channel_prompt_with_links(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            empty_to_none(skills_prompt),
            worker_capabilities,
            self.conversation_context.clone(),
            empty_to_none(status_text),
            None, // coalesce_hint - only set for batched messages
            available_channels,
            sandbox_enabled,
            org_context,
            adapter_prompt,
            project_context,
        )
    }

    /// Register per-turn tools, run the LLM agentic loop, and clean up.
    ///
    /// Returns the prompt result and per-turn flags for the caller to dispatch.
    #[allow(clippy::type_complexity)]
    #[tracing::instrument(skip(self, user_text, system_prompt, attachment_content), fields(channel_id = %self.id, agent_id = %self.deps.agent_id))]
    async fn run_agent_turn(
        &self,
        user_text: &str,
        system_prompt: &str,
        conversation_id: &str,
        attachment_content: Vec<UserContent>,
        is_retrigger: bool,
    ) -> Result<(
        std::result::Result<String, rig::completion::PromptError>,
        crate::tools::SkipFlag,
        crate::tools::RepliedFlag,
        bool,
    )> {
        let skip_flag = crate::tools::new_skip_flag();
        let replied_flag = crate::tools::new_replied_flag();
        let allow_direct_reply = !self.suppress_plaintext_fallback();

        // Set the originating channel on the delegation tool so task completion
        // notifications route back to this conversation.
        let send_agent_message_tool = self
            .send_agent_message_tool
            .clone()
            .map(|tool| tool.with_originating_channel(conversation_id.to_string()));

        if let Err(error) = crate::tools::add_channel_tools(
            &self.tool_server,
            self.state.clone(),
            self.response_tx.clone(),
            conversation_id,
            skip_flag.clone(),
            replied_flag.clone(),
            self.deps.cron_tool.clone(),
            send_agent_message_tool,
            allow_direct_reply,
        )
        .await
        {
            tracing::error!(%error, "failed to add channel tools");
            return Err(AgentError::Other(error.into()).into());
        }

        let rc = &self.deps.runtime_config;
        let routing = rc.routing.load();
        let max_turns = if is_retrigger {
            RETRIGGER_MAX_TURNS
        } else {
            **rc.max_turns.load()
        };
        let model_name = routing.resolve(ProcessType::Channel, None);
        let model = SpacebotModel::make(&self.deps.llm_manager, model_name)
            .with_context(&*self.deps.agent_id, "channel")
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(system_prompt)
            .default_max_turns(max_turns)
            .tool_server_handle(self.tool_server.clone())
            .build();

        let _ = self
            .response_tx
            .send(OutboundResponse::Status(crate::StatusUpdate::Thinking))
            .await;

        // Inject attachments as a user message before the text prompt
        if !attachment_content.is_empty() {
            let mut history = self.state.history.write().await;
            let content = OneOrMany::many(attachment_content).unwrap_or_else(|_| {
                OneOrMany::one(UserContent::text("[attachment processing failed]"))
            });
            history.push(rig::message::Message::User { content });
            drop(history);
        }

        // For retrigger turns, inject a synthetic assistant acknowledgment so the
        // LLM sees proper user/assistant role alternation. Without this, the API
        // receives back-to-back user messages (the original user prompt preserved
        // from the prior turn + the retrigger system message), which causes some
        // models to return empty responses or get confused about whose turn it is.
        if is_retrigger {
            let mut history = self.state.history.write().await;
            // Only inject if the last message is a user message (avoid double-stacking
            // if history already ends with an assistant message).
            let needs_bridge = history
                .last()
                .is_some_and(|m| matches!(m, rig::message::Message::User { .. }));
            if needs_bridge {
                history.push(rig::message::Message::Assistant {
                    id: None,
                    content: OneOrMany::one(rig::message::AssistantContent::text(
                        "[acknowledged — working on it in background]",
                    )),
                });
            }
            drop(history);
        }

        // Clone history out so the write lock is released before the agentic loop.
        // The branch tool needs a read lock on history to clone it for the branch,
        // and holding a write lock across the entire agentic loop would deadlock.
        let mut history = {
            let guard = self.state.history.read().await;
            guard.clone()
        };
        let history_len_before = history.len();

        let mut result = self.hook.prompt_once(&agent, &mut history, user_text).await;

        // If the LLM responded with text that looks like tool call syntax, it failed
        // to use the tool calling API. Inject a correction and retry a couple
        // times so the model can recover by calling `reply` or `skip`.
        const TOOL_SYNTAX_RECOVERY_MAX_ATTEMPTS: usize = 2;
        let mut recovery_attempts = 0;
        while let Ok(ref response) = result {
            if !crate::tools::should_block_user_visible_text(response)
                || recovery_attempts >= TOOL_SYNTAX_RECOVERY_MAX_ATTEMPTS
            {
                break;
            }

            recovery_attempts += 1;
            tracing::warn!(
                channel_id = %self.id,
                attempt = recovery_attempts,
                "LLM emitted blocked structured output, retrying with correction"
            );

            let prompt_engine = self.deps.runtime_config.prompts.load();
            let correction = prompt_engine.render_system_tool_syntax_correction()?;
            result = self
                .hook
                .prompt_once(&agent, &mut history, &correction)
                .await;
        }

        let retrigger_reply_preserved = {
            let mut guard = self.state.history.write().await;
            apply_history_after_turn(
                &result,
                &mut guard,
                history,
                history_len_before,
                &self.id,
                is_retrigger,
            )
        };

        if let Err(error) =
            crate::tools::remove_channel_tools(&self.tool_server, allow_direct_reply).await
        {
            tracing::warn!(%error, "failed to remove channel tools");
        }

        Ok((result, skip_flag, replied_flag, retrigger_reply_preserved))
    }

    /// Send outbound text and record send metrics.
    async fn send_outbound_text(&self, text: String, error_context: &str) {
        match self.response_tx.send(OutboundResponse::Text(text)).await {
            Ok(()) => {
                #[cfg(feature = "metrics")]
                {
                    let channel_type = self.current_adapter().unwrap_or("unknown");
                    crate::telemetry::Metrics::global()
                        .messages_sent_total
                        .with_label_values(&[&self.deps.agent_id, channel_type])
                        .inc();
                }
            }
            Err(error) => {
                #[cfg(feature = "metrics")]
                {
                    let channel_type = self.current_adapter().unwrap_or("unknown");
                    crate::telemetry::Metrics::global()
                        .channel_errors_total
                        .with_label_values(&[&self.deps.agent_id, channel_type, "send_failed"])
                        .inc();
                }
                tracing::error!(%error, channel_id = %self.id, "{error_context}");
            }
        }
    }

    /// Dispatch the LLM result: send fallback text, log errors, clean up typing.
    ///
    /// On retrigger turns (`is_retrigger = true`), fallback text is suppressed
    /// unless the LLM called `skip` — in that case, any text the LLM produced
    /// is sent as a fallback to ensure worker/branch results reach the user.
    /// The LLM sometimes incorrectly skips on retrigger turns thinking the
    /// result was "already processed" when the user hasn't seen it yet.
    async fn handle_agent_result(
        &self,
        result: std::result::Result<String, rig::completion::PromptError>,
        skip_flag: &crate::tools::SkipFlag,
        replied_flag: &crate::tools::RepliedFlag,
        is_retrigger: bool,
    ) {
        #[cfg(feature = "metrics")]
        let metrics = crate::telemetry::Metrics::global();
        #[cfg(feature = "metrics")]
        let metrics_agent_id: &str = &self.deps.agent_id;
        #[cfg(feature = "metrics")]
        let metrics_channel_type = self.current_adapter().unwrap_or("unknown");

        match result {
            Ok(response) => {
                let skipped = skip_flag.load(std::sync::atomic::Ordering::Relaxed);
                let replied = replied_flag.load(std::sync::atomic::Ordering::Relaxed);
                let suppress_plaintext_fallback = self.suppress_plaintext_fallback();
                let adapter = self.current_adapter().unwrap_or("unknown");

                if skipped && is_retrigger {
                    // The LLM skipped on a retrigger turn. This means a worker
                    // or branch completed but the LLM decided not to relay the
                    // result. If the LLM also produced text, send it as a
                    // fallback since the user hasn't seen the result yet.
                    let text = response.trim();
                    if !text.is_empty() {
                        if crate::tools::should_block_user_visible_text(text) {
                            tracing::warn!(
                                channel_id = %self.id,
                                "blocked retrigger fallback output containing structured or tool syntax"
                            );
                        } else if let Some(leak) = crate::secrets::scrub::scan_for_leaks(text) {
                            tracing::warn!(
                                channel_id = %self.id,
                                leak_prefix = %&leak[..leak.len().min(8)],
                                "blocked retrigger fallback output matching secret pattern"
                            );
                        } else if suppress_plaintext_fallback {
                            tracing::info!(
                                channel_id = %self.id,
                                adapter,
                                "suppressing retrigger plaintext fallback for adapter; explicit reply tool call required"
                            );
                        } else {
                            tracing::info!(
                                channel_id = %self.id,
                                response_len = text.len(),
                                "LLM skipped on retrigger but produced text, sending as fallback"
                            );
                            let extracted = extract_reply_from_tool_syntax(text);
                            let source = self
                                .conversation_id
                                .as_deref()
                                .and_then(|conversation_id| conversation_id.split(':').next())
                                .unwrap_or("unknown");
                            let final_text = crate::tools::reply::normalize_discord_mention_tokens(
                                extracted.as_deref().unwrap_or(text),
                                source,
                            );
                            if !final_text.is_empty() {
                                if extracted.is_some() {
                                    tracing::warn!(channel_id = %self.id, "extracted reply from malformed tool syntax in retrigger fallback");
                                }
                                self.state
                                    .conversation_logger
                                    .log_bot_message(&self.state.channel_id, &final_text);
                                self.send_outbound_text(
                                    final_text,
                                    "failed to send retrigger fallback reply",
                                )
                                .await;
                            }
                        }
                    } else {
                        tracing::warn!(
                            channel_id = %self.id,
                            "LLM skipped on retrigger with no text — worker/branch result may not have been relayed"
                        );
                    }
                } else if skipped {
                    tracing::debug!(channel_id = %self.id, "channel turn skipped (no response)");
                } else if replied {
                    #[cfg(feature = "metrics")]
                    metrics
                        .messages_sent_total
                        .with_label_values(&[metrics_agent_id, metrics_channel_type])
                        .inc();
                    tracing::debug!(channel_id = %self.id, "channel turn replied via tool (fallback suppressed)");
                } else if is_retrigger {
                    // On retrigger turns the LLM should use the reply tool, but
                    // some models return the result as raw text instead. Send it
                    // as a fallback so the user still gets the worker/branch output.
                    let text = response.trim();
                    if !text.is_empty() {
                        if crate::tools::should_block_user_visible_text(text) {
                            tracing::warn!(
                                channel_id = %self.id,
                                "blocked retrigger output containing structured or tool syntax"
                            );
                        } else if let Some(leak) = crate::secrets::scrub::scan_for_leaks(text) {
                            tracing::warn!(
                                channel_id = %self.id,
                                leak_prefix = %&leak[..leak.len().min(8)],
                                "blocked retrigger output matching secret pattern"
                            );
                        } else if suppress_plaintext_fallback {
                            tracing::info!(
                                channel_id = %self.id,
                                adapter,
                                "suppressing retrigger plaintext output for adapter; explicit reply tool call required"
                            );
                        } else {
                            tracing::info!(
                                channel_id = %self.id,
                                response_len = text.len(),
                                "retrigger produced text without reply tool, sending as fallback"
                            );
                            let extracted = extract_reply_from_tool_syntax(text);
                            let source = self
                                .conversation_id
                                .as_deref()
                                .and_then(|conversation_id| conversation_id.split(':').next())
                                .unwrap_or("unknown");
                            let final_text = crate::tools::reply::normalize_discord_mention_tokens(
                                extracted.as_deref().unwrap_or(text),
                                source,
                            );
                            if !final_text.is_empty() {
                                self.state
                                    .conversation_logger
                                    .log_bot_message(&self.state.channel_id, &final_text);
                                self.send_outbound_text(
                                    final_text,
                                    "failed to send retrigger fallback reply",
                                )
                                .await;
                            }
                        }
                    } else {
                        tracing::debug!(
                            channel_id = %self.id,
                            "retrigger turn produced no text and no reply tool call"
                        );
                    }
                } else {
                    // If the LLM returned text without using the reply tool, send it
                    // directly. Some models respond with text instead of tool calls.
                    // When the text looks like tool call syntax (e.g. "[reply]\n{\"content\": \"hi\"}"),
                    // attempt to extract the reply content and send that instead.
                    let text = response.trim();
                    if crate::tools::should_block_user_visible_text(text) {
                        tracing::warn!(
                            channel_id = %self.id,
                            "blocked fallback output containing structured or tool syntax"
                        );
                    } else if let Some(leak) = crate::secrets::scrub::scan_for_leaks(text) {
                        tracing::warn!(
                            channel_id = %self.id,
                            leak_prefix = %&leak[..leak.len().min(8)],
                            "blocked fallback output matching secret pattern"
                        );
                    } else if suppress_plaintext_fallback {
                        tracing::info!(
                            channel_id = %self.id,
                            adapter,
                            "suppressing plaintext fallback for adapter; explicit reply tool call required"
                        );
                    } else {
                        let extracted = extract_reply_from_tool_syntax(text);
                        let source = self
                            .conversation_id
                            .as_deref()
                            .and_then(|conversation_id| conversation_id.split(':').next())
                            .unwrap_or("unknown");
                        let final_text = crate::tools::reply::normalize_discord_mention_tokens(
                            extracted.as_deref().unwrap_or(text),
                            source,
                        );
                        if !final_text.is_empty() {
                            if extracted.is_some() {
                                tracing::warn!(channel_id = %self.id, "extracted reply from malformed tool syntax in LLM text output");
                            }
                            self.state.conversation_logger.log_bot_message_with_name(
                                &self.state.channel_id,
                                &final_text,
                                Some(self.agent_display_name()),
                            );
                            self.send_outbound_text(final_text, "failed to send fallback reply")
                                .await;
                        }
                    }

                    tracing::debug!(channel_id = %self.id, "channel turn completed");
                }
            }
            Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                #[cfg(feature = "metrics")]
                metrics
                    .channel_errors_total
                    .with_label_values(&[metrics_agent_id, metrics_channel_type, "max_turns"])
                    .inc();
                tracing::warn!(channel_id = %self.id, "channel hit max turns");
            }
            Err(rig::completion::PromptError::PromptCancelled { reason, .. }) => {
                if reason == "reply delivered" {
                    #[cfg(feature = "metrics")]
                    metrics
                        .messages_sent_total
                        .with_label_values(&[metrics_agent_id, metrics_channel_type])
                        .inc();
                    tracing::debug!(channel_id = %self.id, "channel turn completed via reply tool");
                } else {
                    tracing::info!(channel_id = %self.id, %reason, "channel turn cancelled");
                }
            }
            Err(error) => {
                #[cfg(feature = "metrics")]
                metrics
                    .channel_errors_total
                    .with_label_values(&[metrics_agent_id, metrics_channel_type, "llm_error"])
                    .inc();
                tracing::error!(channel_id = %self.id, %error, "channel LLM call failed");
            }
        }

        // Ensure typing indicator is always cleaned up, even on error paths
        let _ = self
            .response_tx
            .send(OutboundResponse::Status(crate::StatusUpdate::StopTyping))
            .await;
    }

    /// Handle a process event (branch results, worker completions, status updates).
    async fn handle_event(&mut self, event: ProcessEvent) -> Result<()> {
        // Keep mode aligned with live settings updates while this worker runs.
        self.sync_listen_only_mode_from_runtime();

        // Only process events targeted at this channel
        if !event_is_for_channel(&event, &self.id) {
            return Ok(());
        }
        // Update status block
        {
            let mut status = self.state.status_block.write().await;
            status.update(&event);
        }

        let mut should_retrigger = false;
        let mut retrigger_metadata = std::collections::HashMap::new();
        let run_logger = &self.state.process_run_logger;

        match &event {
            ProcessEvent::BranchStarted {
                branch_id,
                channel_id,
                description,
                reply_to_message_id,
                ..
            } => {
                run_logger.log_branch_started(channel_id, *branch_id, description);
                if let Some(message_id) = reply_to_message_id {
                    self.branch_reply_targets
                        .insert(*branch_id, message_id.clone());
                }
            }
            ProcessEvent::BranchResult {
                branch_id,
                conclusion,
                ..
            } => {
                let reply_target_message_id = self.branch_reply_targets.get(branch_id).cloned();
                let was_active = self
                    .state
                    .active_branches
                    .write()
                    .await
                    .remove(branch_id)
                    .is_some();
                let was_memory_persistence = self.memory_persistence_branches.remove(branch_id);
                if !was_active {
                    if was_memory_persistence {
                        tracing::info!(
                            branch_id = %branch_id,
                            "stale memory-persistence branch completion ignored"
                        );
                    }
                    self.branch_reply_targets.remove(branch_id);
                    return Ok(());
                }

                run_logger.log_branch_completed(*branch_id, conclusion);

                #[cfg(feature = "metrics")]
                crate::telemetry::Metrics::global()
                    .active_branches
                    .with_label_values(&[&*self.deps.agent_id])
                    .dec();

                // Memory persistence branches complete silently — no history
                // injection, no re-trigger. The work (memory saves) already
                // happened inside the branch via tool calls.
                if was_memory_persistence {
                    tracing::info!(branch_id = %branch_id, "memory persistence branch completed");
                } else {
                    // Regular branch: accumulate result for the next retrigger.
                    // The result text will be embedded directly in the retrigger
                    // message so the LLM knows exactly which process produced it.
                    self.pending_results.push(PendingResult {
                        process_type: "branch",
                        process_id: branch_id.to_string(),
                        result: conclusion.clone(),
                        success: true,
                    });
                    should_retrigger = true;

                    if let Some(message_id) = reply_target_message_id {
                        retrigger_metadata.insert(
                            crate::metadata_keys::REPLY_TO_MESSAGE_ID.to_string(),
                            serde_json::Value::from(message_id),
                        );
                    }

                    tracing::info!(branch_id = %branch_id, "branch result queued for retrigger");
                }
                self.branch_reply_targets.remove(branch_id);
            }
            ProcessEvent::WorkerStarted {
                worker_id,
                channel_id,
                task,
                worker_type,
                interactive,
                directory,
                ..
            } => {
                let task_preset = self
                    .state
                    .worker_task_presets
                    .read()
                    .await
                    .get(worker_id)
                    .copied()
                    .unwrap_or(WorkerTaskPreset::Default);
                run_logger.log_worker_started(
                    channel_id.as_ref(),
                    *worker_id,
                    task,
                    worker_type,
                    &self.deps.agent_id,
                    *interactive,
                    directory.as_deref().map(std::path::Path::new),
                    task_preset,
                );
            }
            ProcessEvent::WorkerStatus {
                worker_id, status, ..
            } => {
                run_logger.log_worker_status(*worker_id, status);
            }
            ProcessEvent::WorkerIdle { worker_id, .. } => {
                run_logger.log_worker_idle(*worker_id);
            }
            ProcessEvent::WorkerComplete {
                worker_id,
                result,
                notify,
                success,
                ..
            } => {
                // Use worker_handles as the source of truth for active workers.
                // (active_workers is never populated because Worker is consumed by .run())
                if self
                    .state
                    .worker_handles
                    .write()
                    .await
                    .remove(worker_id)
                    .is_none()
                {
                    return Ok(());
                }

                self.state.active_workers.write().await.remove(worker_id);
                self.state.worker_inputs.write().await.remove(worker_id);
                self.state.worker_injections.write().await.remove(worker_id);
                let worker_task_preset = self
                    .state
                    .worker_task_presets
                    .write()
                    .await
                    .remove(worker_id)
                    .unwrap_or(WorkerTaskPreset::Default);

                if *notify {
                    let (result, success) =
                        mediate_worker_result_for_retrigger(result, worker_task_preset, *success);
                    run_logger.log_worker_completed(*worker_id, &result, success);
                    // Accumulate result for the next retrigger instead of
                    // injecting into history as a fake user message.
                    self.pending_results.push(PendingResult {
                        process_type: "worker",
                        process_id: worker_id.to_string(),
                        result,
                        success,
                    });
                    should_retrigger = true;
                } else {
                    run_logger.log_worker_completed(*worker_id, result, *success);
                }

                tracing::info!(worker_id = %worker_id, "worker completed, result queued for retrigger");
            }
            ProcessEvent::OpenCodeSessionCreated {
                worker_id,
                session_id,
                port,
                ..
            } => {
                run_logger.log_opencode_metadata(*worker_id, session_id, *port);
            }
            ProcessEvent::WorkerInitialResult {
                worker_id, result, ..
            } => {
                let worker_task_preset = self
                    .state
                    .worker_task_presets
                    .read()
                    .await
                    .get(worker_id)
                    .copied()
                    .unwrap_or(WorkerTaskPreset::Default);
                let (result, success) =
                    mediate_worker_result_for_retrigger(result, worker_task_preset, true);
                // Interactive worker completed a task (initial or follow-up)
                // but stays alive for more input. Deliver the result to the
                // channel without removing the worker from the active set.
                self.pending_results.push(PendingResult {
                    process_type: "worker",
                    process_id: worker_id.to_string(),
                    result,
                    success,
                });
                should_retrigger = true;
                tracing::info!(
                    worker_id = %worker_id,
                    "interactive worker result queued for retrigger"
                );
            }
            _ => {}
        }

        // Debounce retriggers: instead of firing immediately, set a deadline.
        // Multiple branch/worker completions within the debounce window are
        // coalesced into a single retrigger to prevent message spam.
        if should_retrigger {
            if self.retrigger_count >= MAX_RETRIGGERS_PER_TURN {
                tracing::warn!(
                    channel_id = %self.id,
                    retrigger_count = self.retrigger_count,
                    max = MAX_RETRIGGERS_PER_TURN,
                    "retrigger cap reached, suppressing further retriggers until next user message"
                );
                // Drain any pending results into history as assistant messages
                // so they aren't silently lost when the cap prevents a retrigger.
                if !self.pending_results.is_empty() {
                    let results = std::mem::take(&mut self.pending_results);
                    let mut history = self.state.history.write().await;
                    for r in &results {
                        let status = if r.success { "completed" } else { "failed" };
                        let summary = format!(
                            "[Background {} {} {}]: {}",
                            r.process_type, r.process_id, status, r.result
                        );
                        history.push(rig::message::Message::Assistant {
                            id: None,
                            content: OneOrMany::one(rig::message::AssistantContent::text(summary)),
                        });
                    }
                    tracing::info!(
                        channel_id = %self.id,
                        count = results.len(),
                        "injected capped results into history as assistant messages"
                    );
                }
            } else {
                self.pending_retrigger = true;
                // Merge metadata (later events override earlier ones for the same key)
                for (key, value) in retrigger_metadata {
                    self.pending_retrigger_metadata.insert(key, value);
                }
                self.retrigger_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
                );
            }
        }

        Ok(())
    }

    /// Flush the pending retrigger: send a synthetic system message to re-trigger
    /// the channel LLM so it can process background results and respond.
    ///
    /// Drains `pending_results` and embeds them directly in the retrigger message
    /// so the LLM sees exactly which process(es) completed and what they returned.
    /// No result text is left floating in history as an ambiguous user message.
    ///
    /// Results are drained only after the synthetic message is queued
    /// successfully. On transient failures, retrigger state is kept and retried
    /// so background results are not silently lost.
    async fn flush_pending_retrigger(&mut self) {
        self.retrigger_deadline = None;

        if !self.pending_retrigger {
            return;
        }

        let Some(conversation_id) = &self.conversation_id else {
            tracing::warn!(
                channel_id = %self.id,
                "retrigger pending but conversation_id is missing, dropping pending results"
            );
            self.pending_retrigger = false;
            self.pending_retrigger_metadata.clear();
            self.pending_results.clear();
            return;
        };

        if self.pending_results.is_empty() {
            tracing::warn!(
                channel_id = %self.id,
                "retrigger fired but no pending results to relay"
            );
            self.pending_retrigger = false;
            self.pending_retrigger_metadata.clear();
            return;
        }

        let result_count = self.pending_results.len();

        // Build per-result summaries for the template.
        let result_items: Vec<_> = self
            .pending_results
            .iter()
            .map(|r| crate::prompts::engine::RetriggerResult {
                process_type: r.process_type.to_string(),
                process_id: r.process_id.clone(),
                success: r.success,
                result: r.result.clone(),
            })
            .collect();

        let retrigger_message = match self
            .deps
            .runtime_config
            .prompts
            .load()
            .render_system_retrigger(&result_items)
        {
            Ok(message) => message,
            Err(error) => {
                tracing::error!(
                    channel_id = %self.id,
                    %error,
                    "failed to render retrigger message, retrying"
                );
                self.retrigger_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
                );
                return;
            }
        };

        // Build a compact summary of the results to inject into history after
        // a successful relay. This goes into metadata so handle_message can
        // pull it out without re-parsing the template.
        let result_summary = self
            .pending_results
            .iter()
            .map(|r| {
                let status = if r.success { "completed" } else { "failed" };
                // Truncate very long results for the history record — the user
                // already saw the full version via the reply tool.
                let truncated = if r.result.len() > 500 {
                    let boundary = r.result.floor_char_boundary(500);
                    format!("{}... [truncated]", &r.result[..boundary])
                } else {
                    r.result.clone()
                };
                format!(
                    "[{} {} {}]: {}",
                    r.process_type, r.process_id, status, truncated
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Collect the process IDs so we can mark them as relayed in the
        // status block after the retrigger turn completes successfully.
        let retrigger_process_ids: Vec<String> = self
            .pending_results
            .iter()
            .map(|r| r.process_id.clone())
            .collect();

        let mut metadata = self.pending_retrigger_metadata.clone();
        metadata.insert(
            "retrigger_result_summary".to_string(),
            serde_json::Value::String(result_summary),
        );
        metadata.insert(
            "retrigger_process_ids".to_string(),
            serde_json::json!(retrigger_process_ids),
        );

        let synthetic = InboundMessage {
            id: uuid::Uuid::new_v4().to_string(),
            source: "system".into(),
            adapter: None,
            conversation_id: conversation_id.clone(),
            sender_id: "system".into(),
            agent_id: None,
            content: crate::MessageContent::Text(retrigger_message),
            timestamp: chrono::Utc::now(),
            metadata,
            formatted_author: None,
        };
        match self.self_tx.try_send(synthetic) {
            Ok(()) => {
                self.retrigger_count += 1;
                tracing::info!(
                    channel_id = %self.id,
                    retrigger_count = self.retrigger_count,
                    result_count,
                    "firing debounced retrigger with {} result(s)",
                    result_count,
                );

                self.pending_retrigger = false;
                self.pending_retrigger_metadata.clear();
                self.pending_results.clear();
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    channel_id = %self.id,
                    result_count,
                    "channel self queue is full, retrying retrigger"
                );
                self.retrigger_deadline = Some(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(
                    channel_id = %self.id,
                    "failed to re-trigger channel: queue is closed, dropping pending results"
                );
                self.pending_retrigger = false;
                self.pending_retrigger_metadata.clear();
                self.pending_results.clear();
            }
        }
    }

    /// Get the current status block as a string.
    pub async fn get_status(&self) -> String {
        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let current_time_line = temporal_context.current_time_line();
        let status = self.state.status_block.read().await;
        status.render_with_time_context(Some(&current_time_line))
    }

    /// Check if a memory persistence branch should be spawned based on message count.
    async fn check_memory_persistence(&mut self) {
        let config = **self.deps.runtime_config.memory_persistence.load();
        if !config.enabled || config.message_interval == 0 {
            return;
        }

        if self.message_count < config.message_interval {
            return;
        }

        // Reset counter before spawning so subsequent messages don't pile up
        self.message_count = 0;

        match spawn_memory_persistence_branch(&self.state, &self.deps).await {
            Ok(branch_id) => {
                self.memory_persistence_branches.insert(branch_id);
                tracing::info!(
                    channel_id = %self.id,
                    branch_id = %branch_id,
                    interval = config.message_interval,
                    "memory persistence branch spawned"
                );
            }
            Err(error) => {
                tracing::warn!(
                    channel_id = %self.id,
                    %error,
                    "failed to spawn memory persistence branch"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkerTaskPreset, mediate_worker_result_for_retrigger, recv_channel_event,
        should_process_event_for_channel,
    };
    use crate::memory::MemoryType;
    use crate::{AgentId, ChannelId, ProcessEvent, ProcessId};
    use std::sync::Arc;

    #[tokio::test]
    async fn channel_event_loop_continues_after_lagged_broadcast() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<ProcessEvent>(2);
        let agent_id: AgentId = Arc::from("agent");
        let channel_id: ChannelId = Arc::from("channel");
        let process_id = ProcessId::Channel(channel_id);

        for status in ["one", "two", "three"] {
            event_tx
                .send(ProcessEvent::StatusUpdate {
                    agent_id: agent_id.clone(),
                    process_id: process_id.clone(),
                    status: status.to_string(),
                })
                .ok();
        }

        let first = recv_channel_event(&mut event_rx).await;
        assert!(
            matches!(first, crate::BroadcastRecvResult::Lagged(skipped) if skipped > 0),
            "expected lagged receive, got {first:?}"
        );

        let second = recv_channel_event(&mut event_rx).await;
        assert!(
            matches!(
                second,
                crate::BroadcastRecvResult::Event(ProcessEvent::StatusUpdate { .. })
            ),
            "expected next event after lagged receive, got {second:?}"
        );
    }

    #[tokio::test]
    async fn channel_event_loop_stops_when_event_bus_closes() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel::<ProcessEvent>(2);
        drop(event_tx);

        let event = recv_channel_event(&mut event_rx).await;
        assert!(matches!(event, crate::BroadcastRecvResult::Closed));
    }

    #[test]
    fn channel_coalesce_ignores_unrelated_memory_saved_events() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::MemorySaved {
            agent_id: Arc::from("agent"),
            memory_id: "memory-1".to_string(),
            channel_id: Some(Arc::from("channel-b")),
            memory_type: MemoryType::Fact,
            importance: 0.8,
            content_summary: "saved memory".to_string(),
        };

        assert!(!should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn channel_coalesce_ignores_unrelated_compaction_events() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::CompactionTriggered {
            agent_id: Arc::from("agent"),
            channel_id: Arc::from("channel-b"),
            threshold_reached: 0.85,
        };

        assert!(!should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn channel_coalesce_processes_related_worker_events() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerStatus {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            channel_id: Some(channel_id.clone()),
            status: "running".to_string(),
        };

        assert!(should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn channel_coalesce_processes_related_branch_events() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::BranchResult {
            agent_id: Arc::from("agent"),
            branch_id: uuid::Uuid::new_v4(),
            channel_id: channel_id.clone(),
            conclusion: "done".to_string(),
        };

        assert!(should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn worker_complete_event_matches_own_channel() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerComplete {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            channel_id: Some(channel_id.clone()),
            result: "done".to_string(),
            notify: true,
            success: true,
        };

        assert!(should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn worker_complete_event_ignored_for_other_channel() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerComplete {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            channel_id: Some(Arc::from("channel-b")),
            result: "done".to_string(),
            notify: true,
            success: true,
        };

        assert!(!should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn worker_complete_event_ignored_when_no_channel() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerComplete {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            channel_id: None,
            result: "done".to_string(),
            notify: true,
            success: true,
        };

        assert!(!should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn retrieval_result_mediation_formats_structured_success_payload() {
        let (relayed, success) = mediate_worker_result_for_retrigger(
            r#"{
                "status":"success",
                "summary":"Found the launch video research notes.",
                "sources":[
                    {
                        "source":"qmd",
                        "tool":"qmd_search",
                        "retrieval_mode":"keyword",
                        "locator":"notes/launch-video.md",
                        "title":"Launch Video Plan",
                        "snippet":"Need to emphasize the recall workflow in the intro.",
                        "why_it_matters":"This is the strongest direct evidence for the prior video research."
                    }
                ],
                "gaps":[]
            }"#,
            WorkerTaskPreset::Retrieval,
            true,
        );

        assert!(success);
        assert!(relayed.contains("Found the launch video research notes."));
        assert!(relayed.contains("Supporting evidence:"));
        assert!(relayed.contains("Launch Video Plan"));
        assert!(relayed.contains("Why it matters:"));
        assert!(!crate::tools::should_block_user_visible_text(&relayed));
    }

    #[test]
    fn retrieval_result_mediation_marks_unavailable_as_failed_plaintext() {
        let (relayed, success) = mediate_worker_result_for_retrigger(
            r#"{
                "status":"unavailable",
                "summary":"QMD was unavailable during the lookup.",
                "sources":[],
                "gaps":["The qmd MCP server did not respond."]
            }"#,
            WorkerTaskPreset::Retrieval,
            true,
        );

        assert!(!success);
        assert!(relayed.contains("QMD was unavailable during the lookup."));
        assert!(relayed.contains("Remaining gaps:"));
        assert!(relayed.contains("qmd MCP server did not respond"));
        assert!(!crate::tools::should_block_user_visible_text(&relayed));
    }

    #[test]
    fn retrieval_result_mediation_rejects_malformed_structured_output() {
        let (relayed, success) = mediate_worker_result_for_retrigger(
            "{\"status\":\"success\",\"summary\":\"ok\"}",
            WorkerTaskPreset::Retrieval,
            true,
        );

        assert!(!success);
        assert!(
            relayed.contains("returned malformed structured output")
                || relayed.contains("no supporting evidence")
        );
        assert!(!crate::tools::should_block_user_visible_text(&relayed));
    }

    #[test]
    fn retrieval_result_mediation_accepts_fenced_json_payload() {
        let (relayed, success) = mediate_worker_result_for_retrigger(
            "```json\n{\n  \"status\": \"success\",\n  \"summary\": \"Found it.\",\n  \"sources\": [{\n    \"source\": \"qmd\",\n    \"tool\": \"qmd_search\",\n    \"retrieval_mode\": \"keyword\",\n    \"locator\": \"notes/found.md\",\n    \"title\": \"Found\",\n    \"snippet\": \"Evidence\",\n    \"why_it_matters\": \"It answers the request.\"\n  }],\n  \"gaps\": []\n}\n```",
            WorkerTaskPreset::Retrieval,
            true,
        );

        assert!(success);
        assert!(relayed.contains("Found it."));
        assert!(relayed.contains("Supporting evidence:"));
    }
}
