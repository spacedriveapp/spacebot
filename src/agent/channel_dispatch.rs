//! Branch and worker spawning for channels.
//!
//! Contains the public entry points that channel tools use to create
//! background processes: `spawn_branch_from_state`, `spawn_worker_from_state`,
//! and `spawn_opencode_worker_from_state`.

use crate::agent::branch::{Branch, BranchExecutionConfig};
use crate::agent::channel::ChannelState;
use crate::agent::channel_prompt::TemporalContext;
use crate::agent::worker::{Worker, WorkerOutcome};
use crate::conversation::settings::{WorkerContextMode, WorkerHistoryMode};
use crate::error::{AgentError, Error as SpacebotError};
use crate::tools::{BranchToolProfile, MemoryPersistenceContractState};
use crate::{AgentDeps, BranchId, ChannelId, ProcessEvent, ProcessType, WorkerId};
use futures::FutureExt as _;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::Instrument as _;

/// Validate worker capacity for a channel based on current active worker count.
pub(crate) fn reserve_worker_slot_local(
    active_worker_count: usize,
    channel_id: &Arc<str>,
    max_workers: usize,
) -> std::result::Result<(), AgentError> {
    if active_worker_count >= max_workers {
        return Err(AgentError::WorkerLimitReached {
            channel_id: channel_id.to_string(),
            max: max_workers,
        });
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerCompletionKind {
    Success,
    Partial,
    Cancelled,
    Timeout,
    Blocked,
    Failed,
}

#[derive(Debug, Clone)]
pub(crate) enum WorkerCompletionError {
    Cancelled { reason: String },
    Failed { message: String },
}

impl WorkerCompletionError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }

    fn from_spacebot_error(error: SpacebotError) -> Self {
        match error {
            SpacebotError::Agent(agent_error) => match *agent_error {
                AgentError::Cancelled { reason } => Self::Cancelled { reason },
                other => Self::Failed {
                    message: other.to_string(),
                },
            },
            other => Self::Failed {
                message: other.to_string(),
            },
        }
    }
}

fn classify_worker_completion(
    outcome: std::result::Result<WorkerOutcome, WorkerCompletionError>,
) -> (String, WorkerCompletionKind) {
    match outcome {
        Ok(WorkerOutcome::Success { result }) => (result, WorkerCompletionKind::Success),
        Ok(WorkerOutcome::Partial {
            result,
            segments_run,
        }) => (
            format!(
                "{result}\n\n(reached max segments after {segments_run} attempts — partial result)"
            ),
            WorkerCompletionKind::Partial,
        ),
        Ok(WorkerOutcome::Cancelled { reason }) => (
            format!("Worker cancelled: {reason}"),
            WorkerCompletionKind::Cancelled,
        ),
        Ok(WorkerOutcome::Timeout {
            elapsed_secs,
            segments_run,
        }) => (
            format!(
                "Worker exceeded {elapsed_secs}s wall-clock timeout after {segments_run} segments."
            ),
            WorkerCompletionKind::Timeout,
        ),
        Ok(WorkerOutcome::Blocked { reason, url, .. }) => {
            let body = match url {
                Some(url) => format!("Worker blocked: {} at {url}", reason.describe()),
                None => format!("Worker blocked: {}", reason.describe()),
            };
            (body, WorkerCompletionKind::Blocked)
        }
        Ok(WorkerOutcome::Failed { reason }) => (
            format!("Worker failed: {reason}"),
            WorkerCompletionKind::Failed,
        ),
        Err(WorkerCompletionError::Cancelled { reason }) => (
            format!("Worker cancelled: {reason}"),
            WorkerCompletionKind::Cancelled,
        ),
        Err(WorkerCompletionError::Failed { message }) => (
            format!("Worker failed: {message}"),
            WorkerCompletionKind::Failed,
        ),
    }
}

fn completion_flags(kind: WorkerCompletionKind) -> (bool, bool) {
    let notify = true;
    let success = matches!(
        kind,
        WorkerCompletionKind::Success | WorkerCompletionKind::Partial
    );
    (notify, success)
}

/// Normalize a worker outcome (or terminal error) into event payload fields.
pub(crate) fn map_worker_completion(
    outcome: std::result::Result<WorkerOutcome, WorkerCompletionError>,
) -> (String, bool, bool) {
    let (result_text, kind) = classify_worker_completion(outcome);
    let (notify, success) = completion_flags(kind);
    (result_text, notify, success)
}

/// Build the worker status text (time + system info) used in worker system prompts.
///
/// Centralises the `SystemInfo` + `TemporalContext` assembly so every worker
/// spawn/resume path produces identical status context.
fn build_worker_status_text(
    runtime_config: &crate::config::RuntimeConfig,
    sandbox: &crate::sandbox::Sandbox,
) -> Option<String> {
    let system_info =
        crate::agent::status::SystemInfo::from_runtime_config(runtime_config, sandbox);
    let temporal_context = TemporalContext::from_runtime(runtime_config);
    let current_time_line = temporal_context.current_time_line();
    Some(system_info.render_for_worker(&current_time_line))
}

#[derive(Debug, Clone)]
struct BranchSpawnOptions {
    profile: BranchToolProfile,
}

/// Spawn a branch from a ChannelState. Used by the BranchTool.
pub async fn spawn_branch_from_state(
    state: &ChannelState,
    description: impl Into<String>,
) -> std::result::Result<BranchId, AgentError> {
    let description = description.into();
    let rc = &state.deps.runtime_config;
    let prompt_engine = rc.prompts.load();
    let routing = rc.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let tool_use_enforcement = rc.tool_use_enforcement.load();
    let wiki_enabled = state.deps.wiki_store.is_some();
    let system_prompt = prompt_engine
        .render_branch_prompt(
            &rc.instance_dir.display().to_string(),
            &rc.workspace_dir.display().to_string(),
            wiki_enabled,
        )
        .and_then(|prompt| {
            prompt_engine.maybe_append_tool_use_enforcement(
                prompt,
                tool_use_enforcement.as_ref(),
                &model_name,
            )
        })
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;

    spawn_branch(
        state,
        &description,
        &description,
        &system_prompt,
        &description,
        "branch",
        BranchSpawnOptions {
            profile: BranchToolProfile::Default,
        },
    )
    .await
}

