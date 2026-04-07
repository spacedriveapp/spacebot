//! Tools available to agents.
//!
//! Tools are organized by function, not by consumer. Which agents get which tools
//! is configured via the ToolServer factory functions below.
//!
//! ## ToolServer Topology
//!
//! **Channel ToolServer** (one per channel):
//! - `reply`, `branch`, `spawn_worker`, `route`, `cancel`, `skip`, `react` — added
//!   dynamically per conversation turn via `add_channel_tools()` /
//!   `remove_channel_tools()` because they hold per-channel state.
//! - No memory tools — the channel delegates memory work to branches.
//!
//! **Branch ToolServer** (one per branch, isolated):
//! - `memory_save` + `memory_recall` + `memory_delete` + `channel_recall`
//! - `spacebot_docs` for embedded self-documentation lookup
//! - `task_create` + `task_list` + `task_update`
//! - `spawn_worker` is included for channel-originated branches only
//!
//! **Worker ToolServer** (one per worker, created at spawn time):
//! - `shell`, `file_read`/`file_write`/`file_edit`/`file_list` — stateless, registered at creation
//! - `task_update` — scoped to the worker's assigned task
//! - `set_status` — per-worker instance, registered at creation
//!
//! **Cortex ToolServer** (one per agent):
//! - `memory_save` — registered at startup
//!
//! **Cortex Chat ToolServer** (interactive admin chat):
//! - branch + worker tool superset plus `spacebot_docs`, `config_inspect`, and `spawn_worker`

pub mod attachment_recall;
pub mod branch_tool;
pub mod browser;
pub mod cancel;
pub mod channel_recall;
pub mod config_inspect;
pub mod cron;
pub mod email_search;
pub mod file;
pub mod install_skill;
pub mod mcp;
pub mod memory_delete;
pub mod memory_persistence_complete;
pub mod memory_recall;
pub mod memory_save;
pub mod project_manage;
pub mod react;
pub mod read_skill;
pub mod reply;
pub mod route;
pub mod secret_set;
pub mod send_agent_message;
pub mod send_file;
pub mod send_message_to_another_channel;
pub mod set_status;
pub mod shell;
pub mod skills_search;
pub mod skip;
pub mod spacebot_docs;
pub mod spawn_worker;
pub mod task_create;
pub mod task_get;
pub mod task_list;
pub mod task_update;
pub mod web_search;
pub mod worker_inspect;

pub mod factory_create_agent;
pub mod factory_list_presets;
pub mod factory_load_preset;
pub mod factory_search_context;
pub mod factory_update_config;
pub mod factory_update_identity;

pub use attachment_recall::{
    AttachmentRecallArgs, AttachmentRecallError, AttachmentRecallOutput, AttachmentRecallTool,
};
pub use branch_tool::{BranchArgs, BranchError, BranchOutput, BranchTool};
pub use browser::{
    BrowserError, BrowserOutput, SharedBrowserHandle, TabInfo, new_shared_browser_handle,
    register_browser_tools,
};
pub use cancel::{CancelArgs, CancelError, CancelOutput, CancelTool};
pub use channel_recall::{
    ChannelRecallArgs, ChannelRecallError, ChannelRecallOutput, ChannelRecallTool,
};
pub use config_inspect::{
    ConfigInspectArgs, ConfigInspectError, ConfigInspectOutput, ConfigInspectTool,
};
pub use cron::{CronArgs, CronError, CronOutput, CronTool};
pub use email_search::{EmailSearchArgs, EmailSearchError, EmailSearchOutput, EmailSearchTool};
pub use file::{
    FileEditArgs, FileEditTool, FileEntry, FileEntryOutput, FileError, FileListArgs, FileListTool,
    FileOutput, FileReadArgs, FileReadTool, FileType, FileWriteArgs, FileWriteTool,
    register_file_tools,
};
pub use install_skill::{
    InstallSkillArgs, InstallSkillError, InstallSkillOutput, InstallSkillTool,
};
pub use mcp::{McpToolAdapter, McpToolError, McpToolOutput};
pub use memory_delete::{
    MemoryDeleteArgs, MemoryDeleteError, MemoryDeleteOutput, MemoryDeleteTool,
};
pub use memory_persistence_complete::{
    MemoryPersistenceCompleteArgs, MemoryPersistenceCompleteError, MemoryPersistenceCompleteOutput,
    MemoryPersistenceCompleteTool, MemoryPersistenceContractState,
    MemoryPersistenceTerminalOutcome,
};
pub use memory_recall::{
    MemoryOutput, MemoryRecallArgs, MemoryRecallError, MemoryRecallOutput, MemoryRecallTool,
};
pub use memory_save::{
    AssociationInput, MemorySaveArgs, MemorySaveError, MemorySaveOutput, MemorySaveTool,
};
pub use project_manage::{
    ProjectManageArgs, ProjectManageError, ProjectManageOutput, ProjectManageTool,
};
pub use react::{ReactArgs, ReactError, ReactOutput, ReactTool};
pub use read_skill::{ReadSkillArgs, ReadSkillError, ReadSkillOutput, ReadSkillTool};
pub use reply::{RepliedFlag, ReplyArgs, ReplyError, ReplyOutput, ReplyTool, new_replied_flag};
pub use route::{RouteArgs, RouteError, RouteOutput, RouteTool};
pub use secret_set::{SecretSetArgs, SecretSetError, SecretSetOutput, SecretSetTool};
pub use send_agent_message::{
    SendAgentMessageArgs, SendAgentMessageError, SendAgentMessageOutput, SendAgentMessageTool,
};
pub use send_file::{SendFileArgs, SendFileError, SendFileOutput, SendFileTool};
pub use send_message_to_another_channel::{
    SendMessageArgs, SendMessageError, SendMessageOutput, SendMessageTool,
};
pub use set_status::{SetStatusArgs, SetStatusError, SetStatusOutput, SetStatusTool, StatusKind};
pub use shell::{EnvVar, ShellArgs, ShellError, ShellOutput, ShellResult, ShellTool};
pub use skills_search::{
    SkillsSearchArgs, SkillsSearchError, SkillsSearchOutput, SkillsSearchTool,
};
pub use skip::{SkipArgs, SkipError, SkipFlag, SkipOutput, SkipTool, new_skip_flag};
pub use spacebot_docs::{
    SpacebotDocContent, SpacebotDocsArgs, SpacebotDocsError, SpacebotDocsOutput, SpacebotDocsTool,
};
pub use spawn_worker::{
    DetachedSpawnWorkerTool, SpawnWorkerArgs, SpawnWorkerError, SpawnWorkerOutput, SpawnWorkerTool,
};
pub use task_create::{TaskCreateArgs, TaskCreateError, TaskCreateOutput, TaskCreateTool};
pub use task_get::{TaskGetArgs, TaskGetError, TaskGetOutput, TaskGetTool};
pub use task_list::{TaskListArgs, TaskListError, TaskListOutput, TaskListTool};
pub use task_update::{TaskUpdateArgs, TaskUpdateError, TaskUpdateOutput, TaskUpdateTool};
pub use web_search::{SearchResult, WebSearchArgs, WebSearchError, WebSearchOutput, WebSearchTool};
pub use worker_inspect::{
    WorkerInspectArgs, WorkerInspectError, WorkerInspectOutput, WorkerInspectTool,
};

