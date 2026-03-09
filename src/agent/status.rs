//! StatusBlock: Live status snapshot for channels.

use crate::{BranchId, ProcessEvent, ProcessId, WorkerId};
use chrono::{DateTime, Utc};

/// Static system configuration snapshot injected into the status block.
///
/// Assembled from `RuntimeConfig` each turn and rendered as a compact
/// key-value section at the top of the status block. Gives the channel
/// LLM self-awareness about its own models, limits, and capabilities.
#[derive(Debug, Clone, Default)]
pub struct SystemInfo {
    /// Binary version string (e.g. "0.9.2").
    pub version: String,
    /// Deployment kind: "native", "docker", or "hosted".
    pub deployment: String,
    /// Model assigned to the channel process.
    pub channel_model: String,
    /// Model assigned to branch processes.
    pub branch_model: String,
    /// Model assigned to worker processes.
    pub worker_model: String,
    /// Thinking effort for the channel (e.g. "auto", "low", "high").
    pub channel_thinking: String,
    /// Thinking effort for workers.
    pub worker_thinking: String,
    /// Context window size in tokens.
    pub context_window: usize,
    /// Maximum concurrent workers allowed.
    pub max_workers: usize,
    /// Maximum concurrent branches allowed.
    pub max_branches: usize,
    /// Enabled capability flags (e.g. "browser", "web_search", "opencode").
    pub capabilities: Vec<String>,
    /// Names of connected MCP servers.
    pub mcp_servers: Vec<String>,
    /// Whether sandbox containment is active.
    pub sandbox_active: bool,
    /// Warmup state label (e.g. "warm", "cold", "degraded").
    pub warmup_state: String,
    /// Whether embeddings are loaded and ready.
    pub embedding_ready: bool,
    /// Age of the memory bulletin in minutes, if known.
    pub bulletin_age_minutes: Option<u64>,
    /// Number of registered cron jobs, if known.
    pub cron_job_count: Option<usize>,
}

impl SystemInfo {
    /// Build a system info snapshot from runtime config.
    ///
    /// This is the synchronous base — it populates everything that can be
    /// read without async (no cron count, no MCP tool names). Channels
    /// augment this with async-only fields via `build_system_info`.
    pub fn from_runtime_config(
        rc: &crate::config::RuntimeConfig,
        sandbox: &crate::sandbox::Sandbox,
    ) -> Self {
        let routing = rc.routing.load();

        let mut capabilities = Vec::new();
        if rc.browser_config.load().enabled {
            capabilities.push("browser".to_string());
        }
        if rc.brave_search_key.load().is_some() {
            capabilities.push("web_search".to_string());
        }
        if rc.opencode.load().enabled {
            capabilities.push("opencode".to_string());
        }

        let mcp_servers: Vec<String> = rc
            .mcp
            .load()
            .iter()
            .filter(|server| server.enabled)
            .map(|server| server.name.clone())
            .collect();

        let warmup_status = rc.warmup_status.load();
        let warmup_state = match warmup_status.state {
            crate::config::WarmupState::Cold => "cold",
            crate::config::WarmupState::Warming => "warming",
            crate::config::WarmupState::Warm => "warm",
            crate::config::WarmupState::Degraded => "degraded",
        }
        .to_string();

        let bulletin_age_minutes = warmup_status.bulletin_age_secs.map(|secs| secs / 60);

        Self {
            version: crate::update::CURRENT_VERSION.to_string(),
            deployment: match crate::update::Deployment::detect() {
                crate::update::Deployment::Docker => "docker",
                crate::update::Deployment::Hosted => "hosted",
                crate::update::Deployment::Native => "native",
            }
            .to_string(),
            channel_model: routing.channel.clone(),
            branch_model: routing.branch.clone(),
            worker_model: routing.worker.clone(),
            channel_thinking: routing.channel_thinking_effort.clone(),
            worker_thinking: routing.worker_thinking_effort.clone(),
            context_window: **rc.context_window.load(),
            max_workers: **rc.max_concurrent_workers.load(),
            max_branches: **rc.max_concurrent_branches.load(),
            capabilities,
            mcp_servers,
            sandbox_active: sandbox.containment_active(),
            warmup_state,
            embedding_ready: warmup_status.embedding_ready,
            bulletin_age_minutes,
            cron_job_count: None,
        }
    }