/// Spawn a silent memory persistence branch.
///
/// Uses the same branching infrastructure as regular branches but with a
/// dedicated prompt focused on memory recall + save. The result is not injected
/// into channel history — the channel handles these branch IDs specially.
pub(crate) async fn spawn_memory_persistence_branch(
    state: &ChannelState,
    deps: &AgentDeps,
) -> std::result::Result<BranchId, AgentError> {
    let contract_state = Arc::new(MemoryPersistenceContractState::default());

    let prompt_engine = deps.runtime_config.prompts.load();
    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let tool_use_enforcement = deps.runtime_config.tool_use_enforcement.load();
    let system_prompt = prompt_engine
        .render_static("memory_persistence")
        .and_then(|prompt| {
            prompt_engine.maybe_append_tool_use_enforcement(
                prompt,
                tool_use_enforcement.as_ref(),
                &model_name,
            )
        })
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;
    let prompt = prompt_engine
        .render_system_memory_persistence()
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;

    spawn_branch(
        state,
        "memory persistence",
        &prompt,
        &system_prompt,
        "persisting memories...",
        "memory_persistence_branch",
        BranchSpawnOptions {
            profile: BranchToolProfile::MemoryPersistence {
                contract_state,
                working_memory: Some(state.deps.working_memory.clone()),
                channel_id: Some(state.channel_id.to_string()),
            },
        },
    )
    .await
}

fn ensure_dispatch_readiness(state: &ChannelState, dispatch_type: &'static str) {
    let readiness = state.deps.runtime_config.work_readiness();
    if readiness.ready {
        return;
    }

    let reason = readiness
        .reason
        .map(|value| value.as_str())
        .unwrap_or("unknown");
    tracing::warn!(
        agent_id = %state.deps.agent_id,
        channel_id = %state.channel_id,
        dispatch_type,
        reason,
        warmup_state = ?readiness.warmup_state,
        embedding_ready = readiness.embedding_ready,
        bulletin_age_secs = ?readiness.bulletin_age_secs,
        stale_after_secs = readiness.stale_after_secs,
        "dispatch requested before readiness contract was satisfied"
    );

    #[cfg(feature = "metrics")]
    crate::telemetry::Metrics::global()
        .dispatch_while_cold_count
        .with_label_values(&[&*state.deps.agent_id, dispatch_type, reason])
        .inc();

    let warmup_config = **state.deps.runtime_config.warmup.load();
    let should_trigger = readiness.warmup_state != crate::config::WarmupState::Warming
        && (readiness.reason != Some(crate::config::WorkReadinessReason::EmbeddingNotReady)
            || warmup_config.eager_embedding_load);

    if should_trigger {
        crate::agent::cortex::trigger_forced_warmup(state.deps.clone(), dispatch_type);
    }
}

/// Shared branch spawning logic.
///
/// Checks the branch limit, clones history, creates a Branch, spawns it as
/// a tokio task, and registers it in the channel's active branches and status block.
async fn spawn_branch(
    state: &ChannelState,
    description: &str,
    prompt: &str,
    system_prompt: &str,
    status_label: &str,
    dispatch_type: &'static str,
    branch_options: BranchSpawnOptions,
) -> std::result::Result<BranchId, AgentError> {
    let BranchSpawnOptions { profile } = branch_options;
    let memory_persistence_contract = match &profile {
        BranchToolProfile::MemoryPersistence { contract_state, .. } => Some(contract_state.clone()),
        BranchToolProfile::Default => None,
    };

    let max_branches = **state.deps.runtime_config.max_concurrent_branches.load();
    {
        let branches = state.active_branches.read().await;
        if branches.len() >= max_branches {
            return Err(AgentError::BranchLimitReached {
                channel_id: state.channel_id.to_string(),
                max: max_branches,
            });
        }
    }
    ensure_dispatch_readiness(state, dispatch_type);

    let history = {
        let h = state.history.read().await;
        h.clone()
    };

    let tool_server = crate::tools::create_branch_tool_server(
        Some(state.clone()),
        state.deps.agent_id.clone(),
        state.deps.task_store.clone(),
        state.deps.memory_search.clone(),
        state.deps.runtime_config.clone(),
        state.deps.memory_event_tx.clone(),
        state.conversation_logger.clone(),
        state.channel_store.clone(),
        crate::conversation::ProcessRunLogger::new(state.deps.sqlite_pool.clone()),
        profile,
        state.deps.api_state.clone(),
        state.deps.wiki_store.clone(),
        state.deps.sandbox.clone(),
    );
    let branch_max_turns = **state.deps.runtime_config.branch_max_turns.load();

    let branch = Branch::new(
        state.channel_id.clone(),
        description,
        state.deps.clone(),
        system_prompt,
        history,
        tool_server,
        BranchExecutionConfig {
            max_turns: branch_max_turns,
            memory_persistence_contract,
        },
        state
            .model_overrides
            .resolve_model("branch")
            .map(String::from),
    );

    let branch_id = branch.id;
    let prompt = prompt.to_owned();

    // Capture what the spawned task needs to notify the channel on failure.
    // branch.run() only sends BranchResult on the success path, so the
    // spawner must handle failures to prevent orphaned branches (see #279).
    let event_tx = state.deps.event_tx.clone();
    let agent_id = state.deps.agent_id.clone();
    let channel_id = state.channel_id.clone();
    let secrets_snapshot = state.deps.runtime_config.secrets.load().clone();

    let branch_span = tracing::info_span!(
        "branch.run",
        branch_id = %branch_id,
        channel_id = %state.channel_id,
        description = %description,
    );
    // Acquire the write lock before spawning so the event loop cannot process
    // BranchResult (which also takes a write lock) before we insert the handle.
    // Without this, a fast-completing branch sends BranchResult before the
    // insert, causing `was_active` to be false and suppressing the retrigger.
    let mut branches = state.active_branches.write().await;
    let handle = tokio::spawn(
        async move {
            if let Err(error) = branch.run(&prompt).await {
                tracing::error!(branch_id = %branch_id, %error, "branch failed");
                // Scrub the failure message in case the error contains secrets
                // (e.g. from failed tool calls echoing back prompt content).
                // Layer 1: exact-match redaction of known secrets from the store.
                // Layer 2: regex-based redaction of unknown secret patterns.
                let raw = format!("Branch failed: {error}");
                let conclusion = if let Some(store) = secrets_snapshot.as_ref() {
                    crate::secrets::scrub::scrub_with_store(&raw, store, &agent_id)
                } else {
                    raw
                };
                let conclusion = crate::secrets::scrub::scrub_leaks(&conclusion);
                let _ = event_tx.send(crate::ProcessEvent::BranchResult {
                    agent_id,
                    branch_id,
                    channel_id,
                    conclusion,
                });
            }
        }
        .instrument(branch_span),
    );
    branches.insert(branch_id, handle);
    drop(branches);

    {
        let mut status = state.status_block.write().await;
        status.add_branch(branch_id, status_label);
    }

    #[cfg(feature = "metrics")]
    {
        let metrics = crate::telemetry::Metrics::global();
        metrics
            .active_branches
            .with_label_values(&[&*state.deps.agent_id])
            .inc();
        metrics
            .branches_spawned_total
            .with_label_values(&[&*state.deps.agent_id])
            .inc();
    }

    state
        .deps
        .event_tx
        .send(crate::ProcessEvent::BranchStarted {
            agent_id: state.deps.agent_id.clone(),
            branch_id,
            channel_id: state.channel_id.clone(),
            description: status_label.to_string(),
            reply_to_message_id: state.reply_target_message_id.read().await.clone(),
        })
        .ok();

    tracing::info!(branch_id = %branch_id, description = %status_label, "branch spawned");

    Ok(branch_id)
}

