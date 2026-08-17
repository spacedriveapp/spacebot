//! Channel: User-facing conversation process.

use crate::agent::channel_attachments;
use crate::agent::channel_attachments::download_attachments;
use crate::agent::channel_dispatch::spawn_memory_persistence_branch;
use crate::agent::channel_history::{
    apply_history_after_turn, event_is_for_channel, extract_message_id,
    extract_reply_from_tool_syntax, format_batched_user_message, format_user_message,
    message_display_name, pop_retrigger_bridge_message, with_time_envelope,
};
use crate::agent::channel_prompt::{
    MAX_RETRIGGERS_PER_TURN, RETRIGGER_DEBOUNCE_MS, RETRIGGER_MAX_TURNS, TemporalContext,
};
use crate::agent::chronicle::Chronicler;
use crate::agent::compactor::Compactor;
use crate::agent::process_control::ControlActionResult;
use crate::agent::status::{StatusBlock, SystemInfo};
use crate::config::CompactionMode;
use crate::conversation::settings::{
    DelegationMode, MemoryMode, ResolvedConversationSettings, ResponseMode,
};
use crate::conversation::{
    ActiveParticipant, ChannelStore, ConversationLogger, ProcessRunLogger,
    participant_display_name, participant_memory_key, renderable_participants,
    track_active_participant,
};
use crate::error::{AgentError, Result};
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::{
    AgentDeps, BranchId, ChannelId, InboundMessage, OutboundResponse, ProcessEvent, ProcessId,
    ProcessType, RoutedResponse, RoutedSender, WorkerId,
};
use rig::agent::AgentBuilder;
use rig::completion::CompletionModel;
use rig::message::UserContent;
use rig::one_or_many::OneOrMany;
use rig::tool::server::ToolServer;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};
use tokio::sync::broadcast;
use tokio::sync::{RwLock, mpsc};

/// Shared cache of in-flight branch and worker transcript steps.
pub type LiveProcessTranscripts =
    Arc<RwLock<HashMap<String, Vec<crate::conversation::worker_transcript::TranscriptStep>>>>;

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

const EVENT_LAG_WARNING_INTERVAL_SECS: u64 = 30;
const RETRIGGER_RELAY_RETRY_LIMIT: u64 = 1;
/// Ceiling on messages restored into live history when a channel starts. The
/// compactor and chronicler trim from there under their own thresholds.
const HYDRATE_MESSAGE_LIMIT: i64 = 200;
const DECISION_MARKERS: &[&str] = &[
    "we decided to ",
    "i decided to ",
    "decision:",
    "the decision is ",
    "approved: ",
    "approved to ",
    "moving forward with ",
    "move forward with ",
    "going with ",
    "switching to ",
    "we will use ",
    "i will use ",
    "we'll use ",
    "i'll use ",
    "we will switch to ",
    "i will switch to ",
    "we'll switch to ",
    "i'll switch to ",
    "we will proceed with ",
    "i will proceed with ",
    "we'll proceed with ",
    "i'll proceed with ",
];
const CHANGE_COMPARISON_VERBS: &[&str] = &[
    "use ",
    "switch",
    "adopt ",
    "choose ",
    "pick ",
    "go with ",
    "proceed with ",
];
const BRANCH_CANCELLED_PREFIX: &str = "Branch cancelled:";
const BRANCH_CANCELLED_SENTENCE: &str = "Branch cancelled.";

async fn recv_channel_event(
    event_rx: &mut broadcast::Receiver<ProcessEvent>,
) -> crate::BroadcastRecvResult<ProcessEvent> {
    crate::classify_broadcast_recv_result(event_rx.recv().await)
}

fn should_process_event_for_channel(event: &ProcessEvent, channel_id: &ChannelId) -> bool {
    event_is_for_channel(event, channel_id)
}

/// Whether an inbound message is a recognized Control command. Control
/// dispatch never runs a turn, so it skips the coalesce flush instead of
/// waiting behind a buffered batch's LLM turn.
fn is_control_command(message: &InboundMessage) -> bool {
    if message.source == "system" {
        return false;
    }
    let text = match &message.content {
        crate::MessageContent::Text(text) => text.clone(),
        crate::MessageContent::Command { .. } => message.content.to_string(),
        _ => return false,
    };
    let bot_username = message
        .metadata
        .get("telegram_bot_username")
        .and_then(serde_json::Value::as_str);
    match crate::commands::REGISTRY.parse_addressed(&text, bot_username) {
        crate::commands::ParseResult::Command(command) => matches!(
            command.def.handler,
            crate::commands::CommandHandler::Control(_)
        ),
        _ => false,
    }
}

/// Enrich an ask-tool interaction with the original question context.
///
/// When an inbound `Interaction` has an `action_id` matching the `ask:` prefix,
/// this looks up the pending question, resolves it, and returns a human-readable
/// enrichment like `Alice answered "Which environment?": staging`.
///
/// Non-ask interactions pass through as-is. Expired or already-resolved questions
/// get an `(expired)` marker.
async fn enrich_ask_interaction(
    pool: &sqlx::SqlitePool,
    sender_name: &str,
    action_id: &str,
    values: &[String],
) -> String {
    let (question_id, option_idx) = match crate::tools::ask::parse_ask_custom_id(action_id) {
        Some(parsed) => parsed,
        None => {
            // Not an ask interaction — use standard display
            if !values.is_empty() {
                return format!("[interaction: {action_id} → {}]", values.join(", "));
            }
            return format!("[interaction: {action_id}]");
        }
    };

    let store = crate::questions::QuestionStore::new(pool.clone());

    match store.get(question_id).await {
        Ok(Some(q)) if q.resolved_at.is_none() => {
            let answer_labels: Vec<String> = match option_idx {
                Some(idx) => {
                    // Button click: use the option at this index
                    q.options
                        .get(idx)
                        .map(|opt| vec![opt.label.clone()])
                        .unwrap_or_default()
                }
                None => {
                    // Select menu: parse values to get indices
                    values
                        .iter()
                        .filter_map(|value| {
                            let (_, idx) = crate::tools::ask::parse_ask_custom_id(value)?;
                            idx.and_then(|i| q.options.get(i).map(|opt| opt.label.clone()))
                        })
                        .collect()
                }
            };

            if answer_labels.is_empty() {
                return format!("[interaction: {action_id}] (expired — no matching options)");
            }

            // The conditional update makes only one concurrent interaction the
            // answer; later clicks must not reach the model as another answer.
            match store.resolve(question_id, &answer_labels).await {
                Ok(true) => {
                    let labels_str = answer_labels.join(", ");
                    format!("{sender_name} answered \"{}\": {labels_str}", q.question)
                }
                Ok(false) => format!("[interaction: {action_id}] (expired)"),
                Err(error) => {
                    tracing::warn!(
                        question_id,
                        %error,
                        "failed to resolve pending question"
                    );
                    format!("[interaction: {action_id}] (expired)")
                }
            }
        }
        Ok(Some(_)) => {
            // Already resolved
            format!("[interaction: {action_id}] (expired)")
        }
        _ => {
            // Question not found or store error
            format!("[interaction: {action_id}] (expired)")
        }
    }
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

fn classify_conversational_event_summary(
    summary: &str,
    default_event_type: crate::memory::WorkingMemoryEventType,
) -> (crate::memory::WorkingMemoryEventType, String) {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return (default_event_type, String::new());
    }

    if let Some((prefix, rest)) = trimmed.split_once(':') {
        let rest_trimmed = rest.trim();
        let prefix = prefix.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        if prefix == "outcome" {
            return (
                crate::memory::WorkingMemoryEventType::Outcome,
                rest_trimmed.to_string(),
            );
        }
        if prefix == "blocked_on" {
            return (
                crate::memory::WorkingMemoryEventType::BlockedOn,
                rest_trimmed.to_string(),
            );
        }
        if prefix == "constraint" {
            return (
                crate::memory::WorkingMemoryEventType::Constraint,
                rest_trimmed.to_string(),
            );
        }
        if prefix == "deadline_set" || prefix == "deadline" {
            return (
                crate::memory::WorkingMemoryEventType::DeadlineSet,
                rest_trimmed.to_string(),
            );
        }
    }

    (default_event_type, trimmed.to_string())
}

fn format_conversational_event_summary(
    event_type: crate::memory::WorkingMemoryEventType,
    source: &str,
    event_summary: &str,
) -> String {
    let label = match event_type {
        crate::memory::WorkingMemoryEventType::Outcome => "outcome",
        crate::memory::WorkingMemoryEventType::BlockedOn => "blocked on",
        crate::memory::WorkingMemoryEventType::Constraint => "constraint",
        crate::memory::WorkingMemoryEventType::DeadlineSet => "deadline set",
        crate::memory::WorkingMemoryEventType::Error => "failed",
        crate::memory::WorkingMemoryEventType::BranchCompleted
        | crate::memory::WorkingMemoryEventType::WorkerCompleted => "completed",
        _ => "concluded",
    };

    if event_summary.is_empty() {
        format!("{source} {label}")
    } else {
        format!("{source} {label}: {event_summary}")
    }
}

fn truncate_working_memory_summary(summary: &str) -> String {
    if summary.len() > 200 {
        let boundary = summary.floor_char_boundary(200);
        format!("{}...", &summary[..boundary])
    } else {
        summary.to_string()
    }
}

fn branch_working_memory_event_summary(
    conclusion: &str,
) -> (crate::memory::WorkingMemoryEventType, String) {
    if let Some(reason) = parse_branch_cancellation_reason(conclusion) {
        let reason = truncate_working_memory_summary(reason.trim());
        let summary = if reason.is_empty() {
            "Branch cancelled".to_string()
        } else {
            format!("Branch cancelled: {reason}")
        };
        return (crate::memory::WorkingMemoryEventType::Error, summary);
    }

    let summary = truncate_working_memory_summary(conclusion);
    let (event_type, event_summary) = classify_conversational_event_summary(
        &summary,
        crate::memory::WorkingMemoryEventType::BranchCompleted,
    );
    (
        event_type,
        format_conversational_event_summary(event_type, "Branch", &event_summary),
    )
}

fn parse_branch_cancellation_reason(conclusion: &str) -> Option<&str> {
    let trimmed = conclusion.trim();
    if let Some(rest) = trimmed.strip_prefix(BRANCH_CANCELLED_PREFIX) {
        return Some(rest);
    }
    if let Some(rest) = trimmed.strip_prefix(BRANCH_CANCELLED_SENTENCE) {
        return Some(rest);
    }
    None
}

fn sentence_contains_decision_marker(sentence: &str) -> bool {
    let sentence_lower = sentence.to_ascii_lowercase();
    DECISION_MARKERS
        .iter()
        .any(|marker| sentence_lower.contains(marker))
        || (sentence_lower.contains(" instead of ")
            && CHANGE_COMPARISON_VERBS
                .iter()
                .any(|marker| sentence_lower.contains(marker)))
}

fn extract_decision_summary_from_reply(reply_text: &str) -> Option<String> {
    let normalized = reply_text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let has_explicit_marker = DECISION_MARKERS.iter().any(|marker| lower.contains(marker));
    let has_change_comparison = lower.contains(" instead of ")
        && CHANGE_COMPARISON_VERBS
            .iter()
            .any(|marker| lower.contains(marker));

    if !has_explicit_marker && !has_change_comparison {
        return None;
    }

    let sentences: Vec<&str> = trimmed
        .split_terminator(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect();

    let mut summary = sentences
        .iter()
        .copied()
        .find(|sentence| sentence_contains_decision_marker(sentence))
        .or_else(|| sentences.first().copied())
        .unwrap_or(trimmed)
        .trim()
        .to_string();

    if summary.len() > 200 {
        let boundary = summary.floor_char_boundary(200);
        summary.truncate(boundary);
        summary.push_str("...");
    }

    Some(summary)
}

fn decision_user_id(
    humans: &[crate::config::HumanDef],
    message: &InboundMessage,
    is_retrigger: bool,
) -> Option<String> {
    if is_retrigger || message.source == "system" {
        return None;
    }

    let source = message.source.trim();
    if source.is_empty() || message.sender_id.is_empty() {
        return None;
    }

    Some(participant_memory_key(
        humans,
        source,
        message.adapter.as_deref(),
        &message.sender_id,
    ))
}

struct AgentTurnResult {
    result: std::result::Result<String, rig::completion::PromptError>,
    skip_flag: crate::tools::SkipFlag,
    replied_flag: crate::tools::RepliedFlag,
    delivered_flag: crate::tools::DeliveredFlag,
    retrigger_reply_preserved: bool,
    reply_text: Option<String>,
}

/// What kind of conversation a channel is serving.
///
/// Channels behave differently depending on who is on the other end: user
/// channels are driven by incoming messages, cron channels are one-shot, and
/// autonomy is a resident system channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    User,
    Cron,
    Autonomy,
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Cron => write!(f, "cron"),
            Self::Autonomy => write!(f, "autonomy"),
        }
    }
}

impl ChannelKind {
    /// Cron channels receive one prompt and exit once all work settles.
    pub fn self_exits(&self) -> bool {
        matches!(self, ChannelKind::Cron)
    }

    /// System-initiated runs repeat the same procedure on a schedule and
    /// would grind out noise skills, so they never trigger skill reflection.
    pub fn suppresses_reflection(&self) -> bool {
        matches!(self, ChannelKind::Cron | ChannelKind::Autonomy)
    }

    /// System-initiated channels have no user to send a reset message, so the
    /// retrigger cap would permanently stall multi-worker jobs.
    pub fn caps_retriggers(&self) -> bool {
        matches!(self, ChannelKind::User)
    }
}

/// Shared state that channel tools need to act on the channel.
///
/// Wrapped in Arc and passed to tools (branch, spawn_worker, route, cancel)
/// so they can create real Branch/Worker processes when the LLM invokes them.
#[derive(Clone)]
pub struct ChannelState {
    pub channel_id: ChannelId,
    pub kind: ChannelKind,
    pub history: Arc<RwLock<Vec<rig::message::Message>>>,
    /// Guards `history` against cuts that raced a mutation. Writers outside the
    /// channel's own turn loop must note their mutation so an in-flight cut
    /// holding an older snapshot declines to trim.
    pub history_fence: Arc<crate::agent::chronicle::HistoryFence>,
    pub active_branches: Arc<RwLock<HashMap<BranchId, tokio::task::JoinHandle<()>>>>,
    pub status_block: Arc<RwLock<StatusBlock>>,
    pub deps: AgentDeps,
    pub conversation_logger: ConversationLogger,
    pub process_run_logger: ProcessRunLogger,
    /// Discord message ID to reply to for work spawned in the current turn.
    pub reply_target_message_id: Arc<RwLock<Option<String>>>,
    pub channel_store: ChannelStore,
    pub screenshot_dir: std::path::PathBuf,
    pub logs_dir: std::path::PathBuf,
    /// Prompt snapshot store for debugging prompt construction.
    /// Shared live transcript cache for running workers. When a worker is
    /// cancelled via `handle.abort()`, we drain its accumulated transcript
    /// steps from this cache and persist them to the DB so that cancelled
    /// workers still have their transcript available for review.
    ///
    /// This Arc is shared with `ApiState` — the event loop populates it from
    /// `ToolStarted`/`ToolCompleted` events as they flow through the system.
    /// Defaults to a standalone empty map when the API layer is not active.
    pub live_process_transcripts: LiveProcessTranscripts,
    /// Worker context settings inherited from conversation settings.
    /// Determines what context workers spawned from this channel receive.
    pub worker_context_settings: Arc<RwLock<crate::conversation::settings::WorkerContextMode>>,
    /// Resolved model overrides from conversation settings.
    /// Used by branches, workers, and compactor to resolve their model.
    pub model_overrides: Arc<crate::conversation::settings::ResolvedConversationSettings>,
    /// Active participants seen during the current channel session.
    pub active_participants: Arc<RwLock<HashMap<String, ActiveParticipant>>>,
    /// Optional cron outcome for the `set_outcome` tool.
    /// When set, the `set_outcome` tool is registered for this channel,
    /// allowing the LLM to explicitly store a delivery payload.
    pub cron_outcome: Option<crate::cron::CronOutcome>,
    /// Whether a turn is currently in flight. Read by the inbound router's
    /// busy policy without entering the channel's message queue.
    pub turn_active: Arc<std::sync::atomic::AtomicBool>,
    /// Per-channel session cache of resolved human anchors (participant key
    /// -> anchor content). Misses are cached too (3.1a in-turn resolution).
    pub human_anchor_cache: Arc<tokio::sync::Mutex<HashMap<String, Option<String>>>>,
    /// Live response mode, encoded via `ResponseMode::to_u8`. Shared so the
    /// router-side control plane can apply a mode change mid-turn; the
    /// channel reads it at every gate instead of its startup snapshot.
    pub response_mode: Arc<std::sync::atomic::AtomicU8>,
    /// Current autonomy epoch. The slot survives between epochs while each
    /// generation gets a fresh handle and completion contract.
    pub autonomy_run: Option<crate::agent::autonomy::AutonomyRunSlot>,
}

