//! Spacebot: A Rust agentic system where every LLM process has a dedicated role.

pub mod agent;
pub mod api;
pub mod auth;
pub mod config;
pub mod conversation;
pub mod cron;
pub mod daemon;
pub mod db;
pub mod error;
pub mod hooks;
pub mod identity;
pub mod links;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod messaging;
pub mod openai_auth;
pub mod opencode;
pub mod prompts;
pub mod sandbox;
pub mod secrets;
pub mod self_awareness;
pub mod settings;
pub mod skills;
pub mod tasks;
#[cfg(feature = "metrics")]
pub mod telemetry;
pub mod tools;
pub mod update;

pub use error::{Error, Result};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Signal from the API to the main event loop to trigger provider setup.
#[derive(Debug)]
pub enum ProviderSetupEvent {
    /// New provider keys have been added. Reinitialize agents.
    ProvidersConfigured,
}

/// Agent identifier type.
pub type AgentId = Arc<str>;

/// Channel identifier type.
pub type ChannelId = Arc<str>;

/// Worker identifier type.
pub type WorkerId = uuid::Uuid;

/// Branch identifier type.
pub type BranchId = uuid::Uuid;

/// Process identifier type (union of channel, worker, branch IDs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProcessId {
    Channel(ChannelId),
    Worker(WorkerId),
    Branch(BranchId),
}

impl std::fmt::Display for ProcessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessId::Channel(id) => write!(f, "channel:{}", id),
            ProcessId::Worker(id) => write!(f, "worker:{}", id),
            ProcessId::Branch(id) => write!(f, "branch:{}", id),
        }
    }
}

/// Process types in the system.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessType {
    Channel,
    Branch,
    Worker,
    Compactor,
    Cortex,
}

impl std::fmt::Display for ProcessType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessType::Channel => write!(f, "channel"),
            ProcessType::Branch => write!(f, "branch"),
            ProcessType::Worker => write!(f, "worker"),
            ProcessType::Compactor => write!(f, "compactor"),
            ProcessType::Cortex => write!(f, "cortex"),
        }
    }
}

/// Return a short summary from the first non-empty line, truncated to a
/// character limit.
pub const EVENT_SUMMARY_MAX_CHARS: usize = 160;

pub fn summarize_first_non_empty_line(value: &str, max_chars: usize) -> String {
    let first_line = value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| value.trim());

    truncate_to_chars(first_line, max_chars).to_string()
}

fn truncate_to_chars(value: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }

    if let Some((index, _)) = value.char_indices().nth(max_chars) {
        &value[..index]
    } else {
        value
    }
}

#[derive(Debug)]
pub enum BroadcastRecvResult<T> {
    Event(T),
    Lagged(u64),
    Closed,
}

pub fn classify_broadcast_recv_result<T>(
    result: std::result::Result<T, tokio::sync::broadcast::error::RecvError>,
) -> BroadcastRecvResult<T> {
    match result {
        Ok(event) => BroadcastRecvResult::Event(event),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
            BroadcastRecvResult::Lagged(count)
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => BroadcastRecvResult::Closed,
    }
}

