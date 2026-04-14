//! Shared state for the HTTP API.

use crate::agent::channel::ChannelState;
use crate::agent::cortex_chat::CortexChatSession;
use crate::agent::status::StatusBlock;
use crate::config::{
    Binding, DefaultsConfig, DiscordPermissions, RuntimeConfig, SignalPermissions, SlackPermissions,
};
use crate::conversation::worker_transcript::{ActionContent, ToolResultStatus, TranscriptStep};
use crate::cron::{CronStore, Scheduler};
use crate::llm::LlmManager;
use crate::mcp::McpManager;
use crate::memory::{EmbeddingModel, MemorySearch};
use crate::messaging::MessagingManager;
use crate::messaging::portal::PortalAdapter;
use crate::notifications::{NewNotification, Notification, NotificationStore};
use crate::projects::ProjectStore;
use crate::prompts::PromptEngine;
use crate::tasks::TaskStore;
use crate::update::SharedUpdateStatus;
use crate::{ProcessEvent, ProcessId};

use arc_swap::ArcSwap;
use serde::Serialize;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, broadcast, mpsc};

const MAX_LIVE_TOOL_OUTPUT_BYTES: usize = 50_000;
const LIVE_TOOL_OUTPUT_REDACTION: &str = "[REDACTED:potential-secret]";
const MAX_COMPLETED_WORKER_TOMBSTONES: usize = 4_096;

fn worker_tool_result_status(result: &str) -> ToolResultStatus {
    if result.contains("\"waiting_for_input\":true")
        || result.contains("\"waiting_for_input\": true")
        || result.contains("Command appears to be waiting for interactive input.")
    {
        ToolResultStatus::WaitingForInput
    } else {
        ToolResultStatus::Final
    }
}

fn append_live_output(output: &mut Option<String>, line: &str) {
    let mut combined = output.take().unwrap_or_default();
    combined.push_str(line);
    combined.push('\n');

    if combined.len() > MAX_LIVE_TOOL_OUTPUT_BYTES {
        let mut start = combined.len().saturating_sub(MAX_LIVE_TOOL_OUTPUT_BYTES);
        while start < combined.len() && !combined.is_char_boundary(start) {
            start += 1;
        }
        combined.drain(..start);
    }
    *output = Some(combined);
}

fn upsert_pending_tool_output(
    steps: &mut Vec<TranscriptStep>,
    call_id: String,
    tool_name: String,
    line: &str,
) {
    if let Some(step) = steps.iter_mut().find(|step| {
        matches!(
            step,
            TranscriptStep::ToolResult {
                call_id: existing_call_id,
                ..
            } if existing_call_id == &call_id
        )
    }) && let TranscriptStep::ToolResult {
        live_output,
        status,
        ..
    } = step
    {
        if !matches!(*status, ToolResultStatus::Pending) {
            return;
        }
        append_live_output(live_output, line);
        *status = ToolResultStatus::Pending;
        return;
    }

    steps.push(TranscriptStep::ToolResult {
        call_id,
        name: tool_name,
        text: String::new(),
        live_output: Some(format!("{line}\n")),
        status: ToolResultStatus::Pending,
    });
}

fn push_live_tool_call(
    steps: &mut Vec<TranscriptStep>,
    call_id: String,
    tool_name: String,
    args: String,
) {
    let pending_output_index = steps
        .iter()
        .position(|step| {
            matches!(
                step,
                TranscriptStep::ToolResult {
                    call_id: existing_call_id,
                    ..
                } if existing_call_id == &call_id
            )
        })
        .or_else(|| {
            steps.iter().position(|step| {
                matches!(
                    step,
                    TranscriptStep::ToolResult {
                        name,
                        text,
                        status: ToolResultStatus::Pending,
                        ..
                    } if name == &tool_name && text.is_empty()
                )
            })
        });

    let pending_output = pending_output_index.map(|index| steps.remove(index));

    steps.push(TranscriptStep::Action {
        content: vec![ActionContent::ToolCall {
            id: call_id.clone(),
            name: tool_name.clone(),
            args,
        }],
    });

    if let Some(TranscriptStep::ToolResult {
        text,
        live_output,
        status,
        ..
    }) = pending_output
    {
        steps.push(TranscriptStep::ToolResult {
            call_id,
            name: tool_name,
            text,
            live_output,
            status,
        });
    }
}

fn upsert_final_tool_result(
    steps: &mut Vec<TranscriptStep>,
    call_id: String,
    tool_name: String,
    result: String,
) {
    let status = worker_tool_result_status(&result);
    if let Some(step) = steps.iter_mut().find(|step| {
        matches!(
            step,
            TranscriptStep::ToolResult {
                call_id: existing_call_id,
                ..
            } if existing_call_id == &call_id
        )
    }) && let TranscriptStep::ToolResult {
        name,
        text,
        live_output,
        status: existing_status,
        ..
    } = step
    {
        *name = tool_name;
        *text = result;
        *live_output = None;
        *existing_status = status;
        return;
    }

    steps.push(TranscriptStep::ToolResult {
        call_id,
        name: tool_name,
        text: result,
        live_output: None,
        status,
    });
}

fn sanitize_live_tool_output_line(line: &str) -> String {
    let scrubbed = crate::secrets::scrub::scrub_leaks(line);
    if crate::secrets::scrub::scan_for_leaks(&scrubbed).is_some() {
        return LIVE_TOOL_OUTPUT_REDACTION.to_string();
    }
    scrubbed
}

/// Summary of an agent's configuration, exposed via the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AgentInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gradient_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gradient_end: Option<String>,
    pub workspace: String,
    pub context_window: usize,
    pub max_turns: usize,
    pub max_concurrent_branches: usize,
    pub max_concurrent_workers: usize,
}