impl ChannelState {
    pub fn autonomy_run(&self) -> Option<crate::agent::autonomy::AutonomyRunHandle> {
        self.autonomy_run
            .as_ref()
            .and_then(crate::agent::autonomy::AutonomyRunSlot::current)
    }

    /// Append a message this agent sent into the channel from outside the
    /// channel's own turn loop, so the next turn sees what was said.
    ///
    /// The durable log is written separately by the sender; this keeps the live
    /// history from diverging from it while the channel is resident in memory.
    pub async fn inject_agent_message(&self, text: &str) {
        {
            let mut history = self.history.write().await;
            history.push(rig::message::Message::Assistant {
                id: None,
                content: OneOrMany::one(rig::message::AssistantContent::text(text)),
            });
        }
        self.history_fence.note_head_mutation();
    }

    /// Cancel a running branch by aborting its tokio task.
    /// Returns an error message if the branch is not found.
    pub async fn cancel_branch(&self, branch_id: BranchId) -> std::result::Result<(), String> {
        self.cancel_branch_with_reason(branch_id, "cancelled by channel")
            .await
    }

    /// Cancel a running branch by aborting its tokio task.
    /// Emits a synthetic terminal result so the event handler can clean up
    /// active_branches and trigger a retrigger with the cancellation reason.
    pub async fn cancel_branch_with_reason(
        &self,
        branch_id: BranchId,
        reason: &str,
    ) -> std::result::Result<(), String> {
        // Abort via read access so the handle stays in active_branches.
        // The BranchResult event handler will remove it and trigger a retrigger.
        let aborted = {
            let branches = self.active_branches.read().await;
            if let Some(handle) = branches.get(&branch_id) {
                handle.abort();
                true
            } else {
                false
            }
        };

        if !aborted {
            let removed_status = self.status_block.write().await.remove_branch(branch_id);
            if removed_status {
                return Ok(());
            }
            return Err(format!("Branch {branch_id} not found"));
        }

        let reason = crate::summarize_first_non_empty_line(reason, crate::EVENT_SUMMARY_MAX_CHARS);
        let conclusion = if reason.is_empty() {
            BRANCH_CANCELLED_SENTENCE.to_string()
        } else {
            format!("{BRANCH_CANCELLED_PREFIX} {reason}")
        };
        let live_steps = self
            .live_process_transcripts
            .write()
            .await
            .remove(&ProcessId::Branch(branch_id).to_string());
        let transcript = live_steps.as_deref().and_then(|steps| {
            (!steps.is_empty())
                .then(|| crate::conversation::worker_transcript::serialize_steps(steps))
        });
        let tool_calls = live_steps
            .as_deref()
            .map(count_transcript_tool_calls)
            .unwrap_or(0);
        if let Err(error) = self.deps.event_tx.send(ProcessEvent::BranchResult {
            agent_id: self.deps.agent_id.clone(),
            branch_id,
            channel_id: self.channel_id.clone(),
            conclusion,
            status: "cancelled".to_string(),
            transcript,
            tool_calls,
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

fn count_transcript_tool_calls(
    steps: &[crate::conversation::worker_transcript::TranscriptStep],
) -> i64 {
    steps
        .iter()
        .map(|step| match step {
            crate::conversation::worker_transcript::TranscriptStep::Action { content } => content
                .iter()
                .filter(|content| {
                    matches!(
                        content,
                        crate::conversation::worker_transcript::ActionContent::ToolCall { .. }
                    )
                })
                .count()
                as i64,
            _ => 0,
        })
        .sum()
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

    pub fn state(&self) -> &ChannelState {
        &self.inner.state
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

    /// Whether the channel currently has a turn in flight. Read by the
    /// inbound router's busy policy.
    pub fn turn_active(&self) -> bool {
        self.inner
            .state
            .turn_active
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Live response mode for this channel.
    pub fn response_mode(&self) -> crate::conversation::settings::ResponseMode {
        crate::conversation::settings::ResponseMode::from_u8(
            self.inner
                .state
                .response_mode
                .load(std::sync::atomic::Ordering::Acquire),
        )
    }

    /// Apply a response-mode change to the running channel. Persistence is
    /// the caller's job (the control plane writes the settings store); this
    /// updates the live cell every gate reads.
    pub fn set_response_mode_live(&self, mode: crate::conversation::settings::ResponseMode) {
        self.inner
            .state
            .response_mode
            .store(mode.to_u8(), std::sync::atomic::Ordering::Release);
    }
}

/// RAII flag for the shared turn-active cell: set on entry to message
/// handling, cleared when the turn ends by any path (early return, error,
/// panic unwind).
struct TurnActiveGuard(Arc<std::sync::atomic::AtomicBool>);

impl TurnActiveGuard {
    fn engage(flag: &Arc<std::sync::atomic::AtomicBool>) -> Self {
        flag.store(true, std::sync::atomic::Ordering::Release);
        Self(flag.clone())
    }
}

impl Drop for TurnActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
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
    pub response_tx: mpsc::Sender<RoutedResponse>,
    /// Self-sender for re-triggering the channel after background process completion.
    pub self_tx: mpsc::Sender<InboundMessage>,
    /// The inbound message currently being processed. Used to pair outbound
    /// responses with the correct platform routing metadata (e.g. Slack thread_ts).
    current_inbound: Option<InboundMessage>,
    /// Conversation message id the current turn is answering, so a captured
    /// request can be traced back to the message that caused it.
    current_message_id: Option<String>,
    /// Conversation ID from the first message (for synthetic re-trigger messages).
    pub conversation_id: Option<String>,
    /// Adapter source captured from the first non-system message.
    pub source_adapter: Option<String>,
    /// Conversation context (platform, channel name, server) captured from the first message.
    pub conversation_context: Option<String>,
    /// Context monitor that triggers background compaction.
    pub compactor: Compactor,
    /// Context monitor for chronicle mode. Only one of the two acts per turn,
    /// selected by `CompactionConfig::mode`; short-lived system channels always
    /// use the compactor.
    pub chronicler: Chronicler,
    /// Count of user messages since last memory persistence branch.
    message_count: usize,
    /// When the last memory persistence branch was triggered.
    last_persistence_at: std::time::Instant,
    /// Set when a turn or worker crossed the reflection work threshold.
    /// Consumed by the next persistence branch, which then also reflects
    /// on skills. The worker ids are handed to that branch so it can pull
    /// their transcripts via `worker_inspect` — the lesson usually lives in
    /// what the worker tried, not in the summary it returned. A mutex (not
    /// an atomic) because turn processing marks it through `&self`.
    reflection_signal: std::sync::Mutex<ReflectionSignal>,
    /// When the last skill-reflection pass was spawned, for cooldown.
    last_reflection_at: Option<std::time::Instant>,
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
    /// A result relay that exhausted automatic retries and waits for the next
    /// real user turn before another bounded attempt.
    deferred_retriggers: VecDeque<InboundMessage>,
    consumed_worker_outcomes: HashMap<WorkerId, i64>,
    /// Optional send_agent_message tool (only when agent has active links).
    send_agent_message_tool: Option<crate::tools::SendAgentMessageTool>,
    /// Backfilled conversation history rendered as a system-prompt fragment.
    /// Injected into the system prompt (not into chat history) so the LLM
    /// treats it as read-only context rather than actionable user messages.
    backfill_transcript: Option<String>,
    /// Retry prompts sent for the current autonomy epoch's completion contract.
    autonomy_contract_retries: usize,
    /// A lifecycle event was dropped from the broadcast receiver. While set,
    /// heartbeats reconcile owned workers from durable lifecycle state.
    autonomy_event_lagged: bool,
    /// Handle exposed to the supervision control plane.
    control_handle: ChannelControlHandle,
    /// Per-conversation resolved settings (memory mode, delegation mode, model override).
    pub resolved_settings: ResolvedConversationSettings,
}

/// What accumulated between skill-reflection passes: whether a turn crossed
/// the tool-iteration threshold, and which workers completed since the last
/// pass. Drained by the persistence branch that performs the reflection.
///
/// Failed completions are collected too — their transcripts are where the
/// trials live — but only successful ones make the signal fire: an
/// unresolved failure alone has nothing to teach.
#[derive(Debug, Default, Clone)]
struct ReflectionSignal {
    /// A channel turn crossed `min_tool_iterations` tool calls.
    turn_work: bool,
    /// Workers that completed since the last reflection pass, in completion
    /// order, with whether each succeeded.
    workers: Vec<(WorkerId, bool)>,
}

impl ReflectionSignal {
    fn is_set(&self) -> bool {
        self.turn_work || self.workers.iter().any(|(_, success)| *success)
    }

    /// Record a completed worker for the next reflection pass. Repeat
    /// completions for the same worker are ignored so a retriggered event
    /// can't queue the same transcript twice.
    fn record_worker(&mut self, worker_id: WorkerId, success: bool) {
        if self.workers.iter().any(|(id, _)| *id == worker_id) {
            return;
        }
        self.workers.push((worker_id, success));
    }
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
    fn record_decision_event(&self, reply_text: Option<&str>, user_id: Option<String>) {
        let Some(decision_summary) = reply_text.and_then(extract_decision_summary_from_reply)
        else {
            return;
        };

        let mut event = self
            .deps
            .working_memory
            .emit(
                crate::memory::WorkingMemoryEventType::Decision,
                decision_summary,
            )
            .channel(self.id.as_ref())
            .importance(0.8);
        if let Some(user_id) = user_id {
            event = event.user(user_id);
        }
        event.record();
    }

    /// Create a new channel.
    ///
    /// All tunable config (prompts, routing, thresholds, browser, skills) is read
    /// from `deps.runtime_config` on each use, so changes propagate to running
    /// channels without restart.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ChannelId,
        kind: ChannelKind,
        deps: AgentDeps,
        response_tx: mpsc::Sender<RoutedResponse>,
        event_rx: broadcast::Receiver<ProcessEvent>,
        screenshot_dir: std::path::PathBuf,
        logs_dir: std::path::PathBuf,
        live_process_transcripts: Option<LiveProcessTranscripts>,
        resolved_settings: ResolvedConversationSettings,
        cron_outcome: Option<crate::cron::CronOutcome>,
        autonomy_run: Option<crate::agent::autonomy::AutonomyRunSlot>,
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
        let (message_tx, message_rx) = mpsc::channel(64);

        let conversation_logger = ConversationLogger::new(deps.sqlite_pool.clone());
        let process_run_logger = ProcessRunLogger::new(deps.sqlite_pool.clone());
        let channel_store = ChannelStore::new(deps.sqlite_pool.clone());

        let compactor_model = resolved_settings
            .resolve_model("compactor")
            .map(String::from);
        // One fence shared by both monitors: a mode switch must not leave a
        // rolling compaction and an in-flight chronicle cut mutating the same
        // head independently.
        let history_fence = Arc::new(crate::agent::chronicle::HistoryFence::new());
        let compactor = Compactor::new(
            id.clone(),
            deps.clone(),
            history.clone(),
            compactor_model.clone(),
            history_fence.clone(),
        );
        let chronicler = Chronicler::new(
            id.clone(),
            deps.clone(),
            history.clone(),
            compactor_model,
            history_fence.clone(),
        );

        let state = ChannelState {
            channel_id: id.clone(),
            kind,
            history: history.clone(),
            history_fence: history_fence.clone(),
            active_branches: active_branches.clone(),
            status_block: status_block.clone(),
            deps: deps.clone(),
            conversation_logger,
            process_run_logger,
            reply_target_message_id: Arc::new(RwLock::new(None)),
            channel_store: channel_store.clone(),
            screenshot_dir,
            logs_dir,
            live_process_transcripts: live_process_transcripts
                .unwrap_or_else(|| Arc::new(RwLock::new(HashMap::new()))),
            worker_context_settings: Arc::new(RwLock::new(
                resolved_settings.worker_context.clone(),
            )),
            model_overrides: Arc::new(resolved_settings.clone()),
            active_participants: Arc::new(RwLock::new(HashMap::new())),
            cron_outcome,
            turn_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            response_mode: Arc::new(std::sync::atomic::AtomicU8::new(
                resolved_settings.response_mode.to_u8(),
            )),
            autonomy_run,
            human_anchor_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };

        // Each channel gets its own isolated tool server to avoid races between
        // concurrent channels sharing per-turn add/remove cycles.
        let tool_server = ToolServer::new().run();

        // Construct the send_agent_message tool if this agent has links.
        let send_agent_message_tool = {
            let has_links =
                !crate::links::links_for_agent(&deps.links.load(), &deps.agent_id).is_empty();
            if has_links {
                let mut tool = crate::tools::SendAgentMessageTool::new(
                    deps.agent_id.clone(),
                    deps.links.clone(),
                    deps.agent_names.clone(),
                    deps.task_store.clone(),
                    ConversationLogger::new(deps.sqlite_pool.clone()),
                );
                if let Some(wake_tx) = deps.wake_tx.clone() {
                    tool = tool.with_wake_tx(wake_tx);
                }
                Some(tool)
            } else {
                None
            }
        };

        let self_tx = message_tx.clone();
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
            current_inbound: None,
            current_message_id: None,
            conversation_id: None,
            source_adapter: None,
            conversation_context: None,
            compactor,
            chronicler,
            message_count: 0,
            last_persistence_at: std::time::Instant::now(),
            memory_persistence_branches: HashSet::new(),
            reflection_signal: std::sync::Mutex::new(ReflectionSignal::default()),
            last_reflection_at: None,
            branch_reply_targets: HashMap::new(),
            coalesce_buffer: Vec::new(),
            coalesce_deadline: None,
            retrigger_count: 0,
            pending_retrigger: false,
            pending_retrigger_metadata: HashMap::new(),
            retrigger_deadline: None,
            pending_results: Vec::new(),
            deferred_retriggers: VecDeque::new(),
            consumed_worker_outcomes: HashMap::new(),
            send_agent_message_tool,
            backfill_transcript: None,
            autonomy_contract_retries: 0,
            autonomy_event_lagged: false,
            control_handle,
            resolved_settings,
        };

        (channel, message_tx)
    }

    /// Set the backfill transcript for injection into the system prompt.
    /// Whether this channel sheds history through the chronicle.
    ///
    /// System channels use bounded run summaries instead of chronicles.
    fn uses_chronicle(&self) -> bool {
        self.deps.runtime_config.compaction.load().mode == CompactionMode::Chronicle
            && self.state.kind == ChannelKind::User
    }

    /// Run whichever context monitor this channel is configured for.
    ///
    /// Called after every turn. Both monitors are non-blocking: the LLM work
    /// happens on a spawned task, and only emergency truncation is synchronous.
    async fn maintain_context(&self) {
        let chronicle = self.uses_chronicle();

        // Resolving the mode is also how a switch is detected: a different mode
        // bumps the epoch, which invalidates any cut still running under the
        // previous one before it can commit or trim.
        self.chronicler.fence().observe_mode(chronicle);

        if chronicle {
            self.record_turn_boundary().await;
        }

        let result = if chronicle {
            self.chronicler.check_and_chronicle().await.map(|_| ())
        } else {
            self.compactor.check_and_compact().await.map(|_| ())
        };

        if let Err(error) = result {
            tracing::warn!(channel_id = %self.id, %error, "context maintenance check failed");
        }
    }

    /// Rebuild live history from the durable log so a resident channel resumes
    /// where it left off instead of starting blank after a restart.
    ///
    /// Under chronicle mode the load starts at the newest checkpoint boundary:
    /// the chronicle view already covers everything below it, so loading the
    /// uncovered tail reproduces exactly what the chronicler expects to see.
    /// Rolling compaction has no durable boundary, so it takes the newest slice
    /// it can afford instead.
    ///
    /// System-initiated channels are excluded: each cron or autonomy run is a
    /// fresh single-shot session whose prior rows are its own wake prompts, and
    /// its briefing carries the continuity it needs.
    async fn hydrate_history(&mut self) {
        if self.state.kind != ChannelKind::User {
            return;
        }
        if !self.state.history.read().await.is_empty() {
            return;
        }

        let store =
            crate::conversation::chronicle::ChronicleStore::new(self.deps.sqlite_pool.clone());
        let chronicle = self.deps.runtime_config.compaction.load().mode
            == crate::config::CompactionMode::Chronicle;

        let boundary = if chronicle {
            match store.latest(&self.id, 0).await {
                Ok(Some(checkpoint)) => checkpoint.end_boundary(),
                Ok(None) => crate::conversation::chronicle::ChronicleBoundary::origin(),
                Err(error) => {
                    tracing::warn!(channel_id = %self.id, %error, "chronicle lookup failed, skipping hydration");
                    return;
                }
            }
        } else {
            crate::conversation::chronicle::ChronicleBoundary::origin()
        };

        let loaded = if chronicle {
            store
                .messages_after(&self.id, boundary, HYDRATE_MESSAGE_LIMIT)
                .await
        } else {
            store
                .newest_messages_after(&self.id, boundary, HYDRATE_MESSAGE_LIMIT)
                .await
        };

        let messages = match loaded {
            Ok(messages) if messages.is_empty() => return,
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(channel_id = %self.id, %error, "history hydration failed");
                return;
            }
        };

        let durable_seq = messages.iter().filter_map(|message| message.seq).max();
        let live_len = {
            let mut history = self.state.history.write().await;
            for message in &messages {
                history.push(if message.role == "assistant" {
                    rig::message::Message::Assistant {
                        id: None,
                        content: OneOrMany::one(rig::message::AssistantContent::text(
                            &message.content,
                        )),
                    }
                } else {
                    rig::message::Message::User {
                        content: OneOrMany::one(UserContent::text(&message.content)),
                    }
                });
            }
            history.len()
        };

        if let Some(seq) = durable_seq {
            self.chronicler.fence().record_turn(live_len, seq);
        }
        tracing::info!(
            channel_id = %self.id,
            restored = live_len,
            chronicle,
            "restored live history from durable log"
        );
    }

    /// Pair the live history length with the durable sequence that covers it.
    ///
    /// The chronicle trims only to one of these, so a turn's tool traffic is
    /// never split off from the durable rows that summarize it. The turn's own
    /// writes are fire-and-forget, so they are drained first — reading the
    /// watermark early would place this boundary a turn behind and the trim
    /// would never catch up.
    async fn record_turn_boundary(&self) {
        const WRITE_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

        let drained = self
            .state
            .conversation_logger
            .wait_for_pending_writes(WRITE_DRAIN_TIMEOUT)
            .await;

        let live_len = self.state.history.read().await.len();
        let durable_seq = match self.chronicler.store().max_seq(&self.id).await {
            Ok(seq) => seq,
            Err(error) => {
                tracing::warn!(channel_id = %self.id, %error, "failed to read durable watermark");
                return;
            }
        };

        if !drained {
            tracing::debug!(
                channel_id = %self.id,
                durable_seq,
                "recording a turn boundary before writes drained; the trim will keep more"
            );
        }

        self.chronicler.fence().record_turn(live_len, durable_seq);
    }

    /// The bounded chronicle view for this channel's system prompt.
    ///
    /// Recomputed from durable state each turn, so a restarted channel renders
    /// the same section the running one did.
    async fn render_session_chronicle(&self) -> Option<String> {
        // Deliberately not gated on the current mode. Chronicle mode trims
        // checkpointed ranges out of live history; if switching to rolling also
        // stopped rendering those checkpoints, everything they covered would
        // vanish from the prompt and the channel would resume from only the
        // uncheckpointed tail. A channel that has ever chronicled keeps its
        // chronicle view — cutting new checkpoints is what the mode governs.
        if self.state.kind.self_exits() {
            return None;
        }

        let config = self.deps.runtime_config.compaction.load().chronicle;
        match crate::agent::chronicle::render_chronicle_view(
            self.chronicler.store(),
            &self.id,
            chrono::Utc::now(),
            config,
        )
        .await
        {
            Ok(view) => view,
            Err(error) => {
                tracing::warn!(channel_id = %self.id, %error, "failed to render session chronicle");
                None
            }
        }
    }

    pub fn set_backfill_transcript(&mut self, transcript: String) {
        self.backfill_transcript = Some(transcript);
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

    /// Re-load settings from the database after a SettingsUpdated event.
    async fn reload_settings(&mut self) {
        let agent_id = self.deps.agent_id.to_string();
        let channel_id = self.id.as_ref();

        // Try portal store first, then channel_settings
        let new_settings = if channel_id.starts_with("portal:chat:") {
            let store =
                crate::conversation::PortalConversationStore::new(self.deps.sqlite_pool.clone());
            match store.get(&agent_id, channel_id).await {
                Ok(Some(conv)) => conv.settings,
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        channel_id = %self.id,
                        "failed to reload portal settings, preserving existing"
                    );
                    return;
                }
            }
        } else {
            let store =
                crate::conversation::ChannelSettingsStore::new(self.deps.sqlite_pool.clone());
            match store.get(&agent_id, channel_id).await {
                Ok(settings) => settings,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        channel_id = %self.id,
                        "failed to reload channel settings, preserving existing"
                    );
                    return;
                }
            }
        };