/// Events sent between processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessEvent {
    BranchStarted {
        agent_id: AgentId,
        branch_id: BranchId,
        channel_id: ChannelId,
        description: String,
        reply_to_message_id: Option<String>,
    },
    BranchResult {
        agent_id: AgentId,
        branch_id: BranchId,
        channel_id: ChannelId,
        conclusion: String,
    },
    WorkerStarted {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        task: String,
        worker_type: String,
        interactive: bool,
    },
    WorkerStatus {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        status: String,
    },
    /// An interactive worker has entered the idle state (waiting for follow-up
    /// input). Persisted to the DB so the frontend can show an "idle" badge
    /// instead of "running". The worker remains in the active set.
    WorkerIdle {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
    },
    WorkerComplete {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        result: String,
        notify: bool,
        success: bool,
    },
    ToolStarted {
        agent_id: AgentId,
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        tool_name: String,
        args: String,
    },
    ToolCompleted {
        agent_id: AgentId,
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        tool_name: String,
        result: String,
    },
    MemorySaved {
        agent_id: AgentId,
        memory_id: String,
        channel_id: Option<ChannelId>,
        memory_type: crate::memory::MemoryType,
        importance: f32,
        content_summary: String,
    },
    CompactionTriggered {
        agent_id: AgentId,
        channel_id: ChannelId,
        threshold_reached: f32,
    },
    StatusUpdate {
        agent_id: AgentId,
        process_id: ProcessId,
        status: String,
    },
    WorkerPermission {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        permission_id: String,
        description: String,
        patterns: Vec<String>,
    },
    WorkerQuestion {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        question_id: String,
        questions: Vec<opencode::QuestionInfo>,
    },
    AgentMessageSent {
        from_agent_id: AgentId,
        to_agent_id: AgentId,
        link_id: String,
        channel_id: ChannelId,
    },
    AgentMessageReceived {
        from_agent_id: AgentId,
        to_agent_id: AgentId,
        link_id: String,
        channel_id: ChannelId,
    },
    TaskUpdated {
        agent_id: AgentId,
        task_number: i64,
        status: String,
        /// "created", "updated", or "deleted".
        action: String,
    },
    /// An OpenCode worker created a session, recording metadata for the web UI embed.
    OpenCodeSessionCreated {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        session_id: String,
        port: u16,
    },
    /// A finalized content part from an OpenCode worker session. Emitted on every
    /// `message.part.updated` SSE event so the frontend can build a live transcript.
    OpenCodePartUpdated {
        agent_id: AgentId,
        worker_id: WorkerId,
        part: crate::opencode::types::OpenCodePart,
    },
    /// An interactive worker's initial task completed. The worker remains alive
    /// for follow-ups, but the channel should retrigger to deliver this result.
    /// Unlike `WorkerComplete`, the worker is NOT removed from the active set.
    WorkerInitialResult {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        result: String,
    },
    TextDelta {
        agent_id: AgentId,
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        text_delta: String,
        aggregated_text: String,
    },
}

/// Default broadcast capacity for the per-agent control event bus.
pub const CONTROL_EVENT_BUS_CAPACITY: usize = 256;

/// Default broadcast capacity for the per-agent memory event bus.
pub const MEMORY_EVENT_BUS_CAPACITY: usize = 1024;

/// Create the default pair of per-agent process event buses.
///
/// - `event_tx` carries control/lifecycle events consumed by channels and UI.
/// - `memory_event_tx` carries memory-save telemetry consumed by the cortex.
pub fn create_process_event_buses() -> (
    tokio::sync::broadcast::Sender<ProcessEvent>,
    tokio::sync::broadcast::Sender<ProcessEvent>,
) {
    create_process_event_buses_with_capacity(CONTROL_EVENT_BUS_CAPACITY, MEMORY_EVENT_BUS_CAPACITY)
}

/// Create per-agent process event buses with explicit capacities.
pub fn create_process_event_buses_with_capacity(
    control_event_capacity: usize,
    memory_event_capacity: usize,
) -> (
    tokio::sync::broadcast::Sender<ProcessEvent>,
    tokio::sync::broadcast::Sender<ProcessEvent>,
) {
    let control_event_capacity = control_event_capacity.max(1);
    let memory_event_capacity = memory_event_capacity.max(1);
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(control_event_capacity);
    let (memory_event_tx, _memory_event_rx) =
        tokio::sync::broadcast::channel(memory_event_capacity);
    (event_tx, memory_event_tx)
}

/// Track lagged broadcast events and return the dropped count when a warning
/// should be emitted. Returns `None` when still inside the throttle window.
pub fn drain_lag_warning_count(
    lagged_since_last_warning: &mut u64,
    last_lag_warning: &mut Option<std::time::Instant>,
    newly_lagged_count: u64,
    warning_interval: std::time::Duration,
) -> Option<u64> {
    *lagged_since_last_warning = lagged_since_last_warning.saturating_add(newly_lagged_count);

    let now = std::time::Instant::now();
    let should_warn =
        last_lag_warning.is_none_or(|last| now.saturating_duration_since(last) >= warning_interval);

    if !should_warn {
        return None;
    }

    *last_lag_warning = Some(now);
    Some(std::mem::take(lagged_since_last_warning))
}

/// A message to be injected into a specific channel from outside the normal
/// inbound message flow. Used for cross-agent task completion notifications.
#[derive(Debug, Clone)]
pub struct ChannelInjection {
    /// The conversation_id of the target channel.
    pub conversation_id: String,
    /// The agent that owns the target channel.
    pub agent_id: String,
    /// The message to inject.
    pub message: InboundMessage,
}

