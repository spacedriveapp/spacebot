//! Channel: User-facing conversation process.

use crate::agent::compactor::Compactor;
use crate::error::{AgentError, Result};
use crate::llm::SpacebotModel;
use crate::conversation::ConversationLogger;
use crate::{ChannelId, WorkerId, BranchId, ProcessId, ProcessType, AgentDeps, InboundMessage, ProcessEvent, OutboundResponse};
use crate::hooks::SpacebotHook;
use crate::agent::status::StatusBlock;
use crate::agent::worker::Worker;
use crate::agent::branch::Branch;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use rig::message::{ImageMediaType, MimeType, UserContent};
use rig::one_or_many::OneOrMany;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::sync::broadcast;
use std::collections::HashMap;

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
    pub status_block: Arc<RwLock<StatusBlock>>,
    pub deps: AgentDeps,
    pub conversation_logger: ConversationLogger,
    pub screenshot_dir: std::path::PathBuf,
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
    /// Conversation context (platform, channel name, server) captured from the first message.
    pub conversation_context: Option<String>,
    /// Context monitor that triggers background compaction.
    pub compactor: Compactor,
    /// Count of user messages since last memory persistence branch.
    message_count: usize,
    /// Branch IDs for silent memory persistence branches (results not injected into history).
    memory_persistence_branches: HashSet<BranchId>,
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
    ) -> (Self, mpsc::Sender<InboundMessage>) {
        let process_id = ProcessId::Channel(id.clone());
        let hook = SpacebotHook::new(deps.agent_id.clone(), process_id, ProcessType::Channel, deps.event_tx.clone());
        let status_block = Arc::new(RwLock::new(StatusBlock::new()));
        let history = Arc::new(RwLock::new(Vec::new()));
        let active_branches = Arc::new(RwLock::new(HashMap::new()));
        let active_workers = Arc::new(RwLock::new(HashMap::new()));
        let (message_tx, message_rx) = mpsc::channel(64);

        let conversation_logger = ConversationLogger::new(deps.sqlite_pool.clone());

        let compactor = Compactor::new(
            id.clone(),
            deps.clone(),
            history.clone(),
            conversation_logger.clone(),
        );

        let state = ChannelState {
            channel_id: id.clone(),
            history: history.clone(),
            active_branches: active_branches.clone(),
            active_workers: active_workers.clone(),
            status_block: status_block.clone(),
            deps: deps.clone(),
            conversation_logger,
            screenshot_dir,
        };

        let self_tx = message_tx.clone();
        let channel = Self {
            id: id.clone(),
            title: None,
            deps,
            hook,
            state,
            message_rx,
            event_rx,
            response_tx,
            self_tx,
            conversation_id: None,
            conversation_context: None,
            compactor,
            message_count: 0,
            memory_persistence_branches: HashSet::new(),
        };
        
        (channel, message_tx)
    }
    
    /// Run the channel event loop.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!(channel_id = %self.id, "channel started");
        
        loop {
            tokio::select! {
                Some(message) = self.message_rx.recv() => {
                    if let Err(error) = self.handle_message(message).await {
                        tracing::error!(%error, channel_id = %self.id, "error handling message");
                    }
                }
                Ok(event) = self.event_rx.recv() => {
                    if let Err(error) = self.handle_event(event).await {
                        tracing::error!(%error, channel_id = %self.id, "error handling event");
                    }
                }
                else => break,
            }
        }
        
        tracing::info!(channel_id = %self.id, "channel stopped");
        Ok(())
    }
    
    /// Handle an incoming message by running the channel's LLM agent loop.
    ///
    /// The LLM decides which tools to call: reply (to respond), branch (to think),
    /// spawn_worker (to delegate), route (to follow up with a worker), cancel, or
    /// memory_save. The tools act on the channel's shared state directly.
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
        
        let (raw_text, attachments) = match &message.content {
            crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
            crate::MessageContent::Media { text, attachments } => {
                (text.clone().unwrap_or_default(), attachments.clone())
            }
        };

        // Format the user text with sender attribution so the LLM knows who's talking
        let user_text = format_user_message(&raw_text, &message);

        // Download and process attachments into LLM-ready content
        let attachment_content = if !attachments.is_empty() {
            download_attachments(&self.deps, &attachments).await
        } else {
            Vec::new()
        };

        // Persist user messages (skip system re-triggers)
        if message.source != "system" {
            let sender_name = message.metadata
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
        }

        // Capture conversation context from the first message (platform, channel, server)
        if self.conversation_context.is_none() {
            self.conversation_context = Some(build_conversation_context(&message));
        }

        // Build the system prompt: identity + channel prompt + conversation context + status
        let status_text = {
            let status = self.state.status_block.read().await;
            status.render()
        };
        let rc = &self.deps.runtime_config;
        let identity_context = rc.identity.load().render();
        let channel_prompt = rc.prompts.load().channel.clone();
        let skills = rc.skills.load();

        let mut system_prompt = String::new();
        if !identity_context.is_empty() {
            system_prompt.push_str(&identity_context);
            system_prompt.push_str("\n\n");
        }
        system_prompt.push_str(&channel_prompt);
        let skills_prompt = skills.render_channel_prompt();
        if !skills_prompt.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&skills_prompt);
        }
        if let Some(context) = &self.conversation_context {
            system_prompt.push_str("\n\n## Conversation Context\n\n");
            system_prompt.push_str(context);
        }
        if !status_text.is_empty() {
            system_prompt.push_str("\n\n## Current Status\n\n");
            system_prompt.push_str(&status_text);
        }

        // Register per-turn channel tools (reply, branch, spawn_worker, route, cancel, skip)
        let conversation_id = message.conversation_id.clone();
        let skip_flag = crate::tools::new_skip_flag();
        if let Err(error) = crate::tools::add_channel_tools(
            &self.deps.tool_server,
            self.state.clone(),
            self.response_tx.clone(),
            &conversation_id,
            skip_flag.clone(),
        ).await {
            tracing::error!(%error, "failed to add channel tools");
            return Err(AgentError::Other(error.into()).into());
        }

        // Construct the model and agent for this turn
        let routing = rc.routing.load();
        let max_turns = **rc.max_turns.load();
        let model_name = routing.resolve(ProcessType::Channel, None);
        let model = SpacebotModel::make(&self.deps.llm_manager, model_name)
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(&system_prompt)
            .default_max_turns(max_turns)
            .tool_server_handle(self.deps.tool_server.clone())
            .build();

        // Signal typing indicator while the LLM processes
        let _ = self.response_tx.send(OutboundResponse::Status(crate::StatusUpdate::Thinking)).await;

        // If there are attachments, inject them into history as a user message before the prompt.
        // The LLM will see the images/files followed by the user's text message.
        if !attachment_content.is_empty() {
            let mut history = self.state.history.write().await;
            let content = OneOrMany::many(attachment_content)
                .unwrap_or_else(|_| OneOrMany::one(UserContent::text("[attachment processing failed]")));
            history.push(rig::message::Message::User { content });
            drop(history);
        }

        // Run the agent loop with the channel's persistent history
        let mut history = self.state.history.write().await;
        let result = agent.prompt(&user_text)
            .with_history(&mut history)
            .with_hook(self.hook.clone())
            .await;
        drop(history);

        // Clean up per-turn tools
        if let Err(error) = crate::tools::remove_channel_tools(&self.deps.tool_server).await {
            tracing::warn!(%error, "failed to remove channel tools");
        }

        match result {
            Ok(response) => {
                let skipped = skip_flag.load(std::sync::atomic::Ordering::Relaxed);

                if skipped {
                    tracing::debug!(channel_id = %self.id, "channel turn skipped (no response)");
                } else {
                    // If the LLM returned text without using the reply tool, send it
                    // directly. Some models respond with text instead of tool calls.
                    let text = response.trim();
                    if !text.is_empty() {
                        self.state.conversation_logger.log_bot_message(&self.state.channel_id, text);
                        if let Err(error) = self.response_tx.send(OutboundResponse::Text(text.to_string())).await {
                            tracing::error!(%error, channel_id = %self.id, "failed to send fallback reply");
                        }
                    }

                    tracing::debug!(channel_id = %self.id, "channel turn completed");
                }
            }
            Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                tracing::warn!(channel_id = %self.id, "channel hit max turns");
            }
            Err(rig::completion::PromptError::PromptCancelled { reason, .. }) => {
                tracing::info!(channel_id = %self.id, %reason, "channel turn cancelled");
            }
            Err(error) => {
                tracing::error!(channel_id = %self.id, %error, "channel LLM call failed");
            }
        }

        // Check context size and trigger compaction if needed
        if let Err(error) = self.compactor.check_and_compact().await {
            tracing::warn!(channel_id = %self.id, %error, "compaction check failed");
        }

        // Increment message counter and spawn memory persistence branch if threshold reached
        if message.source != "system" {
            self.message_count += 1;
            self.check_memory_persistence().await;
        }
        
        Ok(())
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
        
        match &event {
            ProcessEvent::BranchResult { branch_id, conclusion, .. } => {
                // Remove from active branches
                let mut branches = self.state.active_branches.write().await;
                branches.remove(branch_id);

                // Memory persistence branches complete silently — no history
                // injection, no re-trigger. The work (memory saves) already
                // happened inside the branch via tool calls.
                if self.memory_persistence_branches.remove(branch_id) {
                    tracing::info!(branch_id = %branch_id, "memory persistence branch completed");
                } else {
                    // Regular branch: inject conclusion into history
                    let mut history = self.state.history.write().await;
                    let branch_message = format!("[Branch result]: {conclusion}");
                    history.push(rig::message::Message::from(branch_message));
                    should_retrigger = true;

                    tracing::info!(branch_id = %branch_id, "branch result incorporated");
                }
            }
            ProcessEvent::WorkerComplete { worker_id, result, notify, .. } => {
                let mut workers = self.state.active_workers.write().await;
                workers.remove(worker_id);

                if *notify {
                    let mut history = self.state.history.write().await;
                    let worker_message = format!("[Worker completed]: {result}");
                    history.push(rig::message::Message::from(worker_message));
                    should_retrigger = true;
                }
                
                tracing::info!(worker_id = %worker_id, "worker completed");
            }
            _ => {}
        }

        // Re-trigger the channel LLM so it can process the result and respond
        if should_retrigger {
            if let Some(conversation_id) = &self.conversation_id {
                let synthetic = InboundMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    source: "system".into(),
                    conversation_id: conversation_id.clone(),
                    sender_id: "system".into(),
                    agent_id: None,
                    content: crate::MessageContent::Text(
                        "[System: a background process has completed. Check your history and status block for the result, then respond to the user.]".into()
                    ),
                    timestamp: chrono::Utc::now(),
                    metadata: std::collections::HashMap::new(),
                };
                if let Err(error) = self.self_tx.try_send(synthetic) {
                    tracing::warn!(%error, "failed to re-trigger channel after process completion");
                }
            }
        }
        
        Ok(())
    }
    
    /// Get the current status block as a string.
    pub async fn get_status(&self) -> String {
        let status = self.state.status_block.read().await;
        status.render()
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

/// Spawn a branch from a ChannelState. Used by the BranchTool.
pub async fn spawn_branch_from_state(
    state: &ChannelState,
    description: impl Into<String>,
) -> std::result::Result<BranchId, AgentError> {
    let description = description.into();

    // Check branch limit (read live from runtime config)
    let max_branches = **state.deps.runtime_config.max_concurrent_branches.load();
    {
        let branches = state.active_branches.read().await;
        if branches.len() >= max_branches {
            return Err(AgentError::BranchLimitReached {
                channel_id: state.channel_id.to_string(),
                max: max_branches,
            });
        }
    }
    
    // Clone history for the branch
    let history = {
        let h = state.history.read().await;
        h.clone()
    };
    
    let prompt = description.clone();
    let branch_system_prompt = state.deps.runtime_config.prompts.load().branch.clone();
    let tool_server = crate::tools::create_branch_tool_server(state.deps.memory_search.clone());
    let branch_max_turns = **state.deps.runtime_config.branch_max_turns.load();
    let branch = Branch::new(
        state.channel_id.clone(),
        &description,
        state.deps.clone(),
        &branch_system_prompt,
        history,
        tool_server,
        branch_max_turns,
    );
    
    let branch_id = branch.id;

    // Spawn the branch as a tokio task
    let handle = tokio::spawn(async move {
        if let Err(error) = branch.run(&prompt).await {
            tracing::error!(branch_id = %branch_id, %error, "branch failed");
        }
    });
    
    {
        let mut branches = state.active_branches.write().await;
        branches.insert(branch_id, handle);
    }
    
    {
        let mut status = state.status_block.write().await;
        status.add_branch(branch_id, "thinking...");
    }
    
    tracing::info!(branch_id = %branch_id, "branch spawned");
    
    Ok(branch_id)
}

/// Spawn a silent memory persistence branch.
///
/// Uses the same branching infrastructure as regular branches but with a
/// dedicated prompt focused on memory recall + save. The result is not injected
/// into channel history — the channel handles these branch IDs specially.
async fn spawn_memory_persistence_branch(
    state: &ChannelState,
    deps: &AgentDeps,
) -> std::result::Result<BranchId, AgentError> {
    // Check branch limit
    let max_branches = **deps.runtime_config.max_concurrent_branches.load();
    {
        let branches = state.active_branches.read().await;
        if branches.len() >= max_branches {
            return Err(AgentError::BranchLimitReached {
                channel_id: state.channel_id.to_string(),
                max: max_branches,
            });
        }
    }

    let history = {
        let h = state.history.read().await;
        h.clone()
    };

    let prompt = "Review the recent conversation and persist any important information as memories. \
                  Start by recalling existing memories related to the topics discussed, then save \
                  new or updated memories with appropriate associations.";

    let system_prompt = deps.runtime_config.prompts.load().memory_persistence.clone();
    let tool_server = crate::tools::create_branch_tool_server(deps.memory_search.clone());
    let branch_max_turns = **deps.runtime_config.branch_max_turns.load();

    let branch = Branch::new(
        state.channel_id.clone(),
        "memory persistence",
        deps.clone(),
        &system_prompt,
        history,
        tool_server,
        branch_max_turns,
    );

    let branch_id = branch.id;

    let handle = tokio::spawn(async move {
        if let Err(error) = branch.run(prompt).await {
            tracing::error!(branch_id = %branch_id, %error, "memory persistence branch failed");
        }
    });

    {
        let mut branches = state.active_branches.write().await;
        branches.insert(branch_id, handle);
    }

    {
        let mut status = state.status_block.write().await;
        status.add_branch(branch_id, "persisting memories...");
    }

    Ok(branch_id)
}

/// Spawn a worker from a ChannelState. Used by the SpawnWorkerTool.
pub async fn spawn_worker_from_state(
    state: &ChannelState,
    task: impl Into<String>,
    interactive: bool,
    skill_name: Option<&str>,
) -> std::result::Result<WorkerId, AgentError> {
    let task = task.into();

    let rc = &state.deps.runtime_config;
    let worker_system_prompt = rc.prompts.load().worker.clone();
    let skills = rc.skills.load();
    let browser_config = (**rc.browser_config.load()).clone();
    let brave_search_key = (**rc.brave_search_key.load()).clone();

    // Build the worker system prompt, optionally prepending skill instructions
    let system_prompt = if let Some(name) = skill_name {
        if let Some(skill_prompt) = skills.render_worker_prompt(name) {
            format!("{}\n\n{}", worker_system_prompt, skill_prompt)
        } else {
            tracing::warn!(skill = %name, "skill not found, spawning worker without skill context");
            worker_system_prompt
        }
    } else {
        worker_system_prompt
    };
    
    let worker = if interactive {
        let (worker, _input_tx) = Worker::new_interactive(
            Some(state.channel_id.clone()),
            &task,
            &system_prompt,
            state.deps.clone(),
            browser_config.clone(),
            state.screenshot_dir.clone(),
            brave_search_key.clone(),
        );
        // TODO: Store input_tx somewhere accessible for routing follow-ups
        worker
    } else {
        Worker::new(
            Some(state.channel_id.clone()),
            &task,
            &system_prompt,
            state.deps.clone(),
            browser_config,
            state.screenshot_dir.clone(),
            brave_search_key,
        )
    };
    
    let worker_id = worker.id;
    
    // Spawn the worker as a tokio task
    let deps_event_tx = state.deps.event_tx.clone();
    let agent_id = state.deps.agent_id.clone();
    let channel_id = Some(state.channel_id.clone());
    tokio::spawn(async move {
        let result = worker.run().await;
        match result {
            Ok(result_text) => {
                let _ = deps_event_tx.send(ProcessEvent::WorkerComplete {
                    agent_id,
                    worker_id,
                    channel_id,
                    result: result_text,
                    notify: true,
                });
            }
            Err(error) => {
                tracing::error!(worker_id = %worker_id, %error, "worker failed");
                let _ = deps_event_tx.send(ProcessEvent::WorkerComplete {
                    agent_id,
                    worker_id,
                    channel_id,
                    result: format!("Worker failed: {error}"),
                    notify: true,
                });
            }
        }
    });
    
    {
        let mut status = state.status_block.write().await;
        status.add_worker(worker_id, &task, false);
    }
    
    tracing::info!(worker_id = %worker_id, task = %task, "worker spawned");
    
    Ok(worker_id)
}

/// Format a user message with sender attribution from message metadata.
///
/// In multi-user channels, this lets the LLM distinguish who said what.
/// System-generated messages (re-triggers) are passed through as-is.
fn format_user_message(raw_text: &str, message: &InboundMessage) -> String {
    if message.source == "system" {
        return raw_text.to_string();
    }

    let display_name = message.metadata
        .get("sender_display_name")
        .and_then(|v| v.as_str())
        .unwrap_or(&message.sender_id);

    format!("[{display_name}]: {raw_text}")
}

/// Build conversation context string from the first message's metadata.
///
/// Injected into the system prompt so the LLM knows what platform and
/// channel it's operating in.
fn build_conversation_context(message: &InboundMessage) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Platform: {}", message.source));

    if let Some(guild_name) = message.metadata.get("discord_guild_name").and_then(|v| v.as_str()) {
        lines.push(format!("Server: {guild_name}"));
    }

    if let Some(channel_name) = message.metadata.get("discord_channel_name").and_then(|v| v.as_str()) {
        lines.push(format!("Channel: #{channel_name}"));
    }

    lines.push("Multiple users may be present. Each message is prefixed with [username].".into());

    lines.join("\n")
}