/// State shared across all API handlers.
pub struct ApiState {
    pub started_at: Instant,
    pub auth_token: Option<String>,
    /// Aggregated event stream from all agents. SSE clients subscribe here.
    pub event_tx: broadcast::Sender<ApiEvent>,
    /// Per-agent SQLite pools for querying channel/conversation data.
    pub agent_pools: arc_swap::ArcSwap<HashMap<String, sqlx::SqlitePool>>,
    /// Per-agent config summaries for the agents list endpoint.
    pub agent_configs: arc_swap::ArcSwap<Vec<AgentInfo>>,
    /// Per-agent memory search instances for the memories API.
    pub memory_searches: arc_swap::ArcSwap<HashMap<String, Arc<MemorySearch>>>,
    /// Live status blocks for active channels, keyed by channel_id.
    pub channel_status_blocks: RwLock<HashMap<String, Arc<tokio::sync::RwLock<StatusBlock>>>>,
    /// Live channel states for active channels, keyed by channel_id.
    /// Used by the cancel API to abort workers and branches.
    pub channel_states: RwLock<HashMap<String, ChannelState>>,
    /// Per-agent cortex chat sessions.
    pub cortex_chat_sessions: arc_swap::ArcSwap<HashMap<String, Arc<CortexChatSession>>>,
    /// Per-agent workspace paths for file tool access.
    pub agent_workspaces: arc_swap::ArcSwap<HashMap<String, PathBuf>>,
    /// Per-agent identity directories (agent root). Identity files live here,
    /// outside the workspace sandbox boundary.
    pub agent_identity_dirs: arc_swap::ArcSwap<HashMap<String, PathBuf>>,
    /// Per-agent data directories (for avatars, logs, databases).
    pub agent_data_dirs: arc_swap::ArcSwap<HashMap<String, PathBuf>>,
    /// Path to the instance config.toml file.
    pub config_path: RwLock<PathBuf>,
    /// Guards read-modify-write cycles on config.toml to prevent concurrent
    /// modifications from clobbering each other.
    pub config_write_mutex: tokio::sync::Mutex<()>,
    /// Per-agent cron stores for cron job CRUD operations.
    pub cron_stores: arc_swap::ArcSwap<HashMap<String, Arc<CronStore>>>,
    /// Per-agent cron schedulers for job timer management.
    pub cron_schedulers: arc_swap::ArcSwap<HashMap<String, Arc<Scheduler>>>,
    /// Instance-level global task store shared across all agents.
    pub task_store: ArcSwap<Option<Arc<TaskStore>>>,
    /// Instance-wide wiki knowledge base.
    pub wiki_store: ArcSwap<Option<Arc<crate::wiki::WikiStore>>>,
    /// Instance-level shared project store.
    pub project_store: ArcSwap<Option<Arc<ProjectStore>>>,
    /// Instance-level notification store for the dashboard inbox.
    pub notification_store: ArcSwap<Option<Arc<NotificationStore>>>,
    /// Per-agent RuntimeConfig for reading live hot-reloaded configuration.
    pub runtime_configs: ArcSwap<HashMap<String, Arc<RuntimeConfig>>>,
    /// Per-agent MCP managers for status and reconnect APIs.
    pub mcp_managers: ArcSwap<HashMap<String, Arc<McpManager>>>,
    /// Per-agent sandbox instances for process containment.
    pub sandboxes: ArcSwap<HashMap<String, Arc<crate::sandbox::Sandbox>>>,
    /// Instance-level secrets store (shared across all agents).
    pub secrets_store: ArcSwap<Option<Arc<crate::secrets::store::SecretsStore>>>,
    /// Shared reference to the Discord permissions ArcSwap (same instance used by the adapter and file watcher).
    pub discord_permissions: RwLock<Option<Arc<ArcSwap<DiscordPermissions>>>>,
    /// Shared reference to the Slack permissions ArcSwap (same instance used by the adapter and file watcher).
    pub slack_permissions: RwLock<Option<Arc<ArcSwap<SlackPermissions>>>>,
    /// Shared reference to the Signal permissions ArcSwap (same instance used by the adapter and file watcher).
    pub signal_permissions: RwLock<Option<Arc<ArcSwap<SignalPermissions>>>>,
    /// Shared reference to the bindings ArcSwap (same instance used by the main loop and file watcher).
    pub bindings: RwLock<Option<Arc<ArcSwap<Vec<Binding>>>>>,
    /// Shared messaging manager for runtime adapter addition.
    pub messaging_manager: RwLock<Option<Arc<MessagingManager>>>,
    /// Sender to signal the main event loop that provider keys have been configured.
    pub provider_setup_tx: mpsc::Sender<crate::ProviderSetupEvent>,
    /// Shared update status, populated by the background update checker.
    pub update_status: SharedUpdateStatus,
    /// Instance directory path for accessing instance-level skills.
    pub instance_dir: ArcSwap<PathBuf>,
    /// Shared LLM manager for agent creation.
    pub llm_manager: RwLock<Option<Arc<LlmManager>>>,
    /// Shared embedding model for agent creation.
    pub embedding_model: RwLock<Option<Arc<EmbeddingModel>>>,
    /// Prompt engine snapshot for agent creation.
    pub prompt_engine: RwLock<Option<PromptEngine>>,
    /// Instance-level defaults for resolving new agent configs.
    pub defaults_config: RwLock<Option<DefaultsConfig>>,
    /// Sender to register newly created agents with the main event loop.
    pub agent_tx: mpsc::Sender<crate::Agent>,
    /// Sender to remove agents from the main event loop.
    pub agent_remove_tx: mpsc::Sender<String>,
    /// Shared portal adapter for session management from API handlers.
    pub portal_adapter: ArcSwap<Option<Arc<PortalAdapter>>>,
    /// Sender for cross-agent message injection.
    pub injection_tx: mpsc::Sender<crate::ChannelInjection>,
    /// Instance-level agent links for the communication graph.
    pub agent_links: ArcSwap<Vec<crate::links::AgentLink>>,
    /// Visual agent groups for the topology UI.
    pub agent_groups: ArcSwap<Vec<crate::config::GroupDef>>,
    /// Org-level humans for the topology UI.
    pub agent_humans: ArcSwap<Vec<crate::config::HumanDef>>,
    /// Live transcript cache for running workers. Accumulates `TranscriptStep`s
    /// from `ToolStarted`/`ToolCompleted` events so that page refreshes can
    /// recover the transcript without waiting for the worker to complete.
    /// Keyed by worker_id, cleared on worker completion.
    pub live_worker_transcripts: Arc<RwLock<HashMap<String, Vec<TranscriptStep>>>>,
    /// Bounded tombstone set of recently completed worker IDs.
    ///
    /// Prevents late/lagged `ToolOutput` events from recreating transcript
    /// entries after `WorkerComplete` has already cleared them.
    pub completed_worker_tombstones: Arc<RwLock<HashSet<String>>>,
    /// In-memory cache of tool calls for running channel turns (direct mode).
    /// Keyed by channel_id, drained when the bot message is persisted.
    pub live_channel_tool_calls: Arc<RwLock<HashMap<String, Vec<ChannelToolCallEntry>>>>,
    /// Serializes SSH daemon enable/disable transitions to prevent races
    /// between overlapping toggle requests.
    pub ssh_mutex: tokio::sync::Mutex<()>,
}