    /// Render a compact status string suitable for worker system prompts.
    ///
    /// Workers get a lighter version: just time + model + context window.
    /// No warmup, no cron, no bulletin — they don't need it.
    pub fn render_for_worker(&self, current_time_line: &str) -> String {
        let mut output = String::from("## System\n");
        output.push_str(&format!("Time: {current_time_line}\n"));
        output.push_str(&format!("Model: {}\n", self.worker_model));
        output.push_str(&format!("Context: {} tokens\n", self.context_window));
        output
    }
}

/// Live status block injected into channel context.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StatusBlock {
    /// Currently running branches.
    pub active_branches: Vec<BranchStatus>,
    /// Currently running workers.
    pub active_workers: Vec<WorkerStatus>,
    /// Recently completed work.
    pub completed_items: Vec<CompletedItem>,
    /// Active link conversations with other agents.
    pub active_link_conversations: Vec<LinkConversationStatus>,
}

/// Status of an active branch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BranchStatus {
    pub id: BranchId,
    pub started_at: DateTime<Utc>,
    pub description: String,
}

/// Status of an active worker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerStatus {
    pub id: WorkerId,
    pub task: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub notify_on_complete: bool,
    pub tool_calls: usize,
    /// Whether this worker accepts follow-up input via route.
    pub interactive: bool,
}

/// Recently completed work item.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletedItem {
    pub id: String,
    pub item_type: CompletedItemType,
    pub description: String,
    pub completed_at: DateTime<Utc>,
    pub result_summary: String,
    /// Whether this item's result has been relayed to the user via retrigger.
    /// Once relayed, the result summary is excluded from the status block to
    /// prevent the LLM from re-summarising stale results.
    pub relayed: bool,
}

/// Status of an active link conversation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkConversationStatus {
    pub peer_agent: String,
    pub started_at: DateTime<Utc>,
    pub turn_count: u32,
}

/// Type of completed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CompletedItemType {
    Branch,
    Worker,
}

impl StatusBlock {
    /// Create a new empty status block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update from a process event.
    pub fn update(&mut self, event: &ProcessEvent) {
        match event {
            ProcessEvent::WorkerStatus {
                worker_id, status, ..
            } => {
                // Update existing worker or add new one
                if let Some(worker) = self.active_workers.iter_mut().find(|w| w.id == *worker_id) {
                    worker.status.clone_from(status);
                }
            }
            ProcessEvent::WorkerIdle { worker_id, .. } => {
                if let Some(worker) = self.active_workers.iter_mut().find(|w| w.id == *worker_id) {
                    worker.status = "idle".to_string();
                }
            }
            ProcessEvent::WorkerComplete {
                worker_id,
                result,
                notify,
                ..
            } => {
                // Remove from active, add to completed
                if let Some(pos) = self.active_workers.iter().position(|w| w.id == *worker_id) {
                    let worker = self.active_workers.remove(pos);

                    if *notify {
                        self.completed_items.push(CompletedItem {
                            id: worker_id.to_string(),
                            item_type: CompletedItemType::Worker,
                            description: worker.task,
                            completed_at: Utc::now(),
                            result_summary: result.clone(),
                            relayed: false,
                        });
                    }
                }
            }
            ProcessEvent::ToolCompleted {
                process_id: ProcessId::Worker(worker_id),
                ..
            } => {
                if let Some(worker) = self.active_workers.iter_mut().find(|w| w.id == *worker_id) {
                    worker.tool_calls += 1;
                }
            }
            ProcessEvent::BranchResult {
                branch_id,
                conclusion,
                ..
            } => {
                // Remove from active branches, add to completed
                if let Some(pos) = self.active_branches.iter().position(|b| b.id == *branch_id) {
                    let branch = self.active_branches.remove(pos);
                    self.completed_items.push(CompletedItem {
                        id: branch_id.to_string(),
                        item_type: CompletedItemType::Branch,
                        description: branch.description,
                        completed_at: Utc::now(),
                        result_summary: conclusion.clone(),
                        relayed: false,
                    });
                }
            }
            ProcessEvent::AgentMessageSent { to_agent_id, .. } => {
                self.track_link_conversation(to_agent_id.as_ref());
            }
            _ => {}
        }

        // Prune completed items: drop relayed items older than 5 minutes,
        // then cap at 10 to bound status block size.
        self.prune_completed_items();
    }

