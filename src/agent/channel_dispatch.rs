//! Branch and worker spawning for channels.
//!
//! Contains the public entry points that channel tools use to create
//! background processes: `spawn_branch_from_state`, `spawn_worker_from_state`,
//! and `spawn_opencode_worker_from_state`.

use crate::agent::branch::Branch;
use crate::agent::channel::ChannelState;
use crate::agent::channel_prompt::{TemporalContext, build_worker_task_with_temporal_context};
use crate::agent::worker::Worker;
use crate::error::{AgentError, Error as SpacebotError};
use crate::{AgentDeps, BranchId, ChannelId, ProcessEvent, WorkerId};
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
    Cancelled,
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

fn classify_worker_completion_result(
    result: std::result::Result<String, WorkerCompletionError>,
) -> (String, WorkerCompletionKind) {
    match result {
        Ok(text) => (text, WorkerCompletionKind::Success),
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
    let success = matches!(kind, WorkerCompletionKind::Success);
    (notify, success)
}

/// Normalize worker completion into event payload fields.
pub(crate) fn map_worker_completion_result(
    result: std::result::Result<String, WorkerCompletionError>,
) -> (String, bool, bool) {
    let (result_text, kind) = classify_worker_completion_result(result);
    let (notify, success) = completion_flags(kind);
    (result_text, notify, success)
}

/// Spawn a branch from a ChannelState. Used by the BranchTool.
pub async fn spawn_branch_from_state(
    state: &ChannelState,
    description: impl Into<String>,
) -> std::result::Result<BranchId, AgentError> {
    let description = description.into();
    let rc = &state.deps.runtime_config;
    let prompt_engine = rc.prompts.load();
    let system_prompt = prompt_engine
        .render_branch_prompt(
            &rc.instance_dir.display().to_string(),
            &rc.workspace_dir.display().to_string(),
        )
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;

    spawn_branch(
        state,
        &description,
        &description,
        &system_prompt,
        &description,
        "branch",
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
    let prompt_engine = deps.runtime_config.prompts.load();
    let system_prompt = prompt_engine
        .render_static("memory_persistence")
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
) -> std::result::Result<BranchId, AgentError> {
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
    );
    let branch_max_turns = **state.deps.runtime_config.branch_max_turns.load();

    let branch = Branch::new(
        state.channel_id.clone(),
        description,
        state.deps.clone(),
        system_prompt,
        history,
        tool_server,
        branch_max_turns,
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
                    crate::secrets::scrub::scrub_with_store(&raw, store)
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

    {
        let mut branches = state.active_branches.write().await;
        branches.insert(branch_id, handle);
    }

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
async fn check_worker_limit(state: &ChannelState) -> std::result::Result<(), AgentError> {
    let max_workers = **state.deps.runtime_config.max_concurrent_workers.load();
    let active_worker_count = state.active_workers.read().await.len();
    reserve_worker_slot_local(active_worker_count, &state.channel_id, max_workers)
}

/// Spawn a worker from a ChannelState. Used by the SpawnWorkerTool.
pub async fn spawn_worker_from_state(
    state: &ChannelState,
    task: impl Into<String>,
    interactive: bool,
    suggested_skills: &[&str],
) -> std::result::Result<WorkerId, AgentError> {
    check_worker_limit(state).await?;
    ensure_dispatch_readiness(state, "worker");
    let task = task.into();

    let rc = &state.deps.runtime_config;
    let prompt_engine = rc.prompts.load();
    let temporal_context = TemporalContext::from_runtime(rc.as_ref());
    let worker_task =
        build_worker_task_with_temporal_context(&task, &temporal_context, &prompt_engine)
            .map_err(|error| AgentError::Other(anyhow::anyhow!("{error}")))?;
    let sandbox_enabled = state.deps.sandbox.mode_enabled();
    let sandbox_containment_active = state.deps.sandbox.containment_active();
    let sandbox_read_allowlist = state.deps.sandbox.prompt_read_allowlist();
    let sandbox_write_allowlist = state.deps.sandbox.prompt_write_allowlist();
    // Collect tool secret names so the worker template can list available credentials.
    let secrets_guard = rc.secrets.load();
    let tool_secret_names = match (*secrets_guard).as_ref() {
        Some(store) => store.tool_secret_names(),
        None => Vec::new(),
    };

    let worker_system_prompt = prompt_engine
        .render_worker_prompt(
            &rc.instance_dir.display().to_string(),
            &rc.workspace_dir.display().to_string(),
            sandbox_enabled,
            sandbox_containment_active,
            sandbox_read_allowlist,
            sandbox_write_allowlist,
            &tool_secret_names,
        )
        .map_err(|e| AgentError::Other(anyhow::anyhow!("{e}")))?;
    let skills = rc.skills.load();
    let browser_config = (**rc.browser_config.load()).clone();
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

    let worker = if interactive {
        let (worker, input_tx) = Worker::new_interactive(
            Some(state.channel_id.clone()),
            &worker_task,
            &system_prompt,
            state.deps.clone(),
            browser_config.clone(),
            state.screenshot_dir.clone(),
            brave_search_key.clone(),
            state.logs_dir.clone(),
        );
        let worker_id = worker.id;
        state
            .worker_inputs
            .write()
            .await
            .insert(worker_id, input_tx);
        worker
    } else {
        Worker::new(
            Some(state.channel_id.clone()),
            &worker_task,
            &system_prompt,
            state.deps.clone(),
            browser_config,
            state.screenshot_dir.clone(),
            brave_search_key,
            state.logs_dir.clone(),
        )
    };

    let worker_id = worker.id;

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
        task = %task,
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
        status.add_worker(worker_id, &task, false, interactive);
    }

    state
        .deps
        .event_tx
        .send(crate::ProcessEvent::WorkerStarted {
            agent_id: state.deps.agent_id.clone(),
            worker_id,
            channel_id: Some(state.channel_id.clone()),
            task: task.clone(),
            worker_type: "builtin".into(),
            interactive,
            directory: None,
        })
        .ok();

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
    ensure_dispatch_readiness(state, "opencode_worker");
    let task = task.into();
    let directory = expand_tilde(directory);

    let rc = &state.deps.runtime_config;
    let prompt_engine = rc.prompts.load();
    let temporal_context = TemporalContext::from_runtime(rc.as_ref());
    let worker_task =
        build_worker_task_with_temporal_context(&task, &temporal_context, &prompt_engine)
            .map_err(|error| AgentError::Other(anyhow::anyhow!("{error}")))?;
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
        task = %task,
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

            Ok::<String, SpacebotError>(result.result_text)
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
    F: std::future::Future<Output = crate::Result<String>> + Send + 'static,
{
    tokio::spawn(async move {
        #[cfg(feature = "metrics")]
        let worker_start = std::time::Instant::now();

        #[cfg(feature = "metrics")]
        crate::telemetry::Metrics::global()
            .active_workers
            .with_label_values(&[&*agent_id])
            .inc();

        let outcome = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        let worker_result: std::result::Result<String, WorkerCompletionError> = match outcome {
            Ok(Ok(text)) => {
                // Scrub tool secret values from the result before it reaches
                // the channel. The channel never sees raw secret values.
                // Layer 1: exact-match redaction of known secrets from the store.
                // Layer 2: regex-based redaction of unknown secret patterns.
                let scrubbed = if let Some(store) = &secrets_store {
                    crate::secrets::scrub::scrub_with_store(&text, store)
                } else {
                    text
                };
                let scrubbed = crate::secrets::scrub::scrub_leaks(&scrubbed);
                Ok(scrubbed)
            }
            Ok(Err(error)) => {
                let failure = WorkerCompletionError::from_spacebot_error(error);
                match failure {
                    WorkerCompletionError::Cancelled { .. } => Err(failure),
                    WorkerCompletionError::Failed { message } => {
                        let scrubbed = if let Some(store) = &secrets_store {
                            crate::secrets::scrub::scrub_with_store(&message, store)
                        } else {
                            message
                        };
                        let scrubbed = crate::secrets::scrub::scrub_leaks(&scrubbed);
                        Err(WorkerCompletionError::Failed { message: scrubbed })
                    }
                }
            }
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
        let (result_text, kind) = classify_worker_completion_result(worker_result);
        match kind {
            WorkerCompletionKind::Success => {}
            WorkerCompletionKind::Cancelled => {
                tracing::info!(worker_id = %worker_id, result = %result_text, "worker cancelled");
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
                task = %idle_worker.task,
                worker_type = "opencode",
            );
            let sqlite_pool = state.deps.sqlite_pool.clone();
            let handle = spawn_worker_task(
                worker_id,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                Some(state.channel_id.clone()),
                oc_secrets_store,
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
                    Ok::<String, SpacebotError>(result.result_text)
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
            let sandbox_enabled = state.deps.sandbox.mode_enabled();
            let sandbox_containment_active = state.deps.sandbox.containment_active();
            let sandbox_read_allowlist = state.deps.sandbox.prompt_read_allowlist();
            let sandbox_write_allowlist = state.deps.sandbox.prompt_write_allowlist();
            let secrets_guard = rc.secrets.load();
            let tool_secret_names = match (*secrets_guard).as_ref() {
                Some(store) => store.tool_secret_names(),
                None => Vec::new(),
            };
            let system_prompt = prompt_engine
                .render_worker_prompt(
                    &rc.instance_dir.display().to_string(),
                    &rc.workspace_dir.display().to_string(),
                    sandbox_enabled,
                    sandbox_containment_active,
                    sandbox_read_allowlist,
                    sandbox_write_allowlist,
                    &tool_secret_names,
                )
                .map_err(|error| format!("failed to render worker prompt: {error}"))?;
            let browser_config = (**rc.browser_config.load()).clone();
            let brave_search_key = (**rc.brave_search_key.load()).clone();

            let (worker, input_tx) = Worker::resume_interactive(
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

            let worker_span = tracing::info_span!(
                "worker.resume",
                worker_id = %worker_id,
                channel_id = %state.channel_id,
                task = %idle_worker.task,
            );
            let secrets_store = state.deps.runtime_config.secrets.load().as_ref().clone();
            let handle = spawn_worker_task(
                worker_id,
                state.deps.event_tx.clone(),
                state.deps.agent_id.clone(),
                Some(state.channel_id.clone()),
                secrets_store,
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
    use super::{WorkerCompletionError, map_worker_completion_result, spawn_worker_task};
    use crate::{ProcessEvent, WorkerId};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    #[test]
    fn cancelled_errors_are_classified_as_cancelled_results() {
        let (text, notify, success) =
            map_worker_completion_result(Err(WorkerCompletionError::Cancelled {
                reason: "user requested".to_string(),
            }));
        assert_eq!(text, "Worker cancelled: user requested");
        assert!(notify);
        assert!(!success);
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
            async {
                Err::<String, crate::Error>(
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
            async { Ok::<String, crate::Error>("result".to_string()) },
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
