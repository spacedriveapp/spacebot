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
//! - `task_create` + `task_list` + `task_update`
//! - `spawn_worker` is included for channel-originated branches only
//!
//! **Worker ToolServer** (one per worker, created at spawn time):
//! - `shell`, `file`, `exec` — stateless, registered at creation
//! - `task_update` — scoped to the worker's assigned task
//! - `set_status` — per-worker instance, registered at creation
//!
//! **Cortex ToolServer** (one per agent):
//! - `memory_save` — registered at startup

pub mod branch_tool;
pub mod browser;
pub mod cancel;
pub mod channel_recall;
pub mod cron;
pub mod email_search;
pub mod exec;
pub mod file;
pub mod mcp;
pub mod memory_delete;
pub mod memory_recall;
pub mod memory_save;
pub mod react;
pub mod read_skill;
pub mod reply;
pub mod route;
pub mod send_agent_message;
pub mod send_file;
pub mod send_message_to_another_channel;
pub mod set_status;
pub mod shell;
pub mod skip;
pub mod spawn_worker;
pub mod task_create;
pub mod task_list;
pub mod task_update;
pub mod web_search;
pub mod worker_inspect;

pub use branch_tool::{BranchArgs, BranchError, BranchOutput, BranchTool};
pub use browser::{
    ActKind, BrowserAction, BrowserArgs, BrowserError, BrowserOutput, BrowserTool, ElementSummary,
    TabInfo,
};
pub use cancel::{CancelArgs, CancelError, CancelOutput, CancelTool};
pub use channel_recall::{
    ChannelRecallArgs, ChannelRecallError, ChannelRecallOutput, ChannelRecallTool,
};
pub use cron::{CronArgs, CronError, CronOutput, CronTool};
pub use email_search::{EmailSearchArgs, EmailSearchError, EmailSearchOutput, EmailSearchTool};
pub use exec::{EnvVar, ExecArgs, ExecError, ExecOutput, ExecResult, ExecTool};
pub use file::{FileArgs, FileEntry, FileEntryOutput, FileError, FileOutput, FileTool, FileType};
pub use mcp::{McpToolAdapter, McpToolError, McpToolOutput};
pub use memory_delete::{
    MemoryDeleteArgs, MemoryDeleteError, MemoryDeleteOutput, MemoryDeleteTool,
};
pub use memory_recall::{
    MemoryOutput, MemoryRecallArgs, MemoryRecallError, MemoryRecallOutput, MemoryRecallTool,
};
pub use memory_save::{
    AssociationInput, MemorySaveArgs, MemorySaveError, MemorySaveOutput, MemorySaveTool,
};
pub use react::{ReactArgs, ReactError, ReactOutput, ReactTool};
pub use read_skill::{ReadSkillArgs, ReadSkillError, ReadSkillOutput, ReadSkillTool};
pub use reply::{RepliedFlag, ReplyArgs, ReplyError, ReplyOutput, ReplyTool, new_replied_flag};
pub use route::{RouteArgs, RouteError, RouteOutput, RouteTool};
pub use send_agent_message::{
    SendAgentMessageArgs, SendAgentMessageError, SendAgentMessageOutput, SendAgentMessageTool,
};
pub use send_file::{SendFileArgs, SendFileError, SendFileOutput, SendFileTool};
pub use send_message_to_another_channel::{
    SendMessageArgs, SendMessageError, SendMessageOutput, SendMessageTool,
};
pub use set_status::{SetStatusArgs, SetStatusError, SetStatusOutput, SetStatusTool};
pub use shell::{ShellArgs, ShellError, ShellOutput, ShellResult, ShellTool};
pub use skip::{SkipArgs, SkipError, SkipFlag, SkipOutput, SkipTool, new_skip_flag};
pub use spawn_worker::{SpawnWorkerArgs, SpawnWorkerError, SpawnWorkerOutput, SpawnWorkerTool};
pub use task_create::{TaskCreateArgs, TaskCreateError, TaskCreateOutput, TaskCreateTool};
pub use task_list::{TaskListArgs, TaskListError, TaskListOutput, TaskListTool};
pub use task_update::{TaskUpdateArgs, TaskUpdateError, TaskUpdateOutput, TaskUpdateTool};
pub use web_search::{SearchResult, WebSearchArgs, WebSearchError, WebSearchOutput, WebSearchTool};
pub use worker_inspect::{
    WorkerInspectArgs, WorkerInspectError, WorkerInspectOutput, WorkerInspectTool,
};

