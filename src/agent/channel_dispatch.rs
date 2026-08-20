//! Branch and worker spawning for channels.
//!
//! Contains the public entry points that channel tools use to create
//! background processes: `spawn_branch_from_state`, `spawn_worker_from_state`,
//! and `spawn_opencode_worker_from_state`.

use crate::agent::branch::{Branch, BranchExecutionConfig};
use crate::agent::channel::ChannelState;
use crate::agent::channel_prompt::TemporalContext;
use crate::agent::worker::{Worker, WorkerOutcome};
use crate::agent::worker::{WorkerTranscriptSnapshot, read_worker_transcript_snapshot};
use crate::conversation::settings::{WorkerContextMode, WorkerHistoryMode};
use crate::conversation::{
    ProcessRunLogger, WorkerCompletionCommit, WorkerLifecycle, WorkerOutcomeKind,
    WorkerTerminalOutcome, WorkerTerminalOwner, WorkerTransitionResult,
};
use crate::error::{AgentError, Error as SpacebotError};
use crate::tools::{BranchDelegationState, BranchToolProfile, MemoryPersistenceContractState};
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

pub struct WorkerTaskControl {
    pub handle: tokio::task::JoinHandle<()>,
    pub cancel_tx: tokio::sync::watch::Sender<bool>,
    pub terminal_notify: Arc<tokio::sync::Notify>,
    pub transcript_snapshot: WorkerTranscriptSnapshot,
    pub opencode_cancellation: Option<
        Arc<tokio::sync::Mutex<Option<crate::opencode::worker::OpenCodeCancellationSession>>>,
    >,
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
        Ok(outcome @ WorkerOutcome::Timeout { .. }) => {
            (outcome.into_text(), WorkerCompletionKind::Timeout)
        }
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

/// How a run ended, as a task's attempt history records it.
///
fn completion_flags(kind: WorkerCompletionKind) -> (bool, bool) {
    let notify = true;
    let success = matches!(
        kind,
        WorkerCompletionKind::Success | WorkerCompletionKind::Partial
    );
    (notify, success)
}

fn outcome_kind(kind: WorkerCompletionKind) -> WorkerOutcomeKind {
    match kind {
        WorkerCompletionKind::Success => WorkerOutcomeKind::Succeeded,
        WorkerCompletionKind::Partial => WorkerOutcomeKind::Partial,
        WorkerCompletionKind::Cancelled => WorkerOutcomeKind::Cancelled,
        WorkerCompletionKind::Timeout => WorkerOutcomeKind::TimedOut,
        WorkerCompletionKind::Blocked => WorkerOutcomeKind::Blocked,
        WorkerCompletionKind::Failed => WorkerOutcomeKind::Failed,
    }
}

/// Normalize a worker outcome (or terminal error) into event payload fields.
#[cfg(test)]
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
    let model_name = state
        .model_overrides
        .resolve_model("branch")
        .unwrap_or_else(|| routing.resolve(ProcessType::Branch, None))
        .to_string();
    let tool_use_enforcement = rc.tool_use_enforcement.load();
    let wiki_enabled = state.deps.wiki_store.is_some();
    let mut system_prompt = prompt_engine
        .render_branch_prompt(
            &rc.instance_dir.display().to_string(),
            &rc.workspace_dir.display().to_string(),
            wiki_enabled,
        )
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;
    let skills_prompt = rc
        .skills
        .load()
        .render_branch_skills(&prompt_engine)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;
    system_prompt.append_section("skills_prompt", &skills_prompt);
    system_prompt.adopt_appended(
        prompt_engine
            .maybe_append_tool_use_enforcement(
                system_prompt.text.clone(),
                tool_use_enforcement.as_ref(),
                &model_name,
            )
            .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?,
        "tool_use_enforcement",
    );