    /// Mark completed items as relayed so the status block stops showing
    /// their full result summaries. Called after a retrigger turn succeeds.
    pub fn mark_relayed(&mut self, process_ids: &[String]) {
        for item in &mut self.completed_items {
            if process_ids.contains(&item.id) {
                item.relayed = true;
            }
        }
    }

    /// Remove stale completed items: relayed items older than 5 minutes are
    /// dropped entirely, then total count is capped at 10.
    fn prune_completed_items(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::minutes(5);
        self.completed_items
            .retain(|item| !(item.relayed && item.completed_at < cutoff));

        // Hard cap: keep the 10 most recent.
        while self.completed_items.len() > 10 {
            self.completed_items.remove(0);
        }
    }

    /// Add a new active branch.
    pub fn add_branch(&mut self, id: BranchId, description: impl Into<String>) {
        self.active_branches.push(BranchStatus {
            id,
            started_at: Utc::now(),
            description: description.into(),
        });
    }

    /// Add a new active worker.
    pub fn add_worker(
        &mut self,
        id: WorkerId,
        task: impl Into<String>,
        notify_on_complete: bool,
        interactive: bool,
    ) {
        self.active_workers.push(WorkerStatus {
            id,
            task: task.into(),
            status: "starting".to_string(),
            started_at: Utc::now(),
            notify_on_complete,
            tool_calls: 0,
            interactive,
        });
    }

    /// Remove an active worker from the status block.
    pub fn remove_worker(&mut self, worker_id: WorkerId) -> bool {
        if let Some(position) = self
            .active_workers
            .iter()
            .position(|worker| worker.id == worker_id)
        {
            self.active_workers.remove(position);
            true
        } else {
            false
        }
    }

    /// Remove an active branch from the status block.
    pub fn remove_branch(&mut self, branch_id: BranchId) -> bool {
        if let Some(position) = self
            .active_branches
            .iter()
            .position(|branch| branch.id == branch_id)
        {
            self.active_branches.remove(position);
            true
        } else {
            false
        }
    }

    /// Render the status block as a string for context injection.
    pub fn render(&self) -> String {
        self.render_with_context(None, None)
    }

    /// Render the status block with optional current time context.
    pub fn render_with_time_context(&self, current_time_line: Option<&str>) -> String {
        self.render_with_context(current_time_line, None)
    }

    /// Render the status block with optional time context and system info.
    pub fn render_with_context(
        &self,
        current_time_line: Option<&str>,
        system_info: Option<&SystemInfo>,
    ) -> String {
        let mut output = String::new();

        // System configuration summary (includes current time when available)
        if let Some(info) = system_info {
            output.push_str(&render_system_info(info, current_time_line));
        } else if let Some(current_time_line) = current_time_line {
            // Fallback: render time standalone when no system info is provided
            output.push_str(&format!("Current date/time: {current_time_line}\n\n"));
        }

        // Active workers
        if !self.active_workers.is_empty() {
            output.push_str("## Active Workers\n");
            for worker in &self.active_workers {
                let tool_calls_str = if worker.tool_calls > 0 {
                    format!(", {} tool calls", worker.tool_calls)
                } else {
                    String::new()
                };
                output.push_str(&format!(
                    "- [{}] {} ({}{}): {}\n",
                    worker.id,
                    worker.task,
                    worker.started_at.format("%H:%M"),
                    tool_calls_str,
                    worker.status
                ));
            }
            output.push('\n');
        }

        // Active branches
        if !self.active_branches.is_empty() {
            output.push_str("## Active Branches\n");
            for branch in &self.active_branches {
                output.push_str(&format!(
                    "- [{}] {} (started {})\n",
                    branch.id,
                    branch.description,
                    branch.started_at.format("%H:%M:%S")
                ));
            }
            output.push('\n');
        }

        // Active link conversations
        if !self.active_link_conversations.is_empty() {
            output.push_str("## Active Link Conversations\n");
            for link in &self.active_link_conversations {
                output.push_str(&format!(
                    "- **{}** ({} turns, started {})\n",
                    link.peer_agent,
                    link.turn_count,
                    link.started_at.format("%H:%M"),
                ));
            }
            output.push('\n');
        }

        // Recently completed — only show items not yet relayed to the user.
        // Relayed items already appeared in conversation via the retrigger flow;
        // keeping their full summaries here causes the LLM to re-summarise them.
        let unrelayed: Vec<_> = self
            .completed_items
            .iter()
            .rev()
            .filter(|item| !item.relayed)
            .take(5)
            .collect();
        if !unrelayed.is_empty() {
            output.push_str("## Recently Completed\n");
            for item in &unrelayed {
                let type_str = match item.item_type {
                    CompletedItemType::Branch => "branch",
                    CompletedItemType::Worker => "worker",
                };
                // Truncate long results to keep the status block manageable
                let summary = if item.result_summary.len() > 500 {
                    let end = item.result_summary.floor_char_boundary(500);
                    format!("{}...", &item.result_summary[..end])
                } else {
                    item.result_summary.clone()
                };
                output.push_str(&format!(
                    "- [{}] {}: {}\n",
                    type_str, item.description, summary,
                ));
            }
            output.push('\n');
        }

        output
    }

