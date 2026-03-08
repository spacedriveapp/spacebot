//! Spawn worker tool for creating new workers.
//!
//! Two variants:
//! - `SpawnWorkerTool`: full-featured, used by channels and branches. Requires `ChannelState`.
//! - `DetachedSpawnWorkerTool`: lightweight, used by cortex chat. Spawns workers with no
//!   parent channel — they log directly to `worker_runs` and emit events with `channel_id: None`.

use crate::WorkerId;
use crate::agent::channel::ChannelState;
use crate::agent::channel_dispatch::{spawn_opencode_worker_from_state, spawn_worker_from_state};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::Instrument as _;

/// Tool for spawning workers.
#[derive(Debug, Clone)]
pub struct SpawnWorkerTool {
    state: ChannelState,
}

impl SpawnWorkerTool {
    /// Create a new spawn worker tool with access to channel state.
    pub fn new(state: ChannelState) -> Self {
        Self { state }
    }
}

/// Error type for spawn worker tool.
#[derive(Debug, thiserror::Error)]
#[error("Worker spawn failed: {0}")]
pub struct SpawnWorkerError(String);

/// Arguments for spawn worker tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnWorkerArgs {
    /// The task description for the worker.
    pub task: String,
    /// Whether this is an interactive worker (accepts follow-up messages).
    #[serde(default)]
    pub interactive: bool,
    /// Optional list of skill names to suggest to the worker. The worker sees
    /// all available skills and can read any of them via read_skill, but
    /// suggested skills are flagged as recommended for this task.
    #[serde(default)]
    pub suggested_skills: Vec<String>,
    /// Worker type: "builtin" (default) runs a Rig agent loop with shell/file
    /// tools. "opencode" spawns an OpenCode subprocess with full coding agent
    /// capabilities. Use "opencode" for complex coding tasks that benefit from
    /// codebase exploration and context management.
    #[serde(default)]
    pub worker_type: Option<String>,
    /// Working directory for the worker. Required for "opencode" workers
    /// unless project_id or worktree_id is set. The OpenCode agent will
    /// operate in this directory.
    #[serde(default)]
    pub directory: Option<String>,
    /// Project ID to associate this worker with. When set, the worker gets
    /// project context in its prompt. If directory is not specified, defaults
    /// to the project root.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Worktree ID within the project. If set, the worker's directory is
    /// automatically set to the worktree path.
    #[serde(default)]
    pub worktree_id: Option<String>,
}

/// Output from spawn worker tool.
#[derive(Debug, Serialize)]
pub struct SpawnWorkerOutput {
    /// The ID of the spawned worker.
    pub worker_id: WorkerId,
    /// Whether the worker was spawned successfully.
    pub spawned: bool,
    /// Whether this is an interactive worker.
    pub interactive: bool,
    /// Status message.
    pub message: String,
}

impl Tool for SpawnWorkerTool {
    const NAME: &'static str = "spawn_worker";

    type Error = SpawnWorkerError;
    type Args = SpawnWorkerArgs;
    type Output = SpawnWorkerOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let rc = &self.state.deps.runtime_config;
        let browser_enabled = rc.browser_config.load().enabled;
        let web_search_enabled = rc.brave_search_key.load().is_some();
        let opencode_enabled = rc.opencode.load().enabled;

        let mut tools_list = vec!["shell", "file_read", "file_write", "file_edit", "file_list"];
        if browser_enabled {
            tools_list.push("browser");
        }
        if web_search_enabled {
            tools_list.push("web_search");
        }

        let opencode_note = if opencode_enabled {
            " Set `worker_type` to \"opencode\" with a `directory` path for complex coding tasks — this spawns a full OpenCode coding agent with codebase exploration, context management, and its own tool suite. If `worker_type` is omitted, the builtin worker is used."
        } else {
            ""
        };

        let base_description = crate::prompts::text::get("tools/spawn_worker");
        let description = base_description
            .replace("{tools}", &tools_list.join(", "))
            .replace("{opencode_note}", opencode_note);