    spawn_branch(
        state,
        &description,
        &description,
        system_prompt,
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
///
/// When `skill_reflection` is set, the same pass also reflects on skills:
/// the prompt gains the reflection section and the branch gets skill tools
/// under agent-origin rails.
pub(crate) async fn spawn_memory_persistence_branch(
    state: &ChannelState,
    deps: &AgentDeps,
    skill_reflection: bool,
    reflection_workers: &[(crate::WorkerId, bool)],
) -> std::result::Result<BranchId, AgentError> {
    let contract_state = Arc::new(MemoryPersistenceContractState::default());

    let prompt_engine = deps.runtime_config.prompts.load();
    let routing = deps.runtime_config.routing.load();
    let model_name = state
        .model_overrides
        .resolve_model("branch")
        .unwrap_or_else(|| routing.resolve(ProcessType::Branch, None))
        .to_string();
    let tool_use_enforcement = deps.runtime_config.tool_use_enforcement.load();
    let reflection_worker_ids: Vec<String> = reflection_workers
        .iter()
        .map(|(id, success)| {
            let status = if *success { "succeeded" } else { "failed" };
            format!("{id} — {status}")
        })
        .collect();
    let mut system_prompt = prompt_engine
        .render_memory_persistence_prompt(skill_reflection, &reflection_worker_ids)
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;
    system_prompt.adopt_appended(
        prompt_engine
            .maybe_append_tool_use_enforcement(
                system_prompt.text.clone(),
                tool_use_enforcement.as_ref(),
                &model_name,
            )
            .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?,
        "tool_use_enforcement",
    );
    let prompt = prompt_engine
        .render_system_memory_persistence()
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;

    spawn_branch(
        state,
        "memory persistence",
        &prompt,
        system_prompt,
        if skill_reflection {
            "persisting memories and reflecting on skills..."
        } else {
            "persisting memories..."
        },
        "memory_persistence_branch",
        BranchSpawnOptions {
            profile: BranchToolProfile::MemoryPersistence {
                contract_state,
                working_memory: Some(state.deps.working_memory.clone()),
                channel_id: Some(state.channel_id.to_string()),
                skill_reflection,
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
        refresh_age_secs = ?readiness.refresh_age_secs,
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
    system_prompt: crate::prompts::SegmentedPrompt,
    status_label: &str,
    dispatch_type: &'static str,
    branch_options: BranchSpawnOptions,
) -> std::result::Result<BranchId, AgentError> {
    if state
        .autonomy_run
        .as_ref()
        .is_some_and(crate::agent::autonomy::AutonomyRunHandle::finish_requested)
    {
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn branch: autonomy run is settling"
        )));
    }
    let BranchSpawnOptions { profile } = branch_options;
    let profile_name = profile.name().to_string();
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

    let branch_id = crate::BranchId::new_v4();
    let branch_delegation = matches!(&profile, BranchToolProfile::Default)
        .then(|| Arc::new(BranchDelegationState::new(branch_id)));
    let tool_server = crate::tools::create_branch_tool_server(
        Some(state.clone()),
        state.deps.agent_id.clone(),
        state.deps.task_store.clone(),
        state.deps.goal_store.clone(),
        state.deps.project_store.clone(),
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
        branch_delegation.clone(),
    );
    let branch_max_turns = **state.deps.runtime_config.branch_max_turns.load();
    let model_override = state
        .model_overrides
        .resolve_model("branch")
        .map(String::from);
    let model_name = model_override.clone().unwrap_or_else(|| {
        state
            .deps
            .runtime_config
            .routing
            .load()
            .resolve(ProcessType::Branch, None)
            .to_string()
    });

    let branch = Branch::new(
        branch_id,
        state.channel_id.clone(),
        description,
        state.deps.clone(),
        system_prompt,
        history,
        tool_server,
        BranchExecutionConfig {
            max_turns: branch_max_turns,
            memory_persistence_contract,
            branch_delegation,
        },
        model_override,
    );

    let prompt = prompt.to_owned();

    state
        .process_run_logger
        .log_branch_started(
            &state.channel_id,
            branch_id,
            description,
            &prompt,
            &profile_name,
            &model_name,
            branch_max_turns,
            state
                .autonomy_run
                .as_ref()
                .map(|autonomy_run| autonomy_run.run_id.as_str()),
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;

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
    {
        let mut status = state.status_block.write().await;
        status.add_branch(branch_id, status_label);
    }

    state
        .deps
        .event_tx
        .send(crate::ProcessEvent::BranchStarted {
            agent_id: state.deps.agent_id.clone(),
            branch_id,
            channel_id: state.channel_id.clone(),
            description: status_label.to_string(),
            input: prompt.clone(),
            profile: profile_name,
            model: model_name,
            max_turns: branch_max_turns,
            reply_to_message_id: state.reply_target_message_id.read().await.clone(),
        })
        .ok();

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
                    status: "failed".to_string(),
                    transcript: None,
                    tool_calls: 0,
                });
            }
        }
        .instrument(branch_span),
    );
    branches.insert(branch_id, handle);
    drop(branches);

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

