//! Channel: User-facing conversation process.

use crate::agent::channel_attachments::download_attachments;
use crate::agent::channel_dispatch::spawn_memory_persistence_branch;
use crate::agent::channel_history::{
    apply_history_after_turn, event_is_for_channel, extract_message_id,
    extract_reply_from_tool_syntax, format_batched_user_message, format_user_message,
    message_display_name, pop_retrigger_bridge_message,
};
use crate::agent::channel_prompt::{
    MAX_RETRIGGERS_PER_TURN, RETRIGGER_DEBOUNCE_MS, RETRIGGER_MAX_TURNS, TemporalContext,
};
use crate::agent::compactor::Compactor;
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
use rig::completion::{CompletionModel, Prompt};
use rig::message::UserContent;
use rig::one_or_many::OneOrMany;
use rig::tool::server::ToolServer;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
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
    /// Input senders for interactive workers, keyed by worker ID.
    /// Used by the route tool to deliver follow-up messages.
    pub worker_inputs: Arc<RwLock<HashMap<WorkerId, tokio::sync::mpsc::Sender<String>>>>,
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
        let handle = self.worker_handles.write().await.remove(&worker_id);
        let removed = self
            .active_workers
            .write()
            .await
            .remove(&worker_id)
            .is_some();
        self.worker_inputs.write().await.remove(&worker_id);

        if let Some(handle) = handle {
            handle.abort();
            // Mark the DB row as cancelled since the abort prevents WorkerComplete from firing
            self.process_run_logger
                .log_worker_completed(worker_id, "Worker cancelled", false);
            Ok(())
        } else if removed {
            self.process_run_logger
                .log_worker_completed(worker_id, "Worker cancelled", false);
            Ok(())
        } else {
            Err(format!("Worker {worker_id} not found"))
        }
    }

    /// Cancel a running branch by aborting its tokio task.
    /// Returns an error message if the branch is not found.
    pub async fn cancel_branch(&self, branch_id: BranchId) -> std::result::Result<(), String> {
        let handle = self.active_branches.write().await.remove(&branch_id);
        if let Some(handle) = handle {
            handle.abort();
            Ok(())
        } else {
            Err(format!("Branch {branch_id} not found"))
        }
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
            worker_inputs: Arc::new(RwLock::new(HashMap::new())),
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

    fn suppress_plaintext_fallback(&self) -> bool {
        matches!(self.current_adapter(), Some("email"))
    }

    /// Run the channel event loop.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!(channel_id = %self.id, "channel started");

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
                Ok(event) = self.event_rx.recv() => {
                    // Events bypass coalescing - flush buffer first if needed
                    if let Err(error) = self.flush_coalesce_buffer().await {
                        tracing::error!(%error, channel_id = %self.id, "error flushing coalesce buffer");
                    }
                    if let Err(error) = self.handle_event(event).await {
                        tracing::error!(%error, channel_id = %self.id, "error handling event");
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
        let mut user_contents: Vec<UserContent> = Vec::new();
        let mut conversation_id = String::new();
        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());

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

                self.state.conversation_logger.log_user_message(
                    &self.state.channel_id,
                    sender_name,
                    &message.sender_id,
                    &raw_text,
                    &message.metadata,
                );
                self.state
                    .channel_store
                    .upsert(&message.conversation_id, &message.metadata);

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

                // Download attachments for this message
                if !attachments.is_empty() {
                    let attachment_content = download_attachments(&self.deps, &attachments).await;
                    for content in attachment_content {
                        user_contents.push(content);
                    }
                }

                user_contents.push(UserContent::text(formatted_text));
            }
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
        let worker_capabilities = prompt_engine.render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
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
        )
    }

    /// Handle an incoming message by running the channel's LLM agent loop.
    ///
    /// The LLM decides which tools to call: reply (to respond), branch (to think),
    /// spawn_worker (to delegate), route (to follow up with a worker), cancel, or
    /// memory_save. The tools act on the channel's shared state directly.
    #[tracing::instrument(skip(self, message), fields(channel_id = %self.id, agent_id = %self.deps.agent_id, message_id = %message.id))]
    async fn handle_message(&mut self, message: InboundMessage) -> Result<()> {
        tracing::info!(
            channel_id = %self.id,
            message_id = %message.id,
            "handling message"
        );

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

        let temporal_context = TemporalContext::from_runtime(self.deps.runtime_config.as_ref());
        let message_timestamp = temporal_context.format_timestamp(message.timestamp);
        let user_text = format_user_message(&raw_text, &message, &message_timestamp);

        let attachment_content = if !attachments.is_empty() {
            download_attachments(&self.deps, &attachments).await
        } else {
            Vec::new()
        };

        // Persist user messages (skip system re-triggers)
        if message.source != "system" {
            let sender_name = message
                .metadata
                .get("sender_display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&message.sender_id);
            self.state.conversation_logger.log_user_message(
                &self.state.channel_id,
                sender_name,
                &message.sender_id,
                &raw_text,
                &message.metadata,
            );
            self.state
                .channel_store
                .upsert(&message.conversation_id, &message.metadata);
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

        let system_prompt = self.build_system_prompt().await?;

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = extract_message_id(&message);
        }

        let is_retrigger = message.source == "system";

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

            let is_human = !self.deps.agent_names.contains_key(other_id.as_str());
            let name = self
                .deps
                .agent_names
                .get(other_id.as_str())
                .cloned()
                .unwrap_or_else(|| other_id.clone());

            let info = crate::prompts::engine::LinkedAgent {
                name,
                id: other_id.clone(),
                is_human,
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
        let worker_capabilities = prompt_engine.render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
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

        let mut result = agent
            .prompt(user_text)
            .with_history(&mut history)
            .with_hook(self.hook.clone())
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
            result = agent
                .prompt(&correction)
                .with_history(&mut history)
                .with_hook(self.hook.clone())
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
                                if let Err(error) = self
                                    .response_tx
                                    .send(OutboundResponse::Text(final_text))
                                    .await
                                {
                                    tracing::error!(%error, channel_id = %self.id, "failed to send retrigger fallback reply");
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
                                if let Err(error) = self
                                    .response_tx
                                    .send(OutboundResponse::Text(final_text))
                                    .await
                                {
                                    tracing::error!(%error, channel_id = %self.id, "failed to send retrigger fallback reply");
                                }
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
                            if let Err(error) = self
                                .response_tx
                                .send(OutboundResponse::Text(final_text))
                                .await
                            {
                                tracing::error!(%error, channel_id = %self.id, "failed to send fallback reply");
                            }
                        }
                    }

                    tracing::debug!(channel_id = %self.id, "channel turn completed");
                }
            }
            Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                tracing::warn!(channel_id = %self.id, "channel hit max turns");
            }
            Err(rig::completion::PromptError::PromptCancelled { reason, .. }) => {
                if reason == "reply delivered" {
                    tracing::debug!(channel_id = %self.id, "channel turn completed via reply tool");
                } else {
                    tracing::info!(channel_id = %self.id, %reason, "channel turn cancelled");
                }
            }
            Err(error) => {
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
                run_logger.log_branch_completed(*branch_id, conclusion);

                // Remove from active branches
                let mut branches = self.state.active_branches.write().await;
                branches.remove(branch_id);

                #[cfg(feature = "metrics")]
                crate::telemetry::Metrics::global()
                    .active_branches
                    .with_label_values(&[&*self.deps.agent_id])
                    .dec();

                // Memory persistence branches complete silently — no history
                // injection, no re-trigger. The work (memory saves) already
                // happened inside the branch via tool calls.
                if self.memory_persistence_branches.remove(branch_id) {
                    self.branch_reply_targets.remove(branch_id);
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

                    if let Some(message_id) = self.branch_reply_targets.remove(branch_id) {
                        retrigger_metadata.insert(
                            crate::metadata_keys::REPLY_TO_MESSAGE_ID.to_string(),
                            serde_json::Value::from(message_id),
                        );
                    }

                    tracing::info!(branch_id = %branch_id, "branch result queued for retrigger");
                }
            }
            ProcessEvent::WorkerStarted {
                worker_id,
                channel_id,
                task,
                worker_type,
                ..
            } => {
                run_logger.log_worker_started(
                    channel_id.as_ref(),
                    *worker_id,
                    task,
                    worker_type,
                    &self.deps.agent_id,
                );
            }
            ProcessEvent::WorkerStatus {
                worker_id, status, ..
            } => {
                run_logger.log_worker_status(*worker_id, status);
            }
            ProcessEvent::WorkerComplete {
                worker_id,
                result,
                notify,
                success,
                ..
            } => {
                run_logger.log_worker_completed(*worker_id, result, *success);

                let mut workers = self.state.active_workers.write().await;
                workers.remove(worker_id);
                drop(workers);

                self.state.worker_handles.write().await.remove(worker_id);
                self.state.worker_inputs.write().await.remove(worker_id);

                if *notify {
                    // Accumulate result for the next retrigger instead of
                    // injecting into history as a fake user message.
                    self.pending_results.push(PendingResult {
                        process_type: "worker",
                        process_id: worker_id.to_string(),
                        result: result.clone(),
                        success: *success,
                    });
                    should_retrigger = true;
                }

                tracing::info!(worker_id = %worker_id, "worker completed, result queued for retrigger");
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

        let mut metadata = self.pending_retrigger_metadata.clone();
        metadata.insert(
            "retrigger_result_summary".to_string(),
            serde_json::Value::String(result_summary),
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