    /// Render the status block with time context and system info (convenience).
    pub fn render_full(&self, current_time_line: &str, system_info: &SystemInfo) -> String {
        self.render_with_context(Some(current_time_line), Some(system_info))
    }

    /// Check if a worker is active.
    pub fn is_worker_active(&self, worker_id: WorkerId) -> bool {
        self.active_workers.iter().any(|w| w.id == worker_id)
    }

    /// Check if an active worker already exists with a matching task.
    ///
    /// The status block stores OpenCode tasks with a `[opencode] ` prefix, so
    /// comparisons strip that prefix before matching. Returns the existing
    /// worker's ID if found.
    pub fn find_duplicate_worker_task(&self, task: &str) -> Option<WorkerId> {
        let normalized = task.strip_prefix("[opencode] ").unwrap_or(task);
        self.active_workers.iter().find_map(|worker| {
            let existing = worker
                .task
                .strip_prefix("[opencode] ")
                .unwrap_or(&worker.task);
            (existing == normalized).then_some(worker.id)
        })
    }

    /// Get the number of active branches.
    pub fn active_branch_count(&self) -> usize {
        self.active_branches.len()
    }

    /// Track a new link conversation or increment turn count.
    pub fn track_link_conversation(&mut self, peer_agent: impl Into<String>) {
        let peer = peer_agent.into();
        if let Some(existing) = self
            .active_link_conversations
            .iter_mut()
            .find(|l| l.peer_agent == peer)
        {
            existing.turn_count += 1;
        } else {
            self.active_link_conversations.push(LinkConversationStatus {
                peer_agent: peer,
                started_at: Utc::now(),
                turn_count: 1,
            });
        }
    }

    /// Remove a link conversation (concluded or timed out).
    pub fn remove_link_conversation(&mut self, peer_agent: &str) {
        self.active_link_conversations
            .retain(|l| l.peer_agent != peer_agent);
    }
}