/// Check whether the channel has capacity for another worker.
///
/// Uses `worker_handles` as the source of truth for active workers, since
/// `active_workers` (the `HashMap<WorkerId, Worker>`) is never populated —
/// `Worker` is consumed by `.run()` inside `spawn_worker_task`.
async fn check_worker_limit(state: &ChannelState) -> std::result::Result<(), AgentError> {
    let max_workers = **state.deps.runtime_config.max_concurrent_workers.load();
    let active_worker_count = state.worker_handles.read().await.len();
    reserve_worker_slot_local(active_worker_count, &state.channel_id, max_workers)
}

/// Atomically check for duplicate tasks and reserve the task description.
///
/// This prevents the TOCTOU race where two concurrent `spawn_worker` calls
/// both pass a read-only duplicate check before either registers in the
/// status block. The reservation is held under a write lock on
/// `reserved_tasks` and checked against both the status block (active
/// workers) and existing reservations. The caller MUST call
/// `release_task_reservation` when the worker is registered in the status
/// block or the spawn fails.
async fn reserve_task_if_unique(
    state: &ChannelState,
    task: &str,
) -> std::result::Result<(), AgentError> {
    // Normalize the task for comparison (strip [opencode] prefix).
    let normalized = task.strip_prefix("[opencode] ").unwrap_or(task).to_string();

    let mut reserved = state.reserved_tasks.write().await;

    // Check existing reservations first (handles concurrent spawns).
    if reserved.contains(&normalized) {
        return Err(AgentError::DuplicateWorkerTask {
            channel_id: state.channel_id.to_string(),
            existing_worker_id: "pending".to_string(),
        });
    }

    // Check the status block for already-running workers.
    let status = state.status_block.read().await;
    if let Some(existing_id) = status.find_duplicate_worker_task(task) {
        return Err(AgentError::DuplicateWorkerTask {
            channel_id: state.channel_id.to_string(),
            existing_worker_id: existing_id.to_string(),
        });
    }
    drop(status);

    // Reserve the task.
    reserved.insert(normalized);
    Ok(())
}

/// Release a task reservation after the worker has been registered in the
/// status block or the spawn failed.
async fn release_task_reservation(state: &ChannelState, task: &str) {
    let normalized = task.strip_prefix("[opencode] ").unwrap_or(task).to_string();
    state.reserved_tasks.write().await.remove(&normalized);
}

