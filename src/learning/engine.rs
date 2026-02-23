//! LearningEngine: async event loop coordinator for the learning system.

use super::config::LearningConfig;
use super::store::LearningStore;
use crate::{AgentDeps, ProcessEvent};

use tokio::sync::broadcast;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Spawn the learning engine as a background task.
///
/// Follows the `spawn_bulletin_loop` / `spawn_association_loop` pattern from
/// the cortex. The engine subscribes to the process event bus, filters for
/// owner-only traces, and logs events to learning.db.
pub fn spawn_learning_loop(
    deps: AgentDeps,
    learning_store: Arc<LearningStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_learning_loop(&deps, &learning_store).await {
            tracing::error!(%error, "learning loop exited with error");
        }
    })
}

async fn run_learning_loop(
    deps: &AgentDeps,
    store: &Arc<LearningStore>,
) -> anyhow::Result<()> {
    let mut event_rx = deps.event_tx.subscribe();

    // Trace IDs from non-owner user messages. Downstream events carrying these
    // trace IDs are also skipped, preventing learning from non-owner interactions.
    let mut non_owner_traces: HashSet<String> = HashSet::new();

    tracing::info!(agent_id = %deps.agent_id, "learning engine started");

    loop {
        let config = load_config(deps);
        if !config.enabled {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        let tick_interval = Duration::from_secs(config.tick_interval_secs);
        let mut heartbeat = tokio::time::interval(tick_interval);
        // Don't pile up missed ticks if event processing takes a while.
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Re-check config each outer iteration; break inner loop on disable.
            let config = load_config(deps);
            if !config.enabled {
                break;
            }

            tokio::select! {
                _ = heartbeat.tick() => {
                    if let Err(error) = store
                        .set_state("learning_heartbeat", chrono::Utc::now().to_rfc3339())
                        .await
                    {
                        tracing::warn!(%error, "failed to update learning heartbeat");
                    }
                }
                event = event_rx.recv() => {
                    match event {
                        Ok(process_event) => {
                            if !should_process_event(&config, &mut non_owner_traces, &process_event) {
                                continue;
                            }
                            // Fail-open: log and continue on any error.
                            if let Err(error) = handle_event(store, &process_event).await {
                                tracing::warn!(%error, "learning engine event handling failed");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            tracing::warn!(count, "learning engine lagged behind event stream");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::info!("event channel closed, learning loop exiting");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Load the current learning config snapshot from RuntimeConfig.
fn load_config(deps: &AgentDeps) -> LearningConfig {
    (**deps.runtime_config.learning.load()).clone()
}

/// Route a process event to the learning_events audit log.
///
/// Milestone 1 just logs events. Later milestones route to Layer 1 (episodes),
/// Layer 2 (patterns), Layer 4 (predictions), etc.
async fn handle_event(
    store: &Arc<LearningStore>,
    event: &ProcessEvent,
) -> anyhow::Result<()> {
    let (event_type, summary) = summarize_event(event);
    let details = serde_json::to_value(event).ok();
    store
        .log_event(&event_type, &summary, details.as_ref())
        .await?;
    Ok(())
}

/// Produce a short (event_type, summary) pair for the audit log.
fn summarize_event(event: &ProcessEvent) -> (String, String) {
    match event {
        ProcessEvent::UserMessage {
            sender_id,
            platform,
            ..
        } => (
            "user_message".into(),
            format!("message from {platform}:{sender_id}"),
        ),
        ProcessEvent::UserReaction {
            reaction,
            sender_id,
            platform,
            ..
        } => (
            "user_reaction".into(),
            format!("{reaction} from {platform}:{sender_id}"),
        ),
        ProcessEvent::WorkerStarted { task, worker_id, .. } => (
            "worker_started".into(),
            format!("worker {} started: {}", worker_id, truncate(task, 120)),
        ),
        ProcessEvent::WorkerComplete {
            worker_id,
            success,
            duration_secs,
            ..
        } => (
            "worker_complete".into(),
            format!(
                "worker {} {} in {:.1}s",
                worker_id,
                if *success { "succeeded" } else { "failed" },
                duration_secs,
            ),
        ),
        ProcessEvent::WorkerStatus {
            worker_id, status, ..
        } => (
            "worker_status".into(),
            format!("worker {}: {}", worker_id, truncate(status, 120)),
        ),
        ProcessEvent::BranchStarted {
            branch_id,
            description,
            ..
        } => (
            "branch_started".into(),
            format!("branch {} started: {}", branch_id, truncate(description, 120)),
        ),
        ProcessEvent::BranchResult { branch_id, .. } => (
            "branch_result".into(),
            format!("branch {} completed", branch_id),
        ),
        ProcessEvent::ToolStarted {
            tool_name, call_id, ..
        } => (
            "tool_started".into(),
            format!("{}[{}]", tool_name, call_id),
        ),
        ProcessEvent::ToolCompleted {
            tool_name, call_id, ..
        } => (
            "tool_completed".into(),
            format!("{}[{}]", tool_name, call_id),
        ),
        ProcessEvent::MemorySaved { memory_id, .. } => (
            "memory_saved".into(),
            format!("memory {memory_id}"),
        ),
        ProcessEvent::CompactionTriggered {
            channel_id,
            threshold_reached,
            ..
        } => (
            "compaction_triggered".into(),
            format!("channel {} at {:.0}%", channel_id, threshold_reached * 100.0),
        ),
        ProcessEvent::StatusUpdate { process_id, status, .. } => (
            "status_update".into(),
            format!("{}: {}", process_id, truncate(status, 120)),
        ),
        ProcessEvent::WorkerPermission { worker_id, .. } => (
            "worker_permission".into(),
            format!("worker {} permission request", worker_id),
        ),
        ProcessEvent::WorkerQuestion { worker_id, .. } => (
            "worker_question".into(),
            format!("worker {} question", worker_id),
        ),
    }
}

/// Owner-only filtering: skip events from non-owner user messages and all
/// downstream events (workers, branches, tools) spawned from those messages.
///
/// Keyed on `trace_id`: when a `UserMessage` from a non-owner is seen, its
/// trace_id is recorded. Any subsequent event carrying that trace_id is also
/// skipped. Events without a trace_id (system/timer events) are always processed.
fn should_process_event(
    config: &LearningConfig,
    non_owner_traces: &mut HashSet<String>,
    event: &ProcessEvent,
) -> bool {
    // If no owner filter is configured, process everything.
    if config.owner_user_ids.is_empty() {
        return true;
    }

    match event {
        ProcessEvent::UserMessage {
            sender_id,
            platform,
            trace_id,
            ..
        } => {
            let key = format!("{platform}:{sender_id}");
            let is_owner = config.owner_user_ids.contains(&key);
            if !is_owner {
                non_owner_traces.insert(trace_id.clone());
            }
            is_owner
        }
        ProcessEvent::UserReaction {
            sender_id,
            platform,
            trace_id,
            ..
        } => {
            let key = format!("{platform}:{sender_id}");
            let is_owner = config.owner_user_ids.contains(&key);
            if !is_owner {
                non_owner_traces.insert(trace_id.clone());
            }
            is_owner
        }
        // Any downstream event that carries a trace_id inherits the filter decision.
        _ => {
            if let Some(trace_id) = extract_trace_id(event) {
                !non_owner_traces.contains(trace_id)
            } else {
                // No trace_id means system/global event — always process.
                true
            }
        }
    }
}

/// Extract trace_id from any ProcessEvent variant that carries one.
fn extract_trace_id(event: &ProcessEvent) -> Option<&str> {
    match event {
        ProcessEvent::WorkerStarted { trace_id, .. }
        | ProcessEvent::WorkerStatus { trace_id, .. }
        | ProcessEvent::WorkerComplete { trace_id, .. }
        | ProcessEvent::BranchStarted { trace_id, .. }
        | ProcessEvent::BranchResult { trace_id, .. }
        | ProcessEvent::ToolStarted { trace_id, .. }
        | ProcessEvent::ToolCompleted { trace_id, .. } => trace_id.as_deref(),
        ProcessEvent::UserMessage { trace_id, .. }
        | ProcessEvent::UserReaction { trace_id, .. } => Some(trace_id.as_str()),
        _ => None,
    }
}

/// Truncate a string to at most `max` bytes on a char boundary.
fn truncate(value: &str, max: usize) -> &str {
    if value.len() <= max {
        value
    } else {
        let end = value.floor_char_boundary(max);
        &value[..end]
    }
}