/// Render the system info section as compact key-value lines.
fn render_system_info(info: &SystemInfo, current_time_line: Option<&str>) -> String {
    let mut output = String::from("## System\n");

    // Current date/time + timezone (first line — source of truth for temporal reasoning)
    if let Some(time_line) = current_time_line {
        output.push_str(&format!("Time: {time_line}\n"));
    }

    // Version + deployment
    output.push_str(&format!(
        "Version: {} ({})\n",
        info.version, info.deployment
    ));

    // Model assignments — collapse if all the same
    if info.channel_model == info.branch_model && info.branch_model == info.worker_model {
        output.push_str(&format!("Models: {}\n", info.channel_model));
    } else if info.channel_model == info.branch_model {
        output.push_str(&format!(
            "Models: channel/branch={}, worker={}\n",
            info.channel_model, info.worker_model
        ));
    } else {
        output.push_str(&format!(
            "Models: channel={}, branch={}, worker={}\n",
            info.channel_model, info.branch_model, info.worker_model
        ));
    }

    // Thinking effort — only show if not all "auto"
    if info.channel_thinking != "auto" || info.worker_thinking != "auto" {
        if info.channel_thinking == info.worker_thinking {
            output.push_str(&format!("Thinking: {}\n", info.channel_thinking));
        } else {
            output.push_str(&format!(
                "Thinking: channel={}, worker={}\n",
                info.channel_thinking, info.worker_thinking
            ));
        }
    }

    // Context + concurrency limits
    let context_label = if info.context_window >= 1000 {
        format!("{}k tokens", info.context_window / 1000)
    } else {
        format!("{} tokens", info.context_window)
    };
    output.push_str(&format!(
        "Context: {} | Workers: max {} | Branches: max {}\n",
        context_label, info.max_workers, info.max_branches
    ));

    // Capabilities — combine flags and MCP into one line
    let mut caps: Vec<&str> = info.capabilities.iter().map(|s| s.as_str()).collect();
    if info.sandbox_active {
        caps.push("sandbox");
    }
    if !caps.is_empty() {
        output.push_str(&format!("Capabilities: {}\n", caps.join(", ")));
    }

    // MCP servers
    if !info.mcp_servers.is_empty() {
        output.push_str(&format!(
            "MCP: {} ({} server{})\n",
            info.mcp_servers.join(", "),
            info.mcp_servers.len(),
            if info.mcp_servers.len() == 1 { "" } else { "s" }
        ));
    }

    // Warmup / readiness
    let mut warmup_parts = vec![info.warmup_state.as_str()];
    let embedding_label = if info.embedding_ready {
        "embeddings ready".to_string()
    } else {
        "embeddings loading".to_string()
    };
    warmup_parts.push(&embedding_label);
    let bulletin_label;
    if let Some(age) = info.bulletin_age_minutes {
        bulletin_label = format!("bulletin {}m ago", age);
        warmup_parts.push(&bulletin_label);
    }
    output.push_str(&format!("Warmup: {}\n", warmup_parts.join(", ")));

    // Cron jobs
    if let Some(count) = info.cron_job_count
        && count > 0
    {
        output.push_str(&format!(
            "Cron: {} active job{}\n",
            count,
            if count == 1 { "" } else { "s" }
        ));
    }

    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::StatusBlock;
    use uuid::Uuid;

    #[test]
    fn render_with_time_context_renders_current_time_when_empty() {
        let status = StatusBlock::new();
        let rendered = status.render_with_time_context(Some("2026-02-26 12:00:00 UTC"));
        assert!(rendered.contains("Current date/time: 2026-02-26 12:00:00 UTC"));
    }

    #[test]
    fn remove_branch_removes_existing_branch() {
        let mut status = StatusBlock::new();
        let branch_id = Uuid::new_v4();
        status.add_branch(branch_id, "work");

        let removed = status.remove_branch(branch_id);

        assert!(removed);
        assert!(status.active_branches.is_empty());
    }

    #[test]
    fn find_duplicate_exact_match() {
        let mut status = StatusBlock::new();
        let worker_id = Uuid::new_v4();
        status.add_worker(worker_id, "Build a landing page", true, false);

        let found = status.find_duplicate_worker_task("Build a landing page");
        assert_eq!(found, Some(worker_id));
    }

    #[test]
    fn find_duplicate_no_match() {
        let mut status = StatusBlock::new();
        let worker_id = Uuid::new_v4();
        status.add_worker(worker_id, "Build a landing page", true, false);

        let found = status.find_duplicate_worker_task("Fix the CSS bug");
        assert_eq!(found, None);
    }

    #[test]
    fn find_duplicate_strips_opencode_prefix() {
        let mut status = StatusBlock::new();
        let worker_id = Uuid::new_v4();
        status.add_worker(worker_id, "[opencode] Build a landing page", true, false);

        // Should match without the prefix
        let found = status.find_duplicate_worker_task("Build a landing page");
        assert_eq!(found, Some(worker_id));

        // Should also match with the prefix
        let found = status.find_duplicate_worker_task("[opencode] Build a landing page");
        assert_eq!(found, Some(worker_id));
    }

    #[test]
    fn find_duplicate_strips_opencode_prefix_in_query() {
        let mut status = StatusBlock::new();
        let worker_id = Uuid::new_v4();
        status.add_worker(worker_id, "Build a landing page", true, false);

        // Querying with prefix should still find the non-prefixed worker
        let found = status.find_duplicate_worker_task("[opencode] Build a landing page");
        assert_eq!(found, Some(worker_id));
    }

    #[test]
    fn find_duplicate_empty_status_block() {
        let status = StatusBlock::new();
        let found = status.find_duplicate_worker_task("any task");
        assert_eq!(found, None);
    }

    #[test]
    fn render_full_includes_system_info_and_time() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            version: "0.9.2".into(),
            deployment: "hosted".into(),
            channel_model: "anthropic/claude-sonnet-4".into(),
            branch_model: "anthropic/claude-sonnet-4".into(),
            worker_model: "anthropic/claude-sonnet-4".into(),
            channel_thinking: "auto".into(),
            worker_thinking: "auto".into(),
            context_window: 128_000,
            max_workers: 5,
            max_branches: 3,
            capabilities: vec!["browser".into(), "web_search".into()],
            mcp_servers: vec!["github".into(), "linear".into()],
            sandbox_active: true,
            warmup_state: "warm".into(),
            embedding_ready: true,
            bulletin_age_minutes: Some(12),
            cron_job_count: Some(4),
        };

        let rendered = status.render_full("2026-03-08 10:30:00 EST", &info);

        // Time is inside System section
        assert!(rendered.contains("Time: 2026-03-08 10:30:00 EST"));
        // Version
        assert!(rendered.contains("Version: 0.9.2 (hosted)"));
        // Models collapsed (all same)
        assert!(rendered.contains("Models: anthropic/claude-sonnet-4"));
        assert!(!rendered.contains("channel="));
        // Thinking effort hidden when all auto
        assert!(!rendered.contains("Thinking:"));
        // Context + limits
        assert!(rendered.contains("128k tokens"));
        assert!(rendered.contains("Workers: max 5"));
        assert!(rendered.contains("Branches: max 3"));
        // Capabilities
        assert!(rendered.contains("browser"));
        assert!(rendered.contains("web_search"));
        assert!(rendered.contains("sandbox"));
        // MCP
        assert!(rendered.contains("github, linear (2 servers)"));
        // Warmup
        assert!(rendered.contains("warm"));
        assert!(rendered.contains("embeddings ready"));
        assert!(rendered.contains("bulletin 12m ago"));
        // Cron
        assert!(rendered.contains("4 active jobs"));
    }

    #[test]
    fn render_system_info_collapses_identical_models() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            channel_model: "anthropic/claude-sonnet-4".into(),
            branch_model: "anthropic/claude-sonnet-4".into(),
            worker_model: "anthropic/claude-sonnet-4".into(),
            ..Default::default()
        };

        let rendered = status.render_with_context(None, Some(&info));
        assert!(rendered.contains("Models: anthropic/claude-sonnet-4\n"));
        assert!(!rendered.contains("channel="));
    }

    #[test]
    fn render_system_info_splits_different_models() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            channel_model: "anthropic/claude-sonnet-4".into(),
            branch_model: "anthropic/claude-sonnet-4".into(),
            worker_model: "anthropic/claude-haiku-35".into(),
            ..Default::default()
        };

        let rendered = status.render_with_context(None, Some(&info));
        assert!(rendered.contains("channel/branch=anthropic/claude-sonnet-4"));
        assert!(rendered.contains("worker=anthropic/claude-haiku-35"));
    }

    #[test]
    fn render_system_info_shows_all_three_when_all_different() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            channel_model: "anthropic/claude-opus-4".into(),
            branch_model: "anthropic/claude-sonnet-4".into(),
            worker_model: "anthropic/claude-haiku-35".into(),
            ..Default::default()
        };

        let rendered = status.render_with_context(None, Some(&info));
        assert!(rendered.contains("channel=anthropic/claude-opus-4"));
        assert!(rendered.contains("branch=anthropic/claude-sonnet-4"));
        assert!(rendered.contains("worker=anthropic/claude-haiku-35"));
    }

    #[test]
    fn render_system_info_shows_thinking_when_not_auto() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            channel_thinking: "high".into(),
            worker_thinking: "low".into(),
            ..Default::default()
        };

        let rendered = status.render_with_context(None, Some(&info));
        assert!(rendered.contains("Thinking: channel=high, worker=low"));
    }

    #[test]
    fn render_system_info_hides_thinking_when_all_auto() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            channel_thinking: "auto".into(),
            worker_thinking: "auto".into(),
            ..Default::default()
        };

        let rendered = status.render_with_context(None, Some(&info));
        assert!(!rendered.contains("Thinking:"));
    }

    #[test]
    fn render_system_info_no_cron_when_zero() {
        use super::SystemInfo;

        let status = StatusBlock::new();
        let info = SystemInfo {
            cron_job_count: Some(0),
            ..Default::default()
        };

        let rendered = status.render_with_context(None, Some(&info));
        assert!(!rendered.contains("Cron:"));
    }
}