        let mut properties = serde_json::json!({
            "task": {
                "type": "string",
                "description": "Clear, specific description of what the worker should do. Include all context needed since the worker can't see your conversation."
            },
            "interactive": {
                "type": "boolean",
                "default": false,
                "description": "If true, the worker stays alive and accepts follow-up messages via route_to_worker. If false (default), the worker runs once and returns. OpenCode workers are always interactive regardless of this flag."
            },
            "suggested_skills": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Skill names from <available_skills> that are likely relevant to this task. The worker sees all skills and decides what to read, but suggested skills are flagged as recommended."
            }
        });

        if opencode_enabled && let Some(obj) = properties.as_object_mut() {
            obj.insert(
                "worker_type".to_string(),
                serde_json::json!({
                    "type": "string",
                    "enum": ["builtin", "opencode"],
                    "default": "builtin",
                    "description": "\"builtin\" (default) runs a Rig agent loop. \"opencode\" spawns a full OpenCode coding agent — use for complex multi-file coding tasks. Do not claim OpenCode unless this field is explicitly set to \"opencode\"."
                }),
            );
            obj.insert(
                "directory".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "Working directory for the worker. Required when worker_type is \"opencode\" unless project_id or worktree_id is set. The OpenCode agent operates in this directory."
                }),
            );
            obj.insert(
                "project_id".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "Project ID to associate this worker with. When set, the worker gets project context. If directory is not specified, defaults to the project root."
                }),
            );
            obj.insert(
                "worktree_id".to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": "Worktree ID within the project. If set, the worker's directory is automatically set to the worktree path."
                }),
            );
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description,
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["task"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let readiness = self.state.deps.runtime_config.work_readiness();
        let is_opencode = args.worker_type.as_deref() == Some("opencode");

        // Reject if an active worker already has the same task. This prevents
        // duplicate workers when the LLM emits multiple spawn_worker calls in
        // a single response and one fails/retries.
        //
        // Returned as a structured result (not an error) so the LLM can
        // recover deterministically — e.g. route to the existing worker.
        {
            let status = self.state.status_block.read().await;
            if let Some(existing_id) = status.find_duplicate_worker_task(&args.task) {
                return Ok(SpawnWorkerOutput {
                    worker_id: existing_id,
                    spawned: false,
                    interactive: args.interactive,
                    message: format!(
                        "A worker is already running this task (worker {existing_id}). \
                         Use route to send additional context to the running worker instead."
                    ),
                });
            }
        }

        // Resolve working directory from project/worktree if not explicitly set.
        let resolved_directory = resolve_directory_from_project(
            &self.state.deps,
            args.directory.as_deref(),
            args.project_id.as_deref(),
            args.worktree_id.as_deref(),
        )
        .await;

        let worker_id = if is_opencode {
            let directory = resolved_directory.as_deref().ok_or_else(|| {
                SpawnWorkerError(
                    "directory is required for opencode workers (set directory, project_id, or worktree_id)".into(),
                )
            })?;

            // OpenCode workers are always interactive — ignore args.interactive.
            spawn_opencode_worker_from_state(&self.state, &args.task, directory, true)
                .await
                .map_err(|e| SpawnWorkerError(format!("{e}")))?
        } else {
            spawn_worker_from_state(
                &self.state,
                &args.task,
                args.interactive,
                &args
                    .suggested_skills
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(|e| SpawnWorkerError(format!("{e}")))?
        };

        // Link the worker to project/worktree if specified (fire-and-forget update).
        if args.project_id.is_some() || args.worktree_id.is_some() {
            self.state.process_run_logger.log_worker_project_link(
                worker_id,
                args.project_id.as_deref(),
                args.worktree_id.as_deref(),
            );
        }

        let worker_type_label = if is_opencode { "OpenCode" } else { "builtin" };
        // OpenCode workers are always interactive regardless of args.interactive.
        let effectively_interactive = args.interactive || is_opencode;
        let message = if effectively_interactive {
            format!(
                "Interactive {worker_type_label} worker {worker_id} spawned for: {}. Route follow-ups with route_to_worker.",
                args.task
            )
        } else {
            format!(
                "{worker_type_label} worker {worker_id} spawned for: {}. It will report back when done.",
                args.task
            )
        };
        let readiness_note = if readiness.ready {
            String::new()
        } else {
            let reason = readiness
                .reason
                .map(|value| value.as_str())
                .unwrap_or("unknown");
            format!(
                " Readiness note: warmup is not fully ready ({reason}, state: {:?}); a warmup pass may already be running or was queued in the background.",
                readiness.warmup_state
            )
        };

        Ok(SpawnWorkerOutput {
            worker_id,
            spawned: true,
            interactive: effectively_interactive,
            message: format!("{message}{readiness_note}"),
        })
    }
}

