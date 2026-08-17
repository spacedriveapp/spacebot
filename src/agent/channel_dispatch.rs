//! Branch and worker spawning for channels.
//!
//! Contains the public entry points that channel tools use to create
//! background processes: `spawn_branch_from_state`, `spawn_worker_from_state`,
//! and `spawn_opencode_worker_from_state`.

use crate::agent::branch::{Branch, BranchExecutionConfig};
use crate::agent::channel::ChannelState;
use crate::agent::channel_prompt::TemporalContext;
use crate::agent::process_control::{
    WorkerBackend, WorkerCallbackContext, WorkerOperationContext, WorkerOperationId,
    WorkerProvenance, WorkerRequester, WorkerResultTarget, WorkerRuntimeControl,
    WorkerRuntimeState,
};
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

const TERMINAL_COMMIT_ATTEMPTS: usize = 3;
const TERMINAL_COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const TERMINAL_COMMIT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

/// Validate worker capacity for a channel based on current active worker count.
#[cfg_attr(not(test), allow(dead_code))]
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

pub struct WorkerStartGate {
    tx: tokio::sync::watch::Sender<bool>,
}

pub struct PreparedWorkerSpawn {
    pub worker_id: WorkerId,
    callback: WorkerCallbackContext,
    registry: Arc<crate::agent::process_control::ProcessControlRegistry>,
    run_logger: ProcessRunLogger,
    start_gate: WorkerStartGate,
    started_event: ProcessEvent,
    event_tx: broadcast::Sender<ProcessEvent>,
    autonomy_run: Option<crate::agent::autonomy::AutonomyRunHandle>,
    operation_id: WorkerOperationId,
}

impl PreparedWorkerSpawn {
    pub async fn is_starting(&self) -> bool {
        self.registry
            .worker_is_in_state(self.callback, WorkerRuntimeState::Starting)
            .await
    }

    pub async fn start(self) -> std::result::Result<WorkerId, AgentError> {
        let Self {
            worker_id,
            callback,
            registry,
            run_logger,
            start_gate,
            started_event,
            event_tx,
            autonomy_run,
            operation_id,
        } = self;
        if registry
            .update_worker_state(callback, WorkerRuntimeState::Running)
            .await
            != crate::agent::process_control::WorkerMutationResult::Applied
        {
            if let Err(error) = commit_worker_outcome_with_retry(
                &run_logger,
                worker_id,
                WorkerOutcomeKind::Cancelled,
                "Worker cancelled before start.",
                None,
                WorkerTerminalOwner::Cancel,
            )
            .await
            {
                tracing::warn!(%error, %worker_id, "failed to persist rejected worker start");
            }
            registry
                .remove_worker_if_registration_matches(callback)
                .await;
            if let Some(run) = &autonomy_run {
                run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                    worker_id,
                    operation_id,
                });
            }
            return Err(AgentError::Other(anyhow::anyhow!(
                "can't start worker: registration is no longer starting"
            )));
        }
        let opened = registry
            .run_if_worker_state(callback, WorkerRuntimeState::Running, move || {
                event_tx.send(started_event).ok();
                start_gate.open();
            })
            .await;
        if opened != crate::agent::process_control::WorkerMutationResult::Applied {
            if let Err(error) = commit_worker_outcome_with_retry(
                &run_logger,
                worker_id,
                WorkerOutcomeKind::Cancelled,
                "Worker cancelled before start.",
                None,
                WorkerTerminalOwner::Cancel,
            )
            .await
            {
                tracing::warn!(%error, %worker_id, "failed to persist cancelled worker start");
            }
            registry
                .remove_worker_if_registration_matches(callback)
                .await;
            if let Some(run) = &autonomy_run {
                run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                    worker_id,
                    operation_id,
                });
            }
            return Err(AgentError::Other(anyhow::anyhow!(
                "can't start worker: registration was cancelled before the gate opened"
            )));
        }
        Ok(worker_id)
    }

    pub async fn fail_before_start(self, reason: &str) {
        let result = format!("Worker failed before start: {reason}");
        if let Err(error) = commit_worker_outcome_with_retry(
            &self.run_logger,
            self.worker_id,
            WorkerOutcomeKind::Failed,
            &result,
            None,
            WorkerTerminalOwner::Worker,
        )
        .await
        {
            tracing::warn!(%error, worker_id = %self.worker_id, "failed to persist pre-start worker failure");
        }
        self.registry
            .remove_worker_if_registration_matches(self.callback)
            .await;
        if let Some(run) = &self.autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id: self.worker_id,
                operation_id: self.operation_id,
            });
        }
    }
}

impl WorkerStartGate {
    pub(crate) fn new() -> (Self, tokio::sync::watch::Receiver<bool>) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (Self { tx }, rx)
    }

    pub(crate) fn open(self) {
        self.tx.send_replace(true);
    }
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
    let autonomy_run = state.autonomy_run();
    if state.kind == crate::agent::channel::ChannelKind::Autonomy && autonomy_run.is_none() {
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn branch: no active autonomy epoch"
        )));
    }
    if autonomy_run
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

    if let Some(run) = &autonomy_run
        && !run.register_child(crate::agent::autonomy::AutonomyChild::Branch(branch_id))
    {
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn branch: autonomy epoch is finishing"
        )));
    }

    if let Err(error) = state
        .process_run_logger
        .log_branch_started(
            &state.channel_id,
            branch_id,
            description,
            &prompt,
            &profile_name,
            &model_name,
            branch_max_turns,
            autonomy_run.as_ref().map(|run| run.run_id.as_str()),
        )
        .await
    {
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::Branch(branch_id));
        }
        return Err(AgentError::Other(anyhow::anyhow!(error)));
    }
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