pub use factory_create_agent::{
    FactoryCreateAgentArgs, FactoryCreateAgentError, FactoryCreateAgentOutput,
    FactoryCreateAgentTool,
};
pub use factory_list_presets::{
    FactoryListPresetsArgs, FactoryListPresetsError, FactoryListPresetsOutput,
    FactoryListPresetsTool,
};
pub use factory_load_preset::{
    FactoryLoadPresetArgs, FactoryLoadPresetError, FactoryLoadPresetOutput, FactoryLoadPresetTool,
};
pub use factory_search_context::{
    FactorySearchContextArgs, FactorySearchContextError, FactorySearchContextOutput,
    FactorySearchContextTool,
};
pub use factory_update_config::{
    FactoryUpdateConfigArgs, FactoryUpdateConfigError, FactoryUpdateConfigOutput,
    FactoryUpdateConfigTool,
};
pub use factory_update_identity::{
    FactoryUpdateIdentityArgs, FactoryUpdateIdentityError, FactoryUpdateIdentityOutput,
    FactoryUpdateIdentityTool,
};

use crate::agent::channel::ChannelState;
use crate::config::{BrowserConfig, RuntimeConfig};
use crate::conversation::settings::WorkerMemoryMode;
use crate::memory::MemorySearch;
use crate::sandbox::Sandbox;
use crate::tasks::TaskStore;
use crate::{AgentId, ChannelId, ProcessEvent, RoutedSender, WorkerId};
use arc_swap::ArcSwap;
use rig::tool::Tool as _;
use rig::tool::server::{ToolServer, ToolServerHandle};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum BranchToolProfile {
    Default,
    MemoryPersistence {
        contract_state: Arc<MemoryPersistenceContractState>,
        working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
        channel_id: Option<String>,
    },
}

/// Deserialize a `u64` that may arrive as either a JSON number or a JSON string.
///
/// LLMs sometimes send `"timeout_seconds": "400"` instead of `"timeout_seconds": 400`.
/// This helper accepts both forms so the tool call doesn't fail on a type mismatch.
pub fn deserialize_string_or_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrU64;

    impl<'de> de::Visitor<'de> for StringOrU64 {
        type Value = u64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a u64 or a string containing a u64")
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<u64, E> {
            u64::try_from(value).map_err(|_| E::custom(format!("negative value: {value}")))
        }

        fn visit_f64<E: de::Error>(self, value: f64) -> Result<u64, E> {
            if value >= 0.0 && value <= u64::MAX as f64 && value.fract() == 0.0 {
                Ok(value as u64)
            } else {
                Err(E::custom(format!("invalid timeout value: {value}")))
            }
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<u64, E> {
            value
                .parse::<u64>()
                .map_err(|_| E::custom(format!("cannot parse \"{value}\" as a positive integer")))
        }
    }

    deserializer.deserialize_any(StringOrU64)
}

/// Maximum byte length for tool output strings (stdout, stderr, file content).
/// ~50KB keeps a single tool result under ~12,500 tokens (at ~4 chars/token).
pub const MAX_TOOL_OUTPUT_BYTES: usize = 50_000;

/// Maximum number of entries returned by directory listings.
pub const MAX_DIR_ENTRIES: usize = 500;

