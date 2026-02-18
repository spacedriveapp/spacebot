//! Spacebot: A Rust agentic system where every LLM process has a dedicated role.

pub mod agent;
pub mod api;
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
    BranchFailed {
        agent_id: AgentId,
        branch_id: BranchId,
        channel_id: ChannelId,
        error: String,
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
    StreamStart,
    StreamChunk(String),
    StreamEnd,
    Status(StatusUpdate),
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