/// Shared dependency bundle for agent processes.
#[derive(Clone)]
pub struct AgentDeps {
    pub agent_id: AgentId,
    pub memory_search: Arc<memory::MemorySearch>,
    pub llm_manager: Arc<llm::LlmManager>,
    pub mcp_manager: Arc<mcp::McpManager>,
    pub task_store: Arc<tasks::TaskStore>,
    pub cron_tool: Option<tools::CronTool>,
    pub runtime_config: Arc<config::RuntimeConfig>,
    pub event_tx: tokio::sync::broadcast::Sender<ProcessEvent>,
    pub memory_event_tx: tokio::sync::broadcast::Sender<ProcessEvent>,
    pub sqlite_pool: sqlx::SqlitePool,
    pub messaging_manager: Option<Arc<messaging::MessagingManager>>,
    pub sandbox: Arc<sandbox::Sandbox>,
    pub links: Arc<arc_swap::ArcSwap<Vec<links::AgentLink>>>,
    /// Map of all agent IDs to display names, for inter-agent message routing.
    pub agent_names: Arc<std::collections::HashMap<String, String>>,
    /// Cross-agent task store registry. Maps agent_id → TaskStore for agents
    /// reachable via links. Used by `send_agent_message` to create tasks on
    /// target agents and by the cortex to look up delegation metadata.
    /// Populated after all agents are initialized.
    pub task_store_registry:
        Arc<arc_swap::ArcSwap<std::collections::HashMap<String, Arc<tasks::TaskStore>>>>,
    pub process_control_registry: Arc<agent::process_control::ProcessControlRegistry>,
    /// Sender for injecting messages into channels from outside the normal
    /// inbound message flow (e.g. cross-agent task completion notifications).
    pub injection_tx: tokio::sync::mpsc::Sender<ChannelInjection>,
}

impl AgentDeps {
    pub fn memory_search(&self) -> &Arc<memory::MemorySearch> {
        &self.memory_search
    }
    pub fn llm_manager(&self) -> &Arc<llm::LlmManager> {
        &self.llm_manager
    }

    /// Load the current routing config snapshot.
    pub fn routing(&self) -> arc_swap::Guard<Arc<llm::RoutingConfig>> {
        self.runtime_config.routing.load()
    }
}

/// A running agent instance with all its isolated resources.
pub struct Agent {
    pub id: AgentId,
    pub config: config::ResolvedAgentConfig,
    pub db: db::Db,
    pub deps: AgentDeps,
}

/// Standard metadata keys set by all adapters.
///
/// Adapters set these alongside their platform-specific keys so consumers
/// can read them without knowing which platform originated the message.
pub mod metadata_keys {
    /// Server / workspace / chat group name (e.g. Discord guild, Slack workspace).
    pub const SERVER_NAME: &str = "server_name";
    /// Channel / conversation name within the server.
    pub const CHANNEL_NAME: &str = "channel_name";
    /// Platform message ID (stringified). Used for reply threading.
    pub const MESSAGE_ID: &str = "message_id";
    /// Reply target message ID for outbound reply threading.
    /// Set on retrigger metadata when a branch/worker completes.
    pub const REPLY_TO_MESSAGE_ID: &str = "reply_to_message_id";
    /// Quoted reply text preview from the message being replied to.
    pub const REPLY_TO_TEXT: &str = "reply_to_text";
}

/// Inbound message from any messaging platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: String,
    pub source: String,
    /// Runtime adapter key that received this message.
    ///
    /// Defaults to the platform source (`source`) when omitted so older payloads
    /// and synthetic messages remain compatible.
    #[serde(default)]
    pub adapter: Option<String>,
    pub conversation_id: String,
    pub sender_id: String,
    /// Set by the router after binding resolution. None until routed.
    pub agent_id: Option<AgentId>,
    pub content: MessageContent,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
    /// Platform-formatted author display (e.g., "Alice (<@123>)" for Discord).
    /// If None, channel falls back to sender_display_name from metadata.
    pub formatted_author: Option<String>,
}

