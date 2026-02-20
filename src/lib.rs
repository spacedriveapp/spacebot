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
pub mod llm;
pub mod memory;
pub mod messaging;
pub mod opencode;
pub mod prompts;
pub mod secrets;
pub mod settings;
pub mod skills;
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

/// Events sent between processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessEvent {
    BranchStarted {
        agent_id: AgentId,
        branch_id: BranchId,
        channel_id: ChannelId,
        description: String,
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
    },
    WorkerStatus {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        status: String,
    },
    WorkerComplete {
        agent_id: AgentId,
        worker_id: WorkerId,
        channel_id: Option<ChannelId>,
        result: String,
        notify: bool,
    },
    ToolStarted {
        agent_id: AgentId,
        process_id: ProcessId,
        channel_id: Option<ChannelId>,
        tool_name: String,
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
}

/// Shared dependency bundle for agent processes.
#[derive(Clone)]
pub struct AgentDeps {
    pub agent_id: AgentId,
    pub memory_search: Arc<memory::MemorySearch>,
    pub llm_manager: Arc<llm::LlmManager>,
    pub cron_tool: Option<tools::CronTool>,
    pub runtime_config: Arc<config::RuntimeConfig>,
    pub event_tx: tokio::sync::broadcast::Sender<ProcessEvent>,
    pub sqlite_pool: sqlx::SqlitePool,
    pub messaging_manager: Option<Arc<messaging::MessagingManager>>,
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

/// Inbound message from any messaging platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: String,
    pub source: String,
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
    StreamChunk(String),
    StreamEnd,
    Status(StatusUpdate),
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