async fn append_worker_memory_context(
    system_prompt: &mut crate::prompts::SegmentedPrompt,
    deps: &AgentDeps,
    channel_id: Option<&ChannelId>,
    memory_mode: crate::conversation::settings::WorkerMemoryMode,
) {
    if !memory_mode.ambient_enabled() {
        return;
    }

    let cortex_config = **deps.runtime_config.cortex.load();
    match crate::memory::render::render_memory_store(
        deps.memory_search.store(),
        &deps.task_store,
        &deps.agent_id,
        cortex_config.memory_render_max_words,
    )
    .await
    {
        Ok(memory_store) if !memory_store.is_empty() => {
            system_prompt.append_section("knowledge_synthesis", &memory_store);
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "worker ambient memory store render failed"),
    }

    let Some(channel_id) = channel_id else {
        return;
    };
    let working_memory_config = **deps.runtime_config.working_memory.load();
    let timezone = deps.working_memory.timezone();
    match crate::memory::working::render_working_memory(
        &deps.working_memory,
        channel_id.as_ref(),
        &working_memory_config,
        timezone,
    )
    .await
    {
        Ok(working_memory) if !working_memory.is_empty() => system_prompt.append_section(
            "working_memory",
            &format!("## Recent Activity\n{working_memory}"),
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "worker ambient working memory render failed"),
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
) -> std::result::Result<PreparedWorkerSpawn, AgentError> {
    let autonomy_run = state.autonomy_run();
    if state.kind == crate::agent::channel::ChannelKind::Autonomy && autonomy_run.is_none() {
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn worker: no active autonomy epoch"
        )));
    }
    if autonomy_run
        .as_ref()
        .is_some_and(crate::agent::autonomy::AutonomyRunHandle::finish_requested)
    {
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn worker: autonomy run is settling"
        )));
    }
    let task = task.into();
    ensure_dispatch_readiness(state, "worker");
    spawn_worker_inner(
        state,
        &task,
        interactive,
        suggested_skills,
        required_skills,
        worker_context,
        task_context,
    )
    .await
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
) -> std::result::Result<PreparedWorkerSpawn, AgentError> {
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

    append_worker_memory_context(
        &mut system_prompt,
        &state.deps,
        Some(&state.channel_id),
        worker_context.memory,
    )
    .await;

    let worker_task = worker_task_prompt(task, task_context.task_context);
    let worker_id = uuid::Uuid::new_v4();
    let autonomy_run = state.autonomy_run();
    let provenance = WorkerProvenance {
        origin_channel_id: Some(state.channel_id.clone()),
        origin_branch_id: task_context.origin_branch_id,
        task: task.to_string(),
        task_id: None,
        autonomy_run_id: autonomy_run.as_ref().map(|run| run.run_id.clone()),
        spawning_process: crate::ProcessId::Channel(state.channel_id.clone()),
    };
    let reservation = state
        .deps
        .process_control_registry
        .reserve_worker(
            worker_id,
            &provenance,
            **state.deps.runtime_config.max_concurrent_workers.load(),
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;
    let callback = reservation.callback_context();
    let initial_operation = WorkerOperationContext {
        operation_id: WorkerOperationId::new(),
        requester: WorkerRequester::Channel {
            channel_id: state.channel_id.clone(),
        },
        result_target: WorkerResultTarget::Channel {
            channel_id: state.channel_id.clone(),
        },
        autonomy_run_id: autonomy_run.as_ref().map(|run| run.run_id.clone()),
    };

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
            worker_id,
            callback,
            initial_operation.clone(),
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
        (worker, Some(input_tx), Some(inject_tx))
    } else {
        let (worker, inject_tx) = Worker::new(
            worker_id,
            callback,
            initial_operation.clone(),
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
        (worker, None, Some(inject_tx))
    };
    let (worker, input_tx, injection_tx) = worker;
    let transcript_snapshot = worker.transcript_snapshot();
    let (runtime_control, cancel_rx, terminal_notify) = WorkerRuntimeControl::new(
        transcript_snapshot.clone(),
        None,
        input_tx,
        injection_tx,
        Some(state.process_run_logger.clone()),
    );
    let admission = state
        .deps
        .process_control_registry
        .register_new_worker(
            reservation,
            provenance,
            WorkerBackend::Builtin,
            interactive,
            initial_operation.clone(),
            "starting",
            runtime_control,
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;
    if let Some(run) = &autonomy_run
        && !run.register_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
            worker_id,
            operation_id: initial_operation.operation_id,
        })
    {
        state
            .deps
            .process_control_registry
            .remove_worker_if_registration_matches(callback)
            .await;
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn worker: autonomy epoch is finishing"
        )));
    }

    if let Err(error) = state
        .process_run_logger
        .log_worker_started(
            Some(&state.channel_id),
            worker_id,
            task,
            "builtin",
            &state.deps.agent_id,
            interactive,
            None,
            autonomy_run.as_ref().map(|run| run.run_id.as_str()),
            task_context.origin_branch_id,
        )
        .await
    {
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: initial_operation.operation_id,
            });
        }
        state
            .deps
            .process_control_registry
            .remove_worker_if_registration_matches(callback)
            .await;
        return Err(AgentError::Other(anyhow::anyhow!(error)));
    }
    if !state
        .deps
        .process_control_registry
        .worker_is_in_state(callback, WorkerRuntimeState::Starting)
        .await
    {
        if let Err(error) = commit_worker_outcome_with_retry(
            &state.process_run_logger,
            worker_id,
            WorkerOutcomeKind::Cancelled,
            "Worker cancelled while its durable start was being recorded.",
            None,
            WorkerTerminalOwner::Cancel,
        )
        .await
        {
            tracing::warn!(%error, %worker_id, "failed to persist cancellation during durable worker start");
        }
        state
            .deps
            .process_control_registry
            .remove_worker_if_registration_matches(callback)
            .await;
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: initial_operation.operation_id,
            });
        }
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't start worker: cancelled during durable start"
        )));
    }

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
    );
    let secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
    let (start_gate, start_rx) = WorkerStartGate::new();
    let handle = spawn_worker_task(
        callback,
        state.deps.process_control_registry.clone(),
        cancel_rx,
        terminal_notify,
        start_rx,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        state.process_run_logger.clone(),
        transcript_snapshot,
        None,
        secrets_store,
        Some(state.deps.task_store.clone()),
        "builtin",
        worker.run().instrument(worker_span),
    );

    if let Err(handle) = state
        .deps
        .process_control_registry
        .install_task_handle(admission.callback_context(), handle)
        .await
    {
        handle.abort();
        if let Err(error) = commit_worker_outcome_with_retry(
            &state.process_run_logger,
            worker_id,
            WorkerOutcomeKind::Cancelled,
            "Worker cancelled before task handle installation.",
            None,
            WorkerTerminalOwner::Cancel,
        )
        .await
        {
            tracing::warn!(%error, %worker_id, "failed to persist cancelled worker installation");
        }
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: initial_operation.operation_id,
            });
        }
        return Err(AgentError::Other(anyhow::anyhow!(
            "worker registration detached before task handle installation"
        )));
    }

    {
        let mut status = state.status_block.write().await;
        status.add_worker(
            worker_id,
            callback.registration_id,
            task,
            false,
            interactive,
        );
    }

    let started_event = crate::ProcessEvent::WorkerStarted {
        agent_id: state.deps.agent_id.clone(),
        worker_id,
        worker_registration_id: callback.registration_id,
        channel_id: Some(state.channel_id.clone()),
        task: task.to_string(),
        worker_type: "builtin".into(),
        interactive,
        directory: None,
    };

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

    Ok(PreparedWorkerSpawn {
        worker_id,
        callback,
        registry: state.deps.process_control_registry.clone(),
        run_logger: state.process_run_logger.clone(),
        start_gate,
        started_event,
        event_tx: state.deps.event_tx.clone(),
        autonomy_run,
        operation_id: initial_operation.operation_id,
    })
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
) -> std::result::Result<PreparedWorkerSpawn, AgentError> {
    if !interactive {
        return Err(AgentError::Other(anyhow::anyhow!(
            "OpenCode workers must be interactive"
        )));
    }

    let task = task.into();
    ensure_dispatch_readiness(state, "opencode_worker");
    spawn_opencode_worker_inner(
        state,
        &task,
        directory,
        interactive,
        required_skills,
        task_context,
    )
    .await
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
) -> std::result::Result<PreparedWorkerSpawn, AgentError> {
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
    let worker_id = uuid::Uuid::new_v4();
    let autonomy_run = state.autonomy_run();
    let persisted_task = format!("[opencode] {task}");
    let provenance = WorkerProvenance {
        origin_channel_id: Some(state.channel_id.clone()),
        origin_branch_id: task_context.origin_branch_id,
        task: persisted_task.clone(),
        task_id: None,
        autonomy_run_id: autonomy_run.as_ref().map(|run| run.run_id.clone()),
        spawning_process: crate::ProcessId::Channel(state.channel_id.clone()),
    };
    let reservation = state
        .deps
        .process_control_registry
        .reserve_worker(
            worker_id,
            &provenance,
            **state.deps.runtime_config.max_concurrent_workers.load(),
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;
    let callback = reservation.callback_context();
    let initial_operation = WorkerOperationContext {
        operation_id: WorkerOperationId::new(),
        requester: WorkerRequester::Channel {
            channel_id: state.channel_id.clone(),
        },
        result_target: WorkerResultTarget::Channel {
            channel_id: state.channel_id.clone(),
        },
        autonomy_run_id: autonomy_run.as_ref().map(|run| run.run_id.clone()),
    };
    let worker = if interactive {
        let (worker, input_tx) = crate::opencode::OpenCodeWorker::new_interactive(
            worker_id,
            callback,
            initial_operation.clone(),
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            &worker_task,
            directory,
            server_pool,
            state.deps.event_tx.clone(),
            state.deps.process_control_registry.clone(),
        );
        let worker = match worker_status_text {
            Some(ref prompt) => worker.with_system_prompt(prompt),
            None => worker,
        };
        let worker = match &oc_secrets_store {
            Some(store) => worker.with_secrets_store(store.clone()),
            None => worker,
        };
        (
            worker.with_sqlite_pool(state.deps.sqlite_pool.clone()),
            Some(input_tx),
        )
    } else {
        let worker = crate::opencode::OpenCodeWorker::new(
            worker_id,
            callback,
            initial_operation.clone(),
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            &worker_task,
            directory,
            server_pool,
            state.deps.event_tx.clone(),
            state.deps.process_control_registry.clone(),
        );
        let worker = match worker_status_text {
            Some(ref prompt) => worker.with_system_prompt(prompt),
            None => worker,
        };
        let worker = match &oc_secrets_store {
            Some(store) => worker.with_secrets_store(store.clone()),
            None => worker,
        };
        (
            worker.with_sqlite_pool(state.deps.sqlite_pool.clone()),
            None,
        )
    };
    let (worker, input_tx) = worker;
    let worker = match state.model_overrides.resolve_model("worker") {
        Some(model) => worker.with_model(model),
        None => worker,
    };
    let transcript_snapshot = worker.transcript_snapshot();
    let opencode_cancellation = worker.cancellation_session();
    let (runtime_control, cancel_rx, terminal_notify) = WorkerRuntimeControl::new(
        transcript_snapshot.clone(),
        Some(opencode_cancellation),
        input_tx,
        None,
        Some(state.process_run_logger.clone()),
    );
    let admission = state
        .deps
        .process_control_registry
        .register_new_worker(
            reservation,
            provenance,
            WorkerBackend::OpenCode,
            true,
            initial_operation.clone(),
            "starting",
            runtime_control,
        )
        .await
        .map_err(|error| AgentError::Other(anyhow::anyhow!(error)))?;
    if let Some(run) = &autonomy_run
        && !run.register_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
            worker_id,
            operation_id: initial_operation.operation_id,
        })
    {
        state
            .deps
            .process_control_registry
            .remove_worker_if_registration_matches(callback)
            .await;
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't spawn worker: autonomy epoch is finishing"
        )));
    }

    if let Err(error) = state
        .process_run_logger
        .log_worker_started(
            Some(&state.channel_id),
            worker_id,
            &format!("[opencode] {task}"),
            "opencode",
            &state.deps.agent_id,
            interactive,
            Some(&persist_directory),
            autonomy_run.as_ref().map(|run| run.run_id.as_str()),
            task_context.origin_branch_id,
        )
        .await
    {
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: initial_operation.operation_id,
            });
        }
        state
            .deps
            .process_control_registry
            .remove_worker_if_registration_matches(callback)
            .await;
        return Err(AgentError::Other(anyhow::anyhow!(error)));
    }
    if !state
        .deps
        .process_control_registry
        .worker_is_in_state(callback, WorkerRuntimeState::Starting)
        .await
    {
        if let Err(error) = commit_worker_outcome_with_retry(
            &state.process_run_logger,
            worker_id,
            WorkerOutcomeKind::Cancelled,
            "Worker cancelled while its durable start was being recorded.",
            None,
            WorkerTerminalOwner::Cancel,
        )
        .await
        {
            tracing::warn!(%error, %worker_id, "failed to persist cancellation during durable OpenCode worker start");
        }
        state
            .deps
            .process_control_registry
            .remove_worker_if_registration_matches(callback)
            .await;
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: initial_operation.operation_id,
            });
        }
        return Err(AgentError::Other(anyhow::anyhow!(
            "can't start worker: cancelled during durable start"
        )));
    }

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
        worker_type = "opencode",
    );
    let (start_gate, start_rx) = WorkerStartGate::new();
    let handle = spawn_worker_task(
        callback,
        state.deps.process_control_registry.clone(),
        cancel_rx,
        terminal_notify,
        start_rx,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        state.process_run_logger.clone(),
        transcript_snapshot,
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

    if let Err(handle) = state
        .deps
        .process_control_registry
        .install_task_handle(admission.callback_context(), handle)
        .await
    {
        handle.abort();
        if let Err(error) = commit_worker_outcome_with_retry(
            &state.process_run_logger,
            worker_id,
            WorkerOutcomeKind::Cancelled,
            "Worker cancelled before task handle installation.",
            None,
            WorkerTerminalOwner::Cancel,
        )
        .await
        {
            tracing::warn!(%error, %worker_id, "failed to persist cancelled worker installation");
        }
        if let Some(run) = &autonomy_run {
            run.settle_child(crate::agent::autonomy::AutonomyChild::WorkerOperation {
                worker_id,
                operation_id: initial_operation.operation_id,
            });
        }
        return Err(AgentError::Other(anyhow::anyhow!(
            "worker registration detached before task handle installation"
        )));
    }

    let opencode_task = format!("[opencode] {task}");
    {
        let mut status = state.status_block.write().await;
        status.add_worker(
            worker_id,
            callback.registration_id,
            &opencode_task,
            false,
            interactive,
        );
    }

    let started_event = crate::ProcessEvent::WorkerStarted {
        agent_id: state.deps.agent_id.clone(),
        worker_id,
        worker_registration_id: callback.registration_id,
        channel_id: Some(state.channel_id.clone()),
        task: opencode_task,
        worker_type: "opencode".into(),
        interactive,
        directory: Some(persist_directory.to_string_lossy().to_string()),
    };

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

    Ok(PreparedWorkerSpawn {
        worker_id,
        callback,
        registry: state.deps.process_control_registry.clone(),
        run_logger: state.process_run_logger.clone(),
        start_gate,
        started_event,
        event_tx: state.deps.event_tx.clone(),
        autonomy_run,
        operation_id: initial_operation.operation_id,
    })
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
    callback: WorkerCallbackContext,
    process_control_registry: Arc<crate::agent::process_control::ProcessControlRegistry>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
    terminal_notify: Arc<tokio::sync::Notify>,
    mut start_rx: tokio::sync::watch::Receiver<bool>,
    event_tx: broadcast::Sender<ProcessEvent>,
    agent_id: crate::AgentId,
    channel_id: Option<ChannelId>,
    run_logger: ProcessRunLogger,
    transcript_snapshot: WorkerTranscriptSnapshot,
    opencode_directory_claim: Option<crate::opencode::server::OpenCodeDirectoryClaim>,
    secrets_store: Option<Arc<crate::secrets::store::SecretsStore>>,
    // Present when the run should be recorded against a task's history.
    task_store: Option<Arc<crate::tasks::TaskStore>>,
    #[cfg_attr(not(feature = "metrics"), allow(unused_variables))] worker_type: &'static str,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = crate::Result<WorkerOutcome>> + Send + 'static,
{
    let worker_id = callback.worker_id;
    let task_terminal_notify = terminal_notify.clone();
    let task_transcript_snapshot = transcript_snapshot.clone();
    tokio::spawn(async move {
        let opencode_directory_claim = opencode_directory_claim;
        loop {
            if *start_rx.borrow() {
                break;
            }
            tokio::select! {
                changed = start_rx.changed() => {
                    if changed.is_err() {
                        let fallback = if *cancel_rx.borrow() {
                            (
                                WorkerOutcomeKind::Cancelled,
                                "Worker cancelled before start.",
                                WorkerTerminalOwner::Cancel,
                            )
                        } else {
                            (
                                WorkerOutcomeKind::Failed,
                                "Worker failed before the start gate opened.",
                                WorkerTerminalOwner::Worker,
                            )
                        };
                        finalize_worker_supervision(
                            callback,
                            &process_control_registry,
                            &run_logger,
                            &event_tx,
                            &agent_id,
                            channel_id.clone(),
                            task_store.as_ref(),
                            fallback.0,
                            fallback.1.to_string(),
                            None,
                            fallback.2,
                            false,
                            &task_terminal_notify,
                        )
                        .await;
                        return;
                    }
                }
                changed = cancel_rx.changed() => {
                    debug_assert!(changed.is_ok(), "worker supervisor retains cancellation sender");
                }
            }
        }
        #[cfg(feature = "metrics")]
        let worker_start = std::time::Instant::now();

        if *cancel_rx.borrow() {
            finalize_worker_supervision(
                callback,
                &process_control_registry,
                &run_logger,
                &event_tx,
                &agent_id,
                channel_id,
                task_store.as_ref(),
                WorkerOutcomeKind::Cancelled,
                "Worker cancelled before execution started.".to_string(),
                None,
                WorkerTerminalOwner::Cancel,
                false,
                &task_terminal_notify,
            )
            .await;
            return;
        }

        #[cfg(feature = "metrics")]
        crate::telemetry::Metrics::global()
            .active_workers
            .with_label_values(&[&*agent_id])
            .inc();

        let execution_handle = tokio::spawn(std::panic::AssertUnwindSafe(future).catch_unwind());
        let execution_abort_handle = execution_handle.abort_handle();
        if process_control_registry
            .install_execution_abort_handle(callback, execution_abort_handle.clone())
            .await
            != crate::agent::process_control::WorkerMutationResult::Applied
        {
            execution_abort_handle.abort();
        }
        tokio::pin!(execution_handle);
        let raw = tokio::select! {
            result = &mut execution_handle => match result {
                Ok(result) => result,
                Err(error) if error.is_cancelled() => Ok(Ok(WorkerOutcome::Cancelled {
                    reason: "cancelled by supervisor".to_string(),
                })),
                Err(error) => Ok(Err(SpacebotError::from(anyhow::anyhow!(
                    "worker execution task failed: {error}"
                )))),
            },
            changed = cancel_rx.changed() => {
                debug_assert!(changed.is_ok(), "worker task retains cancellation sender");
                execution_abort_handle.abort();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    &mut execution_handle,
                )
                .await;
                Ok(Ok(WorkerOutcome::Cancelled {
                    reason: "cancelled by supervisor".to_string(),
                }))
            }
        };
        process_control_registry
            .clear_execution_abort_handle(callback)
            .await;
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
        finalize_worker_supervision(
            callback,
            &process_control_registry,
            &run_logger,
            &event_tx,
            &agent_id,
            channel_id,
            task_store.as_ref(),
            outcome_kind,
            result_text,
            transcript.as_ref(),
            terminal_owner,
            notify,
            &task_terminal_notify,
        )
        .await;
    })
}