/// Truncate a string to a byte limit, appending a notice if truncated.
///
/// Cuts at the last valid char boundary before `max_bytes` so we never split
/// a multi-byte character. The truncation notice tells the LLM the original
/// size and how to get the rest (pipe through head/tail or read with offset).
fn truncate_at_char_boundary(value: &str, max_bytes: usize) -> usize {
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

pub fn truncate_output(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let end = truncate_at_char_boundary(value, max_bytes);

    let total = value.len();
    let truncated_bytes = total - end;
    format!(
        "{}\n\n[output truncated: showed {end} of {total} bytes ({truncated_bytes} bytes omitted). \
         Use head/tail/offset to read specific sections]",
        &value[..end]
    )
}

/// Truncate to a byte limit and append `...`, preserving UTF-8 boundaries.
///
/// The returned string will never exceed `max_bytes`. If there's not enough
/// room for both content and "...", only the content is returned (truncated
/// to fit within `max_bytes`).
pub fn truncate_utf8_ellipsis(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    // Try to leave room for "..." (3 bytes)
    let available = max_bytes.saturating_sub(3);
    let end = truncate_at_char_boundary(value, available);

    // If we have room for "...", append it; otherwise just return truncated content
    if end > 0 && end + 3 <= max_bytes {
        format!("{}...", &value[..end])
    } else {
        // Not enough room for "...", return as much as fits
        let end = truncate_at_char_boundary(value, max_bytes);
        value[..end].to_string()
    }
}

/// Returns true when text looks like structured/tool payloads that should never
/// be sent to end users as plain chat output.
pub fn should_block_user_visible_text(value: &str) -> bool {
    const TOOL_PREFIXES: &[&str] = &[
        "[reply]",
        "(reply)",
        "[react]",
        "(react)",
        "[skip]",
        "(skip)",
        "[branch]",
        "(branch)",
        "[spawn_worker]",
        "(spawn_worker)",
        "[route]",
        "(route)",
        "[cancel]",
        "(cancel)",
    ];

    let trimmed = value.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with('(') {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    if TOOL_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return true;
    }

    let punctuation_trimmed = lower.trim_matches(|character: char| {
        character.is_ascii_punctuation() || character.is_ascii_whitespace()
    });
    if punctuation_trimmed == "skip" {
        return true;
    }

    lower.starts_with("<system-reminder>") || lower.starts_with("<path>")
}

/// Add per-turn tools to a channel's ToolServer.
///
/// Called when a conversation turn begins. These tools hold per-turn state
/// (response sender, skip flag) that changes between turns. Cleaned up via
/// `remove_channel_tools()` when the turn ends.
#[allow(clippy::too_many_arguments)]
pub async fn add_channel_tools(
    handle: &ToolServerHandle,
    state: ChannelState,
    response_tx: RoutedSender,
    conversation_id: impl Into<String>,
    skip_flag: SkipFlag,
    replied_flag: RepliedFlag,
    cron_tool: Option<CronTool>,
    send_agent_message_tool: Option<SendAgentMessageTool>,
    allow_direct_reply: bool,
    current_adapter: Option<String>,
    slack_thread_ts: Option<&str>,
) -> Result<(), rig::tool::server::ToolServerError> {
    let conversation_id = conversation_id.into();

    if allow_direct_reply {
        let agent_display_name = state
            .deps
            .agent_names
            .get(state.deps.agent_id.as_ref())
            .cloned()
            .unwrap_or_else(|| state.deps.agent_id.to_string());
        handle
            .add_tool(ReplyTool::new(
                response_tx.clone(),
                conversation_id.clone(),
                state.conversation_logger.clone(),
                state.channel_id.clone(),
                replied_flag.clone(),
                agent_display_name,
            ))
            .await?;
    }
    handle.add_tool(BranchTool::new(state.clone())).await?;
    handle.add_tool(SpawnWorkerTool::new(state.clone())).await?;
    handle.add_tool(RouteTool::new(state.clone())).await?;
    if let Some(messaging_manager) = &state.deps.messaging_manager {
        let send_message_display_name = state
            .deps
            .agent_names
            .get(state.deps.agent_id.as_ref())
            .cloned()
            .unwrap_or_else(|| state.deps.agent_id.to_string());
        handle
            .add_tool(SendMessageTool::new(
                messaging_manager.clone(),
                state.channel_store.clone(),
                state.conversation_logger.clone(),
                send_message_display_name,
                current_adapter.clone(),
            ))
            .await?;
    }
    handle
        .add_tool(SendFileTool::new(
            response_tx.clone(),
            state.deps.runtime_config.workspace_dir.clone(),
            state.deps.sandbox.clone(),
        ))
        .await?;
    handle
        .add_tool(ProjectManageTool::new(
            state.deps.project_store.clone(),
            state.deps.agent_id.to_string(),
        ))
        .await?;
    // Add attachment recall tool when save_attachments is enabled
    if state
        .deps
        .runtime_config
        .channel_config
        .load()
        .save_attachments
    {
        handle
            .add_tool(AttachmentRecallTool::new(
                state.deps.sqlite_pool.clone(),
                state.channel_id.clone(),
            ))
            .await?;
    }
    handle.add_tool(CancelTool::new(state)).await?;
    handle
        .add_tool(SkipTool::new(skip_flag.clone(), response_tx.clone()))
        .await?;
    handle.add_tool(ReactTool::new(response_tx.clone())).await?;
    if let Some(cron_tool) = cron_tool {
        let cron_tool = cron_tool
            .with_default_delivery_target(default_delivery_target_for_conversation(
                &conversation_id,
                slack_thread_ts,
            ))
            .with_current_adapter(current_adapter.clone());
        handle.add_tool(cron_tool).await?;
    }
    if let Some(mut agent_msg) = send_agent_message_tool {
        agent_msg = agent_msg.with_skip_flag(skip_flag.clone());
        handle.add_tool(agent_msg).await?;
    }
    Ok(())
}

/// Add tools for "Direct" delegation mode - gives channel full tool access.
/// This combines channel tools with memory and execution tools (cortex chat style).
#[allow(clippy::too_many_arguments)]
pub async fn add_direct_mode_tools(
    handle: &ToolServerHandle,
    state: ChannelState,
    response_tx: RoutedSender,
    conversation_id: impl Into<String>,
    skip_flag: SkipFlag,
    replied_flag: RepliedFlag,
    cron_tool: Option<CronTool>,
    send_agent_message_tool: Option<SendAgentMessageTool>,
    allow_direct_reply: bool,
    current_adapter: Option<String>,
    slack_thread_ts: Option<&str>,
) -> Result<(), rig::tool::server::ToolServerError> {
    // First add all standard channel tools
    add_channel_tools(
        handle,
        state.clone(),
        response_tx.clone(),
        conversation_id,
        skip_flag.clone(),
        replied_flag.clone(),
        cron_tool.clone(),
        send_agent_message_tool.clone(),
        allow_direct_reply,
        current_adapter.clone(),
        slack_thread_ts,
    )
    .await?;

    // Then add memory tools (normally only available to branches)
    handle
        .add_tool(MemoryRecallTool::new(state.deps.memory_search.clone()))
        .await?;

    handle
        .add_tool(MemorySaveTool::new(state.deps.memory_search.clone()))
        .await?;

    // Add shell and file tools (normally only available to workers)
    // These need careful implementation - for now, add basic versions
    // Note: The actual shell/file tools might need adaptation for channel context

    Ok(())
}

fn default_delivery_target_for_conversation(
    conversation_id: &str,
    slack_thread_ts: Option<&str>,
) -> Option<String> {
    let parsed = crate::messaging::target::parse_delivery_target(conversation_id)?;
    match parsed.adapter.as_str() {
        // Cron channels can't receive broadcast delivery.
        "cron" => None,
        // Portal conversation IDs use the "portal:" prefix but the messaging
        // adapter is registered as "portal". Remap so the manager can find it,
        // and pass the full original conversation_id as the target.
        "portal" => Some(format!("portal:{conversation_id}")),
        // For Slack, append the originating thread_ts so cron broadcasts land in
        // the correct thread rather than posting top-level.
        "slack" => {
            let base = parsed.to_string();
            match slack_thread_ts {
                Some(ts) => Some(format!("{base}#thread:{ts}")),
                None => Some(base),
            }
        }
        _ => Some(parsed.to_string()),
    }
}

/// Remove per-channel tools from a running ToolServer.
///
/// Called when a conversation turn ends or a channel is torn down. Prevents stale
/// tools from being invoked with dead senders.
pub async fn remove_channel_tools(
    handle: &ToolServerHandle,
    allow_direct_reply: bool,
) -> Result<(), rig::tool::server::ToolServerError> {
    if allow_direct_reply {
        handle.remove_tool(ReplyTool::NAME).await?;
    }
    handle.remove_tool(BranchTool::NAME).await?;
    handle.remove_tool(SpawnWorkerTool::NAME).await?;
    handle.remove_tool(RouteTool::NAME).await?;
    handle.remove_tool(CancelTool::NAME).await?;
    handle.remove_tool(SkipTool::NAME).await?;
    handle.remove_tool(SendFileTool::NAME).await?;
    handle.remove_tool(ReactTool::NAME).await?;
    handle.remove_tool(ProjectManageTool::NAME).await?;
    // Cron, send_message, send_agent_message, and attachment_recall removal is
    // best-effort since not all channels have them
    let _ = handle.remove_tool(CronTool::NAME).await;
    let _ = handle.remove_tool(SendMessageTool::NAME).await;
    let _ = handle.remove_tool(SendAgentMessageTool::NAME).await;
    let _ = handle.remove_tool(AttachmentRecallTool::NAME).await;
    Ok(())
}

fn memory_save_with_events(
    memory_search: Arc<MemorySearch>,
    agent_id: AgentId,
    memory_event_tx: broadcast::Sender<ProcessEvent>,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
) -> MemorySaveTool {
    let tool = MemorySaveTool::new(memory_search).with_event_bus(agent_id, memory_event_tx);
    match working_memory {
        Some(store) => tool.with_working_memory(store),
        None => tool,
    }
}

/// Create a per-branch ToolServer with memory tools.
///
/// Each branch gets its own isolated ToolServer so `memory_recall` is never
/// visible to the channel. Includes memory tools, task-board tools, and
/// `spacebot_docs` for on-demand self-documentation lookup.
#[allow(clippy::too_many_arguments)]
pub fn create_branch_tool_server(
    state: Option<ChannelState>,
    agent_id: AgentId,
    task_store: Arc<TaskStore>,
    memory_search: Arc<MemorySearch>,
    runtime_config: Arc<RuntimeConfig>,
    memory_event_tx: broadcast::Sender<ProcessEvent>,
    conversation_logger: crate::conversation::history::ConversationLogger,
    channel_store: crate::conversation::ChannelStore,
    run_logger: crate::conversation::history::ProcessRunLogger,
    profile: BranchToolProfile,
) -> ToolServerHandle {
    let mut memory_save = memory_save_with_events(
        memory_search.clone(),
        agent_id.clone(),
        memory_event_tx.clone(),
        None,
    );
    if let BranchToolProfile::MemoryPersistence { contract_state, .. } = &profile {
        memory_save = memory_save.with_contract_state(contract_state.clone());
    }

    let mut server = ToolServer::new()
        .tool(memory_save)
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .tool(SpacebotDocsTool::new())
        .tool(EmailSearchTool::new(runtime_config))
        .tool(WorkerInspectTool::new(run_logger, agent_id.to_string()))
        .tool(TaskCreateTool::new(
            task_store.clone(),
            agent_id.to_string(),
            "branch",
        ))
        .tool(TaskListTool::new(task_store.clone(), agent_id.to_string()))
        .tool(TaskUpdateTool::for_branch(task_store, agent_id.clone()));

    if let BranchToolProfile::MemoryPersistence {
        contract_state,
        working_memory,
        channel_id,
    } = profile
    {
        let mut tool = MemoryPersistenceCompleteTool::new(contract_state);
        if let Some(store) = working_memory {
            tool = tool.with_working_memory(store, channel_id);
        }
        server = server.tool(tool);
    }

    if let Some(state) = state {
        server = server.tool(SpawnWorkerTool::new(state));
    }

    server.run()
}

/// Configuration for enabling agent-to-agent delegation in workers.
///
/// When present, the worker gets a `send_agent_message` tool so it can
/// delegate tasks to subordinate agents in the communication graph.
pub struct DelegationConfig {
    pub links: Arc<ArcSwap<Vec<crate::links::AgentLink>>>,
    pub agent_names: Arc<std::collections::HashMap<String, String>>,
    pub conversation_logger: crate::conversation::history::ConversationLogger,
}

/// Create a per-worker ToolServer with task-appropriate tools.
///
/// Each worker gets its own isolated ToolServer. The `set_status` tool is bound to
/// the specific worker's ID so status updates route correctly. The browser tool
/// is included when browser automation is enabled in the agent config.
///
/// Shell commands are sandboxed by the `Sandbox` backend.
/// File operations are restricted to `workspace` via path validation.
#[allow(clippy::too_many_arguments)]
pub fn create_worker_tool_server(
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    task_store: Arc<TaskStore>,
    event_tx: broadcast::Sender<ProcessEvent>,
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
    mcp_tools: Vec<McpToolAdapter>,
    runtime_config: Arc<RuntimeConfig>,
    worker_memory_mode: WorkerMemoryMode,
    memory_search: Arc<MemorySearch>,
    delegation_config: Option<DelegationConfig>,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(ShellTool::new(workspace.clone(), sandbox.clone()))
        .tool(TaskUpdateTool::for_worker(
            task_store.clone(),
            agent_id.clone(),
            worker_id,
        ))
        .tool(TaskCreateTool::new(
            task_store.clone(),
            agent_id.to_string(),
            format!("worker:{}", worker_id),
        ))
        .tool({
            let mut status_tool =
                SetStatusTool::new(agent_id.clone(), worker_id, channel_id, event_tx.clone());
            if let Some(store) = runtime_config.secrets.load().as_ref() {
                status_tool = status_tool.with_tool_secrets(store.tool_secret_pairs());
            }
            status_tool
        })
        .tool(ReadSkillTool::new(runtime_config.clone()));

    server = register_file_tools(server, workspace, sandbox);

    if let Some(store) = runtime_config.secrets.load().as_ref() {
        server = server.tool(SecretSetTool::new(store.clone()));
    }

    if browser_config.enabled {
        server = register_browser_tools(server, browser_config, screenshot_dir, &runtime_config);
    }

    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
    }

    // Clone agent_id for potential use in delegation config (after memory tools may move it).
    let agent_id_for_delegation = agent_id.clone();

    // Conditionally add memory tools based on worker memory mode.
    if worker_memory_mode.recall_enabled() {
        server = server.tool(MemoryRecallTool::new(memory_search.clone()));
    }
    if worker_memory_mode.full_tools_enabled() {
        server = server
            .tool(memory_save_with_events(
                memory_search.clone(),
                agent_id,
                event_tx,
                None,
            ))
            .tool(MemoryDeleteTool::new(memory_search));
    }

    for mcp_tool in mcp_tools {
        server = server.tool(mcp_tool);
    }

    if let Some(config) = delegation_config {
        server = server.tool(SendAgentMessageTool::new(
            agent_id_for_delegation.clone(),
            config.links,
            config.agent_names,
            task_store.clone(),
            config.conversation_logger,
        ));
        server = server.tool(TaskListTool::new(task_store.clone(), agent_id_for_delegation.to_string()));
        server = server.tool(TaskGetTool::new(task_store.clone(), agent_id_for_delegation.clone()));
        server = server.tool(TaskUpdateTool::for_worker(
            task_store.clone(),
            agent_id_for_delegation.clone(),
            worker_id,
        ));
    }

    server.run()
}