/// Build pre-rendered project context for injection into worker/channel prompts.
///
/// Fetches all active projects with their repos and worktrees, converts them
/// to prompt-friendly structs, and renders via the projects_context template.
/// Returns `None` if no projects exist or if rendering fails.
pub async fn build_project_context(
    deps: &AgentDeps,
    prompt_engine: &crate::prompts::engine::PromptEngine,
) -> Option<String> {
    use crate::prompts::engine::{ProjectContext, ProjectRepoContext, ProjectWorktreeContext};

    let store = &deps.project_store;
    let projects = match store
        .list_projects(Some(crate::projects::ProjectStatus::Active))
        .await
    {
        Ok(projects) => projects,
        Err(error) => {
            tracing::warn!(%error, "failed to load projects for prompt injection");
            return None;
        }
    };

    if projects.is_empty() {
        return None;
    }

    let mut contexts = Vec::with_capacity(projects.len());
    for project in &projects {
        let repos = match store.list_repos(&project.id).await {
            Ok(repos) => repos,
            Err(error) => {
                tracing::warn!(%error, project_id = %project.id, "failed to load repos for project");
                Vec::new()
            }
        };

        let worktrees = match store.list_worktrees_with_repos(&project.id).await {
            Ok(worktrees) => worktrees,
            Err(error) => {
                tracing::warn!(%error, project_id = %project.id, "failed to load worktrees for project");
                Vec::new()
            }
        };

        contexts.push(ProjectContext {
            name: project.name.clone(),
            root_path: project.root_path.clone(),
            description: if project.description.is_empty() {
                None
            } else {
                Some(project.description.clone())
            },
            tags: project.tags.clone(),
            repos: repos
                .into_iter()
                .map(|repo| ProjectRepoContext {
                    name: repo.name.clone(),
                    path: repo.path.clone(),
                    default_branch: repo.default_branch.clone(),
                    remote_url: if repo.remote_url.is_empty() {
                        None
                    } else {
                        Some(repo.remote_url.clone())
                    },
                })
                .collect(),
            worktrees: worktrees
                .into_iter()
                .map(|wt| ProjectWorktreeContext {
                    name: wt.worktree.name.clone(),
                    path: wt.worktree.path.clone(),
                    branch: wt.worktree.branch.clone(),
                    repo_name: wt.repo_name.clone(),
                })
                .collect(),
        });
    }

    match prompt_engine.render_projects_context(contexts) {
        Ok(rendered) => {
            let rendered = rendered.trim().to_string();
            if rendered.is_empty() {
                None
            } else {
                Some(rendered)
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to render projects context");
            None
        }
    }
}

/// Spawn a worker from a ChannelState. Used by the SpawnWorkerTool.
pub async fn spawn_worker_from_state(
    state: &ChannelState,
    task: impl Into<String>,
    interactive: bool,
    suggested_skills: &[&str],
    worker_context: &WorkerContextMode,
) -> std::result::Result<WorkerId, AgentError> {
    check_worker_limit(state).await?;
    let task = task.into();
    reserve_task_if_unique(state, &task).await?;
    ensure_dispatch_readiness(state, "worker");

    let result =
        spawn_worker_inner(state, &task, interactive, suggested_skills, worker_context).await;

    // Release the reservation regardless of success or failure.
    // On success the task is now in the status block; on failure it needs cleanup.
    release_task_reservation(state, &task).await;

    result
}

/// Inner implementation of worker spawning, separated so the caller can
/// handle task reservation cleanup in a single place.
async fn spawn_worker_inner(
    state: &ChannelState,
    task: &str,
    interactive: bool,
    suggested_skills: &[&str],
    worker_context: &WorkerContextMode,
) -> std::result::Result<WorkerId, AgentError> {
    let rc = &state.deps.runtime_config;
    let prompt_engine = rc.prompts.load();

    let worker_status_text = build_worker_status_text(rc.as_ref(), &state.deps.sandbox);

    let sandbox_enabled = state.deps.sandbox.mode_enabled();
    let sandbox_containment_active = state.deps.sandbox.containment_active();
    let sandbox_read_allowlist = state.deps.sandbox.prompt_read_allowlist();
    let sandbox_write_allowlist = state.deps.sandbox.prompt_write_allowlist();
    // Collect tool secret names so the worker template can list available credentials.
    let secrets_guard = rc.secrets.load();
    let tool_secret_names = match (*secrets_guard).as_ref() {
        Some(store) => store.tool_secret_names(&state.deps.agent_id),
        None => Vec::new(),
    };

    let browser_config = (**rc.browser_config.load()).clone();
    let routing = rc.routing.load();
    let model_name = routing.resolve(ProcessType::Worker, None).to_string();
    let tool_use_enforcement = rc.tool_use_enforcement.load();
    let project_context = build_project_context(&state.deps, &prompt_engine).await;
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
            worker_status_text,
            worker_context.wiki_write && state.deps.wiki_store.is_some(),
            project_context,
        )
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;
    let skills = rc.skills.load();
    let brave_search_key = (**rc.brave_search_key.load()).clone();

    // Append skills listing to worker system prompt. Suggested skills are
    // flagged so the worker knows the channel's intent, but it can read any
    // skill it decides is relevant via the read_skill tool.
    let system_prompt = match skills.render_worker_skills(suggested_skills, &prompt_engine) {
        Ok(skills_prompt) if !skills_prompt.is_empty() => {
            format!("{worker_system_prompt}\n\n{skills_prompt}")
        }
        Ok(_) => worker_system_prompt,
        Err(error) => {
            tracing::warn!(%error, "failed to render worker skills listing, spawning without skills context");
            worker_system_prompt
        }
    };

    // Append tool-use enforcement after skills so it's the last instruction
    // in the preamble ("last instruction wins").
    let mut system_prompt = prompt_engine
        .maybe_append_tool_use_enforcement(
            system_prompt,
            tool_use_enforcement.as_ref(),
            &model_name,
        )
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;

    // Inject memory context based on worker_context settings
    if worker_context.memory.ambient_enabled() {
        // Get knowledge synthesis and working memory
        let knowledge_synthesis = state.deps.runtime_config.knowledge_synthesis.load();
        let wm_config = **state.deps.runtime_config.working_memory.load();
        let timezone = state.deps.working_memory.timezone();

        if let Ok(working_memory) = crate::memory::working::render_working_memory(
            &state.deps.working_memory,
            state.channel_id.as_ref(),
            &wm_config,
            timezone,
        )
        .await
        {
            system_prompt.push_str("\n\n## Agent's Knowledge\n");
            system_prompt.push_str(&knowledge_synthesis.to_string());
            if !working_memory.is_empty() {
                system_prompt.push_str("\n\n## Recent Activity\n");
                system_prompt.push_str(&working_memory);
            }
        }
    }

    // Inject conversation history if needed
    let initial_history: Vec<rig::message::Message> = match worker_context.history {
        WorkerHistoryMode::None => Vec::new(),
        WorkerHistoryMode::Summary => {
            // TODO: Generate an LLM-based summary of conversation history.
            tracing::warn!(
                "WorkerHistoryMode::Summary is not yet implemented, worker will receive no history"
            );
            Vec::new()
        }
        WorkerHistoryMode::Recent(n) => {
            let history = state.history.read().await;
            history
                .iter()
                .rev()
                .take(n as usize)
                .rev()
                .cloned()
                .collect()
        }
        WorkerHistoryMode::Full => {
            let history = state.history.read().await;
            history.clone()
        }
    };

    let worker_model_override = state
        .model_overrides
        .resolve_model("worker")
        .map(String::from);

    let worker = if interactive {
        let (worker, input_tx, inject_tx) = Worker::new_interactive(
            Some(state.channel_id.clone()),
            task,
            &system_prompt,
            state.deps.clone(),
            browser_config.clone(),
            state.screenshot_dir.clone(),
            brave_search_key.clone(),
            state.logs_dir.clone(),
            initial_history,
            worker_context.memory,
            worker_context.wiki_write,
            worker_model_override,
        );
        let worker_id = worker.id;
        state
            .worker_inputs
            .write()
            .await
            .insert(worker_id, input_tx);
        state
            .worker_injections
            .write()
            .await
            .insert(worker_id, inject_tx);
        worker
    } else {
        let (worker, inject_tx) = Worker::new(
            Some(state.channel_id.clone()),
            task,
            &system_prompt,
            state.deps.clone(),
            browser_config,
            state.screenshot_dir.clone(),
            brave_search_key,
            state.logs_dir.clone(),
            initial_history,
            worker_context.memory,
            worker_context.wiki_write,
            worker_model_override,
        );
        state
            .worker_injections
            .write()
            .await
            .insert(worker.id, inject_tx);
        worker
    };

    let worker_id = worker.id;

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
    );
    let secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
    let handle = spawn_worker_task(
        worker_id,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        secrets_store,
        "builtin",
        worker.run().instrument(worker_span),
    );

    state.worker_handles.write().await.insert(worker_id, handle);

    {
        let mut status = state.status_block.write().await;
        status.add_worker(worker_id, task, false, interactive);
    }

    state
        .deps
        .event_tx
        .send(crate::ProcessEvent::WorkerStarted {
            agent_id: state.deps.agent_id.clone(),
            worker_id,
            channel_id: Some(state.channel_id.clone()),
            task: task.to_string(),
            worker_type: "builtin".into(),
            interactive,
            directory: None,
        })
        .ok();

    state
        .deps
        .working_memory
        .emit(
            crate::memory::WorkingMemoryEventType::WorkerSpawned,
            format!("Worker spawned: {task}"),
        )
        .channel(state.channel_id.to_string())
        .importance(0.6)
        .record();

    tracing::info!(worker_id = %worker_id, task = %task, interactive, "worker spawned");

    Ok(worker_id)
}