#[allow(clippy::too_many_arguments)]
async fn finalize_worker_supervision(
    callback: WorkerCallbackContext,
    process_control_registry: &crate::agent::process_control::ProcessControlRegistry,
    run_logger: &ProcessRunLogger,
    event_tx: &broadcast::Sender<ProcessEvent>,
    agent_id: &crate::AgentId,
    channel_id: Option<ChannelId>,
    task_store: Option<&Arc<crate::tasks::TaskStore>>,
    outcome_kind: WorkerOutcomeKind,
    result_text: String,
    transcript: Option<&crate::agent::worker::WorkerTranscriptPayload>,
    terminal_owner: WorkerTerminalOwner,
    notify: bool,
    terminal_notify: &tokio::sync::Notify,
) {
    let worker_id = callback.worker_id;
    let active_operation = process_control_registry
        .worker_snapshot_for_callback(callback)
        .await
        .and_then(|snapshot| snapshot.active_operation);
    let commit = commit_worker_outcome_with_retry(
        run_logger,
        worker_id,
        outcome_kind,
        &result_text,
        transcript,
        terminal_owner,
    )
    .await;
    if let Some(task_store) = task_store {
        let (resolved, summary) = commit
            .as_ref()
            .ok()
            .and_then(|commit| commit.as_ref())
            .map_or((outcome_kind, result_text.as_str()), |(terminal, _)| {
                (terminal.outcome_kind, terminal.result.as_str())
            });
        if let Err(error) = task_store
            .finish_task_attempt(&worker_id.to_string(), resolved.into(), Some(summary))
            .await
        {
            tracing::warn!(%error, %worker_id, "failed to record the task attempt outcome");
        }
    }
    process_control_registry
        .remove_worker_if_registration_matches(callback)
        .await;
    terminal_notify.notify_waiters();
    match commit {
        Ok(Some((terminal, _))) => {
            event_tx
                .send(worker_complete_event(
                    agent_id.clone(),
                    channel_id,
                    callback,
                    active_operation,
                    terminal,
                    notify,
                ))
                .ok();
        }
        Ok(None) => {
            tracing::error!(%worker_id, "worker terminal outcome remained unavailable after retries");
        }
        Err(error) => {
            tracing::error!(%error, %worker_id, "worker terminal outcome failed after retries");
        }
    }
}

