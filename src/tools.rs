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
//! - `memory_save` + `memory_recall` + `memory_delete` — registered at creation
//!
//! **Worker ToolServer** (one per worker, created at spawn time):
//! - `shell`, `file`, `exec` — stateless, registered at creation
//! - `set_status` — per-worker instance, registered at creation
//!
//! **Cortex ToolServer** (one per agent):
//! - `memory_save` — registered at startup

pub mod reply;
pub mod branch_tool;
pub mod spawn_worker;
pub mod route;
pub mod cancel;
pub mod skip;
pub mod react;
pub mod memory_save;
pub mod memory_recall;
pub mod memory_delete;
pub mod set_status;
pub mod shell;
pub mod file;
pub mod exec;
pub mod browser;
pub mod web_search;
pub mod channel_recall;
pub mod cron;
pub mod send_file;
pub mod send_message_to_another_channel;

pub use reply::{ReplyTool, ReplyArgs, ReplyOutput, ReplyError};
pub use branch_tool::{BranchTool, BranchArgs, BranchOutput, BranchError};
pub use spawn_worker::{SpawnWorkerTool, SpawnWorkerArgs, SpawnWorkerOutput, SpawnWorkerError};
pub use route::{RouteTool, RouteArgs, RouteOutput, RouteError};
pub use cancel::{CancelTool, CancelArgs, CancelOutput, CancelError};
pub use skip::{SkipTool, SkipArgs, SkipOutput, SkipError, SkipFlag, new_skip_flag};
pub use react::{ReactTool, ReactArgs, ReactOutput, ReactError};
pub use memory_save::{MemorySaveTool, MemorySaveArgs, MemorySaveOutput, MemorySaveError, AssociationInput};
pub use memory_recall::{MemoryRecallTool, MemoryRecallArgs, MemoryRecallOutput, MemoryRecallError, MemoryOutput};
pub use memory_delete::{MemoryDeleteTool, MemoryDeleteArgs, MemoryDeleteOutput, MemoryDeleteError};
pub use set_status::{SetStatusTool, SetStatusArgs, SetStatusOutput, SetStatusError};
pub use shell::{ShellTool, ShellArgs, ShellOutput, ShellError, ShellResult};
pub use file::{FileTool, FileArgs, FileOutput, FileError, FileEntryOutput, FileEntry, FileType};
pub use exec::{ExecTool, ExecArgs, ExecOutput, ExecError, ExecResult, EnvVar};
pub use browser::{BrowserTool, BrowserArgs, BrowserOutput, BrowserError, BrowserAction, ActKind, ElementSummary, TabInfo};
pub use web_search::{WebSearchTool, WebSearchArgs, WebSearchOutput, WebSearchError, SearchResult};
pub use channel_recall::{ChannelRecallTool, ChannelRecallArgs, ChannelRecallOutput, ChannelRecallError};
pub use cron::{CronTool, CronArgs, CronOutput, CronError};
pub use send_file::{SendFileTool, SendFileArgs, SendFileOutput, SendFileError};
pub use send_message_to_another_channel::{SendMessageTool, SendMessageArgs, SendMessageOutput, SendMessageError};

use crate::agent::channel::ChannelState;
use crate::config::BrowserConfig;
use crate::memory::MemorySearch;
use crate::{AgentId, ChannelId, OutboundResponse, ProcessEvent, WorkerId};
use rig::tool::Tool as _;
use rig::tool::server::{ToolServer, ToolServerHandle};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

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