/// Spawn an OpenCode-backed worker for coding tasks.
///
/// Instead of a Rig agent loop, this spawns an OpenCode subprocess that has its
/// own codebase exploration, context management, and tool suite. The worker
/// communicates with OpenCode via HTTP + SSE.
pub async fn spawn_opencode_worker_from_state(
    state: &ChannelState,
    task: impl Into<String>,
    directory: &str,
    interactive: bool,
) -> std::result::Result<crate::WorkerId, AgentError> {
    if !interactive {
        return Err(AgentError::Other(anyhow::anyhow!(
            "OpenCode workers must be interactive"
        )));
    }

    check_worker_limit(state).await?;
    let task = task.into();
    reserve_task_if_unique(state, &task).await?;
    ensure_dispatch_readiness(state, "opencode_worker");

    let result = spawn_opencode_worker_inner(state, &task, directory, interactive).await;

    // Release the reservation regardless of success or failure.
    release_task_reservation(state, &task).await;

    result
}

/// Inner implementation of OpenCode worker spawning, separated so the
/// caller can handle task reservation cleanup in a single place.
async fn spawn_opencode_worker_inner(
    state: &ChannelState,
    task: &str,
    directory: &str,
    interactive: bool,
) -> std::result::Result<crate::WorkerId, AgentError> {
    let directory = expand_tilde(directory);

    let rc = &state.deps.runtime_config;
    let opencode_config = rc.opencode.load();

    if !opencode_config.enabled {
        return Err(AgentError::Other(anyhow::anyhow!(
            "OpenCode workers are not enabled in config"
        )));
    }

    let server_pool = rc.opencode_server_pool.load().clone();

    // Prevent multiple opencode workers on the same directory.
    server_pool
        .claim_directory(&directory)
        .await
        .map_err(AgentError::Other)?;

    // Clone for the release call in the async worker task.
    let release_pool = server_pool.clone();
    let release_directory = directory.clone();
    let persist_directory = directory.clone();

    let oc_secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();

    // Build temporal/status context so OpenCode workers get the same system
    // info (time, model, context window) as builtin workers.
    let worker_status_text = build_worker_status_text(rc.as_ref(), &state.deps.sandbox);

    let worker = if interactive {
        let (worker, input_tx) = crate::opencode::OpenCodeWorker::new_interactive(
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            task,
            directory,
            server_pool,
            state.deps.event_tx.clone(),
        );
        let worker_id = worker.id;
        state
            .worker_inputs
            .write()
            .await
            .insert(worker_id, input_tx);
        let worker = match worker_status_text {
            Some(ref prompt) => worker.with_system_prompt(prompt),
            None => worker,
        };
        let worker = match &oc_secrets_store {
            Some(store) => worker.with_secrets_store(store.clone()),
            None => worker,
        };
        worker.with_sqlite_pool(state.deps.sqlite_pool.clone())
    } else {
        let worker = crate::opencode::OpenCodeWorker::new(
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            task,
            directory,
            server_pool,
            state.deps.event_tx.clone(),
        );
        let worker = match worker_status_text {
            Some(ref prompt) => worker.with_system_prompt(prompt),
            None => worker,
        };
        let worker = match &oc_secrets_store {
            Some(store) => worker.with_secrets_store(store.clone()),
            None => worker,
        };
        worker.with_sqlite_pool(state.deps.sqlite_pool.clone())
    };

    let worker_id = worker.id;

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
        worker_type = "opencode",
    );
    let sqlite_pool = state.deps.sqlite_pool.clone();
    let handle = spawn_worker_task(
        worker_id,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        oc_secrets_store,
        "opencode",
        async move {
            let result = worker.run().await.map_err(SpacebotError::from);

            // Release the directory claim regardless of success or failure.
            release_pool.release_directory(&release_directory).await;

            let result = result?;

            // Persist the transcript built from SSE events so the worker detail
            // view can show the full conversation (text + tool calls + results).
            if !result.transcript.is_empty() {
                let blob = crate::conversation::worker_transcript::serialize_steps(
                    &result.transcript,
                );
                let tool_calls = result.tool_calls;
                let wid = worker_id.to_string();
                let pool = sqlite_pool.clone();
                tokio::spawn(async move {
                    if let Err(error) = sqlx::query(
                        "UPDATE worker_runs SET transcript = ?, tool_calls = ? WHERE id = ?",
                    )
                    .bind(&blob)
                    .bind(tool_calls)
                    .bind(&wid)
                    .execute(&pool)
                    .await
                    {
                        tracing::warn!(%error, worker_id = wid, "failed to persist OpenCode transcript");
                    }
                });
            }

            Ok::<WorkerOutcome, SpacebotError>(WorkerOutcome::Success {
                result: result.result_text,
            })
        }
        .instrument(worker_span),
    );

    state.worker_handles.write().await.insert(worker_id, handle);

    let opencode_task = format!("[opencode] {task}");
    {
        let mut status = state.status_block.write().await;
        status.add_worker(worker_id, &opencode_task, false, interactive);
    }

    state
        .deps
        .event_tx
        .send(crate::ProcessEvent::WorkerStarted {
            agent_id: state.deps.agent_id.clone(),
            worker_id,
            channel_id: Some(state.channel_id.clone()),
            task: opencode_task,
            worker_type: "opencode".into(),
            interactive,
            directory: Some(persist_directory.to_string_lossy().to_string()),
        })
        .ok();

    state
        .deps
        .working_memory
        .emit(
            crate::memory::WorkingMemoryEventType::WorkerSpawned,
            format!("Worker spawned (opencode): {task}"),
        )
        .channel(state.channel_id.to_string())
        .importance(0.6)
        .record();

    tracing::info!(worker_id = %worker_id, task = %task, interactive, "OpenCode worker spawned");

    Ok(worker_id)
}

