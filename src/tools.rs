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
//! - `memory_save` + `memory_recall` — registered at creation
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
pub mod set_status;
pub mod shell;
pub mod file;
pub mod exec;
pub mod browser;
pub mod web_search;
pub mod heartbeat;

pub use reply::{ReplyTool, ReplyArgs, ReplyOutput, ReplyError};
pub use branch_tool::{BranchTool, BranchArgs, BranchOutput, BranchError};
pub use spawn_worker::{SpawnWorkerTool, SpawnWorkerArgs, SpawnWorkerOutput, SpawnWorkerError};
pub use route::{RouteTool, RouteArgs, RouteOutput, RouteError};
pub use cancel::{CancelTool, CancelArgs, CancelOutput, CancelError};
pub use skip::{SkipTool, SkipArgs, SkipOutput, SkipError, SkipFlag, new_skip_flag};
pub use react::{ReactTool, ReactArgs, ReactOutput, ReactError};
pub use memory_save::{MemorySaveTool, MemorySaveArgs, MemorySaveOutput, MemorySaveError, AssociationInput};
pub use memory_recall::{MemoryRecallTool, MemoryRecallArgs, MemoryRecallOutput, MemoryRecallError, MemoryOutput};
pub use set_status::{SetStatusTool, SetStatusArgs, SetStatusOutput, SetStatusError};
pub use shell::{ShellTool, ShellArgs, ShellOutput, ShellError, ShellResult};
pub use file::{FileTool, FileArgs, FileOutput, FileError, FileEntryOutput, FileEntry, FileType};
pub use exec::{ExecTool, ExecArgs, ExecOutput, ExecError, ExecResult, EnvVar};
pub use browser::{BrowserTool, BrowserArgs, BrowserOutput, BrowserError, BrowserAction, ActKind, ElementSummary, TabInfo};
pub use web_search::{WebSearchTool, WebSearchArgs, WebSearchOutput, WebSearchError, SearchResult};
pub use heartbeat::{HeartbeatTool, HeartbeatArgs, HeartbeatOutput, HeartbeatError};

use crate::agent::channel::ChannelState;
use crate::config::BrowserConfig;
use crate::memory::MemorySearch;
use crate::{AgentId, ChannelId, OutboundResponse, ProcessEvent, WorkerId};
use rig::tool::Tool as _;
use rig::tool::server::{ToolServer, ToolServerHandle};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Create the ToolServer for a channel.
///
/// Starts empty. Channel-specific tools (reply, branch, spawn_worker, route,
/// cancel, skip, react) are added per conversation turn. Memory tools live on
/// branch and cortex servers only — the channel delegates memory work.
pub fn create_channel_tool_server() -> ToolServerHandle {
    ToolServer::new()
        .run()
}

/// Add per-channel tools to a running ToolServer.
///
/// Called when a conversation turn begins. These tools hold per-channel state
/// (channel history, active branches/workers, response sender) that can't be
/// known at agent startup. Cleaned up via `remove_channel_tools()` when the
/// turn ends.
pub async fn add_channel_tools(
    handle: &ToolServerHandle,
    state: ChannelState,
    response_tx: mpsc::Sender<OutboundResponse>,
    conversation_id: impl Into<String>,
    skip_flag: SkipFlag,
) -> Result<(), rig::tool::server::ToolServerError> {
    handle.add_tool(ReplyTool::new(
        response_tx.clone(),
        conversation_id,
        state.conversation_logger.clone(),
        state.channel_id.clone(),
    )).await?;
    handle.add_tool(BranchTool::new(state.clone())).await?;
    handle.add_tool(SpawnWorkerTool::new(state.clone())).await?;
    handle.add_tool(RouteTool::new(state.clone())).await?;
    handle.add_tool(CancelTool::new(state)).await?;
    handle.add_tool(SkipTool::new(skip_flag, response_tx.clone())).await?;
    handle.add_tool(ReactTool::new(response_tx)).await?;
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
    handle.remove_tool(ReactTool::NAME).await?;
    Ok(())
}

/// Create a per-branch ToolServer with memory tools.
///
/// Each branch gets its own isolated ToolServer so `memory_recall` is never
/// visible to the channel. Both `memory_save` and `memory_recall` are
/// registered at creation.
pub fn create_branch_tool_server(memory_search: Arc<MemorySearch>) -> ToolServerHandle {
    ToolServer::new()
        .tool(MemorySaveTool::new(memory_search.clone()))
        .tool(MemoryRecallTool::new(memory_search))
        .run()
}

/// Create a per-worker ToolServer with task-appropriate tools.
///
/// Each worker gets its own isolated ToolServer. The `set_status` tool is bound to
/// the specific worker's ID so status updates route correctly. The browser tool
/// is included when browser automation is enabled in the agent config.
pub fn create_worker_tool_server(
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
    browser_config: BrowserConfig,
    screenshot_dir: PathBuf,
    brave_search_key: Option<String>,
) -> ToolServerHandle {
    let mut server = ToolServer::new()
        .tool(ShellTool::new())
        .tool(FileTool::new())
        .tool(ExecTool::new())
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