impl InboundMessage {
    /// Runtime adapter key for routing outbound operations.
    ///
    /// Falls back to the platform source for backward compatibility.
    pub fn adapter_key(&self) -> &str {
        self.adapter
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.source)
    }

    /// Platform-scoped adapter selector used by bindings.
    ///
    /// Returns `None` for the default adapter and `Some(name)` for named
    /// adapters (e.g. `telegram:support` -> `Some("support")`).
    pub fn adapter_selector(&self) -> Option<&str> {
        let adapter_key = self.adapter_key();
        if adapter_key == self.source {
            return None;
        }

        adapter_key
            .strip_prefix(&self.source)
            .and_then(|suffix| suffix.strip_prefix(':'))
            .filter(|name| !name.is_empty())
    }
}

/// Message content variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageContent {
    Text(String),
    Media {
        text: Option<String>,
        attachments: Vec<Attachment>,
    },
    /// A platform interactive component was actioned (button click, select menu, etc.).
    ///
    /// Produced by Slack and Discord adapters. The agent can correlate the interaction
    /// back to the original message via `message_ts` (Slack) or `message_id` (Discord).
    Interaction {
        /// Unique identifier for the interactive element (`action_id` on Slack, `custom_id` on Discord).
        action_id: String,
        /// Block/container ID (Slack `block_id`, Discord component row if needed).
        block_id: Option<String>,
        /// The value submitted — button `value`, or selected option value(s).
        /// Single value for buttons, multiple for multi-select menus.
        values: Vec<String>,
        /// Human-readable label of the selected option (select menus only).
        label: Option<String>,
        /// Platform-specific message reference (`ts` on Slack, message ID on Discord).
        message_ts: Option<String>,
    },
}

impl std::fmt::Display for MessageContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageContent::Text(text) => write!(f, "{}", text),
            MessageContent::Media { text, .. } => {
                if let Some(t) = text {
                    write!(f, "{}", t)
                } else {
                    write!(f, "[media]")
                }
            }
            MessageContent::Interaction {
                action_id,
                values,
                label,
                ..
            } => {
                if let Some(l) = label {
                    write!(f, "[interaction: {} → {}]", action_id, l)
                } else if !values.is_empty() {
                    write!(f, "[interaction: {} → {:?}]", action_id, values)
                } else {
                    write!(f, "[interaction: {}]", action_id)
                }
            }
        }
    }
}

/// File attachment metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub url: String,
    pub size_bytes: Option<u64>,
    /// Optional auth header value for private URLs (e.g. Slack's `url_private`).
    /// Excluded from serialization to prevent credential leakage.
    #[serde(skip)]
    pub auth_header: Option<String>,
}

/// Outbound response to messaging platforms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundResponse {
    Text(String),
    /// Create a new thread and send a reply in it. On platforms that don't
    /// support threads this falls back to a regular text message.
    ThreadReply {
        thread_name: String,
        text: String,
    },
    /// Send a file attachment to the user.
    File {
        filename: String,
        #[serde(with = "base64_bytes")]
        data: Vec<u8>,
        mime_type: String,
        caption: Option<String>,
    },
    /// Add a reaction emoji to the triggering message.
    Reaction(String),
    /// Remove a reaction emoji from the triggering message.
    /// No-op on platforms that don't support reaction removal.
    RemoveReaction(String),
    /// Send a message visible only to the triggering user (ephemeral).
    /// Falls back to a regular `Text` message on platforms that don't support it.
    Ephemeral {
        /// The message text (mrkdwn on Slack, plain text on others).
        text: String,
        /// The user ID who should see the message. Required on Slack; ignored elsewhere.
        user_id: String,
    },
    /// Send a rich message with platform-specific formatting.
    /// - Slack: uses `blocks` if present, falls back to `text`
    /// - Discord: uses `cards`, `interactive_elements`, `poll` if present, falls back to `text`
    /// - Other adapters: use `text` as-is
    RichMessage {
        /// Plain-text fallback — always present, used for notifications and adapters
        /// that don't support rich formatting.
        text: String,
        /// Slack Block Kit blocks. Serialised as raw JSON so callers don't need to depend
        /// on slack-morphism types. The Slack adapter deserialises these at send time.
        #[serde(default)]
        blocks: Vec<serde_json::Value>,
        /// Structured cards (maps to Discord Embeds). Ignored by Slack if blocks are present.
        #[serde(default)]
        cards: Vec<Card>,
        /// Interactive elements (buttons, select menus). Maps to Discord ActionRows.
        #[serde(default)]
        interactive_elements: Vec<InteractiveElements>,
        /// An optional poll (Discord only).
        #[serde(default)]
        poll: Option<Poll>,
    },
    /// Schedule a message to be posted at a future Unix timestamp (Slack only).
    /// Other adapters send immediately as a regular `Text` message.
    ScheduledMessage {
        text: String,
        /// Unix epoch seconds when the message should be delivered.
        post_at: i64,
    },
    StreamStart,
    /// Incremental streamed text delta. Adapters that edit placeholders should
    /// accumulate deltas locally and decide when to flush visible updates.
    StreamChunk(String),
    StreamEnd,
    Status(StatusUpdate),
}