use crate::agent::channel::ChannelState;
use crate::config::{BrowserConfig, RuntimeConfig};
use crate::memory::MemorySearch;
use crate::sandbox::Sandbox;
use crate::tasks::TaskStore;
use crate::{AgentId, ChannelId, OutboundResponse, ProcessEvent, WorkerId};
use rig::tool::Tool as _;
use rig::tool::server::{ToolServer, ToolServerHandle};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

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
pub fn truncate_output(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    // Find the last char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    let total = value.len();
    let truncated_bytes = total - end;
    format!(
        "{}\n\n[output truncated: showed {end} of {total} bytes ({truncated_bytes} bytes omitted). \
         Use head/tail/offset to read specific sections]",
        &value[..end]
    )
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

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    if TOOL_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
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
    response_tx: mpsc::Sender<OutboundResponse>,
    conversation_id: impl Into<String>,
    skip_flag: SkipFlag,
    replied_flag: RepliedFlag,
    cron_tool: Option<CronTool>,
    send_agent_message_tool: Option<SendAgentMessageTool>,
    allow_direct_reply: bool,
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
            ))
            .await?;
    }
    handle
        .add_tool(SendFileTool::new(
            response_tx.clone(),
            state.deps.runtime_config.workspace_dir.clone(),
        ))
        .await?;
    handle.add_tool(CancelTool::new(state)).await?;
    handle
        .add_tool(SkipTool::new(skip_flag.clone(), response_tx.clone()))
        .await?;
    handle.add_tool(ReactTool::new(response_tx.clone())).await?;
    if let Some(cron_tool) = cron_tool {
        let cron_tool = cron_tool.with_default_delivery_target(
            default_delivery_target_for_conversation(&conversation_id),
        );
        handle.add_tool(cron_tool).await?;
    }
    if let Some(mut agent_msg) = send_agent_message_tool {
        agent_msg = agent_msg.with_skip_flag(skip_flag.clone());
        handle.add_tool(agent_msg).await?;
    }
    Ok(())
}

fn default_delivery_target_for_conversation(conversation_id: &str) -> Option<String> {
    let parsed = crate::messaging::target::parse_delivery_target(conversation_id)?;
    if parsed.adapter != "discord" {
        return None;
    }
    Some(parsed.to_string())
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
    // Cron, send_message, and send_agent_message removal is best-effort since not all channels have them
    let _ = handle.remove_tool(CronTool::NAME).await;
    let _ = handle.remove_tool(SendMessageTool::NAME).await;
    let _ = handle.remove_tool(SendAgentMessageTool::NAME).await;
    Ok(())
}

/// Create a per-branch ToolServer with memory tools.
///
/// Each branch gets its own isolated ToolServer so `memory_recall` is never
/// visible to the channel. Both `memory_save` and `memory_recall` are
/// registered at creation.
#[allow(clippy::too_many_arguments)]
pub fn create_branch_tool_server(
    state: Option<ChannelState>,
    agent_id: AgentId,
    task_store: Arc<TaskStore>,
    memory_search: Arc<MemorySearch>,
    runtime_config: Arc<RuntimeConfig>,
    conversation_logger: crate::conversation::history::ConversationLogger,
    channel_store: crate::conversation::ChannelStore,
    run_logger: crate::conversation::history::ProcessRunLogger,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .tool(EmailSearchTool::new(runtime_config))
        .tool(WorkerInspectTool::new(run_logger, agent_id.to_string()))
        .tool(TaskCreateTool::new(
            task_store.clone(),
            agent_id.to_string(),
            "branch",
        ))
        .tool(TaskListTool::new(task_store.clone(), agent_id.to_string()))
        .tool(TaskUpdateTool::for_branch(task_store, agent_id.clone()));

    if let Some(state) = state {
        server = server.tool(SpawnWorkerTool::new(state));
    }

    server.run()
}