/// Create a ToolServer for the cortex process.
///
/// Retained for potential future use. The compactor no longer uses this
/// (Phase 5b removed compactor memory_save).
#[allow(dead_code)]
pub fn create_cortex_tool_server(
    agent_id: AgentId,
    memory_event_tx: broadcast::Sender<ProcessEvent>,
    memory_search: Arc<MemorySearch>,
) -> ToolServerHandle {
    ToolServer::new()
        .tool(memory_save_with_events(
            memory_search,
            agent_id,
            memory_event_tx,
            None,
        ))
        .run()
}

/// Create a ToolServer for cortex chat sessions.
///
/// Combines branch tools (memory) with worker tools (shell, file) to give
/// the interactive cortex full capabilities. Does not include channel-specific
/// tools (reply, react, skip) since the cortex chat doesn't talk to platforms.
/// Adds `config_inspect` for live runtime config introspection and
/// `spacebot_docs` for embedded docs/changelog retrieval.
#[allow(clippy::too_many_arguments)]
pub fn create_cortex_chat_tool_server(
    agent_id: AgentId,
    deps: crate::AgentDeps,
    task_store: Arc<TaskStore>,
    memory_search: Arc<MemorySearch>,
    memory_event_tx: broadcast::Sender<ProcessEvent>,
    conversation_logger: crate::conversation::history::ConversationLogger,
    channel_store: crate::conversation::ChannelStore,
    run_logger: crate::conversation::history::ProcessRunLogger,
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
    runtime_config: Arc<RuntimeConfig>,
    api_state: Arc<crate::api::ApiState>,
    cortex_ctx: Option<crate::tools::spawn_worker::CortexChatContext>,
) -> ToolServerHandle {
    let logs_dir = workspace.join(".spacebot").join("logs");

    let spawn_tool = {
        let tool = DetachedSpawnWorkerTool::new(deps, screenshot_dir.clone(), logs_dir);
        match cortex_ctx {
            Some(ctx) => tool.with_cortex_context(ctx),
            None => tool,
        }
    };

    let mut server = ToolServer::new()
        .tool(memory_save_with_events(
            memory_search.clone(),
            agent_id.clone(),
            memory_event_tx,
            None,
        ))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .tool(SpacebotDocsTool::new())
        .tool(ConfigInspectTool::new(
            agent_id.to_string(),
            runtime_config.clone(),
        ))
        .tool(SkillsSearchTool::new(runtime_config.clone()))
        .tool(InstallSkillTool::new(runtime_config.clone(), api_state))
        .tool(WorkerInspectTool::new(run_logger, agent_id.to_string()))
        .tool(spawn_tool)
        .tool(TaskCreateTool::new(
            task_store.clone(),
            agent_id.to_string(),
            "cortex",
        ))
        .tool(TaskListTool::new(task_store.clone(), agent_id.to_string()))
        .tool(TaskUpdateTool::for_branch(task_store, agent_id.clone()))
        .tool(ShellTool::new(workspace.clone(), sandbox.clone()));

    server = register_file_tools(server, workspace, sandbox);

    if browser_config.enabled {
        server = register_browser_tools(server, browser_config, screenshot_dir, &runtime_config);
    }

    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
    }

    server.run()
}