/// A single channel-level tool call accumulated in memory during a direct-mode turn.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChannelToolCallEntry {
    pub id: String,
    pub tool_name: String,
    pub args: String,
    pub result: Option<String>,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
}

/// Events sent to SSE clients. Wraps ProcessEvents with agent context.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiEvent {
    /// An inbound message from a user.
    InboundMessage {
        agent_id: String,
        channel_id: String,
        sender_name: Option<String>,
        sender_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::agent::channel_attachments::SavedAttachmentMeta>,
    },
    /// An outbound message sent by the bot.
    OutboundMessage {
        agent_id: String,
        channel_id: String,
        text: String,
    },
    /// Typing indicator state change.
    TypingState {
        agent_id: String,
        channel_id: String,
        is_typing: bool,
    },
    /// Streaming text delta for an outbound assistant message.
    OutboundMessageDelta {
        agent_id: String,
        channel_id: String,
        text_delta: String,
        aggregated_text: String,
    },
    /// A worker was started.
    WorkerStarted {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
        task: String,
        worker_type: String,
        interactive: bool,
    },
    /// A worker's status changed.
    WorkerStatusUpdate {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
        status: String,
    },
    /// A worker entered the idle state (waiting for follow-up input).
    WorkerIdle {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
    },
    /// A worker completed.
    WorkerCompleted {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
        result: String,
        success: bool,
    },
    /// A branch was started.
    BranchStarted {
        agent_id: String,
        channel_id: String,
        branch_id: String,
        description: String,
    },
    /// A branch completed with a conclusion.
    BranchCompleted {
        agent_id: String,
        channel_id: String,
        branch_id: String,
        conclusion: String,
    },
    /// A tool call started on a process.
    ToolStarted {
        agent_id: String,
        channel_id: Option<String>,
        process_type: String,
        process_id: String,
        call_id: String,
        tool_name: String,
        args: String,
    },
    /// A tool call completed on a process.
    ToolCompleted {
        agent_id: String,
        channel_id: Option<String>,
        process_type: String,
        process_id: String,
        call_id: String,
        tool_name: String,
        result: String,
    },
    /// Configuration was reloaded (skills, identity, etc.).
    ConfigReloaded,
    /// A message was sent from one agent to another.
    AgentMessageSent {
        from_agent_id: String,
        to_agent_id: String,
        link_id: String,
        channel_id: String,
    },
    /// A message was received by an agent from another agent.
    AgentMessageReceived {
        from_agent_id: String,
        to_agent_id: String,
        link_id: String,
        channel_id: String,
    },
    /// A task was created, updated, or deleted.
    TaskUpdated {
        agent_id: String,
        task_number: i64,
        status: String,
        /// "created", "updated", or "deleted".
        action: String,
    },
    /// A finalized content part from an OpenCode worker session.
    OpenCodePartUpdated {
        agent_id: String,
        worker_id: String,
        part: crate::opencode::types::OpenCodePart,
    },
    /// A worker emitted text content (model reasoning between tool calls).
    WorkerText {
        agent_id: String,
        worker_id: String,
        text: String,
    },
    /// A cortex chat auto-triggered response (e.g. after a worker result was
    /// delivered). The frontend appends this as a new assistant message.
    CortexChatMessage {
        agent_id: String,
        thread_id: String,
        content: String,
        tool_calls: Option<Vec<crate::agent::cortex_chat::CortexChatToolCall>>,
    },
    /// A new notification was created and persisted.
    NotificationCreated { notification: Notification },
    /// A notification was updated (read or dismissed) — for cross-tab sync.
    NotificationUpdated {
        id: String,
        read: bool,
        dismissed: bool,
    },
    /// A line of live output from a running tool (e.g. shell stdout/stderr).
    /// Ephemeral — for frontend live display only. The full output is in ToolCompleted.
    ToolOutput {
        agent_id: String,
        channel_id: Option<String>,
        process_type: String,
        process_id: String,
        /// Stable identifier matching the tool_call that initiated this stream.
        call_id: String,
        tool_name: String,
        line: String,
        stream: String,
    },
}