// ---------------------------------------------------------------------------
// DetachedSpawnWorkerTool — lightweight variant for cortex chat
// ---------------------------------------------------------------------------

/// Shared context that links the cortex chat session to detached workers.
/// Updated before each cortex chat turn so spawned workers know which thread
/// to deliver results to.
#[derive(Debug, Clone)]
pub struct CortexChatContext {
    /// Current thread_id for the active cortex chat conversation.
    pub current_thread_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Current channel context (if cortex chat was opened on a channel page).
    pub current_channel_context: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Workers tracked by the cortex chat event loop.
    pub tracked_workers: Arc<
        tokio::sync::RwLock<
            std::collections::HashMap<crate::WorkerId, crate::agent::cortex_chat::TrackedWorker>,
        >,
    >,
}

/// Spawn worker tool for cortex chat sessions.
///
/// Unlike `SpawnWorkerTool` (which requires `ChannelState`), this creates
/// workers with no parent channel. Workers are logged directly to `worker_runs`
/// and emit events with `channel_id: None`.
#[derive(Clone)]
pub struct DetachedSpawnWorkerTool {
    deps: crate::AgentDeps,
    screenshot_dir: PathBuf,
    logs_dir: PathBuf,
    cortex_ctx: Option<CortexChatContext>,
}

impl std::fmt::Debug for DetachedSpawnWorkerTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetachedSpawnWorkerTool")
            .finish_non_exhaustive()
    }
}

impl DetachedSpawnWorkerTool {
    pub fn new(deps: crate::AgentDeps, screenshot_dir: PathBuf, logs_dir: PathBuf) -> Self {
        Self {
            deps,
            screenshot_dir,
            logs_dir,
            cortex_ctx: None,
        }
    }

    pub fn with_cortex_context(mut self, ctx: CortexChatContext) -> Self {
        self.cortex_ctx = Some(ctx);
        self
    }
}

/// Arguments for the detached spawn worker tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DetachedSpawnWorkerArgs {
    /// Clear, specific description of what the worker should do.
    pub task: String,
}

impl Tool for DetachedSpawnWorkerTool {
    const NAME: &'static str = "spawn_worker";

    type Error = SpawnWorkerError;
    type Args = DetachedSpawnWorkerArgs;
    type Output = SpawnWorkerOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let rc = &self.deps.runtime_config;
        let browser_enabled = rc.browser_config.load().enabled;
        let web_search_enabled = rc.brave_search_key.load().is_some();

        let mut tools_list = vec!["shell", "file_read", "file_write", "file_edit", "file_list"];
        if browser_enabled {
            tools_list.push("browser");
        }
        if web_search_enabled {
            tools_list.push("web_search");
        }

        let description = format!(
            "Spawn an independent worker process with {} tools. The worker runs \
             autonomously and reports back when done. Use this for browser-heavy \
             research, long shell operations, or any task that benefits from \
             dedicated execution. The worker only sees the task description you \
             provide — no conversation history.",
            tools_list.join(", ")
        );

