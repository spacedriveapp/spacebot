//! Shared state for the HTTP API.

use crate::agent::channel::ChannelState;
use crate::agent::cortex_chat::CortexChatSession;
use crate::agent::status::StatusBlock;
use crate::config::{Binding, DefaultsConfig, DiscordPermissions, RuntimeConfig, SlackPermissions};
use crate::cron::{CronStore, Scheduler};
use crate::llm::LlmManager;
use crate::memory::{EmbeddingModel, MemorySearch};
use crate::messaging::MessagingManager;
use crate::messaging::webchat::WebChatAdapter;
use crate::prompts::PromptEngine;
use crate::update::SharedUpdateStatus;
use crate::{ProcessEvent, ProcessId};

use arc_swap::ArcSwap;
use serde::Serialize;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, broadcast, mpsc};

/// Summary of an agent's configuration, exposed via the API.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub workspace: PathBuf,
    pub context_window: usize,
    pub max_turns: usize,
    pub max_concurrent_branches: usize,
    pub max_concurrent_workers: usize,
}

/// State shared across all API handlers.
pub struct ApiState {
    pub started_at: Instant,
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
    /// Per-agent workspace paths for identity file access.
    pub agent_workspaces: arc_swap::ArcSwap<HashMap<String, PathBuf>>,
    /// Path to the instance config.toml file.
    pub config_path: RwLock<PathBuf>,
    /// Per-agent cron stores for cron job CRUD operations.
    pub cron_stores: arc_swap::ArcSwap<HashMap<String, Arc<CronStore>>>,
    /// Per-agent cron schedulers for job timer management.
    pub cron_schedulers: arc_swap::ArcSwap<HashMap<String, Arc<Scheduler>>>,
    /// Per-agent RuntimeConfig for reading live hot-reloaded configuration.
    pub runtime_configs: ArcSwap<HashMap<String, Arc<RuntimeConfig>>>,
    /// Shared reference to the Discord permissions ArcSwap (same instance used by the adapter and file watcher).
    pub discord_permissions: RwLock<Option<Arc<ArcSwap<DiscordPermissions>>>>,
    /// Shared reference to the Slack permissions ArcSwap (same instance used by the adapter and file watcher).
    pub slack_permissions: RwLock<Option<Arc<ArcSwap<SlackPermissions>>>>,
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
    /// Shared webchat adapter for session management from API handlers.
    pub webchat_adapter: ArcSwap<Option<Arc<WebChatAdapter>>>,
}

/// Events sent to SSE clients. Wraps ProcessEvents with agent context.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiEvent {
    /// An inbound message from a user.
    InboundMessage {
        agent_id: String,
        channel_id: String,
        sender_name: Option<String>,
        sender_id: String,
        text: String,
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
    /// A worker was started.
    WorkerStarted {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
        task: String,
    },
    /// A worker's status changed.
    WorkerStatusUpdate {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
        status: String,
    },
    /// A worker completed.
    WorkerCompleted {
        agent_id: String,
        channel_id: Option<String>,
        worker_id: String,
        result: String,
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
        tool_name: String,
    },
    /// A tool call completed on a process.
    ToolCompleted {
        agent_id: String,
        channel_id: Option<String>,
        process_type: String,
        process_id: String,
        tool_name: String,
    },
    /// Configuration was reloaded (skills, identity, etc.).
    ConfigReloaded,
}

impl ApiState {
    pub fn new_with_provider_sender(
        provider_setup_tx: mpsc::Sender<crate::ProviderSetupEvent>,
        agent_tx: mpsc::Sender<crate::Agent>,
        agent_remove_tx: mpsc::Sender<String>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(512);
        Self {
            started_at: Instant::now(),
            event_tx,
            agent_pools: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            agent_configs: arc_swap::ArcSwap::from_pointee(Vec::new()),
            memory_searches: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            channel_status_blocks: RwLock::new(HashMap::new()),
            channel_states: RwLock::new(HashMap::new()),
            cortex_chat_sessions: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            agent_workspaces: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            config_path: RwLock::new(PathBuf::new()),
            cron_stores: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            cron_schedulers: arc_swap::ArcSwap::from_pointee(HashMap::new()),
            runtime_configs: ArcSwap::from_pointee(HashMap::new()),
            discord_permissions: RwLock::new(None),
            slack_permissions: RwLock::new(None),
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
            webchat_adapter: ArcSwap::from_pointee(None),
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

    /// Register an agent's event stream. Spawns a task that forwards
    /// ProcessEvents into the aggregated API event stream.
    pub fn register_agent_events(
        &self,
        agent_id: String,
        mut agent_event_rx: broadcast::Receiver<ProcessEvent>,
    ) {
        let api_tx = self.event_tx.clone();
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
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::WorkerStarted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        worker_id: worker_id.to_string(),
                                        task: task.clone(),
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
                            ProcessEvent::WorkerComplete {
                                worker_id,
                                channel_id,
                                result,
                                ..
                            } => {
                                api_tx
                                    .send(ApiEvent::WorkerCompleted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        worker_id: worker_id.to_string(),
                                        result: result.clone(),
                                    })
                                    .ok();
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
                                tool_name,
                                ..
                            } => {
                                let (process_type, id_str) = process_id_info(process_id);
                                api_tx
                                    .send(ApiEvent::ToolStarted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        process_type,
                                        process_id: id_str,
                                        tool_name: tool_name.clone(),
                                    })
                                    .ok();
                            }
                            ProcessEvent::ToolCompleted {
                                process_id,
                                channel_id,
                                tool_name,
                                ..
                            } => {
                                let (process_type, id_str) = process_id_info(process_id);
                                api_tx
                                    .send(ApiEvent::ToolCompleted {
                                        agent_id: agent_id.clone(),
                                        channel_id: channel_id.as_deref().map(|s| s.to_string()),
                                        process_type,
                                        process_id: id_str,
                                        tool_name: tool_name.clone(),
                                    })
                                    .ok();
                            }
                            _ => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        tracing::debug!(agent_id = %agent_id, count, "API event forwarder lagged, skipped events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
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

    /// Set the runtime configs for all agents.
    pub fn set_runtime_configs(&self, configs: HashMap<String, Arc<RuntimeConfig>>) {
        self.runtime_configs.store(Arc::new(configs));
    }

    /// Share the Discord permissions ArcSwap with the API so reads get hot-reloaded values.
    pub async fn set_discord_permissions(&self, permissions: Arc<ArcSwap<DiscordPermissions>>) {
        *self.discord_permissions.write().await = Some(permissions);
    }

    /// Share the Slack permissions ArcSwap with the API so reads get hot-reloaded values.
    pub async fn set_slack_permissions(&self, permissions: Arc<ArcSwap<SlackPermissions>>) {
        *self.slack_permissions.write().await = Some(permissions);
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

    /// Set the shared webchat adapter for API handlers.
    pub fn set_webchat_adapter(&self, adapter: Arc<WebChatAdapter>) {
        self.webchat_adapter.store(Arc::new(Some(adapter)));
    }

    /// Send an event to all SSE subscribers.
    pub fn send_event(&self, event: ApiEvent) {
        let _ = self.event_tx.send(event);
    }
}

/// Extract (process_type, id_string) from a ProcessId.
fn process_id_info(id: &ProcessId) -> (String, String) {
    match id {
        ProcessId::Channel(channel_id) => ("channel".into(), channel_id.to_string()),
        ProcessId::Branch(branch_id) => ("branch".into(), branch_id.to_string()),
        ProcessId::Worker(worker_id) => ("worker".into(), worker_id.to_string()),
    }
}