        let resolved = crate::conversation::settings::ResolvedConversationSettings::resolve(
            new_settings.as_ref(),
            None,
            None,
        );

        tracing::info!(
            channel_id = %self.id,
            response_mode = ?resolved.response_mode,
            model = ?resolved.model,
            "settings hot-reloaded"
        );

        // Update shared state for branches/workers
        *self.state.worker_context_settings.write().await = resolved.worker_context.clone();
        self.state.model_overrides = std::sync::Arc::new(resolved.clone());
        self.state.response_mode.store(
            resolved.response_mode.to_u8(),
            std::sync::atomic::Ordering::Release,
        );
        self.resolved_settings = resolved;
    }

    /// Whether the channel is in a non-active response mode (Observe or MentionOnly).
    fn is_suppressed(&self) -> bool {
        !matches!(self.response_mode(), ResponseMode::Active)
    }

    /// Live response mode. Reads the shared cell rather than the startup
    /// snapshot so router-side `/quiet` (and friends) apply mid-turn.
    fn response_mode(&self) -> ResponseMode {
        ResponseMode::from_u8(
            self.state
                .response_mode
                .load(std::sync::atomic::Ordering::Acquire),
        )
    }

    /// Update the response mode and persist to the channel_settings table.
    async fn set_response_mode(&mut self, mode: ResponseMode) {
        self.resolved_settings.response_mode = mode;
        self.state
            .response_mode
            .store(mode.to_u8(), std::sync::atomic::Ordering::Release);

        // Persist on a spawned task to avoid blocking the channel. The
        // store's atomic field update leaves the rest of the settings row
        // untouched, so this write can't restore stale fields over a
        // concurrent settings writer or a router-side mode change.
        let pool = self.deps.sqlite_pool.clone();
        let agent_id = self.deps.agent_id.clone();
        let channel_id: String = self.id.as_ref().to_owned();
        tokio::spawn(async move {
            let store = crate::conversation::ChannelSettingsStore::new(pool);
            if let Err(error) = store.set_response_mode(&agent_id, &channel_id, mode).await {
                tracing::warn!(
                    %error,
                    %channel_id,
                    ?mode,
                    "failed to persist response_mode to channel_settings"
                );
            }
        });
    }

    /// Persist an inbound user message and return the id it was stored under.
    ///
    /// System messages are not persisted, so they have no id to return.
    fn persist_inbound_user_message(
        &self,
        message: &InboundMessage,
        raw_text: &str,
        saved_attachments: Option<&[channel_attachments::SavedAttachmentMeta]>,
    ) -> Option<String> {
        if message.source == "system" {
            return None;
        }
        let sender_name = participant_display_name(message);

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

        let message_id = self.state.conversation_logger.log_user_message(
            &self.state.channel_id,
            &sender_name,
            &message.sender_id,
            raw_text,
            &metadata,
        );
        self.state
            .channel_store
            .upsert(&message.conversation_id, &metadata);
        Some(message_id)
    }

    fn suppress_plaintext_fallback(&self) -> bool {
        self.state.kind == ChannelKind::Cron || matches!(self.current_adapter(), Some("email"))
    }

    async fn track_participant_from_message(&self, message: &InboundMessage) {
        if message.source == "system" {
            return;
        }

        let humans = self.deps.humans.load();
        let mut participants = self.state.active_participants.write().await;
        track_active_participant(&mut participants, humans.as_ref(), message);
    }

    /// Return a handle that allows external supervision to cancel this channel's
    /// workers and branches without direct access to Channel internals.
    pub fn control_handle(&self) -> ChannelControlHandle {
        self.control_handle.clone()
    }

    fn compute_listen_mode_invocation(
        &self,
        message: &InboundMessage,
        raw_text: &str,
    ) -> (bool, bool, bool) {
        compute_listen_mode_invocation(message, raw_text)
    }

    /// Send a routed response paired with the current inbound message.
    ///
    /// Falls back to a bare response with a placeholder target if no inbound
    /// message is set (should not happen during normal turn processing).
    async fn send_routed(
        &self,
        response: OutboundResponse,
    ) -> std::result::Result<(), mpsc::error::SendError<RoutedResponse>> {
        let routed = match &self.current_inbound {
            Some(target) => RoutedResponse {
                response,
                target: target.clone(),
                delivery_receipt: None,
            },
            None => {
                tracing::warn!(
                    channel_id = %self.id,
                    "sending response without a current inbound message"
                );
                RoutedResponse {
                    response,
                    target: InboundMessage::empty(),
                    delivery_receipt: None,
                }
            }
        };
        self.response_tx.send(routed).await
    }

    async fn send_routed_confirmed(
        &self,
        response: OutboundResponse,
    ) -> std::result::Result<(), crate::RoutedDeliveryError> {
        let target = self
            .current_inbound
            .clone()
            .unwrap_or_else(InboundMessage::empty);
        RoutedSender::new(self.response_tx.clone(), target)
            .send_confirmed(response)
            .await
    }

    /// Drain accumulated channel tool calls from ApiState and serialize as JSON.
    /// Returns `None` if there are no tool calls or ApiState is unavailable.
    async fn drain_tool_calls_json(&self) -> Option<String> {
        let api_state = self.state.deps.api_state.as_ref()?;
        let calls = api_state.take_channel_tool_calls(&self.id).await;
        if calls.is_empty() {
            return None;
        }
        serde_json::to_string(&calls).ok()
    }

    async fn send_builtin_text(&mut self, text: String, log_label: &str) {
        match self.send_routed(OutboundResponse::Text(text.clone())).await {
            Ok(()) => {
                #[cfg(feature = "metrics")]
                {
                    let channel_type = self.current_adapter().unwrap_or("unknown");
                    crate::telemetry::Metrics::global()
                        .messages_sent_total
                        .with_label_values(&[&self.deps.agent_id, channel_type])
                        .inc();
                }
                let tool_calls_json = self.drain_tool_calls_json().await;
                self.state
                    .conversation_logger
                    .log_bot_message_with_metadata(
                        &self.state.channel_id,
                        &text,
                        Some(self.agent_display_name()),
                        tool_calls_json,
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

    /// Execute a control-plane command. These run deterministically against
    /// channel state and never consume an agent turn.
    async fn handle_control_command(
        &mut self,
        def: &'static crate::commands::CommandDef,
        action: crate::commands::ControlAction,
        args: &str,
        is_authority: bool,
    ) {
        use crate::commands::ControlAction;

        match action {
            ControlAction::Status => {
                let temporal_context =
                    TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
                let routing = self.deps.runtime_config.routing.load();
                let channel_model = self
                    .resolved_settings
                    .resolve_model("channel")
                    .unwrap_or_else(|| routing.resolve(ProcessType::Channel, None));
                let branch_model = self
                    .resolved_settings
                    .resolve_model("branch")
                    .unwrap_or_else(|| routing.resolve(ProcessType::Branch, None));
                let body = crate::commands::control::status_text(
                    &self.deps.agent_id,
                    &self.id,
                    self.current_adapter().unwrap_or("unknown"),
                    self.response_mode(),
                    channel_model,
                    branch_model,
                    &temporal_context.current_time_line(),
                    self.deps
                        .settings()
                        .and_then(|settings| settings.home_channel())
                        .as_ref(),
                    self.deps.pause_reason().as_deref(),
                );
                self.send_builtin_text(body, def.name).await;
            }
            ControlAction::SetResponseMode(mode) => {
                self.set_response_mode(mode).await;
                self.send_builtin_text(
                    crate::commands::control::mode_confirmation(mode).to_string(),
                    def.name,
                )
                .await;
            }
            ControlAction::Help => {
                self.send_builtin_text(crate::commands::REGISTRY.help_text(), def.name)
                    .await;
            }
            ControlAction::AgentId => {
                self.send_builtin_text(self.deps.agent_id.to_string(), def.name)
                    .await;
            }
            ControlAction::SetHome => {
                let is_portal = self.current_adapter() == Some("portal");
                let reply =
                    crate::commands::control::set_home_channel(&self.deps, &self.id, is_portal)
                        .await;
                self.send_builtin_text(reply, def.name).await;
            }
            ControlAction::SetPause => {
                let reply = crate::commands::control::set_pause(&self.deps, args);
                self.send_builtin_text(reply, def.name).await;
            }
            ControlAction::WhoAmI => {
                let surface = crate::commands::Surface::from_source(
                    self.current_adapter().unwrap_or("unknown"),
                );
                self.send_builtin_text(
                    crate::commands::control::whoami_text(is_authority, surface),
                    def.name,
                )
                .await;
            }
            ControlAction::AutonomyStatus => {
                let reply = crate::commands::control::autonomy_status(&self.deps).await;
                self.send_builtin_text(reply, def.name).await;
            }
            ControlAction::AutonomyOn => {
                let reply =
                    crate::commands::control::set_autonomy_enabled(&self.deps, true, args).await;
                self.send_builtin_text(reply, def.name).await;
            }
            ControlAction::AutonomyOff => {
                let reply =
                    crate::commands::control::set_autonomy_enabled(&self.deps, false, args).await;
                self.send_builtin_text(reply, def.name).await;
            }
        }
    }

    async fn begin_autonomy_epoch(&mut self, generation: u64) -> bool {
        let Some(run) = self.state.autonomy_run() else {
            return false;
        };
        if run.generation != generation {
            tracing::debug!(
                generation,
                current = run.generation,
                "ignoring stale autonomy epoch message"
            );
            return false;
        }

        self.state.history.write().await.clear();
        self.state.history_fence.note_head_mutation();
        self.message_count = 0;
        self.retrigger_count = 0;
        self.pending_retrigger = false;
        self.pending_retrigger_metadata.clear();
        self.retrigger_deadline = None;
        self.coalesce_buffer.clear();
        self.coalesce_deadline = None;
        self.autonomy_contract_retries = 0;
        if !self.pending_results.is_empty() {
            self.pending_retrigger = true;
            self.retrigger_deadline = Some(
                tokio::time::Instant::now()
                    + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
            );
        }
        true
    }

    async fn drive_autonomy_contract(&mut self) -> Result<()> {
        if self.state.kind != ChannelKind::Autonomy {
            return Ok(());
        }
        let Some(run) = self.state.autonomy_run() else {
            return Ok(());
        };
        if run.finish_requested() {
            if !run.has_active_children()
                && !self.pending_retrigger
                && self.retrigger_deadline.is_none()
            {
                run.mark_quiescent();
            }
            return Ok(());
        }
        if self.message_count == 0
            || run.has_active_children()
            || self.pending_retrigger
            || self.retrigger_deadline.is_some()
        {
            return Ok(());
        }

        if self.autonomy_contract_retries < crate::agent::autonomy::AUTONOMY_CONTRACT_MAX_RETRIES {
            self.autonomy_contract_retries += 1;
            let retry_prompt = self
                .deps
                .runtime_config
                .prompts
                .load()
                .render_system_autonomy_contract_retry()?;
            self.handle_message(InboundMessage {
                id: uuid::Uuid::new_v4().to_string(),
                source: "system".into(),
                adapter: None,
                conversation_id: crate::agent::autonomy::AUTONOMY_CONVERSATION_ID.to_string(),
                sender_id: "system".into(),
                agent_id: Some(self.deps.agent_id.clone()),
                content: crate::MessageContent::Text(retry_prompt),
                timestamp: chrono::Utc::now(),
                metadata: HashMap::new(),
                formatted_author: None,
            })
            .await?;
            return Ok(());
        }

        if run
            .request_finish(crate::agent::autonomy::AutonomyFinishRequest {
                summary: crate::agent::autonomy::AUTONOMY_FALLBACK_SUMMARY.to_string(),
                actions: Vec::new(),
            })
            .is_err()
        {
            return Ok(());
        }
        run.mark_quiescent();
        Ok(())
    }

    async fn recover_lagged_autonomy_children(&mut self) -> Result<()> {
        let Some(run) = self.state.autonomy_run() else {
            return Ok(());
        };
        let mut recovered = 0usize;
        for child in run.active_children() {
            let crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id,
            } = child
            else {
                tracing::warn!(
                    ?child,
                    "cannot recover a lagged autonomy branch result from durable state"
                );
                continue;
            };
            let Some(lifecycle) = self
                .state
                .process_run_logger
                .read_worker_lifecycle(worker_id)
                .await?
            else {
                continue;
            };
            if lifecycle == crate::conversation::WorkerLifecycle::WaitingForInput {
                let operation_completed = self
                    .state
                    .deps
                    .process_control_registry
                    .worker_snapshot(worker_id)
                    .await
                    .is_some_and(|snapshot| {
                        snapshot.last_completed_operation_id == Some(operation_id)
                    });
                if !operation_completed {
                    continue;
                }
                self.pending_results.push(PendingResult {
                    process_type: "worker",
                    process_id: worker_id.to_string(),
                    result: "The interactive worker reached idle, but its live result event was lost. Inspect its durable transcript with worker_inspect before deciding the follow-up."
                        .to_string(),
                    success: true,
                });
                run.settle_child(child);
                recovered += 1;
            } else if lifecycle.is_terminal()
                && let Some(terminal) = self
                    .state
                    .process_run_logger
                    .read_worker_terminal(worker_id)
                    .await?
            {
                self.consumed_worker_outcomes
                    .insert(worker_id, terminal.outcome_version);
                self.state
                    .status_block
                    .write()
                    .await
                    .remove_worker(worker_id);
                self.pending_results.push(PendingResult {
                    process_type: "worker",
                    process_id: worker_id.to_string(),
                    result: terminal.result,
                    success: terminal.outcome_kind.is_success(),
                });
                run.settle_child(child);
                recovered += 1;
            }
        }
        if recovered > 0 {
            self.pending_retrigger = true;
            self.retrigger_deadline = Some(
                tokio::time::Instant::now()
                    + std::time::Duration::from_millis(RETRIGGER_DEBOUNCE_MS),
            );
            tracing::warn!(
                recovered,
                "recovered autonomy child results after event receiver lag"
            );
        }
        Ok(())
    }

    /// Run the channel event loop.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!(channel_id = %self.id, "channel started");
        self.hydrate_history().await;
        let mut lagged_events_since_warning: u64 = 0;
        let mut last_lag_warning: Option<std::time::Instant> = None;

        loop {
            self.drive_autonomy_contract().await?;

            // Self-exiting cron channels have no further user messages
            // after the initial prompt. Once all workers/branches finish and no
            // retrigger is pending, exit so the caller can flush the reply buffer.
            // Without this the channel would wait on the broadcast event_rx (which
            // never closes) until the job timeout kills it.
            let has_origin_workers = self
                .state
                .deps
                .process_control_registry
                .list_worker_snapshots()
                .await
                .iter()
                .any(|worker| worker.provenance.origin_channel_id.as_ref() == Some(&self.id));
            if self.state.kind.self_exits()
                && self.message_count > 0
                && !self.pending_retrigger
                && self.retrigger_deadline.is_none()
                && !has_origin_workers
                && self.state.active_branches.read().await.is_empty()
            {
                tracing::info!(channel_id = %self.id, "self-exiting channel finished all work, exiting");
                break;
            }

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
                    if self.state.kind == ChannelKind::Autonomy
                        && let Some(generation) = message
                            .metadata
                            .get(crate::agent::autonomy::AUTONOMY_GENERATION_KEY)
                            .and_then(serde_json::Value::as_u64)
                    {
                        let epoch_start = message
                            .metadata
                            .get(crate::agent::autonomy::AUTONOMY_EPOCH_START_KEY)
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let current = self.state.autonomy_run().map(|run| run.generation);
                        if current != Some(generation) {
                            tracing::debug!(generation, ?current, "ignoring autonomy message for stale generation");
                            continue;
                        }
                        if epoch_start && !self.begin_autonomy_epoch(generation).await {
                            continue;
                        }
                        if self.autonomy_event_lagged {
                            if let Err(error) = self.recover_lagged_autonomy_children().await {
                                tracing::error!(%error, "failed to reconcile autonomy children on heartbeat");
                            }
                            self.autonomy_event_lagged = self
                                .state
                                .autonomy_run()
                                .is_some_and(|run| run.has_active_children());
                        }
                    }
                    let config = self.deps.runtime_config.coalesce.load();
                    if self.should_coalesce(&message, &config) {
                        self.coalesce_buffer.push(message);
                        self.update_coalesce_deadline(&config).await;
                    } else {
                        // Control commands dispatch immediately without
                        // flushing — the buffer keeps its own debounce clock.
                        // Everything else (including Agent commands, which
                        // are joining the conversation) flushes first so
                        // order is preserved.
                        if !is_control_command(&message)
                            && let Err(error) = self.flush_coalesce_buffer().await
                        {
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
                            if self.state.kind == ChannelKind::Autonomy {
                                self.autonomy_event_lagged = true;
                            }
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
                            if self.state.kind == ChannelKind::Autonomy
                                && let Err(error) = self.recover_lagged_autonomy_children().await
                            {
                                tracing::error!(%error, "failed to recover autonomy children after event lag");
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
            crate::MessageContent::Command { .. } => true,
        };
        if looks_like_command {
            return false;
        }
        true
    }

    /// Check if this is a DM (direct message) conversation based on conversation_id.
    fn is_dm(&self) -> bool {
        self.conversation_id
            .as_deref()
            .is_some_and(is_dm_conversation_id)
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
        let _turn_guard = TurnActiveGuard::engage(&self.state.turn_active);

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

        // Track source adapter from the first non-system message
        // Prefer message.adapter (full adapter string like "signal:work") over message.source
        if self.source_adapter.is_none()
            && let Some(first) = messages.first()
            && first.source != "system"
        {
            self.source_adapter = first.adapter.clone().or_else(|| Some(first.source.clone()));
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
                self.conversation_id.as_deref(),
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
                let sender_name = participant_display_name(message);

                let (raw_text, attachments) = match &message.content {
                    crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
                    crate::MessageContent::Media { text, attachments } => {
                        (text.clone().unwrap_or_default(), attachments.clone())
                    }
                    // Render interactions and commands as their Display form
                    // so the LLM sees plain text.
                    crate::MessageContent::Interaction {
                        action_id, values, ..
                    } => {
                        let text = enrich_ask_interaction(
                            &self.deps.sqlite_pool,
                            &sender_name,
                            action_id,
                            values,
                        )
                        .await;
                        (text, Vec::new())
                    }
                    crate::MessageContent::Command { .. } => {
                        (message.content.to_string(), Vec::new())
                    }
                };

                if self.is_suppressed() {
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

                if message.source != "autonomy" {
                    self.current_message_id =
                        Some(self.state.conversation_logger.log_user_message(
                            &self.state.channel_id,
                            &sender_name,
                            &message.sender_id,
                            &raw_text,
                            &metadata,
                        ));
                }
                self.state
                    .channel_store
                    .upsert(&message.conversation_id, &metadata);
                self.track_participant_from_message(message).await;

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

        // Observe mode: always suppress (even with mentions in batch).
        // MentionOnly mode: suppress only when no invocations in the batch.
        let should_suppress_batch = !self.is_dm()
            && match self.response_mode() {
                ResponseMode::Active => false,
                ResponseMode::Observe => true,
                ResponseMode::MentionOnly => !batch_has_invoke,
            };

        if should_suppress_batch {
            tracing::debug!(
                channel_id = %self.id,
                message_count,
                response_mode = ?self.response_mode(),
                "suppressing unsolicited coalesced batch"
            );
            // Inject batch messages into in-memory history so the agent
            // retains channel context.
            {
                let mut history = self.state.history.write().await;
                for (formatted_text, _, _) in &pending_batch_entries {
                    history.push(rig::message::Message::User {
                        content: OneOrMany::one(UserContent::text(formatted_text)),
                    });
                }
            }
            self.maintain_context().await;
            // Both Observe and MentionOnly keep passive memory capture.
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

        // Build system prompt. Time and the coalesce hint ride on the user
        // message envelope below instead of the system prompt — the prompt
        // must stay byte-stable across turns for provider caches to hit.
        let system_prompt = self.build_system_prompt_segmented().await?;

        // Extract adapter from messages (prefer explicit message.adapter, fall back to stored source_adapter)
        // This preserves per-message adapter for Signal named instances (e.g., "signal:work")
        let batch_adapter = messages
            .iter()
            .find_map(|m| m.adapter.as_deref())
            .or(self.source_adapter.as_deref());

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = messages.iter().rev().find_map(extract_message_id);
        }

        // Pin the inbound routing target from the last non-system message in the
        // batch so the RoutedSender (and send_routed) carry the correct platform
        // metadata (e.g. Slack thread_ts) for outbound responses.
        if let Some(last_real) = messages.iter().rev().find(|m| m.source != "system") {
            self.current_inbound = Some(last_real.clone());
        }

        // Time and the coalesce hint live on the user message envelope, not
        // the system prompt: the prompt must stay byte-stable across turns,
        // while the envelope changes every turn at no cache cost.
        let prompt_engine = self.deps.runtime_config.prompts.load();
        let elapsed_str = format!("{:.1}s", elapsed_secs);
        let mut envelope_body = prompt_engine
            .render_coalesce_hint(message_count, &elapsed_str, unique_sender_count)
            .ok()
            .map(|hint| format!("{hint}\n\n"))
            .unwrap_or_default();
        envelope_body.push_str(&combined_text);
        let live_text = with_time_envelope(&temporal_context.current_time_line(), &envelope_body);

        // Run agent turn with any image/audio attachments preserved
        let turn_result = self
            .run_agent_turn(
                &live_text,
                &system_prompt,
                &conversation_id,
                attachment_parts,
                false, // not a retrigger
                batch_adapter,
                Some(&combined_text),
            )
            .await?;

        let _ = self
            .handle_agent_result(
                turn_result.result,
                &turn_result.skip_flag,
                &turn_result.replied_flag,
                false,
            )
            .await;
        if turn_result
            .replied_flag
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.record_decision_event(turn_result.reply_text.as_deref(), None);
        }
        self.maintain_context().await;

        // Increment message counter for memory persistence
        self.message_count += message_count;
        self.check_memory_persistence().await;
        self.claim_home_channel_if_unset().await;

        Ok(())
    }

    /// Handle an incoming message by running the channel's LLM agent loop.
    ///
    /// The LLM decides which tools to call: reply (to respond), branch (to think),
    /// spawn_worker (to delegate), route (to follow up with a worker), cancel, or
    /// memory_save. The tools act on the channel's shared state directly.
    #[tracing::instrument(skip(self, message), fields(channel_id = %self.id, agent_id = %self.deps.agent_id, message_id = %message.id))]
    async fn handle_message(&mut self, message: InboundMessage) -> Result<()> {
        // Apply runtime-config updates immediately without requiring a restart.
        let _turn_guard = TurnActiveGuard::engage(&self.state.turn_active);

        // Track the inbound message that triggered this turn so outbound
        // responses carry the correct routing metadata (e.g. Slack thread_ts).
        // System retrigger messages keep the previous inbound target.
        if message.source != "system" {
            self.current_inbound = Some(message.clone());
        }

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

        // Track source adapter from non-system messages
        // Prefer message.adapter (full adapter string like "signal:work") over message.source
        if self.source_adapter.is_none() && message.source != "system" {
            self.source_adapter = message
                .adapter
                .clone()
                .or_else(|| Some(message.source.clone()));
        }

        let (raw_text, attachments) = match &message.content {
            crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
            crate::MessageContent::Media { text, attachments } => {
                (text.clone().unwrap_or_default(), attachments.clone())
            }
            // Render interactions and commands as their Display form so the
            // LLM sees plain text; a Command renders as "/name args" and is
            // dispatched by the same parse below.
            crate::MessageContent::Interaction {
                action_id, values, ..
            } => {
                let sender = participant_display_name(&message);
                let raw_text =
                    enrich_ask_interaction(&self.deps.sqlite_pool, &sender, action_id, values)
                        .await;
                (raw_text, Vec::new())
            }
            crate::MessageContent::Command { .. } => (message.content.to_string(), Vec::new()),
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

        self.current_message_id =
            self.persist_inbound_user_message(&message, &raw_text, saved_metas.as_deref());
        self.track_participant_from_message(&message).await;

        // Slash-command dispatch. Control commands execute deterministically
        // on the spot and never consume an agent turn; agent commands are
        // rewritten into their instruction below. System messages are never
        // commands, and unrecognized "/words" flow to the model as text.
        let parsed_command = if message.source == "system" {
            crate::commands::ParseResult::NotACommand
        } else {
            crate::commands::REGISTRY.parse_addressed(
                &raw_text,
                message
                    .metadata
                    .get("telegram_bot_username")
                    .and_then(serde_json::Value::as_str),
            )
        };
        match &parsed_command {
            crate::commands::ParseResult::Command(cmd) => {
                if let crate::commands::CommandHandler::Control(action) = cmd.def.handler {
                    // The router parses with the receiving bot's username and
                    // this path does not, so text it declined as addressed to
                    // another bot still resolves here. Gate on the authority
                    // it stamped, or `/sethome@otherbot` would run unchecked.
                    let is_authority = crate::commands::dispatch::sender_is_authority(&message);
                    if !crate::commands::access_allows(cmd.def, is_authority) {
                        self.send_builtin_text(
                            crate::commands::access::denial_text(
                                cmd.def,
                                crate::commands::Surface::from_source(&message.source),
                            ),
                            cmd.def.name,
                        )
                        .await;
                        return Ok(());
                    }
                    self.handle_control_command(cmd.def, action, &cmd.args, is_authority)
                        .await;
                    return Ok(());
                }
            }
            crate::commands::ParseResult::Usage(_, usage) => {
                self.send_builtin_text(usage.clone(), "command-usage").await;
                return Ok(());
            }
            crate::commands::ParseResult::NotACommand => {}
        }

        // Deterministic liveness ping for Telegram mentions.
        // This avoids model/provider flakiness for simple "you there?" style checks.
        if message.source == "telegram" {
            let (_, has_mention, _) = self.compute_listen_mode_invocation(&message, &raw_text);
            if has_mention && looks_like_liveness_ping(&raw_text) {
                self.send_builtin_text("yeah i'm here".to_string(), "telegram-ping")
                    .await;
                return Ok(());
            }
        }

        // Deterministic ping ack for Discord mention-only mentions/replies to avoid
        // flaky model behavior (e.g. skipping or over-formatting simple liveness checks).
        // Skipped in Observe mode — the agent never responds in Observe.
        if !matches!(self.response_mode(), ResponseMode::Observe)
            && should_send_discord_quiet_mode_ping_ack(&message, &raw_text, self.is_suppressed())
        {
            self.send_builtin_text("yeah i'm here".to_string(), "discord-ping")
                .await;
            return Ok(());
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
                self.conversation_id.as_deref(),
            )?);
        }

        let rewritten_text = match &parsed_command {
            crate::commands::ParseResult::Command(cmd) => match cmd.def.handler {
                crate::commands::CommandHandler::Agent(
                    crate::commands::AgentAction::PromptTemplate(template),
                ) => {
                    let prompt_engine = self.deps.runtime_config.prompts.load();
                    match prompt_engine.render_static(template) {
                        Ok(instruction) => instruction,
                        Err(error) => {
                            tracing::error!(
                                channel_id = %self.id,
                                command = cmd.def.name,
                                %template,
                                %error,
                                "failed to render command prompt template; using raw text"
                            );
                            raw_text.clone()
                        }
                    }
                }
                // Control commands returned above.
                crate::commands::CommandHandler::Control(_) => raw_text.clone(),
            },
            _ => raw_text.clone(),
        };

        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let message_timestamp = temporal_context.format_timestamp(message.timestamp);
        let user_text = format_user_message(&rewritten_text, &message, &message_timestamp);
        // The wall-clock line rides on the live user message, not the system
        // prompt — the prompt must stay byte-stable across turns for provider
        // caches to hit. History and suppressed messages keep the plain text:
        // their per-message timestamps already ground them.
        let live_user_text = with_time_envelope(&temporal_context.current_time_line(), &user_text);

        let mut invoked_by_command = false;
        let mut invoked_by_mention = false;
        let mut invoked_by_reply = false;

        // Response mode guardrail:
        // Observe mode: always suppress — agent learns but never responds.
        // MentionOnly mode: suppress unless explicitly invoked.
        if !matches!(self.response_mode(), ResponseMode::Active)
            && message.source != "system"
            && !self.is_dm()
        {
            // Observe mode always suppresses; MentionOnly checks for invocation.
            let should_suppress = if matches!(self.response_mode(), ResponseMode::Observe) {
                true
            } else {
                (invoked_by_command, invoked_by_mention, invoked_by_reply) =
                    self.compute_listen_mode_invocation(&message, &raw_text);
                !invoked_by_command && !invoked_by_mention && !invoked_by_reply
            };

            if should_suppress {
                tracing::debug!(
                    channel_id = %self.id,
                    source = %message.source,
                    response_mode = ?self.response_mode(),
                    "suppressing unsolicited reply"
                );
                // In Observe and MentionOnly modes, inject the message into
                // in-memory history so the agent retains channel context.
                {
                    let mut history = self.state.history.write().await;
                    history.push(rig::message::Message::User {
                        content: OneOrMany::one(UserContent::text(&user_text)),
                    });
                }
                self.maintain_context().await;
                // Both Observe and MentionOnly keep passive memory capture.
                self.message_count += 1;
                self.check_memory_persistence().await;
                return Ok(());
            }
        }

        let system_prompt = self.build_system_prompt_segmented().await?;

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = extract_message_id(&message);
        }

        let is_autonomy_heartbeat = self.state.kind == ChannelKind::Autonomy
            && message
                .metadata
                .contains_key(crate::agent::autonomy::AUTONOMY_GENERATION_KEY);
        let is_retrigger = message.source == "system" && !is_autonomy_heartbeat;
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

        let adapter = message
            .adapter
            .as_deref()
            .or_else(|| self.current_adapter());
        let turn_result = self
            .run_agent_turn(
                &live_user_text,
                &system_prompt,
                &message.conversation_id,
                attachment_content,
                is_retrigger,
                adapter,
                Some(&user_text),
            )
            .await?;

        let delivered_text = self
            .handle_agent_result(
                turn_result.result,
                &turn_result.skip_flag,
                &turn_result.replied_flag,
                is_retrigger,
            )
            .await;

        if is_retrigger && let Some(text) = delivered_text.as_ref() {
            self.state
                .history
                .write()
                .await
                .push(rig::message::Message::Assistant {
                    id: None,
                    content: OneOrMany::one(rig::message::AssistantContent::text(text)),
                });
        }

        if turn_result
            .replied_flag
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let humans = self.deps.humans.load();
            let user_id = decision_user_id(humans.as_ref(), &message, is_retrigger);
            self.record_decision_event(turn_result.reply_text.as_deref(), user_id);
        }

        // Safety-net: in mention-only mode, explicit mention/reply should never be dropped silently.
        if should_send_quiet_mode_fallback(
            &message,
            ObserveModeFallbackState {
                is_suppressed: self.is_suppressed(),
                is_retrigger,
                invoked_by_command,
                invoked_by_mention,
                invoked_by_reply,
                skip_flag: turn_result
                    .skip_flag
                    .load(std::sync::atomic::Ordering::Relaxed),
                replied_flag: turn_result
                    .replied_flag
                    .load(std::sync::atomic::Ordering::Relaxed),
            },
        ) {
            self.send_builtin_text(
                "yeah i'm here — tell me what you need.".to_string(),
                "quiet-mode-fallback",
            )
            .await;
        }

        if is_retrigger {
            let replied = turn_result
                .replied_flag
                .load(std::sync::atomic::Ordering::Relaxed);
            let delivered = replied
                || turn_result
                    .delivered_flag
                    .load(std::sync::atomic::Ordering::Acquire)
                || delivered_text.is_some();
            let is_autonomy = self.state.kind == ChannelKind::Autonomy;
            if delivered && turn_result.retrigger_reply_preserved {
                tracing::debug!(
                    channel_id = %self.id,
                    "skipping retrigger summary injection; relay reply already preserved"
                );
            } else if is_autonomy {
                let summary = message
                    .metadata
                    .get("retrigger_result_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("[background work completed]");
                let record = format!("[background process results]\n{summary}");
                let mut history = self.state.history.write().await;
                let replaced = pop_retrigger_bridge_message(&mut history);
                tracing::debug!(
                    channel_id = %self.id,
                    replaced_bridge = replaced,
                    "preserving autonomy process results in run history"
                );
                history.push(rig::message::Message::Assistant {
                    id: None,
                    content: OneOrMany::one(rig::message::AssistantContent::text(record)),
                });
            } else if !delivered {
                let relay_attempt = message
                    .metadata
                    .get("retrigger_relay_attempt")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if relay_attempt < RETRIGGER_RELAY_RETRY_LIMIT {
                    let mut retry = message.clone();
                    retry.id = uuid::Uuid::new_v4().to_string();
                    retry.timestamp = chrono::Utc::now();
                    retry.metadata.insert(
                        "retrigger_relay_attempt".to_string(),
                        serde_json::json!(relay_attempt + 1),
                    );
                    if let Err(error) = self.self_tx.try_send(retry) {
                        tracing::warn!(
                            channel_id = %self.id,
                            %error,
                            "failed to queue background result relay retry"
                        );
                        self.deferred_retriggers.push_back(message.clone());
                        self.notify_retrigger_delivery_failure().await;
                    } else {
                        tracing::warn!(
                            channel_id = %self.id,
                            attempt = relay_attempt + 1,
                            "background result relay failed; queued bounded retry"
                        );
                    }
                } else {
                    tracing::warn!(
                        channel_id = %self.id,
                        attempts = relay_attempt + 1,
                        "background result relay retries exhausted"
                    );
                    self.deferred_retriggers.push_back(message.clone());
                    self.notify_retrigger_delivery_failure().await;
                }
            }

            // Mark the completed items as relayed in the status block so their
            // full result summaries stop appearing on subsequent turns. This
            // prevents the LLM from re-summarising stale worker/branch results.
            //
            // For autonomy there is no user-facing relay, but the results are
            // now recorded in history (either via the reply path or the record
            // above), so marking them prevents the same results being
            // re-injected on every subsequent turn of the run.
            if (delivered || is_autonomy)
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

        self.maintain_context().await;

        // Increment message counter and spawn memory persistence branch if threshold reached
        if !is_retrigger {
            self.retrigger_count = 0;
            self.message_count += 1;
            self.check_memory_persistence().await;
            self.claim_home_channel_if_unset().await;
            self.queue_deferred_retrigger();
        }

        Ok(())
    }

    /// A fresh instance has no home, which is when proactive behavior most
    /// wants one. The first conversation to complete a turn adopts it, and
    /// says so — the destination is never a default the user discovers by
    /// receiving something unexpected.
    async fn claim_home_channel_if_unset(&mut self) {
        if self.state.kind != ChannelKind::User {
            return;
        }
        let is_portal = self.current_adapter() == Some("portal");
        let Some(target) =
            crate::commands::control::adopt_home_channel(&self.deps, &self.id, is_portal).await
        else {
            return;
        };

        self.send_builtin_text(
            format!(
                "heads up: nothing was set as my home channel, so i've taken this chat \
                 ({target}). anything i bring up on my own lands here. use /sethome \
                 elsewhere to move it."
            ),
            "home-adopted",
        )
        .await;
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

        // Filter out the current channel, cron channels, and link channels.
        // Link channels (platform == "link") are internal audit trails between agents;
        // they have no real messaging adapter. Inter-agent communication goes through
        // `send_agent_message` (task delegation), not `send_message_to_another_channel`.
        // Exposing link channels here causes the LLM to attempt direct routing via
        // `send_message_to_another_channel`, which fails with a platform resolution error
        // because `resolve_broadcast_target` has no handler for the "link" platform.
        let entries: Vec<crate::prompts::engine::ChannelEntry> = channels
            .into_iter()
            .filter(|channel| {
                channel.id.as_str() != self.id.as_ref()
                    && channel.platform != "cron"
                    && channel.platform != "webhook"
                    && channel.platform != "link"
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

        // Build a lookup map for humans so we can surface display names,
        // roles, and descriptions in the org context prompt.
        let all_humans = self.deps.humans.load();
        let humans_by_id: std::collections::HashMap<&str, &crate::config::HumanDef> =
            all_humans.iter().map(|h| (h.id.as_str(), h)).collect();

        if links.is_empty() {
            return None;
        }

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
                description_total_chars: None,
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

        prompt_engine
            .render_org_context(
                org_context,
                **self.deps.runtime_config.human_profile_cap.load(),
            )
            .ok()
    }

    async fn render_memory_layers(&self) -> (String, String, String, Option<String>) {
        if matches!(self.resolved_settings.memory, MemoryMode::Off) {
            return (String::new(), String::new(), String::new(), None);
        }

        let rc = &self.deps.runtime_config;
        // The knowledge-context slot is a deterministic store render — no
        // LLM synthesis, byte-stable between memory writes.
        let knowledge_synthesis_text = {
            let cortex_config = **rc.cortex.load();
            match crate::memory::render::render_memory_store(
                self.deps.memory_search.store(),
                &self.deps.task_store,
                &self.deps.agent_id,
                cortex_config.memory_render_max_words,
            )
            .await
            {
                Ok(text) if !text.is_empty() => Some(text),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(channel_id = %self.id, %error, "memory store render failed");
                    None
                }
            }
        };
        let wm_config = **rc.working_memory.load();
        let timezone = self.deps.working_memory.timezone();

        let working_memory = match crate::memory::working::render_working_memory(
            &self.deps.working_memory,
            self.id.as_ref(),
            &wm_config,
            timezone,
        )
        .await
        {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(channel_id = %self.id, %error, "working memory render failed");
                String::new()
            }
        };

        let channel_activity_map = match crate::memory::working::render_channel_activity_map(
            &self.deps.sqlite_pool,
            &self.deps.working_memory,
            self.id.as_ref(),
            &wm_config,
            timezone,
        )
        .await
        {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(channel_id = %self.id, %error, "channel activity map render failed");
                String::new()
            }
        };

        let participant_config = **rc.participant_context.load();
        let tracked_participants = {
            let participants = self.state.active_participants.read().await;
            renderable_participants(&participants, &participant_config)
        };
        // The session anchor cache is shared and mutated in place, so
        // concurrent prompt builds accumulate resolved entries instead of
        // clobbering each other, and entries survive a failed render.
        let participant_context = match crate::memory::working::render_participant_context(
            &self.deps.working_memory,
            &tracked_participants,
            self.id.as_ref(),
            &participant_config,
            self.deps.memory_search.store(),
            &self.state.human_anchor_cache,
        )
        .await
        {
            Ok(rendered) => rendered,
            Err(error) => {
                tracing::warn!(%error, channel_id = %self.id, "participant context render failed");
                String::new()
            }
        };

        (
            working_memory,
            channel_activity_map,
            participant_context,
            knowledge_synthesis_text,
        )
    }

    /// Render the compact active-goals list for the system prompt.
    ///
    /// Returns `None` when there are no active goals so injection is skipped
    /// entirely — goals should cost nothing when unused.
    async fn render_active_goals(&self) -> Option<String> {
        match crate::goals::render_active_goals(&self.deps.goal_store).await {
            Ok(text) if !text.is_empty() => Some(text),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(channel_id = %self.id, %error, "active goals render failed");
                None
            }
        }
    }

    /// Build pre-rendered project context for prompt injection.
    ///
    /// Delegates to the standalone `build_project_context` function shared
    /// with worker spawning paths.
    async fn build_project_context(
        &self,
        prompt_engine: &crate::prompts::engine::PromptEngine,
    ) -> Option<String> {
        crate::agent::channel_dispatch::build_project_context(&self.deps, prompt_engine).await
    }

    /// Build a snapshot of the system configuration for status block injection.
    async fn build_system_info(&self) -> SystemInfo {
        let runtime_config = &self.deps.runtime_config;
        let mut info = SystemInfo::from_runtime_config(runtime_config, &self.deps.sandbox);

        // Add async-only fields that the base constructor can't populate
        let cron_job_count = {
            let scheduler_guard = runtime_config.cron_scheduler.load();
            match scheduler_guard.as_ref() {
                Some(scheduler) => Some(scheduler.job_count().await),
                None => None,
            }
        };
        info.cron_job_count = cron_job_count;

        info
    }

    /// Build the channel's full system prompt: template, identity, memory
    /// layers, status block, skills, tool notes.
    ///
    /// Public for fixture harnesses and context inspection — this is the
    /// exact byte stream the channel sends, so behavioral fixtures can gate
    /// on it without duplicating the composition.
    pub async fn build_system_prompt(&self) -> crate::error::Result<String> {
        Ok(self.build_system_prompt_segmented().await?.text)
    }

    /// Build the channel's system prompt along with the map of the blocks it
    /// is assembled from.
    pub async fn build_system_prompt_segmented(
        &self,
    ) -> crate::error::Result<crate::prompts::SegmentedPrompt> {
        let rc = &self.deps.runtime_config;
        let prompt_engine = rc.prompts.load();

        let identity_context = rc.identity.load().render();
        let skills = rc.skills.load();
        let skills_prompt = skills.render_channel_prompt(&prompt_engine)?;

        let browser_enabled = rc.browser_config.load().enabled;
        let web_search_enabled = rc.brave_search_key.load().is_some();
        let opencode_enabled = rc.opencode.load().enabled;
        let mcp_tool_names = self.deps.mcp_manager.get_tool_names().await;
        let worker_context = self.state.worker_context_settings.read().await.clone();
        let autonomy_level = self
            .deps
            .runtime_config
            .autonomy
            .load()
            .level
            .min(**self.deps.autonomy_ceiling.load());
        let worker_capabilities = if self.state.kind == ChannelKind::Autonomy
            && autonomy_level != crate::config::AutonomyLevel::Act
        {
            String::new()
        } else {
            prompt_engine.render_worker_capabilities(
                browser_enabled,
                web_search_enabled,
                opencode_enabled,
                &mcp_tool_names,
                &worker_context,
                self.state.kind != ChannelKind::Autonomy,
            )?
        };

        // Time no longer renders here — it rides on the current user message
        // envelope instead, so this prompt stays byte-stable across turns
        // (see `with_time_envelope`).
        let system_info = self.build_system_info().await;
        let registry_workers = self
            .state
            .deps
            .process_control_registry
            .list_worker_snapshots()
            .await
            .into_iter()
            .filter(|worker| worker.provenance.origin_channel_id.as_ref() == Some(&self.id))
            .collect();
        let status_text = {
            let mut status = self.state.status_block.write().await;
            status.replace_workers_from_registry(registry_workers);
            status.render_with_context(None, Some(&system_info))
        };

        let available_channels = self.build_available_channels().await;

        let org_context = self.build_org_context(&prompt_engine);

        let adapter_prompt = match self.state.kind {
            ChannelKind::Cron => prompt_engine.render_channel_adapter_prompt("cron"),
            ChannelKind::User | ChannelKind::Autonomy => self
                .current_adapter()
                .and_then(|adapter| prompt_engine.render_channel_adapter_prompt(adapter)),
        };

        let project_context = self.build_project_context(&prompt_engine).await;

        let (working_memory, channel_activity_map, participant_context, knowledge_synthesis_text) =
            self.render_memory_layers().await;

        let active_goals = self.render_active_goals().await;

        let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };
        let non_empty_option = |value: Option<String>| value.filter(|text| !text.is_empty());
        let routing = rc.routing.load();
        // Enforcement is model-specific, so it has to be rendered for the model
        // the turn will actually use — a conversation override, when set, not
        // the routing default.
        let model_name = self
            .resolved_settings
            .resolve_model("channel")
            .unwrap_or_else(|| routing.resolve(ProcessType::Channel, None))
            .to_string();
        let tool_use_enforcement = rc.tool_use_enforcement.load();
        let direct_mode = self.resolved_settings.delegation == DelegationMode::Direct;
        let execution_mode = if direct_mode {
            prompt_engine
                .render_static("fragments/execution_direct")
                .unwrap_or_default()
        } else {
            prompt_engine
                .render_static("fragments/execution_standard")
                .unwrap_or_default()
        };
        let authority = prompt_engine
            .render_static("fragments/authority")
            .unwrap_or_default();

        let mut segmented =
            prompt_engine.render_channel_prompt(crate::prompts::ChannelPromptInputs {
                identity_context: empty_to_none(identity_context),
                knowledge_synthesis: non_empty_option(knowledge_synthesis_text),
                skills_prompt: empty_to_none(skills_prompt),
                worker_capabilities,
                conversation_context: self.conversation_context.clone(),
                status_text: empty_to_none(status_text),
                available_channels,
                agent_links: self.send_agent_message_tool.is_some(),
                org_context,
                adapter_prompt,
                project_context,
                backfill_transcript: self.backfill_transcript.clone(),
                session_chronicle: self.render_session_chronicle().await,
                working_memory: empty_to_none(working_memory),
                channel_activity_map: empty_to_none(channel_activity_map),
                participant_context: empty_to_none(participant_context),
                active_goals,
                execution_mode,
                authority,
                autonomy_channel: self.state.kind == ChannelKind::Autonomy,
            })?;

        segmented.adopt_appended(
            prompt_engine.maybe_append_tool_use_enforcement(
                segmented.text.clone(),
                tool_use_enforcement.as_ref(),
                &model_name,
            )?,
            "tool_use_enforcement",
        );

        self.chronicler.fence().record_prompt_tokens(
            crate::agent::compactor::estimate_text_tokens(&segmented.text),
        );
        Ok(segmented)
    }

    /// Register per-turn tools, run the LLM agentic loop, and clean up.
    ///
    /// Returns the prompt result and per-turn flags for the caller to dispatch.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip(self, user_text, system_prompt, attachment_content), fields(channel_id = %self.id, agent_id = %self.deps.agent_id))]
    async fn run_agent_turn(
        &self,
        user_text: &str,
        system_prompt: &crate::prompts::SegmentedPrompt,
        conversation_id: &str,
        attachment_content: Vec<UserContent>,
        is_retrigger: bool,
        adapter: Option<&str>,
        history_user_text: Option<&str>,
    ) -> Result<AgentTurnResult> {
        let skip_flag = crate::tools::new_skip_flag();
        let replied_flag = crate::tools::new_replied_flag();
        let delivered_flag = crate::tools::new_delivered_flag();
        // Autonomy runs never talk to users — no reply tool. Output goes to
        // task state, working memory, and autonomy_complete.
        let allow_direct_reply =
            self.state.kind == ChannelKind::User && !self.suppress_plaintext_fallback();
        let allow_ask = allow_direct_reply && !is_retrigger;

        // Set the originating channel on the delegation tool so task completion
        // notifications route back to this conversation.
        let send_agent_message_tool = self
            .send_agent_message_tool
            .clone()
            .map(|tool| tool.with_originating_channel(conversation_id.to_string()));

        let current_inbound = self
            .current_inbound
            .clone()
            .unwrap_or_else(InboundMessage::empty);
        let routed_sender = RoutedSender::new(self.response_tx.clone(), current_inbound.clone());

        // Extract Slack thread_ts from the current inbound message so cron
        // delivery targets include the originating thread.
        let slack_thread_ts = current_inbound
            .metadata
            .get("slack_thread_ts")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // reply() always sends live — cron channels use set_outcome() for delivery.
        let reply_target = crate::tools::ReplyTarget::Live(Box::new(routed_sender.clone()));

        // Tools that change instance-wide state are registered per turn
        // against the sender driving it, so a non-authority turn never has
        // them on the table to be talked into calling.
        let sender_is_authority = crate::commands::dispatch::sender_is_authority(&current_inbound);

        if self.state.kind == ChannelKind::Autonomy {
            if let Err(error) =
                crate::tools::add_autonomy_tools(&self.tool_server, self.state.clone()).await
            {
                tracing::error!(%error, "failed to add autonomy tools");
                return Err(AgentError::Other(error.into()).into());
            }
        } else {
            match self.resolved_settings.delegation {
                DelegationMode::Standard => {
                    // Current behavior - standard channel tools only
                    if let Err(error) = crate::tools::add_channel_tools(
                        &self.tool_server,
                        self.state.clone(),
                        routed_sender,
                        reply_target,
                        conversation_id,
                        skip_flag.clone(),
                        replied_flag.clone(),
                        delivered_flag.clone(),
                        self.deps.cron_tool.clone(),
                        send_agent_message_tool,
                        allow_direct_reply,
                        allow_ask,
                        adapter.map(|s| s.to_string()),
                        slack_thread_ts.as_deref(),
                        self.state.cron_outcome.clone(),
                        sender_is_authority,
                    )
                    .await
                    {
                        tracing::error!(%error, "failed to add channel tools");
                        return Err(AgentError::Other(error.into()).into());
                    }
                }
                DelegationMode::Direct => {
                    // Full tool access (cortex chat style)
                    if let Err(error) = crate::tools::add_direct_mode_tools(
                        &self.tool_server,
                        self.state.clone(),
                        routed_sender,
                        reply_target,
                        conversation_id,
                        skip_flag.clone(),
                        replied_flag.clone(),
                        delivered_flag.clone(),
                        self.deps.cron_tool.clone(),
                        send_agent_message_tool,
                        allow_direct_reply,
                        allow_ask,
                        adapter.map(|s| s.to_string()),
                        slack_thread_ts.as_deref(),
                        self.state.cron_outcome.clone(),
                        sender_is_authority,
                    )
                    .await
                    {
                        tracing::error!(%error, "failed to add direct mode tools");
                        return Err(AgentError::Other(error.into()).into());
                    }
                }
            }
        }

        let rc = &self.deps.runtime_config;
        let routing = rc.routing.load();
        let max_turns = if self.state.kind == ChannelKind::Autonomy {
            // Autonomy runs carry their own turn budget; on exhaustion the
            // channel behaves like a soft timeout and wraps up.
            (rc.autonomy.load().max_turns.max(1)) as usize
        } else if is_retrigger {
            RETRIGGER_MAX_TURNS
        } else {
            **rc.max_turns.load()
        };

        // Check for model override from conversation settings.
        // Priority: per-process override > blanket override > routing config.
        let model_name =
            if let Some(override_model) = self.resolved_settings.resolve_model("channel") {
                override_model
            } else {
                routing.resolve(ProcessType::Channel, None)
            };

        let usage_accumulator = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::llm::usage::UsageAccumulator::new(),
        ));
        let model = SpacebotModel::make(&self.deps.llm_manager, model_name)
            .with_context(&*self.deps.agent_id, "channel")
            .with_routing((**routing).clone())
            .with_accumulator(usage_accumulator.clone())
            .with_debug(
                self.deps.prompt_records(),
                crate::llm::record::DebugContext {
                    process: Some(crate::llm::record::ProcessRef {
                        kind: "channel".to_string(),
                        id: Some(self.id.to_string()),
                        process_type: Some(self.state.kind.to_string()),
                        channel_id: Some(self.id.to_string()),
                    }),
                    trigger: Some(crate::llm::record::Trigger {
                        kind: if is_retrigger {
                            "retrigger".to_string()
                        } else {
                            "user_message".to_string()
                        },
                        message_id: self.current_message_id.clone(),
                        input: Some(user_text.to_string()),
                        parent: None,
                    }),
                    blocks: system_prompt.blocks.clone(),
                },
            );

        let agent = AgentBuilder::new(model)
            .preamble(&system_prompt.text)
            .default_max_turns(max_turns)
            .tool_server_handle(self.tool_server.clone())
            .build();

        self.send_routed(OutboundResponse::Status(crate::StatusUpdate::Thinking))
            .await
            .ok();

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

        // ── Pre-send budget check ──
        //
        // Context maintenance runs *after* a turn, so a large incoming message
        // can push this request over the window before compaction ever sees it.
        // Checking here catches that. The estimate excludes serialized tool
        // schemas — Rig assembles those inside the `ToolServer` at call time and
        // does not expose them — so it is a lower bound, and this warns rather
        // than blocking a turn the model may still be able to serve.
        {
            let context_window = **self.deps.runtime_config.context_window.load();
            let estimated = crate::agent::chronicle::estimate_request_tokens(
                crate::agent::compactor::estimate_text_tokens(&system_prompt.text),
                &history,
                user_text,
                context_window,
            );
            if estimated > context_window {
                tracing::warn!(
                    channel_id = %self.id,
                    estimated,
                    context_window,
                    "request exceeds the context window before tool schemas are counted; \
                     compaction will run after this turn"
                );
            }
        }

        let mut result = self
            .hook
            .prompt_once_streaming(&agent, &mut history, user_text, max_turns)
            .await;

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
                .prompt_once_streaming(&agent, &mut history, &correction, max_turns)
                .await;
        }

        // Count tool iterations from the pre-sanitization turn history.
        // apply_history_after_turn strips tool-call messages on
        // PromptCancelled (the normal reply-tool ending), so counting the
        // applied guard would report zero for exactly the tool-heavy turns
        // reflection exists to catch. PromptCancelled and MaxTurnsError carry
        // the authoritative messages in the error; Ok turns carry them in
        // `history`.
        let turn_tool_calls = {
            let source: &[rig::message::Message] = match &result {
                Err(rig::completion::PromptError::PromptCancelled { chat_history, .. })
                | Err(rig::completion::PromptError::MaxTurnsError { chat_history, .. }) => {
                    chat_history
                }
                _ => &history,
            };
            let appended_from = history_len_before.min(source.len());
            crate::agent::channel_history::count_tool_call_messages(&source[appended_from..])
        };

        let applied_history = {
            let mut guard = self.state.history.write().await;
            apply_history_after_turn(
                &result,
                &mut guard,
                history,
                history_len_before,
                &self.id,
                is_retrigger,
                history_user_text.map(|plain| (user_text, plain)),
            )
        };

        {
            let reflection = self.deps.runtime_config.skills_config.load().reflection;
            if reflection.enabled && turn_tool_calls >= reflection.min_tool_iterations {
                self.mark_reflection_signal("turn_tool_calls");
            }
        }

        let remove_result = if self.state.kind == ChannelKind::Autonomy {
            crate::tools::remove_direct_mode_tools(&self.tool_server, allow_direct_reply).await
        } else {
            match self.resolved_settings.delegation {
                DelegationMode::Direct => {
                    crate::tools::remove_direct_mode_tools(&self.tool_server, allow_direct_reply)
                        .await
                }
                DelegationMode::Standard => {
                    crate::tools::remove_channel_tools(&self.tool_server, allow_direct_reply).await
                }
            }
        };
        if let Err(error) = remove_result {
            tracing::warn!(%error, "failed to remove channel tools");
        }

        // Flush accumulated token usage to the database.
        let acc = usage_accumulator.lock().await;
        if let Err(error) = acc
            .flush(
                &self.deps.sqlite_pool,
                &self.deps.agent_id,
                "channel",
                Some(conversation_id),
            )
            .await
        {
            tracing::warn!(%error, "failed to flush token usage");
        }

        Ok(AgentTurnResult {
            result,
            skip_flag,
            replied_flag,
            delivered_flag,
            retrigger_reply_preserved: applied_history.retrigger_reply_preserved,
            reply_text: applied_history.reply_text,
        })
    }

    /// Send outbound text and record send metrics.
    async fn send_outbound_text(&self, text: String, error_context: &str) -> bool {
        match self
            .send_routed_confirmed(OutboundResponse::Text(text))
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
                true
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
                false
            }
        }
    }

    async fn notify_retrigger_delivery_failure(&self) {
        let text = "Background work finished, but I couldn't deliver its result. The result is still pending; ask me to retry.";
        if self
            .send_outbound_text(text.to_string(), "failed to send background result notice")
            .await
        {
            self.state
                .conversation_logger
                .log_bot_message(&self.state.channel_id, text);
        }
    }

    fn queue_deferred_retrigger(&mut self) {
        let Some(mut message) = self.deferred_retriggers.pop_front() else {
            return;
        };
        message.id = uuid::Uuid::new_v4().to_string();
        message.timestamp = chrono::Utc::now();
        message
            .metadata
            .insert("retrigger_relay_attempt".to_string(), serde_json::json!(0));
        match self.self_tx.try_send(message) {
            Ok(()) => tracing::info!(
                channel_id = %self.id,
                "retrying deferred background result after user activity"
            ),
            Err(error) => {
                let message = error.into_inner();
                tracing::warn!(
                    channel_id = %self.id,
                    "failed to queue deferred background result retry"
                );
                self.deferred_retriggers.push_front(message);
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
    ) -> Option<String> {
        let mut delivered_text = None;
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
                                if self
                                    .send_outbound_text(
                                        final_text.clone(),
                                        "failed to send retrigger fallback reply",
                                    )
                                    .await
                                {
                                    self.state
                                        .conversation_logger
                                        .log_bot_message(&self.state.channel_id, &final_text);
                                    delivered_text = Some(final_text);
                                }
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
                            if !final_text.is_empty()
                                && self
                                    .send_outbound_text(
                                        final_text.clone(),
                                        "failed to send retrigger fallback reply",
                                    )
                                    .await
                            {
                                self.state
                                    .conversation_logger
                                    .log_bot_message(&self.state.channel_id, &final_text);
                                delivered_text = Some(final_text);
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
                            let tool_calls_json = self.drain_tool_calls_json().await;
                            self.state
                                .conversation_logger
                                .log_bot_message_with_metadata(
                                    &self.state.channel_id,
                                    &final_text,
                                    Some(self.agent_display_name()),
                                    tool_calls_json,
                                );
                            if self
                                .send_outbound_text(
                                    final_text.clone(),
                                    "failed to send fallback reply",
                                )
                                .await
                            {
                                delivered_text = Some(final_text);
                            }
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
                } else if reason == "skip" {
                    tracing::debug!(channel_id = %self.id, "channel turn skipped via tool");
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
                if !is_retrigger {
                    let error_msg = format!("I encountered an error: {}", error);
                    self.send_routed(OutboundResponse::Text(error_msg))
                        .await
                        .ok();
                }
                tracing::error!(channel_id = %self.id, %error, "channel LLM call failed");
            }
        }

        // Ensure typing indicator is always cleaned up, even on error paths
        self.send_routed(OutboundResponse::Status(crate::StatusUpdate::StopTyping))
            .await
            .ok();
        delivered_text
    }

    /// Handle a process event (branch results, worker completions, status updates).
    async fn handle_event(&mut self, event: ProcessEvent) -> Result<()> {
        // Keep mode aligned with live settings updates while this worker runs.

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
                reply_to_message_id: Some(message_id),
                ..
            } => {
                self.branch_reply_targets
                    .insert(*branch_id, message_id.clone());
            }
            ProcessEvent::BranchResult {
                branch_id,
                conclusion,
                status,
                transcript,
                tool_calls,
                ..
            } => {
                let committed = run_logger
                    .log_branch_terminal(
                        *branch_id,
                        conclusion,
                        status,
                        transcript.as_deref(),
                        *tool_calls,
                    )
                    .await?;
                if !committed {
                    tracing::debug!(branch_id = %branch_id, "duplicate branch terminal event ignored");
                    return Ok(());
                }
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
                if let Some(run) = self.state.autonomy_run() {
                    run.settle_child(crate::agent::autonomy::AutonomyChild::Branch(*branch_id));
                }

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
                    let branch_success = parse_branch_cancellation_reason(conclusion).is_none();
                    self.pending_results.push(PendingResult {
                        process_type: "branch",
                        process_id: branch_id.to_string(),
                        result: conclusion.clone(),
                        success: branch_success,
                    });
                    should_retrigger = true;

                    if let Some(message_id) = reply_target_message_id {
                        retrigger_metadata.insert(
                            crate::metadata_keys::REPLY_TO_MESSAGE_ID.to_string(),
                            serde_json::Value::from(message_id),
                        );
                    }

                    let (event_type, event_summary) =
                        branch_working_memory_event_summary(conclusion);
                    self.deps
                        .working_memory
                        .emit(event_type, event_summary)
                        .channel(self.id.to_string())
                        .importance(0.7)
                        .record();

                    tracing::info!(branch_id = %branch_id, "branch result queued for retrigger");
                }
                self.branch_reply_targets.remove(branch_id);
            }
            ProcessEvent::WorkerStarted { .. } => {}
            ProcessEvent::WorkerStatus { .. } | ProcessEvent::WorkerIdle { .. } => {}
            ProcessEvent::WorkerComplete {
                worker_id,
                active_operation,
                notify,
                outcome_kind,
                outcome_version,
                transcript_version,
                terminal_owner,
                ..
            } => {
                if worker_outcome_already_consumed(
                    &self.consumed_worker_outcomes,
                    *worker_id,
                    *outcome_version,
                ) {
                    return Ok(());
                }

                let Some(terminal) = run_logger.read_worker_terminal(*worker_id).await? else {
                    tracing::warn!(%worker_id, outcome_version, "worker completion event has no durable terminal outcome");
                    return Ok(());
                };
                if terminal.outcome_version != *outcome_version
                    || terminal.transcript_version != *transcript_version
                    || terminal.outcome_kind != *outcome_kind
                    || terminal.terminal_owner != *terminal_owner
                {
                    tracing::warn!(
                        %worker_id,
                        event_outcome_version = outcome_version,
                        durable_outcome_version = terminal.outcome_version,
                        "worker completion event did not match durable terminal outcome"
                    );
                    return Ok(());
                }
                self.consumed_worker_outcomes
                    .insert(*worker_id, terminal.outcome_version);
                if let Some(run) = self.state.autonomy_run()
                    && let Some(operation) = active_operation
                {
                    run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                        worker_id: *worker_id,
                        operation_id: operation.operation_id,
                    });
                }
                let result = terminal.result;
                let success = terminal.outcome_kind.is_success();

                let worker_event = if success {
                    crate::wakes::SystemEvent::WorkerCompleted
                } else {
                    crate::wakes::SystemEvent::WorkerFailed
                };
                // Wake emission writes to SQLite; run it off the event loop so
                // a slow disk cannot stall channel event handling.
                let emit_deps = self.deps.clone();
                let dedupe_key = format!("worker:{worker_id}");
                let payload = serde_json::json!({
                    "worker_id": worker_id.to_string(),
                    "success": success,
                    "summary": crate::summarize_first_non_empty_line(
                        &result,
                        crate::EVENT_SUMMARY_MAX_CHARS,
                    ),
                });
                tokio::spawn(async move {
                    crate::wakes::emit_system_event(
                        &emit_deps,
                        worker_event,
                        &dedupe_key,
                        &payload,
                    )
                    .await;
                });

                // Every completion is recorded for the next reflection pass —
                // failed transcripts carry the trials — but only success fires
                // the signal: a worker finishing real work successfully means
                // the session likely produced a reusable lesson.
                let reflection = self.deps.runtime_config.skills_config.load().reflection;
                if reflection.enabled {
                    self.mark_reflection_worker(*worker_id, success);
                    if success {
                        // A worker can finish after the last user turn;
                        // check now so reflection doesn't sit pending
                        // until the next inbound message.
                        self.check_memory_persistence().await;
                    }
                }

                // Record worker completion in working memory.
                let worker_summary = if result.len() > 200 {
                    format!("{}...", &result[..200])
                } else {
                    result.clone()
                };
                let default_event_type = if success {
                    crate::memory::WorkingMemoryEventType::WorkerCompleted
                } else {
                    crate::memory::WorkingMemoryEventType::Error
                };
                let (event_type, event_summary) =
                    classify_conversational_event_summary(&worker_summary, default_event_type);
                self.deps
                    .working_memory
                    .emit(
                        event_type,
                        format_conversational_event_summary(event_type, "Worker", &event_summary),
                    )
                    .channel(self.id.to_string())
                    .importance(if success { 0.6 } else { 0.8 })
                    .record();

                if *notify {
                    // Accumulate result for the next retrigger instead of
                    // injecting into history as a fake user message.
                    self.pending_results.push(PendingResult {
                        process_type: "worker",
                        process_id: worker_id.to_string(),
                        result: result.clone(),
                        success,
                    });
                    should_retrigger = true;
                }

                tracing::info!(worker_id = %worker_id, "worker completed, result queued for retrigger");
            }
            ProcessEvent::OpenCodeSessionCreated { .. } => {}
            ProcessEvent::WorkerOperationResult {
                worker_id,
                operation_id,
                result,
                ..
            } => {
                let child = crate::agent::autonomy::AutonomyChild::WorkerOperation {
                    worker_id: *worker_id,
                    operation_id: *operation_id,
                };
                if self.state.kind == ChannelKind::Autonomy
                    && let Some(run) = self.state.autonomy_run()
                    && !run.owns_child(child)
                {
                    tracing::debug!(%worker_id, "duplicate or stale autonomy worker result ignored");
                    return Ok(());
                }
                // Interactive worker completed a task (initial or follow-up)
                // but stays alive for more input. Deliver the result to the
                // channel without removing the worker from the active set.
                self.pending_results.push(PendingResult {
                    process_type: "worker",
                    process_id: worker_id.to_string(),
                    result: result.clone(),
                    success: true,
                });
                if let Some(run) = self.state.autonomy_run() {
                    run.settle_child(child);
                }
                should_retrigger = true;
                tracing::info!(
                    worker_id = %worker_id,
                    "interactive worker result queued for retrigger"
                );
            }
            ProcessEvent::SettingsUpdated { channel_id, .. } if *channel_id == self.id => {
                self.reload_settings().await;
            }
            _ => {}
        }

        // Debounce retriggers: instead of firing immediately, set a deadline.
        // Multiple branch/worker completions within the debounce window are
        // coalesced into a single retrigger to prevent message spam.
        if should_retrigger {
            if self.state.kind == ChannelKind::Autonomy && self.state.autonomy_run().is_none() {
                self.deps.autonomy_control.request_check();
                return Ok(());
            }
            let cap_applies = self.state.kind.caps_retriggers();
            if cap_applies && self.retrigger_count >= MAX_RETRIGGERS_PER_TURN {
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

        // Autonomy channels have no user-facing reply surface, so the
        // retrigger must frame results as run context instead of demanding
        // they be relayed to a user (which the model cannot do and would
        // resolve by returning an empty message).
        let prompts = self.deps.runtime_config.prompts.load();
        let retrigger_message = if self.state.kind == ChannelKind::Autonomy {
            prompts.render_system_retrigger_autonomy(&result_items)
        } else {
            prompts.render_system_retrigger(&result_items)
        };

        let retrigger_message = match retrigger_message {
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
        let system_info = self.build_system_info().await;
        let status = self.state.status_block.read().await;
        status.render_full(&current_time_line, &system_info)
    }

    /// Note that this conversation just did substantial work. The next
    /// persistence pass will also reflect on skills, cooldown permitting.
    ///
    /// System-initiated conversations (cron, autonomy) never reflect: they
    /// repeat the same procedure on a schedule and would grind out noise skills.
    fn mark_reflection_signal(&self, source: &'static str) {
        if self.state.kind.suppresses_reflection() {
            return;
        }
        let mut signal = self
            .reflection_signal
            .lock()
            .expect("reflection signal lock");
        let was_set = signal.is_set();
        signal.turn_work = true;
        if !was_set {
            tracing::debug!(channel_id = %self.id, source, "skill reflection signal set");
        }
    }

    /// Record a completed worker for the next reflection pass, which pulls
    /// its transcript via `worker_inspect`. Failed workers are recorded too
    /// — their transcripts carry the trials — but don't fire the signal by
    /// themselves.
    fn mark_reflection_worker(&self, worker_id: WorkerId, success: bool) {
        if self.id.starts_with("cron") {
            return;
        }
        let mut signal = self
            .reflection_signal
            .lock()
            .expect("reflection signal lock");
        let was_set = signal.is_set();
        signal.record_worker(worker_id, success);
        if !was_set && signal.is_set() {
            tracing::debug!(
                channel_id = %self.id,
                %worker_id,
                "skill reflection signal set by worker completion"
            );
        }
    }

    /// Whether the next persistence pass should reflect on skills.
    fn reflection_due(&self) -> bool {
        if !self
            .reflection_signal
            .lock()
            .expect("reflection signal lock")
            .is_set()
        {
            return false;
        }
        let config = self.deps.runtime_config.skills_config.load().reflection;
        if !config.enabled {
            return false;
        }
        match self.last_reflection_at {
            Some(at) => at.elapsed().as_secs() >= config.cooldown_secs,
            None => true,
        }
    }

    /// Check if a memory persistence branch should be spawned.
    ///
    /// Three memory triggers (any one fires): message count, time since last
    /// persistence, and working-memory event density. A pending skill
    /// reflection signal is a fourth trigger; the branch it spawns also
    /// reflects on skills. Reflection rides the persistence pass, so every
    /// trigger — reflection included — obeys the conversation's memory
    /// persistence controls.
    async fn check_memory_persistence(&mut self) {
        if self.state.kind == ChannelKind::Autonomy {
            return;
        }
        let config = **self.deps.runtime_config.memory_persistence.load();
        let persistence_enabled = config.enabled
            && config.message_interval != 0
            && self.resolved_settings.memory.persistence_enabled();
        if !persistence_enabled {
            return;
        }
        let reflection_due = self.reflection_due();

        let wm_config = **self.deps.runtime_config.working_memory.load();
        let elapsed = self.last_persistence_at.elapsed();

        // Trigger 1: Message count threshold.
        let message_trigger =
            persistence_enabled && self.message_count >= wm_config.persistence_message_threshold;

        // Trigger 2: Time-based — only if conversation is active (message_count > 0).
        let time_trigger = persistence_enabled
            && self.message_count > 0
            && elapsed.as_secs() >= wm_config.persistence_time_threshold_secs;

        // Trigger 3: Event density — working memory events from this channel.
        let density_trigger = if persistence_enabled && !message_trigger && !time_trigger {
            // Only check DB if the cheap triggers didn't fire.
            let since = chrono::Utc::now() - chrono::Duration::seconds(elapsed.as_secs() as i64);
            match self
                .deps
                .working_memory
                .count_events_since(self.id.as_ref(), since)
                .await
            {
                Ok(count) => count as usize >= wm_config.persistence_event_density_threshold,
                Err(error) => {
                    tracing::debug!(%error, "event density check failed, skipping");
                    false
                }
            }
        } else {
            false
        };

        if !message_trigger && !time_trigger && !density_trigger && !reflection_due {
            return;
        }

        let trigger = if message_trigger {
            "message_count"
        } else if time_trigger {
            "time"
        } else if density_trigger {
            "event_density"
        } else {
            "reflection"
        };

        // Reset counters before spawning so subsequent messages don't pile up.
        self.message_count = 0;
        self.last_persistence_at = std::time::Instant::now();

        // Snapshot the completed workers for the reflection pass; the
        // signal itself is only cleared once the branch actually spawns.
        let reflection_workers: Vec<(WorkerId, bool)> = if reflection_due {
            self.reflection_signal
                .lock()
                .expect("reflection signal lock")
                .workers
                .clone()
        } else {
            Vec::new()
        };

        match spawn_memory_persistence_branch(
            &self.state,
            &self.deps,
            reflection_due,
            &reflection_workers,
        )
        .await
        {
            Ok(branch_id) => {
                // Consume the reflection request only once the branch exists;
                // a failed spawn leaves the signal set so the next check
                // retries instead of losing the reflection for a cooldown.
                if reflection_due {
                    self.last_reflection_at = Some(std::time::Instant::now());
                    *self
                        .reflection_signal
                        .lock()
                        .expect("reflection signal lock") = ReflectionSignal::default();
                }
                self.memory_persistence_branches.insert(branch_id);
                tracing::info!(
                    channel_id = %self.id,
                    branch_id = %branch_id,
                    trigger,
                    skill_reflection = reflection_due,
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

fn worker_outcome_already_consumed(
    consumed: &HashMap<WorkerId, i64>,
    worker_id: WorkerId,
    outcome_version: i64,
) -> bool {
    consumed
        .get(&worker_id)
        .is_some_and(|version| *version >= outcome_version)
}

fn compute_listen_mode_invocation(message: &InboundMessage, raw_text: &str) -> (bool, bool, bool) {
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
                            || text_lower[..start]
                                .chars()
                                .next_back()
                                .is_none_or(|character| {
                                    !(character.is_ascii_alphanumeric() || character == '_')
                                });
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

fn looks_like_liveness_ping(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    text.contains("you here")
        || text.contains("ping")
        || text.ends_with(" yo")
        || text == "yo"
        || text.contains("alive")
        || text.contains("there?")
}

fn should_send_discord_quiet_mode_ping_ack(
    message: &InboundMessage,
    raw_text: &str,
    is_suppressed: bool,
) -> bool {
    if message.source != "discord" || !is_suppressed {
        return false;
    }

    let (_, invoked_by_mention, invoked_by_reply) =
        compute_listen_mode_invocation(message, raw_text);
    (invoked_by_mention || invoked_by_reply) && looks_like_liveness_ping(raw_text)
}

#[derive(Debug, Clone, Copy)]
struct ObserveModeFallbackState {
    is_suppressed: bool,
    is_retrigger: bool,
    invoked_by_command: bool,
    invoked_by_mention: bool,
    invoked_by_reply: bool,
    skip_flag: bool,
    replied_flag: bool,
}

fn should_send_quiet_mode_fallback(
    message: &InboundMessage,
    state: ObserveModeFallbackState,
) -> bool {
    state.is_suppressed
        && !state.is_retrigger
        && !state.invoked_by_command
        && (state.invoked_by_mention || state.invoked_by_reply)
        && state.skip_flag
        && !state.replied_flag
        && matches!(
            message.source.as_str(),
            "discord" | "telegram" | "slack" | "twitch" | "signal"
        )
}

/// Check if a conversation ID represents a DM (direct message).
///
/// Discord and Mattermost embed a `:dm:` segment in the conversation ID.
/// Slack uses `slack:TEAM:DCHANNEL` where the channel ID starts with `D`.
fn is_dm_conversation_id(conv_id: &str) -> bool {
    conv_id.contains(":dm:")
        || conv_id.starts_with("slack:")
            && conv_id
                .rsplit(':')
                .next()
                .is_some_and(|last| last.starts_with('D'))
}

#[cfg(test)]
mod tests {
    use super::{
        ObserveModeFallbackState, ReflectionSignal, branch_working_memory_event_summary,
        classify_conversational_event_summary, compute_listen_mode_invocation, decision_user_id,
        extract_decision_summary_from_reply, format_conversational_event_summary,
        is_dm_conversation_id, recv_channel_event, should_process_event_for_channel,
        should_send_discord_quiet_mode_ping_ack, should_send_quiet_mode_fallback,
        worker_outcome_already_consumed,
    };
    use crate::memory::{MemoryType, WorkingMemoryEventType};
    use crate::{AgentId, ChannelId, InboundMessage, MessageContent, ProcessEvent, ProcessId};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn inbound_message(
        source: &str,
        metadata: &[(&str, serde_json::Value)],
        content: &str,
    ) -> InboundMessage {
        let mut message_metadata = HashMap::new();
        for (key, value) in metadata {
            message_metadata.insert((*key).to_string(), value.clone());
        }

        InboundMessage {
            id: "message-1".into(),
            source: source.into(),
            adapter: None,
            conversation_id: format!("{source}:conversation"),
            sender_id: "user-1".into(),
            agent_id: None,
            content: MessageContent::Text(content.into()),
            timestamp: chrono::Utc::now(),
            metadata: message_metadata,
            formatted_author: None,
        }
    }

    #[test]
    fn reflection_signal_records_failed_workers_without_firing() {
        let mut signal = ReflectionSignal::default();
        let failed = uuid::Uuid::new_v4();
        signal.record_worker(failed, false);

        assert_eq!(signal.workers, vec![(failed, false)]);
        assert!(
            !signal.is_set(),
            "a failure on its own has no lesson to reflect on"
        );
    }

    #[test]
    fn reflection_signal_carries_failed_predecessors_of_a_success() {
        let mut signal = ReflectionSignal::default();
        let first_failure = uuid::Uuid::new_v4();
        let second_failure = uuid::Uuid::new_v4();
        let success = uuid::Uuid::new_v4();
        signal.record_worker(first_failure, false);
        signal.record_worker(second_failure, false);
        signal.record_worker(success, true);

        assert!(signal.is_set());
        assert_eq!(
            signal.workers,
            vec![
                (first_failure, false),
                (second_failure, false),
                (success, true),
            ],
            "reflection needs the failed attempts that preceded the success"
        );
    }

    #[test]
    fn reflection_signal_fires_on_turn_work_alone() {
        let mut signal = ReflectionSignal::default();
        assert!(!signal.is_set());
        signal.turn_work = true;
        assert!(signal.is_set());
    }

    #[test]
    fn reflection_signal_ignores_repeat_completions() {
        let mut signal = ReflectionSignal::default();
        let worker = uuid::Uuid::new_v4();
        signal.record_worker(worker, false);
        signal.record_worker(worker, true);

        assert_eq!(
            signal.workers,
            vec![(worker, false)],
            "the first completion is the terminal one"
        );
    }

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
    fn extracts_decision_summary_from_reply_text() {
        let summary = extract_decision_summary_from_reply(
            "We'll switch to the new persistence trigger thresholds and remove the old 50-message cadence.",
        );

        assert_eq!(
            summary.as_deref(),
            Some(
                "We'll switch to the new persistence trigger thresholds and remove the old 50-message cadence"
            )
        );
        assert_eq!(
            extract_decision_summary_from_reply(
                "We decided to use the participant map instead of transcript scans."
            )
            .as_deref(),
            Some("We decided to use the participant map instead of transcript scans")
        );
        assert_eq!(
            extract_decision_summary_from_reply(
                "Decision: move forward with the config-backed participant resolver."
            )
            .as_deref(),
            Some("Decision: move forward with the config-backed participant resolver")
        );
        assert!(extract_decision_summary_from_reply("Here's the current status update.").is_none());
        assert!(extract_decision_summary_from_reply("I'll check that and report back.").is_none());
        assert!(extract_decision_summary_from_reply("Let's debug this first.").is_none());
        assert!(extract_decision_summary_from_reply("We'll look into it tomorrow.").is_none());
        assert!(
            extract_decision_summary_from_reply(
                "I approved the review comment and will follow up."
            )
            .is_none()
        );
        assert_eq!(
            extract_decision_summary_from_reply("Got it. We'll switch to the new routing config.")
                .as_deref(),
            Some("We'll switch to the new routing config")
        );
    }

    #[test]
    fn decision_user_id_skips_retrigger_messages() {
        let humans = vec![crate::config::HumanDef {
            id: "victor".to_string(),
            display_name: Some("Victor".to_string()),
            role: None,
            bio: None,
            description: None,
            discord_id: Some("12345".to_string()),
            telegram_id: None,
            slack_id: None,
            email: None,
        }];
        let message = InboundMessage {
            id: "message-1".to_string(),
            source: "system".to_string(),
            adapter: None,
            conversation_id: "discord:chan-1".to_string(),
            sender_id: "12345".to_string(),
            agent_id: None,
            content: crate::MessageContent::Text("retrigger".to_string()),
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
            formatted_author: None,
        };

        assert!(decision_user_id(&humans, &message, true).is_none());
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
            worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
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
            status: "done".to_string(),
            transcript: None,
            tool_calls: 0,
        };

        assert!(should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn worker_complete_event_matches_own_channel() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerComplete {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
            active_operation: None,
            channel_id: Some(channel_id.clone()),
            result: "done".to_string(),
            notify: true,
            success: true,
            outcome_kind: crate::conversation::WorkerOutcomeKind::Succeeded,
            outcome_version: 1,
            transcript_version: 0,
            terminal_owner: Some(crate::conversation::WorkerTerminalOwner::Worker),
        };

        assert!(should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn worker_complete_event_ignored_for_other_channel() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerComplete {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
            active_operation: None,
            channel_id: Some(Arc::from("channel-b")),
            result: "done".to_string(),
            notify: true,
            success: true,
            outcome_kind: crate::conversation::WorkerOutcomeKind::Succeeded,
            outcome_version: 1,
            transcript_version: 0,
            terminal_owner: Some(crate::conversation::WorkerTerminalOwner::Worker),
        };

        assert!(!should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn worker_complete_event_ignored_when_no_channel() {
        let channel_id: ChannelId = Arc::from("channel-a");
        let event = ProcessEvent::WorkerComplete {
            agent_id: Arc::from("agent"),
            worker_id: uuid::Uuid::new_v4(),
            worker_registration_id: crate::agent::process_control::WorkerRegistrationId::new(1),
            active_operation: None,
            channel_id: None,
            result: "done".to_string(),
            notify: true,
            success: true,
            outcome_kind: crate::conversation::WorkerOutcomeKind::Succeeded,
            outcome_version: 1,
            transcript_version: 0,
            terminal_owner: Some(crate::conversation::WorkerTerminalOwner::Worker),
        };

        assert!(!should_process_event_for_channel(&event, &channel_id));
    }

    #[test]
    fn duplicate_worker_outcome_version_is_consumed_once_without_handle_state() {
        let worker_id = uuid::Uuid::new_v4();
        let mut consumed = HashMap::new();
        assert!(!worker_outcome_already_consumed(&consumed, worker_id, 1));
        consumed.insert(worker_id, 1);
        assert!(worker_outcome_already_consumed(&consumed, worker_id, 1));
        assert!(!worker_outcome_already_consumed(&consumed, worker_id, 2));
    }

    #[test]
    fn conversational_event_summary_extracts_outcome_prefix() {
        let (event_type, summary) = classify_conversational_event_summary(
            "outcome: implemented the migration safety check",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::Outcome);
        assert_eq!(summary, "implemented the migration safety check");
    }

    #[test]
    fn conversational_event_summary_extracts_blocked_on_prefix() {
        let (event_type, summary) = classify_conversational_event_summary(
            "blocked_on: waiting for review from infra",
            WorkingMemoryEventType::Error,
        );
        assert_eq!(event_type, WorkingMemoryEventType::BlockedOn);
        assert_eq!(summary, "waiting for review from infra");
    }

    #[test]
    fn conversational_event_summary_falls_back_to_default_type() {
        let (event_type, summary) = classify_conversational_event_summary(
            "completed with no blockers",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::WorkerCompleted);
        assert_eq!(summary, "completed with no blockers");
    }

    #[test]
    fn conversational_event_summary_extracts_constraint_prefix_case_insensitively() {
        let (event_type, summary) = classify_conversational_event_summary(
            "CoNsTrAiNt: must keep migrations immutable",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::Constraint);
        assert_eq!(summary, "must keep migrations immutable");
    }

    #[test]
    fn conversational_event_summary_is_case_insensitive_across_prefixes() {
        let (event_type, summary) = classify_conversational_event_summary(
            "OUTCOME: implemented the follow-up",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::Outcome);
        assert_eq!(summary, "implemented the follow-up");

        let (event_type, summary) = classify_conversational_event_summary(
            "Blocked_On: waiting on reviewer signoff",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::BlockedOn);
        assert_eq!(summary, "waiting on reviewer signoff");

        let (event_type, summary) = classify_conversational_event_summary(
            "blocked on: user approval",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::BlockedOn);
        assert_eq!(summary, "user approval");
    }

    #[test]
    fn conversational_event_summary_treats_empty_prefixed_content_as_empty_summary() {
        let (event_type, summary) = classify_conversational_event_summary(
            "outcome:   ",
            WorkingMemoryEventType::WorkerCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::Outcome);
        assert!(summary.is_empty());
        assert_eq!(
            format_conversational_event_summary(event_type, "Worker", &summary),
            "Worker outcome"
        );
    }

    #[test]
    fn conversational_event_summary_extracts_deadline_prefix() {
        let (event_type, summary) = classify_conversational_event_summary(
            "deadline-set: ship by 2026-04-20",
            WorkingMemoryEventType::BranchCompleted,
        );
        assert_eq!(event_type, WorkingMemoryEventType::DeadlineSet);
        assert_eq!(summary, "ship by 2026-04-20");
        assert_eq!(
            format_conversational_event_summary(event_type, "Branch", &summary),
            "Branch deadline set: ship by 2026-04-20"
        );
    }

    #[test]
    fn branch_working_memory_event_records_cancellation_as_error() {
        let (event_type, summary) =
            branch_working_memory_event_summary("Branch cancelled: superseded by user request");

        assert_eq!(event_type, WorkingMemoryEventType::Error);
        assert_eq!(summary, "Branch cancelled: superseded by user request");
    }

    #[test]
    fn branch_working_memory_event_records_sentence_cancellation_as_error() {
        let (event_type, summary) = branch_working_memory_event_summary("Branch cancelled.");

        assert_eq!(event_type, WorkingMemoryEventType::Error);
        assert_eq!(summary, "Branch cancelled");
    }

    #[test]
    fn quiet_mode_invocation_uses_discord_mention_and_reply_metadata() {
        let message = inbound_message(
            "discord",
            &[
                ("discord_mentioned_bot", true.into()),
                ("discord_reply_to_bot", false.into()),
            ],
            "@bot ping",
        );

        let (invoked_by_command, invoked_by_mention, invoked_by_reply) =
            compute_listen_mode_invocation(&message, "@bot ping");

        assert!(!invoked_by_command);
        assert!(invoked_by_mention);
        assert!(!invoked_by_reply);
    }

    #[test]
    fn discord_quiet_mode_ping_ack_requires_directed_ping() {
        let directed_message = inbound_message(
            "discord",
            &[("discord_reply_to_bot", true.into())],
            "ping are you there?",
        );
        let ambient_message = inbound_message(
            "discord",
            &[("discord_reply_to_bot", false.into())],
            "ping are you there?",
        );

        assert!(should_send_discord_quiet_mode_ping_ack(
            &directed_message,
            "ping are you there?",
            true
        ));
        assert!(!should_send_discord_quiet_mode_ping_ack(
            &ambient_message,
            "ping are you there?",
            true
        ));
        assert!(!should_send_discord_quiet_mode_ping_ack(
            &directed_message,
            "ping are you there?",
            false
        ));
    }

    #[test]
    fn quiet_mode_fallback_requires_directed_skipped_turn_without_reply() {
        let message = inbound_message("discord", &[], "hey");

        assert!(should_send_quiet_mode_fallback(
            &message,
            ObserveModeFallbackState {
                is_suppressed: true,
                is_retrigger: false,
                invoked_by_command: false,
                invoked_by_mention: true,
                invoked_by_reply: false,
                skip_flag: true,
                replied_flag: false,
            }
        ));
        assert!(!should_send_quiet_mode_fallback(
            &message,
            ObserveModeFallbackState {
                is_suppressed: true,
                is_retrigger: false,
                invoked_by_command: false,
                invoked_by_mention: true,
                invoked_by_reply: false,
                skip_flag: false,
                replied_flag: false,
            }
        ));
        assert!(!should_send_quiet_mode_fallback(
            &message,
            ObserveModeFallbackState {
                is_suppressed: true,
                is_retrigger: false,
                invoked_by_command: false,
                invoked_by_mention: true,
                invoked_by_reply: false,
                skip_flag: true,
                replied_flag: true,
            }
        ));
        assert!(!should_send_quiet_mode_fallback(
            &message,
            ObserveModeFallbackState {
                is_suppressed: true,
                is_retrigger: true,
                invoked_by_command: false,
                invoked_by_mention: true,
                invoked_by_reply: false,
                skip_flag: true,
                replied_flag: false,
            }
        ));
    }

    #[test]
    fn is_dm_conversation_id_detects_dm_patterns() {
        // Slack DMs — channel ID starts with 'D'
        assert!(is_dm_conversation_id("slack:T07GZRRFRRT:D0AHN0BM8D8"));
        assert!(is_dm_conversation_id(
            "slack:adapter:T07GZRRFRRT:D0AHN0BM8D8"
        ));

        // Discord DMs
        assert!(is_dm_conversation_id("discord:dm:123456789"));

        // Mattermost DMs
        assert!(is_dm_conversation_id("mattermost:team1:dm:user1"));

        // Generic :dm: pattern
        assert!(is_dm_conversation_id("platform:dm:some-id"));

        // Non-DM patterns
        assert!(!is_dm_conversation_id("slack:T07GZRRFRRT:C12345"));
        assert!(!is_dm_conversation_id("discord:guild:123:channel:456"));
        assert!(!is_dm_conversation_id("discord:conversation"));
        assert!(!is_dm_conversation_id(""));
    }

    /// `build_available_channels` must never surface link-platform channels to the LLM.
    ///
    /// Link channels (e.g. "link:agent1:agent2") are internal audit trails; they have no
    /// real messaging adapter.  Exposing them causes the LLM to try
    /// `send_message_to_another_channel` which fails with a platform-resolution error
    /// because `resolve_broadcast_target` has no handler for platform == "link".
    /// Inter-agent communication is exclusively via `send_agent_message` (task delegation).
    #[test]
    fn available_channels_filter_excludes_link_platform() {
        use crate::conversation::channels::ChannelInfo;

        let make_channel = |id: &str, platform: &str| ChannelInfo {
            id: id.to_string(),
            platform: platform.to_string(),
            display_name: None,
            platform_meta: None,
            is_active: true,
            created_at: chrono::Utc::now(),
            last_activity_at: chrono::Utc::now(),
        };

        let channels = vec![
            make_channel("discord:guild:123", "discord"),
            make_channel("link:agent1:agent2", "link"),
            make_channel("link:agent2:agent1", "link"),
            make_channel("cron:daily", "cron"),
            make_channel("webhook:intake", "webhook"),
            make_channel("slack:T01:C01", "slack"),
        ];

        let current_id = "discord:guild:123";

        // Mirror the exact filter used in `build_available_channels`.
        let visible: Vec<_> = channels
            .into_iter()
            .filter(|ch| {
                ch.id.as_str() != current_id
                    && ch.platform != "cron"
                    && ch.platform != "webhook"
                    && ch.platform != "link"
            })
            .collect();

        // Only the slack channel should pass through.
        assert_eq!(
            visible.len(),
            1,
            "only real-platform channels should be visible"
        );
        assert_eq!(visible[0].platform, "slack");
    }
}