pub(crate) async fn commit_worker_outcome_with_retry(
    run_logger: &ProcessRunLogger,
    worker_id: WorkerId,
    outcome_kind: WorkerOutcomeKind,
    result: &str,
    transcript: Option<&crate::agent::worker::WorkerTranscriptPayload>,
    terminal_owner: WorkerTerminalOwner,
) -> crate::Result<Option<(WorkerTerminalOutcome, bool)>> {
    let mut last_error = None;
    for attempt in 0..TERMINAL_COMMIT_ATTEMPTS {
        match tokio::time::timeout(
            TERMINAL_COMMIT_TIMEOUT,
            commit_worker_outcome(
                run_logger,
                worker_id,
                outcome_kind,
                result,
                transcript,
                terminal_owner,
            ),
        )
        .await
        {
            Ok(Ok(Some(commit))) => return Ok(Some(commit)),
            Ok(Ok(None)) => {}
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(
                    anyhow::anyhow!(
                        "worker terminal commit timed out after {TERMINAL_COMMIT_TIMEOUT:?}"
                    )
                    .into(),
                );
            }
        }
        if attempt + 1 < TERMINAL_COMMIT_ATTEMPTS {
            tokio::time::sleep(TERMINAL_COMMIT_RETRY_DELAY).await;
        }
    }
    match last_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