/// Create a per-worker ToolServer with task-appropriate tools.
///
/// Each worker gets its own isolated ToolServer. The `set_status` tool is bound to
/// the specific worker's ID so status updates route correctly. The browser tool
/// is included when browser automation is enabled in the agent config.
///
/// Shell and exec commands are sandboxed via the `Sandbox` backend.
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
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(ShellTool::new(workspace.clone(), sandbox.clone()))
        .tool(FileTool::new(workspace.clone()))
        .tool(ExecTool::new(workspace, sandbox))
        .tool(TaskUpdateTool::for_worker(
            task_store,
            agent_id.clone(),
            worker_id,
        ))
        .tool(SetStatusTool::new(
            agent_id, worker_id, channel_id, event_tx,
        ))
        .tool(ReadSkillTool::new(runtime_config));

    if browser_config.enabled {
        server = server.tool(BrowserTool::new(browser_config, screenshot_dir));
    }

    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
    }

    for mcp_tool in mcp_tools {
        server = server.tool(mcp_tool);
    }

    server.run()
}

/// Create a ToolServer for the cortex process.
///
/// The cortex only needs memory_save for consolidation. Additional tools can be
/// added later as cortex capabilities expand.
pub fn create_cortex_tool_server(memory_search: Arc<MemorySearch>) -> ToolServerHandle {
    ToolServer::new()
        .tool(MemorySaveTool::new(memory_search))
        .run()
}

/// Create a ToolServer for cortex chat sessions.
///
/// Combines branch tools (memory) with worker tools (shell, file, exec) to give
/// the interactive cortex full capabilities. Does not include channel-specific
/// tools (reply, react, skip) since the cortex chat doesn't talk to platforms.
#[allow(clippy::too_many_arguments)]
pub fn create_cortex_chat_tool_server(
    agent_id: AgentId,
    task_store: Arc<TaskStore>,
    memory_search: Arc<MemorySearch>,
    conversation_logger: crate::conversation::history::ConversationLogger,
    channel_store: crate::conversation::ChannelStore,
    run_logger: crate::conversation::history::ProcessRunLogger,
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .tool(WorkerInspectTool::new(run_logger, agent_id.to_string()))
        .tool(TaskCreateTool::new(
            task_store.clone(),
            agent_id.to_string(),
            "cortex",
        ))
        .tool(TaskListTool::new(task_store.clone(), agent_id.to_string()))
        .tool(TaskUpdateTool::for_branch(task_store, agent_id.clone()))
        .tool(ShellTool::new(workspace.clone(), sandbox.clone()))
        .tool(FileTool::new(workspace.clone()))
        .tool(ExecTool::new(workspace, sandbox));

    if browser_config.enabled {
        server = server.tool(BrowserTool::new(browser_config, screenshot_dir));
    }

    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
    }

    server.run()
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
    fn exec_args_parses_timeout_as_string() {
        let args: exec::ExecArgs =
            serde_json::from_str(r#"{"program": "/bin/ls", "timeout_seconds": "300"}"#).unwrap();
        assert_eq!(args.timeout_seconds, 300);
    }

    #[test]
    fn exec_args_parses_timeout_as_integer() {
        let args: exec::ExecArgs =
            serde_json::from_str(r#"{"program": "/bin/ls", "timeout_seconds": 90}"#).unwrap();
        assert_eq!(args.timeout_seconds, 90);
    }

    #[test]
    fn blocks_json_bracket_and_tool_syntax_output() {
        assert!(should_block_user_visible_text("{\"content\":\"hello\"}"));
        assert!(should_block_user_visible_text("[\"one\",\"two\"]"));
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
        assert!(!should_block_user_visible_text("- first\n- second"));
    }
}