impl ApiState {
    pub fn new_with_provider_sender(
        provider_setup_tx: mpsc::Sender<crate::ProviderSetupEvent>,
        agent_tx: mpsc::Sender<crate::Agent>,
        agent_remove_tx: mpsc::Sender<String>,
        injection_tx: mpsc::Sender<crate::ChannelInjection>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(512);
        Self {
            started_at: Instant::now(),
            auth_token: None,
            event_tx,
            agent_pools: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            agent_configs: arc_swap::ArcSwap::from_pointee(Vec::new()),
            memory_searches: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            channel_status_blocks: RwLock::new(HashMap::new()),
            channel_states: RwLock::new(HashMap::new()),
            cortex_chat_sessions: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            agent_workspaces: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            agent_identity_dirs: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            agent_data_dirs: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            config_path: RwLock::new(PathBuf::new()),
            config_write_mutex: tokio::sync::Mutex::new(()),
            cron_stores: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            cron_schedulers: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            task_store: ArcSwap::from_pointee(None),
            wiki_store: ArcSwap::from_pointee(None),
            project_store: ArcSwap::from_pointee(None),
            notification_store: ArcSwap::from_pointee(None),
            runtime_configs: ArcSwap::from_pointee(HashMap::new()),
            mcp_managers: ArcSwap::from_pointee(HashMap::new()),
            sandboxes: ArcSwap::from_pointee(HashMap::new()),
            secrets_store: ArcSwap::from_pointee(None),
            discord_permissions: RwLock::new(None),
            slack_permissions: RwLock::new(None),
            signal_permissions: RwLock::new(None),
            bindings: RwLock::new(None),
            messaging_manager: RwLock::new(None),
            provider_setup_tx,
            update_status: crate::update::new_shared_status(),
            instance_dir: ArcSwap::from_pointee(PathBuf::new()),
            llm_manager: RwLock::new(None),
            embedding_model: RwLock::new(None),
            prompt_engine: RwLock::new(None),
            defaults_config: RwLock::new(None),
            agent_tx,
            agent_remove_tx,
            injection_tx,
            portal_adapter: ArcSwap::from_pointee(None),
            agent_links: ArcSwap::from_pointee(Vec::new()),
            agent_groups: ArcSwap::from_pointee(Vec::new()),
            agent_humans: ArcSwap::from_pointee(Vec::new()),
            live_worker_transcripts: Arc::new(RwLock::new(HashMap::new())),
            completed_worker_tombstones: Arc::new(RwLock::new(HashSet::new())),
            live_channel_tool_calls: Arc::new(RwLock::new(HashMap::new())),
            ssh_mutex: tokio::sync::Mutex::new(()),
        }
    }

    /// Register a channel's status block so the API can read snapshots.
    pub async fn register_channel_status(
        &self,
        channel_id: String,
        status_block: Arc<tokio::sync::RwLock<StatusBlock>>,
    ) {
        self.channel_status_blocks
            .write()
            .await
            .insert(channel_id, status_block);
    }

    /// Remove a channel's status block when it's dropped.
    pub async fn unregister_channel_status(&self, channel_id: &str) {
        self.channel_status_blocks.write().await.remove(channel_id);
    }

    /// Register a channel's state for API-driven cancellation.
    pub async fn register_channel_state(&self, channel_id: String, state: ChannelState) {
        self.channel_states.write().await.insert(channel_id, state);
    }

    /// Remove a channel's state when it's dropped.
    pub async fn unregister_channel_state(&self, channel_id: &str) {
        self.channel_states.write().await.remove(channel_id);
    }

    /// Retrieve the live transcript cache for a running worker.
    ///
    /// Returns `Some` with the accumulated transcript steps if the worker is
    /// currently running and has emitted tool calls. Returns `None` if no
    /// cached transcript exists (worker completed or never started).
    pub async fn get_live_transcript(&self, worker_id: &str) -> Option<Vec<TranscriptStep>> {
        let guard = self.live_worker_transcripts.read().await;
        guard.get(worker_id).cloned()
    }