/// Create a ToolServer for factory conversations (agent creation/refinement).
///
/// Factory tools are system-level — they receive `Arc<ApiState>` directly since
/// they perform cross-agent mutations (creating agents, writing config, creating
/// links). The memory search is from the main agent (the one running the factory
/// conversation) to provide organizational context.
pub fn create_factory_tool_server(
    state: Arc<crate::api::ApiState>,
    memory_search: Arc<MemorySearch>,
) -> ToolServerHandle {
    ToolServer::new()
        .tool(FactoryListPresetsTool::new())
        .tool(FactoryLoadPresetTool::new())
        .tool(FactorySearchContextTool::new(memory_search))
        .tool(FactoryCreateAgentTool::new(state.clone()))
        .tool(FactoryUpdateIdentityTool::new(state.clone()))
        .tool(FactoryUpdateConfigTool::new(state))
        .run()
}

/// Add factory tools (agent creation/refinement) to an existing tool server handle.
///
/// Used to augment the cortex chat tool server with factory capabilities.
/// The `memory_search` should be from the agent running the factory conversation
/// (typically the main agent) so `factory_search_context` queries its memories.
pub async fn add_factory_tools(
    handle: &ToolServerHandle,
    state: Arc<crate::api::ApiState>,
    memory_search: Arc<MemorySearch>,
) -> Result<(), rig::tool::server::ToolServerError> {
    handle.add_tool(FactoryListPresetsTool::new()).await?;
    handle.add_tool(FactoryLoadPresetTool::new()).await?;
    handle
        .add_tool(FactorySearchContextTool::new(memory_search))
        .await?;
    handle
        .add_tool(FactoryCreateAgentTool::new(state.clone()))
        .await?;
    handle
        .add_tool(FactoryUpdateIdentityTool::new(state.clone()))
        .await?;
    handle.add_tool(FactoryUpdateConfigTool::new(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_args_parses_timeout_as_integer() {
        let args: shell::ShellArgs =
            serde_json::from_str(r#"{"command": "ls", "timeout_seconds": 120}"#).unwrap();
        assert_eq!(args.timeout_seconds, 120);
    }

    #[test]
    fn shell_args_parses_timeout_as_string() {
        let args: shell::ShellArgs =
            serde_json::from_str(r#"{"command": "ls", "timeout_seconds": "400"}"#).unwrap();
        assert_eq!(args.timeout_seconds, 400);
    }

    #[test]
    fn shell_args_uses_default_when_timeout_missing() {
        let args: shell::ShellArgs = serde_json::from_str(r#"{"command": "ls"}"#).unwrap();
        assert_eq!(args.timeout_seconds, 60);
    }

    #[test]
    fn shell_args_rejects_non_numeric_string() {
        let result: Result<shell::ShellArgs, _> =
            serde_json::from_str(r#"{"command": "ls", "timeout_seconds": "abc"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn shell_args_parses_env_field() {
        let args: shell::ShellArgs = serde_json::from_str(
            r#"{"command": "echo $FOO", "env": [{"key": "FOO", "value": "bar"}]}"#,
        )
        .unwrap();
        assert_eq!(args.env.len(), 1);
        assert_eq!(args.env[0].key, "FOO");
        assert_eq!(args.env[0].value, "bar");
    }

    #[test]
    fn shell_args_defaults_empty_env() {
        let args: shell::ShellArgs = serde_json::from_str(r#"{"command": "ls"}"#).unwrap();
        assert!(args.env.is_empty());
    }

    #[tokio::test]
    async fn shell_rejects_empty_env_var_name() {
        let config = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::sandbox::SandboxConfig::default(),
        ));
        let workspace = std::env::temp_dir();
        let sandbox = std::sync::Arc::new(crate::sandbox::Sandbox::new_for_test(
            config,
            workspace.clone(),
        ));
        let tool = shell::ShellTool::new(workspace, sandbox);
        let args = shell::ShellArgs {
            command: "echo hi".into(),
            working_dir: None,
            env: vec![shell::EnvVar {
                key: "".into(),
                value: "val".into(),
            }],
            timeout_seconds: 5,
        };
        let result = rig::tool::Tool::call(&tool, args).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("cannot be empty"),
            "should reject empty env var name"
        );
    }

    #[tokio::test]
    async fn shell_rejects_env_var_name_with_equals() {
        let config = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::sandbox::SandboxConfig::default(),
        ));
        let workspace = std::env::temp_dir();
        let sandbox = std::sync::Arc::new(crate::sandbox::Sandbox::new_for_test(
            config,
            workspace.clone(),
        ));
        let tool = shell::ShellTool::new(workspace, sandbox);
        let args = shell::ShellArgs {
            command: "echo hi".into(),
            working_dir: None,
            env: vec![shell::EnvVar {
                key: "FOO=BAR".into(),
                value: "val".into(),
            }],
            timeout_seconds: 5,
        };
        let result = rig::tool::Tool::call(&tool, args).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot contain '='"),
            "should reject env var name containing ="
        );
    }

    #[tokio::test]
    async fn shell_rejects_env_var_with_null_bytes() {
        let config = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::sandbox::SandboxConfig::default(),
        ));
        let workspace = std::env::temp_dir();
        let sandbox = std::sync::Arc::new(crate::sandbox::Sandbox::new_for_test(
            config,
            workspace.clone(),
        ));
        let tool = shell::ShellTool::new(workspace, sandbox);
        let args = shell::ShellArgs {
            command: "echo hi".into(),
            working_dir: None,
            env: vec![shell::EnvVar {
                key: "FOO\0BAR".into(),
                value: "val".into(),
            }],
            timeout_seconds: 5,
        };
        let result = rig::tool::Tool::call(&tool, args).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot contain null"),
            "should reject env var with null bytes"
        );
    }

    #[tokio::test]
    async fn shell_rejects_dangerous_env_var() {
        let config = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::sandbox::SandboxConfig::default(),
        ));
        let workspace = std::env::temp_dir();
        let sandbox = std::sync::Arc::new(crate::sandbox::Sandbox::new_for_test(
            config,
            workspace.clone(),
        ));
        let tool = shell::ShellTool::new(workspace, sandbox);
        let args = shell::ShellArgs {
            command: "echo hi".into(),
            working_dir: None,
            env: vec![shell::EnvVar {
                key: "LD_PRELOAD".into(),
                value: "/evil.so".into(),
            }],
            timeout_seconds: 5,
        };
        let result = rig::tool::Tool::call(&tool, args).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("code injection"),
            "should reject dangerous env var"
        );
    }

    #[tokio::test]
    async fn shell_rejects_dangerous_env_var_case_insensitive() {
        let config = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::sandbox::SandboxConfig::default(),
        ));
        let workspace = std::env::temp_dir();
        let sandbox = std::sync::Arc::new(crate::sandbox::Sandbox::new_for_test(
            config,
            workspace.clone(),
        ));
        let tool = shell::ShellTool::new(workspace, sandbox);
        let args = shell::ShellArgs {
            command: "echo hi".into(),
            working_dir: None,
            env: vec![shell::EnvVar {
                key: "ld_preload".into(),
                value: "/evil.so".into(),
            }],
            timeout_seconds: 5,
        };
        let result = rig::tool::Tool::call(&tool, args).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("code injection"),
            "should reject dangerous env var regardless of case"
        );
    }

    #[test]
    fn blocks_json_bracket_and_tool_syntax_output() {
        assert!(should_block_user_visible_text("{\"content\":\"hello\"}"));
        assert!(should_block_user_visible_text("[\"one\",\"two\"]"));
        assert!(should_block_user_visible_text(
            "(just commentary - skipping)"
        ));
        assert!(should_block_user_visible_text(
            "(Empty response: {'content': [{'type': 'thinking'}]})"
        ));
        assert!(should_block_user_visible_text("skip"));
        assert!(should_block_user_visible_text("  SKIP  "));
        assert!(should_block_user_visible_text(
            "[reply]\n{\"content\":\"hello\"}"
        ));
        assert!(should_block_user_visible_text(
            "<system-reminder>hidden</system-reminder>"
        ));
    }

    #[test]
    fn allows_normal_plaintext_output() {
        assert!(!should_block_user_visible_text("hello team"));
        assert!(!should_block_user_visible_text("skipping for now"));
        assert!(!should_block_user_visible_text(
            "I can skip that if you want."
        ));
        assert!(!should_block_user_visible_text("- first\n- second"));
    }

    #[test]
    fn truncate_helpers_preserve_utf8_boundaries() {
        let text = "🙂🙂🙂";
        // For max_bytes=5: can fit "🙂" (4 bytes) but not "🙂..." (7 bytes)
        // So we get just "🙂" without ellipsis since it wouldn't fit
        assert_eq!(truncate_utf8_ellipsis(text, 5), "🙂");
        // For larger max_bytes=10: can fit "🙂..." (7 bytes)
        assert_eq!(truncate_utf8_ellipsis(text, 10), "🙂...");
        assert!(truncate_output(text, 5).starts_with("🙂"));
    }

    #[test]
    fn truncate_cyrillic_does_not_panic() {
        // Cyrillic chars are 2 bytes each in UTF-8
        let text = "Привет, мир!"; // "Hello, world!" in Russian
        // Cutting at byte 5 would land inside 'и' (bytes 4..5) - this must not panic
        let result = truncate_utf8_ellipsis(text, 5);
        assert!(!result.is_empty());
        // "П" (2 bytes) + "..." (3 bytes) = 5, fits exactly
        assert_eq!(result, "П...");

        let result = truncate_utf8_ellipsis(text, 10);
        assert!(!result.is_empty());
        // Should truncate to a valid char boundary and append "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_cjk_does_not_panic() {
        // CJK chars are 3 bytes each in UTF-8
        let text = "你好世界测试"; // "Hello world test" in Chinese
        // Cutting at byte 4 would land inside '好' (bytes 3..5)
        let result = truncate_utf8_ellipsis(text, 4);
        assert_eq!(result, "你"); // only 3 bytes fit, no room for "..."

        let result = truncate_utf8_ellipsis(text, 10);
        assert!(!result.is_empty());
        // 2 CJK chars (6 bytes) + "..." (3 bytes) = 9 bytes fits in 10
        assert_eq!(result, "你好...");
    }

    #[test]
    fn truncate_emoji_does_not_panic() {
        // Emoji are 4 bytes each in UTF-8
        let text = "Hello 😀🎉🚀 World";
        let result = truncate_utf8_ellipsis(text, 10);
        assert!(!result.is_empty());
        // "Hello " (6 bytes) + "😀" won't fit with "..." in 10 bytes
        // "Hello " (6 bytes) + "..." (3 bytes) = 9 bytes fits
        assert_eq!(result, "Hello ...");

        // Larger budget
        let result = truncate_utf8_ellipsis(text, 15);
        assert!(!result.is_empty());
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_mixed_multibyte_content() {
        // Mix of ASCII, Cyrillic (2-byte), CJK (3-byte), and emoji (4-byte)
        let text = "Hi Привет 你好 😀";
        let result = truncate_utf8_ellipsis(text, 20);
        assert!(!result.is_empty());
        assert!(result.ends_with("..."));
        // Verify it's valid UTF-8 by iterating chars
        assert!(result.chars().count() > 0);
    }

    #[test]
    fn truncate_output_multibyte_does_not_panic() {
        let cyrillic = "Привет, мир! Это тестовая строка для проверки.";
        let result = truncate_output(cyrillic, 15);
        assert!(!result.is_empty());

        let cjk = "你好世界，这是一个测试字符串。";
        let result = truncate_output(cjk, 10);
        assert!(!result.is_empty());

        let emoji = "🎉🚀😀🌍💻🔥";
        let result = truncate_output(emoji, 6);
        assert!(!result.is_empty());
    }

    #[test]
    fn truncate_at_exact_char_boundary_works() {
        let text = "абв"; // 3 Cyrillic chars, 6 bytes total
        // Exactly at a char boundary (4 bytes = 2 chars)
        let result = truncate_utf8_ellipsis(text, 6);
        assert_eq!(result, "абв"); // fits entirely

        let result = truncate_utf8_ellipsis(text, 7);
        assert_eq!(result, "абв"); // also fits, no truncation needed
    }
}