        ToolDefinition {
            name: Self::NAME.to_string(),
            description,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Clear, specific description of what the worker should do. Include all context needed since the worker can't see your conversation."
                    }
                },
                "required": ["task"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let rc = &self.deps.runtime_config;
        let prompt_engine = rc.prompts.load();
        let sandbox_enabled = self.deps.sandbox.mode_enabled();
        let sandbox_containment_active = self.deps.sandbox.containment_active();
        let sandbox_read_allowlist = self.deps.sandbox.prompt_read_allowlist();
        let sandbox_write_allowlist = self.deps.sandbox.prompt_write_allowlist();

        let secrets_guard = rc.secrets.load();
        let tool_secret_names = match (*secrets_guard).as_ref() {
            Some(store) => store.tool_secret_names(),
            None => Vec::new(),
        };

        let browser_config = (**rc.browser_config.load()).clone();
        let worker_system_prompt = prompt_engine
            .render_worker_prompt(
                &rc.instance_dir.display().to_string(),
                &rc.workspace_dir.display().to_string(),
                sandbox_enabled,
                sandbox_containment_active,
                sandbox_read_allowlist,
                sandbox_write_allowlist,
                &tool_secret_names,
                browser_config.persist_session,
            )
            .map_err(|error| {
                SpawnWorkerError(format!("failed to render worker prompt: {error}"))
            })?;

        let brave_search_key = (**rc.brave_search_key.load()).clone();

        let worker = crate::agent::worker::Worker::new(
            None, // no parent channel
            &args.task,
            worker_system_prompt,
            self.deps.clone(),
            browser_config,
            self.screenshot_dir.clone(),
            brave_search_key,
            self.logs_dir.clone(),
        );

        let (worker, _input_tx) = worker;
        let worker_id = worker.id;

        // Emit WorkerStarted event so the UI can track it.
        let _ = self.deps.event_tx.send(crate::ProcessEvent::WorkerStarted {
            agent_id: self.deps.agent_id.clone(),
            worker_id,
            channel_id: None,
            task: args.task.clone(),
            worker_type: "cortex".into(),
            interactive: false,
            directory: None,
        });

        // Log to worker_runs directly since there's no parent channel to do it.
        let run_logger =
            crate::conversation::history::ProcessRunLogger::new(self.deps.sqlite_pool.clone());
        run_logger.log_worker_started(
            None,
            worker_id,
            &args.task,
            "cortex",
            &self.deps.agent_id,
            false,
            None,
        );

        let secrets_store = rc.secrets.load().as_ref().clone();
        let worker_span = tracing::info_span!(
            "worker.run",
            worker_id = %worker_id,
            spawned_by = "cortex_chat",
        );
        crate::agent::channel_dispatch::spawn_worker_task(
            worker_id,
            self.deps.event_tx.clone(),
            self.deps.agent_id.clone(),
            None,
            secrets_store,
            "builtin",
            worker.run().instrument(worker_span),
        );

        // Register the worker with the cortex chat event loop so it can
        // auto-trigger a follow-up turn when the worker completes.
        if let Some(ctx) = &self.cortex_ctx {
            let thread_id: Option<String> = ctx.current_thread_id.read().await.clone();
            let channel_context: Option<String> = ctx.current_channel_context.read().await.clone();
            if let Some(thread_id) = thread_id {
                let mut workers = ctx.tracked_workers.write().await;
                workers.insert(
                    worker_id,
                    crate::agent::cortex_chat::TrackedWorker {
                        thread_id,
                        channel_context,
                    },
                );
            }
        }

        tracing::info!(worker_id = %worker_id, task = %args.task, "cortex chat spawned detached worker");

        Ok(SpawnWorkerOutput {
            worker_id,
            spawned: true,
            interactive: false,
            message: format!(
                "Worker {worker_id} spawned for: {}. It will report back when done.",
                args.task
            ),
        })
    }
}

/// Resolve a working directory from project/worktree IDs.
///
/// Priority: explicit `directory` > `worktree_id` > `project_id` root.
/// Returns the explicit directory if set, otherwise looks up worktree or
/// project root from the store.
async fn resolve_directory_from_project(
    deps: &crate::AgentDeps,
    directory: Option<&str>,
    project_id: Option<&str>,
    worktree_id: Option<&str>,
) -> Option<String> {
    // Explicit directory takes precedence.
    if let Some(dir) = directory {
        return Some(dir.to_string());
    }

    let store = &deps.project_store;
    let agent_id = &deps.agent_id;

    // Worktree resolution: look up the worktree, derive absolute path from project root.
    if let Some(worktree_id) = worktree_id
        && let Ok(Some(worktree)) = store.get_worktree(worktree_id).await
    {
        // Always use the worktree's own project_id to resolve the path.
        // If the caller also provided a project_id, verify it matches.
        if let Some(pid) = project_id
            && pid != worktree.project_id
        {
            tracing::warn!(
                worktree_id,
                provided_project_id = pid,
                actual_project_id = %worktree.project_id,
                "project_id/worktree_id mismatch — using worktree's project"
            );
        }
        if let Ok(Some(project)) = store.get_project(agent_id, &worktree.project_id).await {
            let abs_path = std::path::Path::new(&project.root_path).join(&worktree.path);
            return Some(abs_path.to_string_lossy().to_string());
        }
    }

    // Project root resolution.
    if let Some(project_id) = project_id
        && let Ok(Some(project)) = store.get_project(agent_id, project_id).await
    {
        return Some(project.root_path.clone());
    }

    None
}