/// Check if a ProcessEvent is targeted at a specific channel.
///
/// Events from branches and workers carry a channel_id. We only process events
/// that originated from this channel — otherwise broadcast events from one
/// channel's workers would leak into sibling channels (e.g. threads).
fn event_is_for_channel(event: &ProcessEvent, channel_id: &ChannelId) -> bool {
    match event {
        ProcessEvent::BranchResult { channel_id: event_channel, .. } => {
            event_channel == channel_id
        }
        ProcessEvent::WorkerComplete { channel_id: event_channel, .. } => {
            event_channel.as_ref() == Some(channel_id)
        }
        ProcessEvent::WorkerStatus { channel_id: event_channel, .. } => {
            event_channel.as_ref() == Some(channel_id)
        }
        // Status block updates, tool events, etc. — match on agent_id which
        // is already filtered by the event bus subscription. Let them through.
        _ => true,
    }
}

/// Image MIME types we support for vision.
const IMAGE_MIME_PREFIXES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Text-based MIME types where we inline the content.
const TEXT_MIME_PREFIXES: &[&str] = &[
    "text/", "application/json", "application/xml", "application/javascript",
    "application/typescript", "application/toml", "application/yaml",
];

/// Download attachments and convert them to LLM-ready UserContent parts.
///
/// Images become `UserContent::Image` (base64). Text files get inlined.
/// Other file types get a metadata-only description.
async fn download_attachments(
    deps: &AgentDeps,
    attachments: &[crate::Attachment],
) -> Vec<UserContent> {
    let http = deps.llm_manager.http_client();
    let mut parts = Vec::new();

    for attachment in attachments {
        let is_image = IMAGE_MIME_PREFIXES.iter().any(|p| attachment.mime_type.starts_with(p));
        let is_text = TEXT_MIME_PREFIXES.iter().any(|p| attachment.mime_type.starts_with(p));

        if is_image {
            match http.get(&attachment.url).send().await {
                Ok(response) => {
                    match response.bytes().await {
                        Ok(bytes) => {
                            use base64::Engine as _;
                            let base64_data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            let media_type = ImageMediaType::from_mime_type(&attachment.mime_type);
                            parts.push(UserContent::image_base64(base64_data, media_type, None));
                            tracing::info!(
                                filename = %attachment.filename,
                                mime = %attachment.mime_type,
                                size = bytes.len(),
                                "downloaded image attachment"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(%error, filename = %attachment.filename, "failed to read attachment bytes");
                            parts.push(UserContent::text(format!(
                                "[Failed to download image: {}]", attachment.filename
                            )));
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, filename = %attachment.filename, "failed to download attachment");
                    parts.push(UserContent::text(format!(
                        "[Failed to download image: {}]", attachment.filename
                    )));
                }
            }
        } else if is_text {
            match http.get(&attachment.url).send().await {
                Ok(response) => {
                    match response.text().await {
                        Ok(content) => {
                            // Truncate very large files to avoid blowing up context
                            let truncated = if content.len() > 50_000 {
                                format!("{}...\n[truncated — {} bytes total]", &content[..50_000], content.len())
                            } else {
                                content
                            };
                            parts.push(UserContent::text(format!(
                                "<file name=\"{}\" mime=\"{}\">\n{}\n</file>",
                                attachment.filename, attachment.mime_type, truncated
                            )));
                            tracing::info!(
                                filename = %attachment.filename,
                                mime = %attachment.mime_type,
                                "downloaded text attachment"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(%error, filename = %attachment.filename, "failed to read text attachment");
                            parts.push(UserContent::text(format!(
                                "[Failed to read file: {}]", attachment.filename
                            )));
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, filename = %attachment.filename, "failed to download text attachment");
                    parts.push(UserContent::text(format!(
                        "[Failed to download file: {}]", attachment.filename
                    )));
                }
            }
        } else {
            // Unknown file type — just describe it
            let size_str = attachment.size_bytes
                .map(|s| format!("{:.1} KB", s as f64 / 1024.0))
                .unwrap_or_else(|| "unknown size".into());
            parts.push(UserContent::text(format!(
                "[Attachment: {} ({}, {})]",
                attachment.filename, attachment.mime_type, size_str
            )));
        }
    }

    parts
}