/// Spawn a future as a tokio task that sends a `WorkerComplete` event on completion.
///
/// Handles both success and error cases, logging failures and sending the
/// appropriate event. Used by both builtin workers and OpenCode workers.
/// Returns the JoinHandle so the caller can store it for cancellation.
///
/// The result text is scrubbed through the secret store's tool secret values
/// before being sent via the event — tool secret values are replaced with
/// `[REDACTED:<name>]` so they never propagate to channel context.
pub(crate) fn spawn_worker_task<F>(
    worker_id: WorkerId,
    event_tx: broadcast::Sender<ProcessEvent>,
    agent_id: crate::AgentId,
    channel_id: Option<ChannelId>,
    secrets_store: Option<Arc<crate::secrets::store::SecretsStore>>,
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables))] worker_type: &'static str,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = crate::Result<WorkerOutcome>> + Send + 'static,
{
    tokio::spawn(async move {
        #[cfg(feature = "metrics")]
        let worker_start = std::time::Instant::now();

        #[cfg(feature = "metrics")]
        crate::telemetry::Metrics::global()
            .active_workers
            .with_label_values(&[&*agent_id])
            .inc();

        let raw = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        let scrub = |text: String| -> String {
            let layer1 = if let Some(store) = &secrets_store {
                crate::secrets::scrub::scrub_with_store(&text, store, &agent_id)
            } else {
                text
            };
            crate::secrets::scrub::scrub_leaks(&layer1)
        };
        let worker_result: std::result::Result<WorkerOutcome, WorkerCompletionError> = match raw {
            Ok(Ok(outcome)) => Ok(scrub_outcome(outcome, &scrub)),
            Ok(Err(error)) => match WorkerCompletionError::from_spacebot_error(error) {
                WorkerCompletionError::Cancelled { reason } => {
                    Err(WorkerCompletionError::Cancelled { reason })
                }
                WorkerCompletionError::Failed { message } => Err(WorkerCompletionError::Failed {
                    message: scrub(message),
                }),
            },
            Err(panic_payload) => {
                let panic_message = crate::agent::panic_payload_to_string(&*panic_payload);
                tracing::error!(
                    worker_id = %worker_id,
                    panic_message = %panic_message,
                    "worker task panicked"
                );
                Err(WorkerCompletionError::failed(format!(
                    "worker task panicked: {panic_message}"
                )))
            }
        };
        let (result_text, kind) = classify_worker_completion(worker_result);
        match kind {
            WorkerCompletionKind::Success | WorkerCompletionKind::Partial => {}
            WorkerCompletionKind::Cancelled => {
                tracing::info!(worker_id = %worker_id, result = %result_text, "worker cancelled");
            }
            WorkerCompletionKind::Timeout => {
                tracing::warn!(worker_id = %worker_id, result = %result_text, "worker timed out");
            }
            WorkerCompletionKind::Blocked => {
                tracing::warn!(worker_id = %worker_id, result = %result_text, "worker blocked");
            }
            WorkerCompletionKind::Failed => {
                tracing::error!(worker_id = %worker_id, result = %result_text, "worker failed");
            }
        };
        let (notify, success) = completion_flags(kind);
        #[cfg(feature = "metrics")]
        {
            let metrics = crate::telemetry::Metrics::global();
            metrics
                .active_workers
                .with_label_values(&[&*agent_id])
                .dec();
            metrics
                .worker_duration_seconds
                .with_label_values(&[&*agent_id, worker_type])
                .observe(worker_start.elapsed().as_secs_f64());
        }

        let _ = event_tx.send(ProcessEvent::WorkerComplete {
            agent_id,
            worker_id,
            channel_id,
            result: result_text,
            notify,
            success,
        });
    })
}

/// Apply scrubbing to any text content carried by a `WorkerOutcome`.
fn scrub_outcome<F>(outcome: WorkerOutcome, scrub: &F) -> WorkerOutcome
where
    F: Fn(String) -> String,
{
    match outcome {
        WorkerOutcome::Success { result } => WorkerOutcome::Success {
            result: scrub(result),
        },
        WorkerOutcome::Partial {
            result,
            segments_run,
        } => WorkerOutcome::Partial {
            result: scrub(result),
            segments_run,
        },
        WorkerOutcome::Cancelled { reason } => WorkerOutcome::Cancelled { reason },
        WorkerOutcome::Timeout {
            elapsed_secs,
            segments_run,
        } => WorkerOutcome::Timeout {
            elapsed_secs,
            segments_run,
        },
        WorkerOutcome::Blocked {
            reason,
            url,
            mut evidence,
        } => {
            // Scrub URL too — query params and path segments routinely
            // carry bearer tokens / session ids. The blocked URL flows
            // into WorkerComplete.result and channel logs, so an
            // un-scrubbed URL is a credential-leak path.
            let url = url.map(scrub);
            if let Some(snippet) = evidence.html_snippet.take() {
                evidence.html_snippet = Some(scrub(snippet));
            }
            if let Some(final_url) = evidence.final_url.take() {
                evidence.final_url = Some(scrub(final_url));
            }
            WorkerOutcome::Blocked {
                reason,
                url,
                evidence,
            }
        }
        WorkerOutcome::Failed { reason } => WorkerOutcome::Failed {
            reason: scrub(reason),
        },
    }
}