pub(crate) fn worker_complete_event(
    agent_id: crate::AgentId,
    channel_id: Option<ChannelId>,
    callback: WorkerCallbackContext,
    active_operation: Option<WorkerOperationContext>,
    terminal: WorkerTerminalOutcome,
    notify: bool,
) -> ProcessEvent {
    ProcessEvent::WorkerComplete {
        agent_id,
        worker_id: terminal
            .worker_id
            .parse()
            .expect("persisted worker IDs are UUIDs"),
        worker_registration_id: callback.registration_id,
        active_operation,
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
                WorkerLifecycle::Cancelling => {
                    outcome_kind = if transcript.is_some() {
                        WorkerOutcomeKind::Partial
                    } else {
                        WorkerOutcomeKind::Cancelled
                    };
                    lifecycle
                }
                WorkerLifecycle::TimingOut => {
                    outcome_kind = if transcript.is_some() {
                        WorkerOutcomeKind::Partial
                    } else {
                        WorkerOutcomeKind::TimedOut
                    };
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

    if lifecycle == WorkerLifecycle::Cancelling
        && matches!(
            outcome_kind,
            WorkerOutcomeKind::Succeeded
                | WorkerOutcomeKind::Partial
                | WorkerOutcomeKind::Blocked
                | WorkerOutcomeKind::Failed
        )
    {
        outcome_kind = if transcript.is_some() {
            WorkerOutcomeKind::Partial
        } else {
            WorkerOutcomeKind::Cancelled
        };
    } else if lifecycle == WorkerLifecycle::TimingOut
        && matches!(
            outcome_kind,
            WorkerOutcomeKind::Succeeded
                | WorkerOutcomeKind::Partial
                | WorkerOutcomeKind::Blocked
                | WorkerOutcomeKind::Failed
        )
    {
        outcome_kind = if transcript.is_some() {
            WorkerOutcomeKind::Partial
        } else {
            WorkerOutcomeKind::TimedOut
        };
    }

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

pub struct WorkerRestorationContext {
    pub deps: AgentDeps,
    pub channel_id: Option<ChannelId>,
    pub process_run_logger: ProcessRunLogger,
    pub screenshot_dir: std::path::PathBuf,
    pub logs_dir: std::path::PathBuf,
    pub worker_context: WorkerContextMode,
    pub model_overrides: Arc<crate::conversation::settings::ResolvedConversationSettings>,
}

/// Restore an idle interactive worker directly into its agent registry.
pub async fn restore_idle_worker_into_registry(
    state: &WorkerRestorationContext,
    idle_worker: &crate::conversation::history::IdleWorkerRow,
) -> std::result::Result<WorkerId, String> {
    let worker_id: WorkerId = idle_worker
        .id
        .parse::<uuid::Uuid>()
        .map_err(|error| format!("invalid worker ID '{}': {error}", idle_worker.id))?;
    let provenance = WorkerProvenance {
        origin_channel_id: idle_worker.channel_id.as_deref().map(Arc::<str>::from),
        origin_branch_id: None,
        task: idle_worker.task.clone(),
        task_id: None,
        autonomy_run_id: None,
        spawning_process: crate::ProcessId::Worker(worker_id),
    };
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
            let admission_scope = provenance
                .origin_channel_id
                .clone()
                .unwrap_or_else(|| Arc::from("cortex"));
            let reservation = state
                .deps
                .process_control_registry
                .reserve_worker_in_scope(
                    worker_id,
                    &provenance,
                    admission_scope,
                    **state.deps.runtime_config.max_concurrent_workers.load(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let callback = reservation.callback_context();
            let result = crate::opencode::OpenCodeWorker::resume_interactive(
                worker_id,
                callback,
                state.channel_id.clone(),
                state.deps.agent_id.clone(),
                &idle_worker.task,
                directory,
                server_pool,
                state.deps.event_tx.clone(),
                session_id.to_string(),
                idle_worker.transcript.clone(),
                state.deps.process_control_registry.clone(),
            )
            .await;

            let Some((mut worker, input_tx)) = result else {
                state
                    .deps
                    .process_control_registry
                    .release_worker_reservation(reservation)
                    .await;
                return Err(
                    "failed to reconnect to OpenCode session (server dead or session expired)"
                        .to_string(),
                );
            };
            if let Some(model) = state.model_overrides.resolve_model("worker") {
                worker = worker.with_model(model);
            }

            // Apply builder chain (same as spawn_opencode_worker_from_state).
            let oc_secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
            if let Some(store) = &oc_secrets_store {
                worker = worker.with_secrets_store(store.clone());
            }
            worker = worker.with_sqlite_pool(state.deps.sqlite_pool.clone());

            let worker_span = tracing::info_span!(
                "worker.resume",
                worker_id = %worker_id,
                channel_id = ?state.channel_id,
                worker_type = "opencode",
            );
            let transcript_snapshot = worker.transcript_snapshot();
            let opencode_cancellation = worker.cancellation_session();
            let (runtime_control, cancel_rx, terminal_notify) = WorkerRuntimeControl::new(
                transcript_snapshot.clone(),
                Some(opencode_cancellation),
                Some(input_tx),
                None,
                Some(state.process_run_logger.clone()),
            );
            let admission = state
                .deps
                .process_control_registry
                .register_restored_worker(
                    reservation,
                    provenance,
                    WorkerBackend::OpenCode,
                    true,
                    "idle",
                    usize::try_from(idle_worker.tool_calls).unwrap_or(usize::MAX),
                    runtime_control,
                )
                .await
                .map_err(|error| error.to_string())?;
            let (start_gate, start_rx) = WorkerStartGate::new();
            let handle = spawn_worker_task(
                callback,
                state.deps.process_control_registry.clone(),
                cancel_rx,
                terminal_notify,
                start_rx,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                state.channel_id.clone(),
                state.process_run_logger.clone(),
                transcript_snapshot,
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

            if let Err(handle) = state
                .deps
                .process_control_registry
                .install_task_handle(admission.callback_context(), handle)
                .await
            {
                handle.abort();
                state
                    .deps
                    .process_control_registry
                    .remove_worker_if_registration_matches(callback)
                    .await;
                return Err("restored worker detached before task installation".to_string());
            }
            let opencode_task = format!("[opencode] {}", idle_worker.task);

            let event_tx = state.deps.event_tx.clone();
            let started_event = ProcessEvent::WorkerStarted {
                agent_id: state.deps.agent_id.clone(),
                worker_id,
                worker_registration_id: callback.registration_id,
                channel_id: state.channel_id.clone(),
                task: opencode_task,
                worker_type: "opencode".into(),
                interactive: true,
                directory: Some(directory_str.clone()),
            };
            if state
                .deps
                .process_control_registry
                .run_if_worker_state(callback, WorkerRuntimeState::WaitingForInput, move || {
                    event_tx.send(started_event).ok();
                    start_gate.open();
                })
                .await
                != crate::agent::process_control::WorkerMutationResult::Applied
            {
                state
                    .deps
                    .process_control_registry
                    .remove_worker_if_registration_matches(callback)
                    .await;
                return Err("restored worker was cancelled before its gate opened".to_string());
            }

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
                    state.worker_context.wiki_write && state.deps.wiki_store.is_some(),
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
            append_worker_memory_context(
                &mut system_prompt,
                &state.deps,
                state.channel_id.as_ref(),
                state.worker_context.memory,
            )
            .await;
            let brave_search_key = (**rc.brave_search_key.load()).clone();
            let admission_scope = provenance
                .origin_channel_id
                .clone()
                .unwrap_or_else(|| Arc::from("cortex"));
            let reservation = state
                .deps
                .process_control_registry
                .reserve_worker_in_scope(
                    worker_id,
                    &provenance,
                    admission_scope,
                    **state.deps.runtime_config.max_concurrent_workers.load(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let callback = reservation.callback_context();

            let (worker, input_tx, inject_tx) = Worker::resume_interactive(
                worker_id,
                callback,
                state.channel_id.clone(),
                &idle_worker.task,
                system_prompt,
                state.deps.clone(),
                browser_config,
                state.screenshot_dir.clone(),
                brave_search_key,
                state.logs_dir.clone(),
                prior_history,
                state.worker_context.memory,
                state.worker_context.wiki_write,
                state
                    .model_overrides
                    .resolve_model("worker")
                    .map(String::from),
            );

            let worker_span = tracing::info_span!(
                "worker.resume",
                worker_id = %worker_id,
                channel_id = ?state.channel_id,
            );
            let secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
            let transcript_snapshot = worker.transcript_snapshot();
            let (runtime_control, cancel_rx, terminal_notify) = WorkerRuntimeControl::new(
                transcript_snapshot.clone(),
                None,
                Some(input_tx),
                Some(inject_tx),
                Some(state.process_run_logger.clone()),
            );
            let admission = state
                .deps
                .process_control_registry
                .register_restored_worker(
                    reservation,
                    provenance,
                    WorkerBackend::Builtin,
                    true,
                    "idle",
                    usize::try_from(idle_worker.tool_calls).unwrap_or(usize::MAX),
                    runtime_control,
                )
                .await
                .map_err(|error| error.to_string())?;
            let (start_gate, start_rx) = WorkerStartGate::new();
            let handle = spawn_worker_task(
                callback,
                state.deps.process_control_registry.clone(),
                cancel_rx,
                terminal_notify,
                start_rx,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                state.channel_id.clone(),
                state.process_run_logger.clone(),
                transcript_snapshot,
                None,
                secrets_store,
                Some(state.deps.task_store.clone()),
                "builtin",
                worker.run().instrument(worker_span),
            );

            if let Err(handle) = state
                .deps
                .process_control_registry
                .install_task_handle(admission.callback_context(), handle)
                .await
            {
                handle.abort();
                state
                    .deps
                    .process_control_registry
                    .remove_worker_if_registration_matches(callback)
                    .await;
                return Err("restored worker detached before task installation".to_string());
            }
            let event_tx = state.deps.event_tx.clone();
            let started_event = ProcessEvent::WorkerStarted {
                agent_id: state.deps.agent_id.clone(),
                worker_id,
                worker_registration_id: callback.registration_id,
                channel_id: state.channel_id.clone(),
                task: idle_worker.task.clone(),
                worker_type: "builtin".into(),
                interactive: true,
                directory: None,
            };
            if state
                .deps
                .process_control_registry
                .run_if_worker_state(callback, WorkerRuntimeState::WaitingForInput, move || {
                    event_tx.send(started_event).ok();
                    start_gate.open();
                })
                .await
                != crate::agent::process_control::WorkerMutationResult::Applied
            {
                state
                    .deps
                    .process_control_registry
                    .remove_worker_if_registration_matches(callback)
                    .await;
                return Err("restored worker was cancelled before its gate opened".to_string());
            }

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
        WorkerCompletionError, WorkerOutcome, commit_worker_outcome,
        commit_worker_outcome_with_retry, map_worker_completion, spawn_worker_task,
        worker_task_prompt,
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

    async fn spawn_test_worker_task<F>(
        worker_id: WorkerId,
        channel_id: &str,
        event_tx: broadcast::Sender<ProcessEvent>,
        run_logger: ProcessRunLogger,
        future: F,
    ) -> Arc<crate::agent::process_control::ProcessControlRegistry>
    where
        F: std::future::Future<Output = crate::Result<WorkerOutcome>> + Send + 'static,
    {
        use crate::agent::process_control::{
            ProcessControlRegistry, WorkerBackend, WorkerOperationContext, WorkerOperationId,
            WorkerProvenance, WorkerRequester, WorkerResultTarget, WorkerRuntimeControl,
            WorkerRuntimeState,
        };

        let registry = Arc::new(ProcessControlRegistry::new());
        let channel_id: crate::ChannelId = Arc::from(channel_id);
        let provenance = WorkerProvenance {
            origin_channel_id: Some(channel_id.clone()),
            origin_branch_id: None,
            task: "task".to_string(),
            task_id: None,
            autonomy_run_id: None,
            spawning_process: crate::ProcessId::Worker(worker_id),
        };
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 4)
            .await
            .unwrap();
        let callback = reservation.callback_context();
        let operation = WorkerOperationContext {
            operation_id: WorkerOperationId::new(),
            requester: WorkerRequester::Channel {
                channel_id: channel_id.clone(),
            },
            result_target: WorkerResultTarget::Channel {
                channel_id: channel_id.clone(),
            },
            autonomy_run_id: None,
        };
        let snapshot = crate::agent::worker::new_worker_transcript_snapshot();
        let (control, cancel_rx, terminal_notify) =
            WorkerRuntimeControl::new(snapshot.clone(), None, None, None, Some(run_logger.clone()));
        let admission = registry
            .register_new_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                false,
                operation,
                "starting",
                control,
            )
            .await
            .unwrap();
        let (start_gate, start_rx) = super::WorkerStartGate::new();
        let handle = spawn_worker_task(
            callback,
            registry.clone(),
            cancel_rx,
            terminal_notify,
            start_rx,
            event_tx,
            Arc::from("agent"),
            Some(channel_id),
            run_logger,
            snapshot,
            None,
            None,
            None,
            "builtin",
            future,
        );
        registry
            .install_task_handle(admission.callback_context(), handle)
            .await
            .unwrap();
        assert_eq!(
            registry
                .update_worker_state(callback, WorkerRuntimeState::Running)
                .await,
            crate::agent::process_control::WorkerMutationResult::Applied
        );
        start_gate.open();
        registry
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

        let _registry = spawn_test_worker_task(worker_id, "channel", event_tx, run_logger, async {
            Err::<WorkerOutcome, crate::Error>(
                crate::error::AgentError::Cancelled {
                    reason: "user requested".to_string(),
                }
                .into(),
            )
        })
        .await;

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("worker completion event should be delivered")
            .expect("broadcast receive should succeed");

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
    async fn origin_cleanup_waits_for_worker_terminalization() {
        let worker_id = Uuid::new_v4();
        let channel_id: crate::ChannelId = Arc::from("cron:test-cleanup");
        let logger = setup_worker(worker_id, &channel_id).await;
        let (event_tx, _event_rx) = broadcast::channel(8);
        let registry = spawn_test_worker_task(
            worker_id,
            &channel_id,
            event_tx,
            logger.clone(),
            std::future::pending(),
        )
        .await;

        assert_eq!(
            registry
                .cancel_workers_by_origin_channel(&channel_id, Duration::from_secs(1))
                .await,
            1
        );
        assert!(registry.worker_snapshot(worker_id).await.is_none());
        assert_eq!(
            logger
                .read_worker_terminal(worker_id)
                .await
                .unwrap()
                .unwrap()
                .outcome_kind,
            WorkerOutcomeKind::Cancelled
        );
    }

    #[tokio::test]
    async fn forced_cancellation_converges_durable_state_and_publishes_completion() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "forced-cancel-channel").await;
        let registry = spawn_test_worker_task(
            worker_id,
            "forced-cancel-channel",
            event_tx,
            run_logger.clone(),
            std::future::pending(),
        )
        .await;

        assert_eq!(
            registry
                .cancel_worker_runtime(worker_id, Duration::ZERO)
                .await,
            crate::agent::process_control::ControlActionResult::Cancelled
        );
        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("supervisor should publish terminal completion")
            .unwrap();
        let ProcessEvent::WorkerComplete {
            worker_id: completed_worker_id,
            outcome_kind,
            ..
        } = event
        else {
            panic!("expected worker completion");
        };
        assert_eq!(completed_worker_id, worker_id);
        assert_eq!(outcome_kind, WorkerOutcomeKind::Cancelled);
        assert!(registry.worker_snapshot(worker_id).await.is_none());
        assert_eq!(
            run_logger.read_worker_lifecycle(worker_id).await.unwrap(),
            Some(WorkerLifecycle::Cancelled)
        );
    }

    #[tokio::test]
    async fn terminal_commit_retry_exhaustion_leaves_missing_row_unavailable() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let logger = ProcessRunLogger::new(pool);

        assert!(
            commit_worker_outcome_with_retry(
                &logger,
                Uuid::new_v4(),
                WorkerOutcomeKind::Failed,
                "missing",
                None,
                WorkerTerminalOwner::Worker,
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn cancellation_before_start_gate_never_polls_worker_future() {
        use crate::agent::process_control::{
            ProcessControlRegistry, WorkerBackend, WorkerOperationContext, WorkerOperationId,
            WorkerProvenance, WorkerRequester, WorkerResultTarget, WorkerRuntimeControl,
            WorkerRuntimeState,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "pre-gate-channel").await;
        let registry = Arc::new(ProcessControlRegistry::new());
        let channel_id: crate::ChannelId = Arc::from("pre-gate-channel");
        let provenance = WorkerProvenance {
            origin_channel_id: Some(channel_id.clone()),
            origin_branch_id: None,
            task: "task".to_string(),
            task_id: None,
            autonomy_run_id: None,
            spawning_process: crate::ProcessId::Worker(worker_id),
        };
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 1)
            .await
            .unwrap();
        let callback = reservation.callback_context();
        let operation = WorkerOperationContext {
            operation_id: WorkerOperationId::new(),
            requester: WorkerRequester::System,
            result_target: WorkerResultTarget::None,
            autonomy_run_id: None,
        };
        let snapshot = crate::agent::worker::new_worker_transcript_snapshot();
        let (control, cancel_rx, terminal_notify) =
            WorkerRuntimeControl::new(snapshot.clone(), None, None, None, None);
        let admission = registry
            .register_new_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                false,
                operation,
                "starting",
                control,
            )
            .await
            .unwrap();
        let (start_gate, start_rx) = super::WorkerStartGate::new();
        let polls = Arc::new(AtomicUsize::new(0));
        let future_polls = polls.clone();
        let handle = spawn_worker_task(
            callback,
            registry.clone(),
            cancel_rx,
            terminal_notify,
            start_rx,
            event_tx,
            Arc::from("agent"),
            Some(channel_id),
            run_logger.clone(),
            snapshot,
            None,
            None,
            None,
            "builtin",
            std::future::poll_fn(move |_context| {
                future_polls.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Pending
            }),
        );
        registry
            .install_task_handle(admission.callback_context(), handle)
            .await
            .unwrap();

        assert_eq!(
            registry
                .cancel_worker_runtime(worker_id, Duration::from_secs(1))
                .await,
            crate::agent::process_control::ControlActionResult::Cancelled
        );
        let task_binding_mutations = AtomicUsize::new(0);
        if registry
            .worker_is_in_state(callback, WorkerRuntimeState::Starting)
            .await
        {
            task_binding_mutations.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(task_binding_mutations.load(Ordering::SeqCst), 0);
        drop(start_gate);

        tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("pre-gate cancellation should converge")
            .unwrap();

        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert!(registry.worker_snapshot(worker_id).await.is_none());
        assert_eq!(
            run_logger.read_worker_lifecycle(worker_id).await.unwrap(),
            Some(WorkerLifecycle::Cancelled)
        );
    }

    #[tokio::test]
    async fn restored_idle_worker_runs_after_gate_without_leaving_idle_state() {
        use crate::agent::process_control::{
            ProcessControlRegistry, WorkerBackend, WorkerProvenance, WorkerRuntimeControl,
            WorkerRuntimeState,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "restored-channel").await;
        assert!(matches!(
            run_logger.log_worker_idle(worker_id).await.unwrap(),
            crate::conversation::WorkerTransitionResult::Applied { .. }
        ));
        let registry = Arc::new(ProcessControlRegistry::new());
        let channel_id: crate::ChannelId = Arc::from("restored-channel");
        let provenance = WorkerProvenance {
            origin_channel_id: Some(channel_id.clone()),
            origin_branch_id: None,
            task: "task".to_string(),
            task_id: None,
            autonomy_run_id: None,
            spawning_process: crate::ProcessId::Worker(worker_id),
        };
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 1)
            .await
            .unwrap();
        let callback = reservation.callback_context();
        let snapshot = crate::agent::worker::new_worker_transcript_snapshot();
        let (control, cancel_rx, terminal_notify) =
            WorkerRuntimeControl::new(snapshot.clone(), None, None, None, Some(run_logger.clone()));
        let admission = registry
            .register_restored_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                true,
                "idle",
                0,
                control,
            )
            .await
            .unwrap();
        let (start_gate, start_rx) = super::WorkerStartGate::new();
        let polls = Arc::new(AtomicUsize::new(0));
        let future_polls = polls.clone();
        let handle = spawn_worker_task(
            callback,
            registry.clone(),
            cancel_rx,
            terminal_notify,
            start_rx,
            event_tx,
            Arc::from("agent"),
            Some(channel_id),
            run_logger.clone(),
            snapshot,
            None,
            None,
            None,
            "builtin",
            std::future::poll_fn(move |_context| {
                future_polls.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Pending
            }),
        );
        registry
            .install_task_handle(admission.callback_context(), handle)
            .await
            .unwrap();
        start_gate.open();

        tokio::time::timeout(Duration::from_secs(1), async {
            while polls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("restored worker future should be polled after the gate opens");
        let live = registry.worker_snapshot(worker_id).await.unwrap();
        assert_eq!(live.state, WorkerRuntimeState::WaitingForInput);
        assert!(live.active_operation.is_none());

        assert_eq!(
            registry
                .cancel_worker_runtime(worker_id, Duration::from_secs(1))
                .await,
            crate::agent::process_control::ControlActionResult::Cancelled
        );
        tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("restored worker cancellation should converge")
            .unwrap();
        assert_eq!(
            run_logger.read_worker_lifecycle(worker_id).await.unwrap(),
            Some(WorkerLifecycle::Cancelled)
        );
    }

    #[tokio::test]
    async fn dropping_parent_control_does_not_cancel_worker() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "detached-channel").await;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();

        let registry = spawn_test_worker_task(
            worker_id,
            "detached-channel",
            event_tx,
            run_logger,
            async move {
                started_tx.send(()).expect("test receiver remains active");
                finish_rx.await.expect("test sender remains active");
                Ok::<WorkerOutcome, crate::Error>(WorkerOutcome::Success {
                    result: "completed after parent exit".to_string(),
                })
            },
        )
        .await;

        started_rx.await.expect("worker should start");
        drop(registry);
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

        let _registry =
            spawn_test_worker_task(worker_id, &channel_id, event_tx, run_logger, async {
                Ok::<WorkerOutcome, crate::Error>(WorkerOutcome::Success {
                    result: "result".to_string(),
                })
            })
            .await;

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("worker completion event should be delivered")
            .expect("broadcast receive should succeed");

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
        assert!(_registry.worker_snapshot(worker_id).await.is_none());
    }

    #[tokio::test]
    async fn worker_completion_is_durable_before_notification() {
        let (event_tx, mut event_rx) = broadcast::channel(8);
        let worker_id = Uuid::new_v4();
        let run_logger = setup_worker(worker_id, "durable-channel").await;
        let inspect_logger = run_logger.clone();
        let _registry =
            spawn_test_worker_task(worker_id, "durable-channel", event_tx, run_logger, async {
                Ok::<WorkerOutcome, crate::Error>(WorkerOutcome::Success {
                    result: "durable result".to_string(),
                })
            })
            .await;

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
    }
}