/// Add per-turn tools to a channel's ToolServer.
///
/// Called when a conversation turn begins. These tools hold per-turn state
/// (response sender, skip flag) that changes between turns. Cleaned up via
/// `remove_channel_tools()` when the turn ends.
pub async fn add_channel_tools(
    handle: &ToolServerHandle,
    state: ChannelState,
    response_tx: mpsc::Sender<OutboundResponse>,
    conversation_id: impl Into<String>,
    skip_flag: SkipFlag,
    cron_tool: Option<CronTool>,
) -> Result<(), rig::tool::server::ToolServerError> {
    handle.add_tool(ReplyTool::new(
        response_tx.clone(),
        conversation_id,
        state.conversation_logger.clone(),
        state.channel_id.clone(),
        skip_flag.clone(),
    )).await?;
    handle.add_tool(BranchTool::new(state.clone())).await?;
    handle.add_tool(SpawnWorkerTool::new(state.clone())).await?;
    handle.add_tool(RouteTool::new(state.clone())).await?;
    if let Some(messaging_manager) = &state.deps.messaging_manager {
        handle.add_tool(SendMessageTool::new(
            messaging_manager.clone(),
            state.channel_store.clone(),
        )).await?;
    }
    handle.add_tool(CancelTool::new(state)).await?;
    handle.add_tool(SkipTool::new(skip_flag, response_tx.clone())).await?;
    handle.add_tool(SendFileTool::new(response_tx.clone())).await?;
    handle.add_tool(ReactTool::new(response_tx)).await?;
    if let Some(cron) = cron_tool {
        handle.add_tool(cron).await?;
    }
    Ok(())
}

/// Remove per-channel tools from a running ToolServer.
///
/// Called when a conversation turn ends or a channel is torn down. Prevents stale
/// tools from being invoked with dead senders.
pub async fn remove_channel_tools(
    handle: &ToolServerHandle,
) -> Result<(), rig::tool::server::ToolServerError> {
    handle.remove_tool(ReplyTool::NAME).await?;
    handle.remove_tool(BranchTool::NAME).await?;
    handle.remove_tool(SpawnWorkerTool::NAME).await?;
    handle.remove_tool(RouteTool::NAME).await?;
    handle.remove_tool(CancelTool::NAME).await?;
    handle.remove_tool(SkipTool::NAME).await?;
    handle.remove_tool(SendFileTool::NAME).await?;
    handle.remove_tool(ReactTool::NAME).await?;
    // Cron and send_message removal is best-effort since not all channels have them
    let _ = handle.remove_tool(CronTool::NAME).await;
    let _ = handle.remove_tool(SendMessageTool::NAME).await;
    Ok(())
}

/// Create a per-branch ToolServer with memory tools.
///
/// Each branch gets its own isolated ToolServer so `memory_recall` is never
/// visible to the channel. Both `memory_save` and `memory_recall` are
/// registered at creation.
pub fn create_branch_tool_server(
    memory_search: Arc<MemorySearch>,
    conversation_logger: crate::conversation::history::ConversationLogger,
    channel_store: crate::conversation::ChannelStore,
) -> ToolServerHandle {
    ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .run()
}

/// Create a per-worker ToolServer with task-appropriate tools.
///
/// Each worker gets its own isolated ToolServer. The `set_status` tool is bound to
/// the specific worker's ID so status updates route correctly. The browser tool
/// is included when browser automation is enabled in the agent config.
///
/// File operations are restricted to `workspace`. Shell and exec commands are
/// blocked from accessing sensitive files in `instance_dir`.
pub fn create_worker_tool_server(
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
    workspace: PathBuf,
    instance_dir: PathBuf,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(ShellTool::new(instance_dir.clone(), workspace.clone()))
        .tool(FileTool::new(workspace.clone()))
        .tool(ExecTool::new(instance_dir, workspace))
        .tool(SetStatusTool::new(agent_id, worker_id, channel_id, event_tx));

    if browser_config.enabled {
        server = server.tool(BrowserTool::new(browser_config, screenshot_dir));
    }

    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
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
pub fn create_cortex_chat_tool_server(
    memory_search: Arc<MemorySearch>,
    conversation_logger: crate::conversation::history::ConversationLogger,
    channel_store: crate::conversation::ChannelStore,
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
    workspace: PathBuf,
    instance_dir: PathBuf,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search.clone()))
        .tool(MemoryDeleteTool::new(memory_search))
        .tool(ChannelRecallTool::new(conversation_logger, channel_store))
        .tool(ShellTool::new(instance_dir.clone(), workspace.clone()))
        .tool(FileTool::new(workspace.clone()))
        .tool(ExecTool::new(instance_dir, workspace));

    if browser_config.enabled {
        server = server.tool(BrowserTool::new(browser_config, screenshot_dir));
    }

    if let Some(key) = brave_search_key {
        server = server.tool(WebSearchTool::new(key));
    }

    server.run()
}