/// Resume an idle interactive worker into a channel's state after restart.
///
/// Loads the prior transcript, creates a resumed worker (builtin or opencode),
/// registers it into the channel's worker_inputs/worker_handles/status_block,
/// and spawns the follow-up loop. Returns `Ok(worker_id)` on success, or
/// an error string if the worker couldn't be resumed.
pub async fn resume_idle_worker_into_state(
    state: &ChannelState,
    idle_worker: &crate::conversation::history::IdleWorkerRow,
) -> std::result::Result<WorkerId, String> {
    let worker_id: WorkerId = idle_worker
        .id
        .parse::<uuid::Uuid>()
        .map_err(|error| format!("invalid worker ID '{}': {error}", idle_worker.id))?;

    match idle_worker.worker_type.as_str() {
        "opencode" => {
            let session_id = idle_worker
                .opencode_session_id
                .as_deref()
                .ok_or("opencode worker has no session_id, cannot resume")?;

            let rc = &state.deps.runtime_config;
            let opencode_config = rc.opencode.load();
            if !opencode_config.enabled {
                return Err("OpenCode workers are not enabled".into());
            }

            let directory = idle_worker
                .directory
                .as_deref()
                .map(std::path::PathBuf::from)
                .ok_or("idle OpenCode worker has no directory persisted, cannot resume")?;
            let server_pool = rc.opencode_server_pool.load().clone();

            let directory_str = directory.to_string_lossy().to_string();
            let result = crate::opencode::OpenCodeWorker::resume_interactive(
                worker_id,
                Some(state.channel_id.clone()),
                state.deps.agent_id.clone(),
                &idle_worker.task,
                directory,
                server_pool,
                state.deps.event_tx.clone(),
                session_id.to_string(),
                idle_worker.transcript.clone(),
            )
            .await;

            let (mut worker, input_tx) = result.ok_or_else(|| {
                "failed to reconnect to OpenCode session (server dead or session expired)"
                    .to_string()
            })?;

            // Apply builder chain (same as spawn_opencode_worker_from_state).
            let oc_secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
            if let Some(store) = &oc_secrets_store {
                worker = worker.with_secrets_store(store.clone());
            }
            worker = worker.with_sqlite_pool(state.deps.sqlite_pool.clone());

            state
                .worker_inputs
                .write()
                .await
                .insert(worker_id, input_tx);

            let worker_span = tracing::info_span!(
                "worker.resume",
                worker_id = %worker_id,
                channel_id = %state.channel_id,
                worker_type = "opencode",
            );
            let sqlite_pool = state.deps.sqlite_pool.clone();
            let handle = spawn_worker_task(
                worker_id,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                Some(state.channel_id.clone()),
                oc_secrets_store,
                "opencode",
                async move {
                    let result = worker.run().await.map_err(SpacebotError::from)?;
                    // Persist final transcript.
                    if !result.transcript.is_empty() {
                        let blob = crate::conversation::worker_transcript::serialize_steps(
                            &result.transcript,
                        );
                        let tool_calls = result.tool_calls;
                        let wid = worker_id.to_string();
                        let pool = sqlite_pool.clone();
                        tokio::spawn(async move {
                            if let Err(error) = sqlx::query(
                                "UPDATE worker_runs SET transcript = ?, tool_calls = ? WHERE id = ?",
                            )
                            .bind(&blob)
                            .bind(tool_calls)
                            .bind(&wid)
                            .execute(&pool)
                            .await
                            {
                                tracing::warn!(%error, worker_id = wid, "failed to persist OpenCode transcript");
                            }
                        });
                    }
                    Ok::<WorkerOutcome, SpacebotError>(WorkerOutcome::Success {
                result: result.result_text,
            })
                }
                .instrument(worker_span),
            );

            state.worker_handles.write().await.insert(worker_id, handle);

            let opencode_task = format!("[opencode] {}", idle_worker.task);
            {
                let mut status = state.status_block.write().await;
                status.add_worker(worker_id, &opencode_task, false, true);
            }

            state
                .deps
                .event_tx
                .send(ProcessEvent::WorkerStarted {
                    agent_id: state.deps.agent_id.clone(),
                    worker_id,
                    channel_id: Some(state.channel_id.clone()),
                    task: opencode_task,
                    worker_type: "opencode".into(),
                    interactive: true,
                    directory: Some(directory_str.clone()),
                })
                .ok();

            tracing::info!(worker_id = %worker_id, task = %idle_worker.task, "OpenCode worker resumed");
            Ok(worker_id)
        }
        _ => {
            // Builtin worker resume: deserialize transcript blob back into
            // Rig message history so the LLM can continue the conversation.
            let prior_history = if let Some(blob) = &idle_worker.transcript {
                let steps = crate::conversation::worker_transcript::deserialize_transcript(blob)
                    .map_err(|error| format!("failed to deserialize transcript: {error}"))?;
                crate::conversation::worker_transcript::transcript_to_history(&steps)
            } else {
                return Err("no transcript blob to restore history from".into());
            };

            let rc = &state.deps.runtime_config;
            let prompt_engine = rc.prompts.load();

            let worker_status_text = build_worker_status_text(rc.as_ref(), &state.deps.sandbox);

            let sandbox_enabled = state.deps.sandbox.mode_enabled();
            let sandbox_containment_active = state.deps.sandbox.containment_active();
            let sandbox_read_allowlist = state.deps.sandbox.prompt_read_allowlist();
            let sandbox_write_allowlist = state.deps.sandbox.prompt_write_allowlist();
            let secrets_guard = rc.secrets.load();
            let tool_secret_names = match (*secrets_guard).as_ref() {
                Some(store) => store.tool_secret_names(&state.deps.agent_id),
                None => Vec::new(),
            };
            let browser_config = (**rc.browser_config.load()).clone();
            let routing = rc.routing.load();
            let model_name = routing.resolve(ProcessType::Worker, None).to_string();
            let tool_use_enforcement = rc.tool_use_enforcement.load();
            let project_context = build_project_context(&state.deps, &prompt_engine).await;
            let system_prompt = prompt_engine
                .render_worker_prompt(
                    &rc.instance_dir.display().to_string(),
                    &rc.workspace_dir.display().to_string(),
                    sandbox_enabled,
                    sandbox_containment_active,
                    sandbox_read_allowlist,
                    sandbox_write_allowlist,
                    &tool_secret_names,
                    browser_config.persist_session,
                    worker_status_text,
                    false, // resumed workers use original context; wiki not re-injected
                    project_context,
                )
                .and_then(|prompt| {
                    prompt_engine.maybe_append_tool_use_enforcement(
                        prompt,
                        tool_use_enforcement.as_ref(),
                        &model_name,
                    )
                })
                .map_err(|error| format!("failed to render worker prompt: {error}"))?;
            let brave_search_key = (**rc.brave_search_key.load()).clone();

            let (worker, input_tx, inject_tx) = Worker::resume_interactive(
                worker_id,
                Some(state.channel_id.clone()),
                &idle_worker.task,
                &system_prompt,
                state.deps.clone(),
                browser_config,
                state.screenshot_dir.clone(),
                brave_search_key,
                state.logs_dir.clone(),
                prior_history,
            );

            state
                .worker_inputs
                .write()
                .await
                .insert(worker_id, input_tx);
            state
                .worker_injections
                .write()
                .await
                .insert(worker_id, inject_tx);

            let worker_span = tracing::info_span!(
                "worker.resume",
                worker_id = %worker_id,
                channel_id = %state.channel_id,
            );
            let secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
            let handle = spawn_worker_task(
                worker_id,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                Some(state.channel_id.clone()),
                secrets_store,
                "builtin",
                worker.run().instrument(worker_span),
            );

            state.worker_handles.write().await.insert(worker_id, handle);

            {
                let mut status = state.status_block.write().await;
                status.add_worker(worker_id, &idle_worker.task, false, true);
            }

            state
                .deps
                .event_tx
                .send(ProcessEvent::WorkerStarted {
                    agent_id: state.deps.agent_id.clone(),
                    worker_id,
                    channel_id: Some(state.channel_id.clone()),
                    task: idle_worker.task.clone(),
                    worker_type: "builtin".into(),
                    interactive: true,
                    directory: None,
                })
                .ok();

            tracing::info!(worker_id = %worker_id, task = %idle_worker.task, "builtin worker resumed");
            Ok(worker_id)
        }
    }
}