fn worker_task_prompt(task: &str, task_context: Option<&str>) -> String {
    match task_context {
        Some(task_context) => format!("{task}\n\n{task_context}"),
        None => task.to_string(),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WorkerTaskContext<'a> {
    pub task_context: Option<&'a str>,
    pub origin_branch_id: Option<BranchId>,
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
///
/// `required_skills` differ from `suggested_skills`: their full content is
/// injected into the worker's system prompt rather than flagged in the
/// index, so the worker cannot skip them.
pub async fn spawn_worker_from_state(
    state: &ChannelState,
    task: impl Into<String>,
    interactive: bool,
    suggested_skills: &[&str],
    required_skills: &[&str],
    worker_context: &WorkerContextMode,
    task_context: WorkerTaskContext<'_>,
) -> std::result::Result<WorkerId, AgentError> {
    if state
        .autonomy_run
        .as_ref()
        .is_some_and(crate::agent::autonomy::AutonomyRunHandle::finish_requested)
    {
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn worker: autonomy run is settling"
        )));
    }
    check_worker_limit(state).await?;
    let task = task.into();
    reserve_task_if_unique(state, &task).await?;
    ensure_dispatch_readiness(state, "worker");

    let result = spawn_worker_inner(
        state,
        &task,
        interactive,
        suggested_skills,
        required_skills,
        worker_context,
        task_context,
    )
    .await;

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
    required_skills: &[&str],
    worker_context: &WorkerContextMode,
    task_context: WorkerTaskContext<'_>,
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
    let model_name = state
        .model_overrides
        .resolve_model("worker")
        .unwrap_or_else(|| routing.resolve(ProcessType::Worker, None))
        .to_string();
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
    let mut system_prompt = worker_system_prompt;
    match skills.render_worker_skills(suggested_skills, &prompt_engine) {
        Ok(skills_prompt) => system_prompt.append_section("skills_prompt", &skills_prompt),
        Err(error) => {
            tracing::warn!(%error, "failed to render worker skills listing, spawning without skills context");
        }
    };

    // Required skills are a task contract: full content in the system prompt,
    // not an entry in the index the worker may or may not read. Unresolvable
    // names were rejected at task creation and at spawn.
    if let Some(required_block) = skills.render_required_skills(required_skills) {
        system_prompt.append_section("required_skills", &required_block);
    }

    // Append tool-use enforcement after skills so it's the last instruction
    // in the preamble ("last instruction wins").
    system_prompt.adopt_appended(
        prompt_engine
            .maybe_append_tool_use_enforcement(
                system_prompt.text.clone(),
                tool_use_enforcement.as_ref(),
                &model_name,
            )
            .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?,
        "tool_use_enforcement",
    );

    // Inject memory context based on worker_context settings
    if worker_context.memory.ambient_enabled() {
        // Render the memory store directly (deterministic, LLM-free) plus
        // working memory.
        let wm_config = **state.deps.runtime_config.working_memory.load();
        let timezone = state.deps.working_memory.timezone();

        let cortex_config = **state.deps.runtime_config.cortex.load();
        let memory_store = match crate::memory::render::render_memory_store(
            state.deps.memory_search.store(),
            &state.deps.task_store,
            &state.deps.agent_id,
            cortex_config.memory_render_max_words,
        )
        .await
        {
            Ok(text) if !text.is_empty() => Some(text),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "worker ambient memory store render failed");
                None
            }
        };

        if let Ok(working_memory) = crate::memory::working::render_working_memory(
            &state.deps.working_memory,
            state.channel_id.as_ref(),
            &wm_config,
            timezone,
        )
        .await
        {
            if let Some(memory_store) = memory_store {
                system_prompt.append_section("knowledge_synthesis", &memory_store);
            }
            if !working_memory.is_empty() {
                system_prompt.append_section(
                    "working_memory",
                    &format!("## Recent Activity\n{working_memory}"),
                );
            }
        }
    }

    let worker_task = worker_task_prompt(task, task_context.task_context);

    // Fork the channel's conversation history under the worker's own system
    // prompt — the same fork semantic branches use. An oversized fork is
    // compacted here so the worker's first LLM call doesn't start life in
    // overflow recovery.
    let initial_history: Vec<rig::message::Message> = match worker_context.history {
        WorkerHistoryMode::Clean => Vec::new(),
        WorkerHistoryMode::Fork => {
            let mut history = state.history.read().await.clone();
            let context_window = **state.deps.runtime_config.context_window.load();
            // The preamble is fully rendered by now — skills and ambient
            // memory included — so the fork is budgeted against what the
            // worker's first call actually leaves for history.
            let prompt_tokens = crate::agent::compactor::estimate_text_tokens(&system_prompt.text)
                + crate::agent::compactor::estimate_text_tokens(&worker_task);
            let removed = crate::agent::compactor::precompact_forked_history(
                &mut history,
                context_window,
                0.50,
                prompt_tokens,
            );
            if removed > 0 {
                tracing::info!(
                    channel_id = %state.channel_id,
                    removed,
                    history_len = history.len(),
                    "worker fork pre-compacted history"
                );
            }
            history
        }
    };

    let worker_model_override = state
        .model_overrides
        .resolve_model("worker")
        .map(String::from);

    let worker = if interactive {
        let (worker, input_tx, inject_tx) = Worker::new_interactive(
            Some(state.channel_id.clone()),
            &worker_task,
            system_prompt.clone(),
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
            &worker_task,
            system_prompt,
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
    let transcript_snapshot = worker.transcript_snapshot();

    state
        .process_run_logger
        .log_worker_started(
            Some(&state.channel_id),
            worker_id,
            task,
            "builtin",
            &state.deps.agent_id,
            interactive,
            None,
            state
                .autonomy_run
                .as_ref()
                .map(|autonomy_run| autonomy_run.run_id.as_str()),
            task_context.origin_branch_id,
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;

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
        state.process_run_logger.clone(),
        transcript_snapshot,
        None,
        None,
        secrets_store,
        Some(state.deps.task_store.clone()),
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
    required_skills: &[&str],
    task_context: WorkerTaskContext<'_>,
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

    let result = spawn_opencode_worker_inner(
        state,
        &task,
        directory,
        interactive,
        required_skills,
        task_context,
    )
    .await;

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
    required_skills: &[&str],
    task_context: WorkerTaskContext<'_>,
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

    let directory_claim = crate::opencode::server::OpenCodeDirectoryClaim::new(
        server_pool.clone(),
        directory.clone(),
    );
    let persist_directory = directory.clone();

    let oc_secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();

    // Build temporal/status context so OpenCode workers get the same system
    // info (time, model, context window) as builtin workers.
    let mut worker_status_text = build_worker_status_text(rc.as_ref(), &state.deps.sandbox);

    let task_management = crate::prompts::text::get("fragments/opencode_task_management").trim();
    worker_status_text = Some(match worker_status_text {
        Some(existing) => format!("{existing}\n\n{task_management}"),
        None => task_management.to_string(),
    });

    // OpenCode reads files natively, so required skills arrive as read-first
    // file references in the system prompt rather than inlined content.
    if !required_skills.is_empty() {
        let skills = rc.skills.load();
        let mut entries = Vec::new();
        for name in required_skills {
            match skills.get(name) {
                Some(skill) => {
                    entries.push(format!("- {} — {}", skill.file_path.display(), skill.name));
                }
                None => {
                    tracing::warn!(skill = %name, "required skill not found, skipping injection");
                }
            }
        }
        if !entries.is_empty() {
            let block = format!(
                "## Required Skills\n\nBefore starting the task, read each of these skill \
                 files and follow them — they are part of the task's contract, not \
                 suggestions:\n{}",
                entries.join("\n")
            );
            worker_status_text = Some(match worker_status_text {
                Some(existing) => format!("{existing}\n\n{block}"),
                None => block,
            });
        }
    }

    let worker_task = worker_task_prompt(task, task_context.task_context);
    let worker = if interactive {
        let (worker, input_tx) = crate::opencode::OpenCodeWorker::new_interactive(
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            &worker_task,
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
            &worker_task,
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

    state
        .process_run_logger
        .log_worker_started(
            Some(&state.channel_id),
            worker_id,
            &format!("[opencode] {task}"),
            "opencode",
            &state.deps.agent_id,
            interactive,
            Some(&persist_directory),
            state
                .autonomy_run
                .as_ref()
                .map(|autonomy_run| autonomy_run.run_id.as_str()),
            task_context.origin_branch_id,
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
        worker_type = "opencode",
    );
    let transcript_snapshot = worker.transcript_snapshot();
    let opencode_cancellation = worker.cancellation_session();
    let handle = spawn_worker_task(
        worker_id,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        state.process_run_logger.clone(),
        transcript_snapshot,
        Some(opencode_cancellation),
        Some(directory_claim),
        oc_secrets_store,
        Some(state.deps.task_store.clone()),
        "opencode",
        async move {
            let result = worker.run().await.map_err(SpacebotError::from);
            let result = result?;

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
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_worker_task<F>(
    worker_id: WorkerId,
    event_tx: broadcast::Sender<ProcessEvent>,
    agent_id: crate::AgentId,
    channel_id: Option<ChannelId>,
    run_logger: ProcessRunLogger,
    transcript_snapshot: WorkerTranscriptSnapshot,
    opencode_cancellation: Option<
        Arc<tokio::sync::Mutex<Option<crate::opencode::worker::OpenCodeCancellationSession>>>,
    >,
    opencode_directory_claim: Option<crate::opencode::server::OpenCodeDirectoryClaim>,
    secrets_store: Option<Arc<crate::secrets::store::SecretsStore>>,
    // Present when the run should be recorded against a task's history.
    task_store: Option<Arc<crate::tasks::TaskStore>>,
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables))] worker_type: &'static str,
    future: F,
) -> WorkerTaskControl
where
    F: std::future::Future<Output = crate::Result<WorkerOutcome>> + Send + 'static,
{
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let terminal_notify = Arc::new(tokio::sync::Notify::new());
    let task_terminal_notify = terminal_notify.clone();
    let task_transcript_snapshot = transcript_snapshot.clone();
    // The parent owns cancellation authority, but its teardown must detach a
    // worker rather than turn a dropped sender into a cancellation request.
    let task_cancel_tx = cancel_tx.clone();
    let handle = tokio::spawn(async move {
        let opencode_directory_claim = opencode_directory_claim;
        let _task_cancel_tx = task_cancel_tx;
        #[cfg(feature = "metrics")]
        let worker_start = std::time::Instant::now();

        #[cfg(feature = "metrics")]
        crate::telemetry::Metrics::global()
            .active_workers
            .with_label_values(&[&*agent_id])
            .inc();

        let worker_future = std::panic::AssertUnwindSafe(future).catch_unwind();
        tokio::pin!(worker_future);
        let raw = tokio::select! {
            result = &mut worker_future => result,
            changed = cancel_rx.changed() => {
                debug_assert!(changed.is_ok(), "worker task retains cancellation sender");
                Ok(Ok(WorkerOutcome::Cancelled {
                    reason: "cancelled by supervisor".to_string(),
                }))
            }
        };
        if let Some(directory_claim) = opencode_directory_claim {
            directory_claim.release().await;
        }
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
        let (notify, _success) = completion_flags(kind);
        let outcome_kind = outcome_kind(kind);

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

        let terminal_owner = match outcome_kind {
            WorkerOutcomeKind::Cancelled => WorkerTerminalOwner::Cancel,
            WorkerOutcomeKind::TimedOut => WorkerTerminalOwner::Timeout,
            WorkerOutcomeKind::Succeeded
            | WorkerOutcomeKind::Partial
            | WorkerOutcomeKind::Blocked
            | WorkerOutcomeKind::Failed => WorkerTerminalOwner::Worker,
        };
        let transcript = read_worker_transcript_snapshot(&task_transcript_snapshot);
        let commit = commit_worker_outcome(
            &run_logger,
            worker_id,
            outcome_kind,
            &result_text,
            transcript.as_ref(),
            terminal_owner,
        )
        .await;

        // Close this run in the task's attempt history, using the outcome the
        // commit settled on: a completion racing a cancel or a timeout lands on
        // a different terminal kind than the raw classification, and the board
        // has to agree with the durable worker record. A commit that produced
        // nothing still closes the attempt with what was classified here, so a
        // failure to commit cannot leave the task blocked by an open run.
        // Keyed by worker id, so a run never bound to a task matches nothing.
        if let Some(task_store) = &task_store {
            let (resolved, summary_source) = match &commit {
                Ok(Some((terminal, _))) => (terminal.outcome_kind, terminal.result.as_str()),
                _ => (outcome_kind, result_text.as_str()),
            };
            if let Err(error) = task_store
                .finish_task_attempt(
                    &worker_id.to_string(),
                    resolved.into(),
                    Some(summary_source),
                )
                .await
            {
                tracing::warn!(%error, %worker_id, "failed to record the task attempt outcome");
            }
        }

        let (terminal, newly_committed) = match commit {
            Ok(Some(commit)) => commit,
            Ok(None) => {
                tracing::error!(%worker_id, "worker terminal outcome could not be committed");
                task_terminal_notify.notify_one();
                return;
            }
            Err(error) => {
                tracing::error!(%error, %worker_id, "failed to commit worker terminal outcome");
                task_terminal_notify.notify_one();
                return;
            }
        };
        task_terminal_notify.notify_one();
        if newly_committed {
            let _ = event_tx.send(worker_complete_event(
                agent_id, channel_id, terminal, notify,
            ));
        }
    });
    WorkerTaskControl {
        handle,
        cancel_tx,
        terminal_notify,
        transcript_snapshot,
        opencode_cancellation,
    }
}

pub(crate) fn worker_complete_event(
    agent_id: crate::AgentId,
    channel_id: Option<ChannelId>,
    terminal: WorkerTerminalOutcome,
    notify: bool,
) -> ProcessEvent {
    ProcessEvent::WorkerComplete {
        agent_id,
        worker_id: terminal
            .worker_id
            .parse()
            .expect("persisted worker IDs are UUIDs"),
        channel_id,
        result: terminal.result,
        notify,
        success: terminal.outcome_kind.is_success(),
        outcome_kind: terminal.outcome_kind,
        outcome_version: terminal.outcome_version,
        transcript_version: terminal.transcript_version,
        terminal_owner: terminal.terminal_owner,
    }
}

pub(crate) async fn commit_worker_outcome(
    run_logger: &ProcessRunLogger,
    worker_id: WorkerId,
    outcome_kind: WorkerOutcomeKind,
    result: &str,
    transcript: Option<&crate::agent::worker::WorkerTranscriptPayload>,
    terminal_owner: WorkerTerminalOwner,
) -> crate::Result<Option<(WorkerTerminalOutcome, bool)>> {
    let mut outcome_kind = outcome_kind;
    let Some(mut lifecycle) = run_logger.read_worker_lifecycle(worker_id).await? else {
        return Ok(None);
    };
    if lifecycle.is_terminal() {
        return Ok(run_logger
            .read_worker_terminal(worker_id)
            .await?
            .map(|terminal| (terminal, false)));
    }

    lifecycle = match outcome_kind {
        WorkerOutcomeKind::Succeeded | WorkerOutcomeKind::Partial | WorkerOutcomeKind::Blocked => {
            match lifecycle {
                WorkerLifecycle::Completing => lifecycle,
                WorkerLifecycle::Cancelling | WorkerLifecycle::TimingOut => {
                    if outcome_kind == WorkerOutcomeKind::Blocked {
                        outcome_kind = WorkerOutcomeKind::Partial;
                    }
                    lifecycle
                }
                WorkerLifecycle::Running | WorkerLifecycle::WaitingForInput => {
                    match run_logger
                        .claim_worker_completion(worker_id, lifecycle)
                        .await?
                    {
                        WorkerTransitionResult::Applied { current, .. } => current,
                        WorkerTransitionResult::Conflict { current } => current,
                        WorkerTransitionResult::NotFound => return Ok(None),
                    }
                }
                WorkerLifecycle::Created
                | WorkerLifecycle::Succeeded
                | WorkerLifecycle::Partial
                | WorkerLifecycle::Cancelled
                | WorkerLifecycle::TimedOut
                | WorkerLifecycle::Blocked
                | WorkerLifecycle::Failed => lifecycle,
            }
        }
        WorkerOutcomeKind::Cancelled => match lifecycle {
            WorkerLifecycle::Running | WorkerLifecycle::WaitingForInput => {
                match run_logger
                    .transition_worker(worker_id, lifecycle, WorkerLifecycle::Cancelling)
                    .await?
                {
                    WorkerTransitionResult::Applied { current, .. } => current,
                    WorkerTransitionResult::Conflict { current } => current,
                    WorkerTransitionResult::NotFound => return Ok(None),
                }
            }
            WorkerLifecycle::Completing => {
                outcome_kind = WorkerOutcomeKind::Partial;
                WorkerLifecycle::Completing
            }
            WorkerLifecycle::TimingOut => {
                outcome_kind = WorkerOutcomeKind::TimedOut;
                WorkerLifecycle::TimingOut
            }
            _ => lifecycle,
        },
        WorkerOutcomeKind::TimedOut => match lifecycle {
            WorkerLifecycle::Running => match run_logger
                .transition_worker(worker_id, lifecycle, WorkerLifecycle::TimingOut)
                .await?
            {
                WorkerTransitionResult::Applied { current, .. } => current,
                WorkerTransitionResult::Conflict { current } => current,
                WorkerTransitionResult::NotFound => return Ok(None),
            },
            WorkerLifecycle::Completing => {
                outcome_kind = WorkerOutcomeKind::Partial;
                WorkerLifecycle::Completing
            }
            WorkerLifecycle::Cancelling => {
                outcome_kind = if transcript.is_some() {
                    WorkerOutcomeKind::Partial
                } else {
                    WorkerOutcomeKind::Cancelled
                };
                WorkerLifecycle::Cancelling
            }
            _ => lifecycle,
        },
        WorkerOutcomeKind::Failed => match lifecycle {
            WorkerLifecycle::Cancelling => {
                outcome_kind = if transcript.is_some() {
                    WorkerOutcomeKind::Partial
                } else {
                    WorkerOutcomeKind::Cancelled
                };
                lifecycle
            }
            WorkerLifecycle::TimingOut => {
                outcome_kind = WorkerOutcomeKind::TimedOut;
                lifecycle
            }
            _ => lifecycle,
        },
    };

    let commit = run_logger
        .complete_worker(
            worker_id,
            lifecycle,
            outcome_kind,
            Some(result),
            result,
            transcript.map(|payload| payload.transcript.as_slice()),
            transcript.map_or(0, |payload| payload.tool_calls),
            terminal_owner,
        )
        .await?;
    match commit {
        WorkerCompletionCommit::Committed(terminal) => Ok(Some((terminal, true))),
        WorkerCompletionCommit::Existing(terminal) => Ok(Some((terminal, false))),
        WorkerCompletionCommit::Conflict { current } => {
            tracing::warn!(%worker_id, lifecycle = current.as_str(), "worker terminal commit conflicted");
            Ok(run_logger
                .read_worker_terminal(worker_id)
                .await?
                .map(|terminal| (terminal, false)))
        }
        WorkerCompletionCommit::NotFound => Ok(None),
    }
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
            result,
        } => WorkerOutcome::Timeout {
            elapsed_secs,
            segments_run,
            result: scrub(result),
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
            server_pool
                .claim_directory(&directory)
                .await
                .map_err(|error| error.to_string())?;
            let directory_claim = crate::opencode::server::OpenCodeDirectoryClaim::new(
                server_pool.clone(),
                directory.clone(),
            );
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
            let transcript_snapshot = worker.transcript_snapshot();
            let opencode_cancellation = worker.cancellation_session();
            let handle = spawn_worker_task(
                worker_id,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                Some(state.channel_id.clone()),
                state.process_run_logger.clone(),
                transcript_snapshot,
                Some(opencode_cancellation),
                Some(directory_claim),
                oc_secrets_store,
                Some(state.deps.task_store.clone()),
                "opencode",
                async move {
                    let result = worker.run().await.map_err(SpacebotError::from)?;
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
            let model_name = state
                .model_overrides
                .resolve_model("worker")
                .unwrap_or_else(|| routing.resolve(ProcessType::Worker, None))
                .to_string();
            let tool_use_enforcement = rc.tool_use_enforcement.load();
            let project_context = build_project_context(&state.deps, &prompt_engine).await;
            let mut system_prompt = prompt_engine
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
                .map_err(|error| format!("failed to render worker prompt: {error}"))?;
            system_prompt.adopt_appended(
                prompt_engine
                    .maybe_append_tool_use_enforcement(
                        system_prompt.text.clone(),
                        tool_use_enforcement.as_ref(),
                        &model_name,
                    )
                    .map_err(|error| format!("failed to render worker prompt: {error}"))?,
                "tool_use_enforcement",
            );
            let brave_search_key = (**rc.brave_search_key.load()).clone();

            let (worker, input_tx, inject_tx) = Worker::resume_interactive(
                worker_id,
                Some(state.channel_id.clone()),
                &idle_worker.task,
                system_prompt,
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
            let transcript_snapshot = worker.transcript_snapshot();
            let handle = spawn_worker_task(
                worker_id,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                Some(state.channel_id.clone()),
                state.process_run_logger.clone(),
                transcript_snapshot,
                None,
                None,
                secrets_store,
                Some(state.deps.task_store.clone()),
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
    use super::{
        WorkerCompletionError, WorkerOutcome, commit_worker_outcome, map_worker_completion,
        spawn_worker_task, worker_task_prompt,
    };
    use crate::conversation::{
        ProcessRunLogger, WorkerLifecycle, WorkerOutcomeKind, WorkerTerminalOwner,
    };
    use crate::tasks::TaskAttemptOutcome;
    use crate::{ProcessEvent, WorkerId};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    #[test]
    fn task_context_is_appended_to_the_worker_message() {
        let prompt = worker_task_prompt(
            "Audit task #31 without writes.",
            Some("## Runtime-Injected Task Context\n\n```json\n{}\n```"),
        );

        assert!(prompt.starts_with("Audit task #31 without writes."));
        assert!(prompt.contains("## Runtime-Injected Task Context"));
        assert!(prompt.ends_with("```json\n{}\n```"));
    }

    async fn setup_worker(worker_id: WorkerId, channel_id: &str) -> ProcessRunLogger {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO channels (id, platform) VALUES (?, 'test')")
            .bind(channel_id)
            .execute(&pool)
            .await
            .unwrap();
        let logger = ProcessRunLogger::new(pool);
        logger
            .log_worker_started(
                Some(&Arc::<str>::from(channel_id)),
                worker_id,
                "task",
                "builtin",
                &Arc::<str>::from("agent"),
                false,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        logger
    }

    /// A cancel arriving while the worker is already completing commits as
    /// partial. The attempt has to record what was committed: recording the raw
    /// classification would put `cancelled` on the board against a worker record
    /// that says `partial`.
    #[tokio::test]
    async fn a_cancel_racing_a_completion_records_what_was_committed() {
        let worker_id = Uuid::new_v4();
        let logger = setup_worker(worker_id, "test:race-cancel").await;
        let lifecycle = logger
            .read_worker_lifecycle(worker_id)
            .await
            .unwrap()
            .unwrap();
        logger
            .claim_worker_completion(worker_id, lifecycle)
            .await
            .unwrap();

        let (terminal, committed) = commit_worker_outcome(
            &logger,
            worker_id,
            WorkerOutcomeKind::Cancelled,
            "cancelled while finishing",
            None,
            WorkerTerminalOwner::Cancel,
        )
        .await
        .unwrap()
        .unwrap();

        assert!(committed);
        assert_eq!(terminal.outcome_kind, WorkerOutcomeKind::Partial);
        assert_eq!(
            TaskAttemptOutcome::from(terminal.outcome_kind),
            TaskAttemptOutcome::Partial
        );
        assert_ne!(
            TaskAttemptOutcome::from(WorkerOutcomeKind::Cancelled),
            TaskAttemptOutcome::from(terminal.outcome_kind),
            "the raw classification is what the attempt used to record"
        );
    }

    /// The same disagreement in the other direction: a timeout landing on a
    /// worker already cancelling, with nothing to show for the run, commits as
    /// cancelled rather than timed out.
    #[tokio::test]
    async fn a_timeout_racing_a_cancel_records_what_was_committed() {
        let worker_id = Uuid::new_v4();
        let logger = setup_worker(worker_id, "test:race-timeout").await;
        let lifecycle = logger
            .read_worker_lifecycle(worker_id)
            .await
            .unwrap()
            .unwrap();
        logger
            .transition_worker(worker_id, lifecycle, WorkerLifecycle::Cancelling)
            .await
            .unwrap();

        let (terminal, _) = commit_worker_outcome(
            &logger,
            worker_id,
            WorkerOutcomeKind::TimedOut,
            "timed out",
            None,
            WorkerTerminalOwner::Timeout,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(terminal.outcome_kind, WorkerOutcomeKind::Cancelled);
        assert_eq!(
            TaskAttemptOutcome::from(terminal.outcome_kind),
            TaskAttemptOutcome::Cancelled
        );
    }

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
            result: String::new(),
        }));
        assert_eq!(
            text,
            "Worker exceeded 1800s wall-clock timeout after 7 segments."
        );
        assert!(notify);
        assert!(!success);
    }

    /// A run that produced findings before its budget ran out relays them
    /// rather than reporting only the timeout.
    #[test]
    fn timeout_outcome_relays_recovered_work() {
        let (text, ..) = map_worker_completion(Ok(WorkerOutcome::Timeout {
            elapsed_secs: 1800,
            segments_run: 7,
            result: "Found three candidate repositories.".to_string(),
        }));
        assert!(text.starts_with("Found three candidate repositories."));
        assert!(text.contains("partial result"));
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
        let run_logger = setup_worker(worker_id, "channel").await;

        let mut control = spawn_worker_task(
            worker_id,
            event_tx,
            Arc::<str>::from("agent"),
            Some(Arc::<str>::from("channel")),
            run_logger,
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
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
        (&mut control.handle)
            .await
            .expect("worker task should join cleanly");

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
    async fn dropping_parent_control_does_not_cancel_worker() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "detached-channel").await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();

        let control = spawn_worker_task(
            worker_id,
            event_tx,
            Arc::<str>::from("agent"),
            Some(Arc::<str>::from("detached-channel")),
            run_logger,
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
            "builtin",
            async move {
                started_tx.send(()).expect("test receiver remains active");
                finish_rx.await.expect("test sender remains active");
                Ok::<WorkerOutcome, crate::Error>(WorkerOutcome::Success {
                    result: "completed after parent exit".to_string(),
                })
            },
        );

        started_rx.await.expect("worker should start");
        drop(control);
        finish_tx.send(()).expect("worker should still be running");

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("worker completion event should be delivered")
            .expect("broadcast receive should succeed");
        let ProcessEvent::WorkerComplete {
            result, success, ..
        } = event
        else {
            panic!("expected worker completion");
        };
        assert_eq!(result, "completed after parent exit");
        assert!(success);
    }

    #[tokio::test]
    async fn spawn_worker_task_carries_channel_id() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id: WorkerId = Uuid::new_v4();
        let channel_id: crate::ChannelId = Arc::from("test-channel");
        let run_logger = setup_worker(worker_id, &channel_id).await;

        let mut control = spawn_worker_task(
            worker_id,
            event_tx,
            Arc::<str>::from("agent"),
            Some(channel_id.clone()),
            run_logger,
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
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
        (&mut control.handle)
            .await
            .expect("worker task should join cleanly");

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

    #[tokio::test]
    async fn worker_completion_is_durable_before_notification() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "durable-channel").await;
        let inspect_logger = run_logger.clone();
        let mut control = spawn_worker_task(
            worker_id,
            event_tx,
            Arc::<str>::from("agent"),
            Some(Arc::<str>::from("durable-channel")),
            run_logger,
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
            "builtin",
            async {
                Ok::<WorkerOutcome, crate::Error>(WorkerOutcome::Success {
                    result: "durable result".to_string(),
                })
            },
        );

        let event = event_rx.recv().await.unwrap();
        let ProcessEvent::WorkerComplete {
            outcome_version,
            outcome_kind,
            ..
        } = event
        else {
            panic!("expected worker completion");
        };
        let terminal = inspect_logger
            .read_worker_terminal(worker_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(terminal.outcome_version, outcome_version);
        assert_eq!(terminal.outcome_kind, outcome_kind);
        (&mut control.handle).await.unwrap();
    }
}
