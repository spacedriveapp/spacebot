use super::*;

use crate::agent::branch::Branch;
use crate::agent::worker::Worker;
use crate::error::AgentError;

use tracing::Instrument as _;

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
pub(super) async fn spawn_memory_persistence_branch(
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
    crate::telemetry::Metrics::global()
        .active_branches
        .with_label_values(&[&*state.deps.agent_id])
        .inc();

    state
        .deps
        .event_tx
        .send(crate::ProcessEvent::BranchStarted {
            agent_id: state.deps.agent_id.clone(),
            branch_id,
            channel_id: state.channel_id.clone(),
            description: status_label.to_string(),
            reply_to_message_id: *state.reply_target_message_id.read().await,
        })
        .ok();

    tracing::info!(branch_id = %branch_id, description = %status_label, "branch spawned");

    Ok(branch_id)
}

/// Check whether the channel has capacity for another worker.
async fn check_worker_limit(state: &ChannelState) -> std::result::Result<(), AgentError> {
    let max_workers = **state.deps.runtime_config.max_concurrent_workers.load();
    let workers = state.active_workers.read().await;
    if workers.len() >= max_workers {
        return Err(AgentError::WorkerLimitReached {
            channel_id: state.channel_id.to_string(),
            max: max_workers,
        });
    }
    Ok(())
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
    let worker_system_prompt = prompt_engine
        .render_worker_prompt(
            &rc.instance_dir.display().to_string(),
            &rc.workspace_dir.display().to_string(),
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
            &task,
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
            &task,
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
    let handle = spawn_worker_task(
        worker_id,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        worker.run().instrument(worker_span),
    );

    state.worker_handles.write().await.insert(worker_id, handle);

    {
        let mut status = state.status_block.write().await;
        status.add_worker(worker_id, &task, false);
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
        })
        .ok();

    tracing::info!(worker_id = %worker_id, task = %task, "worker spawned");

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
    check_worker_limit(state).await?;
    ensure_dispatch_readiness(state, "opencode_worker");
    let task = task.into();
    let directory = std::path::PathBuf::from(directory);

    let rc = &state.deps.runtime_config;
    let opencode_config = rc.opencode.load();

    if !opencode_config.enabled {
        return Err(AgentError::Other(anyhow::anyhow!(
            "OpenCode workers are not enabled in config"
        )));
    }

    let server_pool = rc.opencode_server_pool.clone();

    let worker = if interactive {
        let (worker, input_tx) = crate::opencode::OpenCodeWorker::new_interactive(
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            &task,
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
        worker
    } else {
        crate::opencode::OpenCodeWorker::new(
            Some(state.channel_id.clone()),
            state.deps.agent_id.clone(),
            &task,
            directory,
            server_pool,
            state.deps.event_tx.clone(),
        )
    };

    let worker_id = worker.id;

    let worker_span = tracing::info_span!(
        "worker.run",
        worker_id = %worker_id,
        channel_id = %state.channel_id,
        task = %task,
        worker_type = "opencode",
    );
    let handle = spawn_worker_task(
        worker_id,
        state.deps.event_tx.clone(),
        state.deps.agent_id.clone(),
        Some(state.channel_id.clone()),
        async move {
            let result = worker.run().await?;
            Ok::<String, anyhow::Error>(result.result_text)
        }
        .instrument(worker_span),
    );

    state.worker_handles.write().await.insert(worker_id, handle);

    let opencode_task = format!("[opencode] {task}");
    {
        let mut status = state.status_block.write().await;
        status.add_worker(worker_id, &opencode_task, false);
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
        })
        .ok();

    tracing::info!(worker_id = %worker_id, task = %task, "OpenCode worker spawned");

    Ok(worker_id)
}

/// Spawn a future as a tokio task that sends a `WorkerComplete` event on completion.
///
/// Handles both success and error cases, logging failures and sending the
/// appropriate event. Used by both builtin workers and OpenCode workers.
/// Returns the JoinHandle so the caller can store it for cancellation.
fn spawn_worker_task<F, E>(
    worker_id: WorkerId,
    event_tx: tokio::sync::broadcast::Sender<ProcessEvent>,
    agent_id: crate::AgentId,
    channel_id: Option<ChannelId>,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = std::result::Result<String, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    tokio::spawn(async move {
        #[cfg(feature = "metrics")]
        let worker_start = std::time::Instant::now();

        #[cfg(feature = "metrics")]
        crate::telemetry::Metrics::global()
            .active_workers
            .with_label_values(&[&*agent_id])
            .inc();

        let (result_text, notify, success) = match future.await {
            Ok(text) => (text, true, true),
            Err(error) => {
                tracing::error!(worker_id = %worker_id, %error, "worker failed");
                (format!("Worker failed: {error}"), true, false)
            }
        };
        #[cfg(feature = "metrics")]
        {
            let metrics = crate::telemetry::Metrics::global();
            metrics
                .active_workers
                .with_label_values(&[&*agent_id])
                .dec();
            metrics
                .worker_duration_seconds
                .with_label_values(&[&*agent_id, "builtin"])
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