/// Expand a leading `~` or `~/` in a path to the user's home directory.
///
/// LLMs consistently produce tilde-prefixed paths because that's what appears
/// in conversation context. `std::path::Path::canonicalize()` doesn't expand
/// tildes (that's a shell feature), so paths like `~/Projects/foo` fail with
/// "directory does not exist". This handles the common cases.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~" {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
    } else if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/"))
            .join(rest)
    } else {
        std::path::PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkerCompletionError, WorkerOutcome, map_worker_completion, spawn_worker_task};
    use crate::{ProcessEvent, WorkerId};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    #[test]
    fn cancelled_errors_are_classified_as_cancelled_results() {
        let (text, notify, success) =
            map_worker_completion(Err(WorkerCompletionError::Cancelled {
                reason: "user requested".to_string(),
            }));
        assert_eq!(text, "Worker cancelled: user requested");
        assert!(notify);
        assert!(!success);
    }

    #[test]
    fn timeout_outcome_is_classified_as_unsuccessful() {
        let (text, notify, success) = map_worker_completion(Ok(WorkerOutcome::Timeout {
            elapsed_secs: 1800,
            segments_run: 7,
        }));
        assert_eq!(
            text,
            "Worker exceeded 1800s wall-clock timeout after 7 segments."
        );
        assert!(notify);
        assert!(!success);
    }

    #[test]
    fn blocked_outcome_is_classified_as_unsuccessful() {
        use crate::agent::worker::{BlockEvidence, BlockReason};
        let (text, notify, success) = map_worker_completion(Ok(WorkerOutcome::Blocked {
            reason: BlockReason::Captcha {
                provider: "cloudflare-turnstile".to_string(),
            },
            url: Some("https://example.com/signup".to_string()),
            evidence: Box::new(BlockEvidence::default()),
        }));
        assert!(text.contains("captcha"));
        assert!(text.contains("https://example.com/signup"));
        assert!(notify);
        assert!(!success);
    }

    #[test]
    fn partial_outcome_is_classified_as_successful() {
        let (text, notify, success) = map_worker_completion(Ok(WorkerOutcome::Partial {
            result: "partial body".to_string(),
            segments_run: 10,
        }));
        assert!(text.contains("partial body"));
        assert!(text.contains("max segments"));
        assert!(notify);
        assert!(success);
    }

    #[tokio::test]
    async fn spawn_worker_task_emits_cancelled_completion_event() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id: WorkerId = Uuid::new_v4();

        let handle = spawn_worker_task(
            worker_id,
            event_tx,
            Arc::<str>::from("agent"),
            Some(Arc::<str>::from("channel")),
            None,
            "builtin",
            async {
                Err::<WorkerOutcome, crate::Error>(
                    crate::error::AgentError::Cancelled {
                        reason: "user requested".to_string(),
                    }
                    .into(),
                )
            },
        );

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("worker completion event should be delivered")
            .expect("broadcast receive should succeed");
        handle.await.expect("worker task should join cleanly");

        match event {
            ProcessEvent::WorkerComplete {
                worker_id: completed_worker_id,
                result,
                notify,
                success,
                ..
            } => {
                assert_eq!(completed_worker_id, worker_id);
                assert_eq!(result, "Worker cancelled: user requested");
                assert!(notify);
                assert!(!success);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_worker_task_carries_channel_id() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id: WorkerId = Uuid::new_v4();
        let channel_id: crate::ChannelId = Arc::from("test-channel");

        let handle = spawn_worker_task(
            worker_id,
            event_tx,
            Arc::<str>::from("agent"),
            Some(channel_id.clone()),
            None,
            "builtin",
            async {
                Ok::<WorkerOutcome, crate::Error>(WorkerOutcome::Success {
                    result: "result".to_string(),
                })
            },
        );

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("worker completion event should be delivered")
            .expect("broadcast receive should succeed");
        handle.await.expect("worker task should join cleanly");

        match event {
            ProcessEvent::WorkerComplete {
                channel_id: event_channel_id,
                worker_id: completed_worker_id,
                success,
                ..
            } => {
                assert_eq!(completed_worker_id, worker_id);
                assert_eq!(event_channel_id, Some(channel_id));
                assert!(success);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