    /// Register an agent's event stream. Spawns a task that forwards
    /// ProcessEvents into the aggregated API event stream.
    pub fn register_agent_events(
        &self,
        agent_id: String,
        mut agent_event_rx: broadcast::Receiver<ProcessEvent>,
    ) {
        let api_tx = self.event_tx.clone();
        let live_transcripts = self.live_worker_transcripts.clone();
        let completed_worker_tombstones = self.completed_worker_tombstones.clone();
        let live_channel_tools = self.live_channel_tool_calls.clone();
        // Snapshot the notification store at registration time. It is set once
        // at startup before any agents register, so the snapshot is always valid.
        let _notif_store_snap = self.notification_store.load_full();
        tokio::spawn(async move {
            loop {
                match agent_event_rx.recv().await {
                    Ok(event) => {
                        // Translate ProcessEvents into typed ApiEvents
                        match &event {
                            ProcessEvent::WorkerStarted {
                                worker_id,
                                channel_id,
                                task,
                                worker_type,
                                interactive,
                                ..
                            } => {
                                let worker_key = worker_id.to_string();
                                let mut completed_guard = completed_worker_tombstones.write().await;
                                completed_guard.remove(&worker_key);
                                live_transcripts
                                    .write()
                                    .await
                                    .entry(worker_key)
                                    .or_default();
                                api_tx
                                    .send(ApiEvent::WorkerStarted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        worker_id: worker_id.to_string(),
                                        task: task.clone(),
                                        worker_type: worker_type.clone(),
                                        interactive: *interactive,
                                    })
                                    .ok();
                            }
                            ProcessEvent::BranchStarted {
                                branch_id,
                                channel_id,
                                description,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::BranchStarted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.to_string(),
                                        branch_id: branch_id.to_string(),
                                        description: description.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::WorkerStatus {
                                worker_id,
                                channel_id,
                                status,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::WorkerStatusUpdate {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        worker_id: worker_id.to_string(),
                                        status: status.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::WorkerIdle {
                                worker_id,
                                channel_id,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::WorkerIdle {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        worker_id: worker_id.to_string(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::WorkerComplete {
                                worker_id,
                                channel_id,
                                result,
                                success,
                                ..
                            } => {
                                let worker_key = worker_id.to_string();
                                let mut completed_guard = completed_worker_tombstones.write().await;
                                if completed_guard.len() >= MAX_COMPLETED_WORKER_TOMBSTONES {
                                    completed_guard.clear();
                                }
                                completed_guard.insert(worker_key.clone());
                                live_transcripts.write().await.remove(&worker_key);
                                api_tx
                                    .send(ApiEvent::WorkerCompleted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        worker_id: worker_id.to_string(),
                                        result: result.clone(),
                                        success: *success,
                                    })
                                    .ok();
                                // TODO: re-enable WorkerFailed notifications once action_url points somewhere useful
                                // if !success {
                                //     if let Some(ref store) = *notif_store_snap {
                                //         let store = store.clone();
                                //         let event_tx = api_tx.clone();
                                //         let agent_id_n = agent_id.clone();
                                //         let worker_id_n = worker_id.to_string();
                                //         let body = if result.is_empty() {
                                //             None
                                //         } else {
                                //             Some(result.chars().take(300).collect::<String>())
                                //         };
                                //         tokio::spawn(async move {
                                //             let n = NewNotification {
                                //                 kind: NotificationKind::WorkerFailed,
                                //                 severity: NotificationSeverity::Error,
                                //                 title: format!("Worker failed: {worker_id_n}"),
                                //                 body,
                                //                 agent_id: Some(agent_id_n),
                                //                 related_entity_type: Some("worker".to_string()),
                                //                 related_entity_id: Some(worker_id_n),
                                //                 action_url: None,
                                //                 metadata: None,
                                //             };
                                //             match store.insert(n).await {
                                //                 Ok(Some(notification)) => {
                                //                     event_tx
                                //                         .send(ApiEvent::NotificationCreated {
                                //                             notification,
                                //                         })
                                //                         .ok();
                                //                 }
                                //                 Ok(None) => {}
                                //                 Err(error) => {
                                //                     tracing::warn!(
                                //                         %error,
                                //                         "failed to insert worker failure notification"
                                //                     );
                                //                 }
                                //             }
                                //         });
                                //     }
                                // }
                            }
                            ProcessEvent::BranchResult {
                                branch_id,
                                channel_id,
                                conclusion,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::BranchCompleted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.to_string(),
                                        branch_id: branch_id.to_string(),
                                        conclusion: conclusion.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::ToolStarted {
                                process_id,
                                channel_id,
                                call_id,
                                tool_name,
                                args,
                                ..
                            } => {
                                let (process_type, id_str) = process_id_info(process_id);
                                // Accumulate tool call into live transcript for workers.
                                if let ProcessId::Worker(worker_id) = process_id {
                                    let worker_key = worker_id.to_string();
                                    let mut guard = live_transcripts.write().await;
                                    if let Some(steps) = guard.get_mut(&worker_key) {
                                        push_live_tool_call(
                                            steps,
                                            call_id.clone(),
                                            tool_name.clone(),
                                            args.clone(),
                                        );
                                    }
                                }
                                // Accumulate channel-level tool calls in memory,
                                // skipping conversation/routing tools.
                                if let ProcessId::Channel(ch_id) = process_id {
                                    if is_hidden_channel_tool(tool_name) {
                                        // skip
                                    } else {
                                        let entry = ChannelToolCallEntry {
                                            id: call_id.clone(),
                                            tool_name: tool_name.clone(),
                                            args: args.clone(),
                                            result: None,
                                            status: "running".into(),
                                            started_at: chrono::Utc::now().to_rfc3339(),
                                            completed_at: None,
                                        };
                                        let mut guard = live_channel_tools.write().await;
                                        guard.entry(ch_id.to_string()).or_default().push(entry);
                                    }
                                }
                                api_tx
                                    .send(ApiEvent::ToolStarted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        process_type,
                                        process_id: id_str,
                                        call_id: call_id.clone(),
                                        tool_name: tool_name.clone(),
                                        args: args.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::ToolCompleted {
                                process_id,
                                channel_id,
                                call_id,
                                tool_name,
                                result,
                                ..
                            } => {
                                let (process_type, id_str) = process_id_info(process_id);
                                // Accumulate tool result into live transcript for workers.
                                if let ProcessId::Worker(worker_id) = process_id {
                                    let worker_key = worker_id.to_string();
                                    let mut guard = live_transcripts.write().await;
                                    if let Some(steps) = guard.get_mut(&worker_key) {
                                        upsert_final_tool_result(
                                            steps,
                                            call_id.clone(),
                                            tool_name.clone(),
                                            result.clone(),
                                        );
                                    }
                                }
                                // Complete channel-level tool call in memory (FIFO).
                                if let ProcessId::Channel(ch_id) = process_id {
                                    let mut guard = live_channel_tools.write().await;
                                    if let Some(calls) = guard.get_mut(&ch_id.to_string())
                                        && let Some(entry) = calls.iter_mut().find(|c| {
                                            c.id == call_id.as_str() && c.status == "running"
                                        })
                                    {
                                        entry.result = Some(result.clone());
                                        entry.status = "completed".into();
                                        entry.completed_at = Some(chrono::Utc::now().to_rfc3339());
                                    }
                                }
                                api_tx
                                    .send(ApiEvent::ToolCompleted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        process_type,
                                        process_id: id_str,
                                        call_id: call_id.clone(),
                                        tool_name: tool_name.clone(),
                                        result: result.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::AgentMessageSent {
                                from_agent_id,
                                to_agent_id,
                                link_id,
                                channel_id,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::AgentMessageSent {
                                        from_agent_id: from_agent_id.to_string(),
                                        to_agent_id: to_agent_id.to_string(),
                                        link_id: link_id.clone(),
                                        channel_id: channel_id.to_string(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::AgentMessageReceived {
                                from_agent_id,
                                to_agent_id,
                                link_id,
                                channel_id,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::AgentMessageReceived {
                                        from_agent_id: from_agent_id.to_string(),
                                        to_agent_id: to_agent_id.to_string(),
                                        link_id: link_id.clone(),
                                        channel_id: channel_id.to_string(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::TaskUpdated {
                                task_number,
                                status,
                                action,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::TaskUpdated {
                                        agent_id: agent_id.clone(),
                                        task_number: *task_number,
                                        status: status.clone(),
                                        action: action.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::TextDelta {
                                channel_id: Some(channel_id),
                                text_delta,
                                aggregated_text,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::OutboundMessageDelta {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.to_string(),
                                        text_delta: text_delta.clone(),
                                        aggregated_text: aggregated_text.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::OpenCodePartUpdated {
                                worker_id, part, ..
                            } => {
                                api_tx
                                    .send(ApiEvent::OpenCodePartUpdated {
                                        agent_id: agent_id.clone(),
                                        worker_id: worker_id.to_string(),
                                        part: part.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::WorkerText {
                                worker_id, text, ..
                            } => {
                                // Accumulate into live transcript cache
                                let step = TranscriptStep::Action {
                                    content: vec![ActionContent::Text { text: text.clone() }],
                                };
                                let mut guard = live_transcripts.write().await;
                                if let Some(steps) = guard.get_mut(&worker_id.to_string()) {
                                    steps.push(step);
                                }
                                drop(guard);

                                api_tx
                                    .send(ApiEvent::WorkerText {
                                        agent_id: agent_id.clone(),
                                        worker_id: worker_id.to_string(),
                                        text: text.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::CortexChatUpdate {
                                thread_id,
                                content,
                                tool_calls_json,
                                ..
                            } => {
                                let tool_calls: Option<
                                    Vec<crate::agent::cortex_chat::CortexChatToolCall>,
                                > = tool_calls_json
                                    .as_deref()
                                    .and_then(|json| serde_json::from_str(json).ok());
                                api_tx
                                    .send(ApiEvent::CortexChatMessage {
                                        agent_id: agent_id.clone(),
                                        thread_id: thread_id.clone(),
                                        content: content.clone(),
                                        tool_calls,
                                    })
                                    .ok();
                            }
                            _ => {}
                        }
                    }
                    Err(error) => {
                        match crate::classify_broadcast_recv_result::<crate::ProcessEvent>(Err(
                            error,
                        )) {
                            crate::BroadcastRecvResult::Lagged(count) => {
                                tracing::debug!(
                                    agent_id = %agent_id,
                                    count,
                                    "API event forwarder lagged, skipped events"
                                );
                            }
                            crate::BroadcastRecvResult::Closed => break,
                            crate::BroadcastRecvResult::Event(_) => unreachable!(
                                "classifying an Err recv result should never produce Event"
                            ),
                        }
                    }
                }
            }
        });
    }

    /// Register an agent's tool output stream. Spawns a task that forwards
    /// ToolOutput ProcessEvents into the aggregated API event stream and
    /// accumulates streaming output into the live transcript cache so it's
    /// available on refresh.
    pub fn register_tool_output_stream(
        &self,
        agent_id: String,
        mut tool_output_rx: broadcast::Receiver<ProcessEvent>,
    ) {
        let api_tx = self.event_tx.clone();
        let live_transcripts = self.live_worker_transcripts.clone();
        let completed_worker_tombstones = self.completed_worker_tombstones.clone();
        tokio::spawn(async move {
            loop {
                match tool_output_rx.recv().await {
                    Ok(event) => {
                        if let ProcessEvent::ToolOutput {
                            process_id,
                            channel_id,
                            call_id,
                            tool_name,
                            line,
                            stream,
                            ..
                        } = &event
                        {
                            let sanitized_line = sanitize_live_tool_output_line(line);
                            let (process_type, id_str) = process_id_info(process_id);
                            // Accumulate streaming output into live transcript for workers.
                            if let ProcessId::Worker(worker_id) = process_id {
                                let worker_key = worker_id.to_string();
                                let completed_guard = completed_worker_tombstones.write().await;
                                if !completed_guard.contains(&worker_key) {
                                    let mut guard = live_transcripts.write().await;
                                    let steps = guard.entry(worker_key).or_default();
                                    upsert_pending_tool_output(
                                        steps,
                                        call_id.clone(),
                                        tool_name.clone(),
                                        &sanitized_line,
                                    );
                                }
                            }
                            api_tx
                                .send(ApiEvent::ToolOutput {
                                    agent_id: agent_id.clone(),
                                    channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                    process_type,
                                    process_id: id_str,
                                    call_id: call_id.clone(),
                                    tool_name: tool_name.clone(),
                                    line: sanitized_line,
                                    stream: stream.clone(),
                                })
                                .ok();
                        }
                    }
                    Err(error) => {
                        match crate::classify_broadcast_recv_result::<crate::ProcessEvent>(Err(
                            error,
                        )) {
                            crate::BroadcastRecvResult::Lagged(count) => {
                                tracing::trace!(
                                    agent_id = %agent_id,
                                    count,
                                    "tool output stream lagged, dropped lines"
                                );
                            }
                            crate::BroadcastRecvResult::Closed => break,
                            crate::BroadcastRecvResult::Event(_) => unreachable!(),
                        }
                    }
                }
            }
        });
    }

    /// Set the SQLite pools for all agents.
    pub fn set_agent_pools(&self, pools: HashMap<String, sqlx::SqlitePool>) {
        self.agent_pools.store(Arc::new(pools));
    }

    /// Set the agent config summaries for the agents list endpoint.
    pub fn set_agent_configs(&self, configs: Vec<AgentInfo>) {
        self.agent_configs.store(Arc::new(configs));
    }

    /// Set the memory search instances for all agents.
    pub fn set_memory_searches(&self, searches: HashMap<String, Arc<MemorySearch>>) {
        self.memory_searches.store(Arc::new(searches));
    }

    /// Set the cortex chat sessions for all agents.
    pub fn set_cortex_chat_sessions(&self, sessions: HashMap<String, Arc<CortexChatSession>>) {
        self.cortex_chat_sessions.store(Arc::new(sessions));
    }

    /// Set the workspace paths for all agents.
    pub fn set_agent_workspaces(&self, workspaces: HashMap<String, PathBuf>) {
        self.agent_workspaces.store(Arc::new(workspaces));
    }

    /// Set the identity directory paths for all agents.
    pub fn set_agent_identity_dirs(&self, dirs: HashMap<String, PathBuf>) {
        self.agent_identity_dirs.store(Arc::new(dirs));
    }

    pub fn set_agent_data_dirs(&self, dirs: HashMap<String, PathBuf>) {
        self.agent_data_dirs.store(Arc::new(dirs));
    }

    /// Set the config.toml path.
    pub async fn set_config_path(&self, path: PathBuf) {
        let mut guard = self.config_path.write().await;
        *guard = path;
    }

    /// Set the cron stores for all agents.
    pub fn set_cron_stores(&self, stores: HashMap<String, Arc<CronStore>>) {
        self.cron_stores.store(Arc::new(stores));
    }

    /// Set the cron schedulers for all agents.
    pub fn set_cron_schedulers(&self, schedulers: HashMap<String, Arc<Scheduler>>) {
        self.cron_schedulers.store(Arc::new(schedulers));
    }

    /// Set the global task store.
    pub fn set_task_store(&self, store: Arc<TaskStore>) {
        self.task_store.store(Arc::new(Some(store)));
    }

    /// Set the instance-wide wiki store.
    pub fn set_wiki_store(&self, store: Arc<crate::wiki::WikiStore>) {
        self.wiki_store.store(Arc::new(Some(store)));
    }

    /// Set the shared project store.
    pub fn set_project_store(&self, store: Arc<ProjectStore>) {
        self.project_store.store(Arc::new(Some(store)));
    }

    /// Set the instance-level notification store.
    pub fn set_notification_store(&self, store: Arc<NotificationStore>) {
        self.notification_store.store(Arc::new(Some(store)));
    }

    /// Insert a notification and broadcast `NotificationCreated` via SSE.
    /// Fire-and-forget: spawns a task and returns immediately.
    pub fn emit_notification(&self, n: NewNotification) {
        let store = self.notification_store.load().as_ref().clone();
        let event_tx = self.event_tx.clone();
        let Some(store) = store else { return };
        tokio::spawn(async move {
            match store.insert(n).await {
                Ok(Some(notification)) => {
                    event_tx
                        .send(ApiEvent::NotificationCreated { notification })
                        .ok();
                }
                Ok(None) => {} // duplicate suppressed by unique index
                Err(error) => tracing::warn!(%error, "failed to insert notification"),
            }
        });
    }

    /// Set the runtime configs for all agents.
    pub fn set_runtime_configs(&self, configs: HashMap<String, Arc<RuntimeConfig>>) {
        self.runtime_configs.store(Arc::new(configs));
    }

    /// Set the MCP managers for all agents.
    pub fn set_mcp_managers(&self, managers: HashMap<String, Arc<McpManager>>) {
        self.mcp_managers.store(Arc::new(managers));
    }

    /// Set the sandbox instances for all agents.
    pub fn set_sandboxes(&self, sandboxes: HashMap<String, Arc<crate::sandbox::Sandbox>>) {
        self.sandboxes.store(Arc::new(sandboxes));
    }

    /// Set the instance-level secrets store.
    pub fn set_secrets_store(&self, store: Arc<crate::secrets::store::SecretsStore>) {
        self.secrets_store.store(Arc::new(Some(store)));
    }

    /// Share the Discord permissions ArcSwap with the API so reads get hot-reloaded values.
    pub async fn set_discord_permissions(&self, permissions: Arc<ArcSwap<DiscordPermissions>>) {
        *self.discord_permissions.write().await = Some(permissions);
    }

    /// Share the Slack permissions ArcSwap with the API so reads get hot-reloaded values.
    pub async fn set_slack_permissions(&self, permissions: Arc<ArcSwap<SlackPermissions>>) {
        *self.slack_permissions.write().await = Some(permissions);
    }

    /// Share the Signal permissions ArcSwap with the API so reads get hot-reloaded values.
    pub async fn set_signal_permissions(&self, permissions: Arc<ArcSwap<SignalPermissions>>) {
        *self.signal_permissions.write().await = Some(permissions);
    }

    /// Share the bindings ArcSwap with the API so reads get hot-reloaded values.
    pub async fn set_bindings(&self, bindings: Arc<ArcSwap<Vec<Binding>>>) {
        *self.bindings.write().await = Some(bindings);
    }

    /// Share the messaging manager for runtime adapter addition from API handlers.
    pub async fn set_messaging_manager(&self, manager: Arc<MessagingManager>) {
        *self.messaging_manager.write().await = Some(manager);
    }

    /// Set the instance directory path.
    pub fn set_instance_dir(&self, dir: PathBuf) {
        self.instance_dir.store(Arc::new(dir));
    }

    /// Set the shared LLM manager for runtime agent creation.
    pub async fn set_llm_manager(&self, manager: Arc<LlmManager>) {
        *self.llm_manager.write().await = Some(manager);
    }

    /// Set the shared embedding model for runtime agent creation.
    pub async fn set_embedding_model(&self, model: Arc<EmbeddingModel>) {
        *self.embedding_model.write().await = Some(model);
    }

    /// Set the prompt engine snapshot for runtime agent creation.
    pub async fn set_prompt_engine(&self, engine: PromptEngine) {
        *self.prompt_engine.write().await = Some(engine);
    }

    /// Set the instance-level defaults for runtime agent creation.
    pub async fn set_defaults_config(&self, defaults: DefaultsConfig) {
        *self.defaults_config.write().await = Some(defaults);
    }

    /// Set the shared portal adapter for API handlers.
    pub fn set_portal_adapter(&self, adapter: Arc<PortalAdapter>) {
        self.portal_adapter.store(Arc::new(Some(adapter)));
    }

    /// Set the agent links for the communication graph.
    pub fn set_agent_links(&self, links: Vec<crate::links::AgentLink>) {
        self.agent_links.store(Arc::new(links));
    }

    /// Set the visual agent groups for the topology UI.
    pub fn set_agent_groups(&self, groups: Vec<crate::config::GroupDef>) {
        self.agent_groups.store(Arc::new(groups));
    }

    /// Set the org-level humans for the topology UI.
    pub fn set_agent_humans(&self, humans: Vec<crate::config::HumanDef>) {
        self.agent_humans.store(Arc::new(humans));
    }

    /// Drain accumulated channel tool calls for a channel.
    ///
    /// Called when the bot message is about to be persisted so the tool calls
    /// can be stored in the message metadata.
    pub async fn take_channel_tool_calls(&self, channel_id: &str) -> Vec<ChannelToolCallEntry> {
        let mut guard = self.live_channel_tool_calls.write().await;
        guard.remove(channel_id).unwrap_or_default()
    }

    /// Send an event to all SSE subscribers.
    pub fn send_event(&self, event: ApiEvent) {
        let _ = self.event_tx.send(event);
    }
}

/// Conversation/routing tools that should not be stored or surfaced as channel tool calls.
fn is_hidden_channel_tool(name: &str) -> bool {
    matches!(
        name,
        "reply" | "react" | "skip" | "set_outcome" | "spawn_worker" | "branch" | "route" | "cancel"
    )
}

/// Extract (process_type, id_string) from a ProcessId.
fn process_id_info(id: &ProcessId) -> (String, String) {
    match id {
        ProcessId::Channel(channel_id) => ("channel".into(), channel_id.to_string()),
        ProcessId::Branch(branch_id) => ("branch".into(), branch_id.to_string()),
        ProcessId::Worker(worker_id) => ("worker".into(), worker_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_LIVE_TOOL_OUTPUT_BYTES, append_live_output, sanitize_live_tool_output_line,
        upsert_pending_tool_output,
    };
    use crate::conversation::worker_transcript::{ToolResultStatus, TranscriptStep};
    use crate::{ProcessEvent, ProcessId};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn append_live_output_caps_buffer_bytes() {
        let mut output = Some("a".repeat(MAX_LIVE_TOOL_OUTPUT_BYTES - 1));
        append_live_output(&mut output, "bc");
        let capped = output.expect("output should be present");
        assert!(capped.len() <= MAX_LIVE_TOOL_OUTPUT_BYTES);
    }

    #[test]
    fn append_live_output_preserves_utf8_boundaries_when_capping() {
        let mut output = Some("🙂".repeat((MAX_LIVE_TOOL_OUTPUT_BYTES / 4) + 4));
        append_live_output(&mut output, "🙂");
        let capped = output.expect("output should be present");
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());
        assert!(capped.len() <= MAX_LIVE_TOOL_OUTPUT_BYTES);
    }

    #[test]
    fn upsert_pending_tool_output_ignores_finalized_call() {
        let mut steps = vec![TranscriptStep::ToolResult {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            text: "done".to_string(),
            live_output: None,
            status: ToolResultStatus::Final,
        }];

        upsert_pending_tool_output(
            &mut steps,
            "call-1".to_string(),
            "shell".to_string(),
            "late output",
        );

        let TranscriptStep::ToolResult {
            text, live_output, ..
        } = &steps[0]
        else {
            panic!("expected tool result");
        };
        assert_eq!(text, "done");
        assert!(live_output.is_none());
    }

    #[test]
    fn sanitize_live_tool_output_line_redacts_detected_leaks() {
        let line = "key=sk-ant-api03-abcdefghijklmnopqrstuvwxyz";
        let sanitized = sanitize_live_tool_output_line(line);
        assert_ne!(sanitized, line);
        assert!(crate::secrets::scrub::scan_for_leaks(&sanitized).is_none());
    }

    #[tokio::test]
    async fn tool_output_before_worker_started_is_cached_and_stale_output_ignored_after_complete() {
        let (provider_setup_tx, _provider_setup_rx) = tokio::sync::mpsc::channel(1);
        let (agent_tx, _agent_rx) = tokio::sync::mpsc::channel(1);
        let (agent_remove_tx, _agent_remove_rx) = tokio::sync::mpsc::channel(1);
        let (injection_tx, _injection_rx) = tokio::sync::mpsc::channel(1);
        let api_state = super::ApiState::new_with_provider_sender(
            provider_setup_tx,
            agent_tx,
            agent_remove_tx,
            injection_tx,
        );

        let (control_tx, control_rx) = tokio::sync::broadcast::channel(16);
        let (tool_output_tx, tool_output_rx) = tokio::sync::broadcast::channel(16);
        api_state.register_agent_events("agent".to_string(), control_rx);
        api_state.register_tool_output_stream("agent".to_string(), tool_output_rx);

        let agent_id: crate::AgentId = Arc::from("agent");
        let worker_id = uuid::Uuid::new_v4();
        let process_id = ProcessId::Worker(worker_id);
        let worker_key = worker_id.to_string();

        let _ = tool_output_tx.send(ProcessEvent::ToolOutput {
            agent_id: agent_id.clone(),
            process_id: process_id.clone(),
            channel_id: None,
            call_id: "shell_call_early".to_string(),
            tool_name: "shell".to_string(),
            line: "early line".to_string(),
            stream: "stdout".to_string(),
        });

        let early_cached = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(steps) = api_state.get_live_transcript(&worker_key).await
                    && steps.iter().any(|step| {
                        matches!(
                            step,
                            TranscriptStep::ToolResult { live_output: Some(output), .. }
                                if output.contains("early line")
                        )
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            early_cached.is_ok(),
            "early streamed output should be cached"
        );

        let _ = control_tx.send(ProcessEvent::WorkerComplete {
            agent_id: agent_id.clone(),
            worker_id,
            channel_id: None,
            result: "done".to_string(),
            notify: false,
            success: true,
        });

        let removed_after_complete = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if api_state.get_live_transcript(&worker_key).await.is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            removed_after_complete.is_ok(),
            "worker completion should clear live transcript cache"
        );

        let _ = tool_output_tx.send(ProcessEvent::ToolOutput {
            agent_id,
            process_id,
            channel_id: None,
            call_id: "shell_call_late".to_string(),
            tool_name: "shell".to_string(),
            line: "late line".to_string(),
            stream: "stdout".to_string(),
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            api_state.get_live_transcript(&worker_key).await.is_none(),
            "late output should not recreate cache after worker completion"
        );
    }
}