impl OutboundResponse {
    /// Ensure `RichMessage` variants have a non-empty `text` fallback.
    ///
    /// Some LLMs emit card-only payloads with empty content. This derives a
    /// readable plaintext fallback from cards so adapters that don't support
    /// rich formatting (or use `text` for notifications) always have content.
    pub fn ensure_text_fallback(&mut self) {
        if let OutboundResponse::RichMessage { text, cards, .. } = self
            && text.trim().is_empty()
        {
            let derived = Self::text_from_cards(cards);
            if !derived.trim().is_empty() {
                *text = derived;
            }
        }
    }

    /// Derive a plaintext representation from a slice of [`Card`]s.
    ///
    /// Used as a fallback when the LLM provides cards but no text content.
    /// Adapters can call this directly when they destructure `RichMessage`
    /// and need a text fallback without reconstructing the enum.
    pub fn text_from_cards(cards: &[Card]) -> String {
        let mut sections = Vec::new();
        for card in cards {
            let mut lines = Vec::new();
            if let Some(title) = &card.title
                && !title.trim().is_empty()
            {
                lines.push(title.trim().to_string());
            }
            if let Some(description) = &card.description
                && !description.trim().is_empty()
            {
                lines.push(description.trim().to_string());
            }
            for field in &card.fields {
                let name = field.name.trim();
                let value = field.value.trim();
                if !name.is_empty() || !value.is_empty() {
                    lines.push(format!("{name}\n{value}").trim().to_string());
                }
            }
            if let Some(footer) = &card.footer
                && !footer.trim().is_empty()
            {
                lines.push(footer.trim().to_string());
            }
            if !lines.is_empty() {
                sections.push(lines.join("\n\n"));
            }
        }
        sections.join("\n\n")
    }
}

/// A generic rich-formatted card (maps to Embeds in Discord).
#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
pub struct Card {
    pub title: Option<String>,
    pub description: Option<String>,
    pub color: Option<u32>,
    pub url: Option<String>,
    #[serde(default)]
    pub fields: Vec<CardField>,
    pub footer: Option<String>,
}

/// A field within a generic Card.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CardField {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub inline: bool,
}

/// Container for interactive elements (maps to ActionRows in Discord).
/// In Discord, an action row can contain either buttons or a single select menu.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InteractiveElements {
    Buttons { buttons: Vec<Button> },
    Select { select: SelectMenu },
}

/// A generic interactive button.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Button {
    pub label: String,
    pub custom_id: Option<String>,
    pub style: ButtonStyle,
    pub url: Option<String>,
}

/// Styles for interactive buttons.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Success,
    Danger,
    Link,
}

/// A select menu option.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    pub description: Option<String>,
    pub emoji: Option<String>,
}

/// A generic select menu component.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SelectMenu {
    pub custom_id: String,
    pub options: Vec<SelectOption>,
    pub placeholder: Option<String>,
}

/// A generic poll definition.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Poll {
    pub question: String,
    pub answers: Vec<String>,
    #[serde(default)]
    pub allow_multiselect: bool,
    #[serde(default = "default_poll_duration")]
    pub duration_hours: u32,
}

fn default_poll_duration() -> u32 {
    24
}

/// Serde helper for encoding `Vec<u8>` as base64 in JSON.
mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}

/// Status updates for messaging platforms.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusUpdate {
    Thinking,
    /// Cancel the typing indicator (e.g. when the skip tool fires).
    StopTyping,
    ToolStarted {
        tool_name: String,
    },
    ToolCompleted {
        tool_name: String,
    },
    BranchStarted {
        branch_id: BranchId,
    },
    WorkerStarted {
        worker_id: WorkerId,
        task: String,
    },
    WorkerCompleted {
        worker_id: WorkerId,
        result: String,
    },
}
