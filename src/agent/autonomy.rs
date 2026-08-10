//! The autonomy channel: the agent's process for self-directed work.
//!
//! One channel wakes on a configured interval (or when wake events are
//! pending), surveys task state, enriches and proposes work according to the
//! configured [`AutonomyLevel`], executes user-approved tasks at level `act`,
//! records a run summary via `autonomy_complete`, and exits. See
//! `docs/design-docs/autonomy.md` and `docs/design-docs/wakes.md`.

use crate::agent::channel::{Channel, ChannelKind};
use crate::config::{AutonomyConfig, AutonomyLevel};
use crate::conversation::settings::{DelegationMode, ResolvedConversationSettings};
use crate::prompts::engine::{AutonomyRunHistoryView, AutonomyWakeEventView};
use crate::tasks::{Task, TaskListFilter, TaskStatus};
use crate::wakes::{AutonomyRunStatus, AutonomyRunStore};
use crate::{AgentDeps, InboundMessage, MessageContent, RoutedResponse};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Conversation id (and channel id) for the autonomy channel. One per agent.
pub const AUTONOMY_CONVERSATION_ID: &str = "autonomy";

/// Retention window for consumed wake events, pruned after each run.
const WAKE_EVENT_RETENTION_DAYS: u32 = 30;

/// Maximum pending wake events pulled into a single run's context.
const WAKE_EVENT_BATCH_LIMIT: i64 = 200;

/// Grace period after the hard-timeout wrap-up message before aborting.
const HARD_TIMEOUT_GRACE_SECS: u64 = 60;

/// Retry budget for the completion contract — the same budget the
/// memory-persistence contract uses.
pub const AUTONOMY_CONTRACT_MAX_RETRIES: usize =
    crate::hooks::SpacebotHook::MEMORY_PERSISTENCE_CONTRACT_MAX_RETRIES;

/// Fallback summary recorded when a run ends without calling `autonomy_complete`.
pub const AUTONOMY_FALLBACK_SUMMARY: &str = "run ended without summary";

/// The run's allowance of distinct tasks, enforced at the tool boundary.
///
/// `max_tasks_per_run` used to live only in the system prompt, where it was a
/// request rather than a limit. Spending the allowance per task — not per
/// comment — lets a run keep working something it already started while still
/// refusing to open a new one.
#[derive(Debug, Clone)]
pub struct EnrichmentBudget {
    max_tasks: u32,
    touched: Arc<std::sync::Mutex<std::collections::HashSet<i64>>>,
}

impl EnrichmentBudget {
    pub fn new(max_tasks: u32) -> Self {
        Self {
            max_tasks: max_tasks.max(1),
            touched: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub fn max_tasks(&self) -> u32 {
        self.max_tasks
    }

    /// Reserve this run's allowance for `task_number`. Returns false only when
    /// the allowance is spent and this is a task the run has not touched.
    pub fn try_touch(&self, task_number: i64) -> bool {
        let mut touched = self
            .touched
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if touched.contains(&task_number) {
            return true;
        }
        if touched.len() >= self.max_tasks as usize {
            return false;
        }
        touched.insert(task_number);
        true
    }

    pub fn touched_count(&self) -> usize {
        self.touched
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

/// Shared state between the run driver, the channel, and the
/// `autonomy_complete` tool for a single autonomy run.
#[derive(Debug, Clone)]
pub struct AutonomyRunHandle {
    pub run_id: String,
    pub store: Arc<AutonomyRunStore>,
    pub budget: EnrichmentBudget,
    completed: Arc<AtomicBool>,
}

impl AutonomyRunHandle {
    pub fn new(run_id: String, store: Arc<AutonomyRunStore>, max_tasks_per_run: u32) -> Self {
        Self {
            run_id,
            store,
            budget: EnrichmentBudget::new(max_tasks_per_run),
            completed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Record that `autonomy_complete` was called for this run.
    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Release);
    }

    pub fn completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }
}

/// Whether the cortex may pick up `ready` tasks for execution.
///
/// Execution without a user present is Act-only: `observe` surveys, `suggest`
/// enriches and proposes, but only `act` runs approved work. `off` disables
/// autonomous pickup entirely.
pub fn ready_pickup_allowed(level: AutonomyLevel) -> bool {
    level == AutonomyLevel::Act
}

/// Whether an autonomy run is due right now.
///
/// Pure over its inputs so the decision is unit-testable: a run is due when
/// the level is on, the current hour is inside the active window, and either
/// unconsumed wake events are pending or the interval has elapsed since the
/// last run (a never-run agent is immediately due).
pub fn autonomy_run_due(
    level: AutonomyLevel,
    now: chrono::DateTime<chrono::Utc>,
    last_run_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pending_wake_events: i64,
    active_hours: Option<(u8, u8)>,
    current_hour: u8,
    interval_secs: u64,
) -> bool {
    if level == AutonomyLevel::Off {
        return false;
    }
    if let Some((start, end)) = active_hours
        && !crate::cron::scheduler::hour_in_active_window(current_hour, start, end)
    {
        return false;
    }
    if pending_wake_events > 0 {
        return true;
    }
    match last_run_started_at {
        None => true,
        Some(last) => {
            now.signed_duration_since(last) >= chrono::Duration::seconds(interval_secs as i64)
        }
    }
}

/// Whether a task belongs in this agent's autonomy context.
///
/// Assigned tasks are visible only to their assignee; unassigned tasks are
/// visible only when the agent claims unowned work.
pub fn task_visible_to_agent(task: &Task, agent_id: &str, claim_unowned: bool) -> bool {
    match task.assigned_agent_id.as_deref() {
        Some(assigned) => assigned == agent_id,
        None => claim_unowned,
    }
}

/// Cortex-tick check: start an autonomy run when one is due.
///
/// The run row is inserted before the run task is spawned so the next tick's
/// `has_active_run` guard sees it — the cortex tick is serial, which makes
/// this the single-flight gate.
pub async fn maybe_run_autonomy(deps: &AgentDeps) {
    let config = **deps.runtime_config.autonomy.load();
    // The instance ceiling caps the per-agent dial without overwriting it:
    // the run executes at the intersection of the two levels.
    let config = AutonomyConfig {
        level: config.level.min(**deps.autonomy_ceiling.load()),
        ..config
    };
    if config.level == AutonomyLevel::Off {
        return;
    }

    let stale_after_secs = config.timeout_secs.saturating_mul(2).max(60);
    match deps
        .autonomy_run_store
        .has_active_run(stale_after_secs)
        .await
    {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to check for active autonomy run");
            return;
        }
    }

    let last_run_started_at = match deps.autonomy_run_store.last_run_started_at().await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to read last autonomy run start");
            return;
        }
    };
    let pending_wake_events = match deps.wake_event_store.pending_count().await {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(%error, "failed to count pending wake events");
            return;
        }
    };
    let (current_hour, _timezone) =
        crate::cron::scheduler::current_hour_and_timezone(&deps.runtime_config);

    if !autonomy_run_due(
        config.level,
        chrono::Utc::now(),
        last_run_started_at,
        pending_wake_events,
        config.active_hours,
        current_hour,
        config.interval_secs,
    ) {
        return;
    }

    let run_id = match deps.autonomy_run_store.begin_run().await {
        Ok(run_id) => run_id,
        Err(error) => {
            tracing::warn!(%error, "failed to begin autonomy run");
            return;
        }
    };

    tracing::info!(
        agent_id = %deps.agent_id,
        run_id = %run_id,
        level = %config.level,
        pending_wake_events,
        "starting autonomy run"
    );

    let deps = deps.clone();
    tokio::spawn(async move {
        if let Err(error) = run_autonomy_channel(&deps, run_id.clone(), config).await {
            tracing::error!(%error, run_id = %run_id, "autonomy run failed");
            if let Err(finish_error) = deps
                .autonomy_run_store
                .finish_run_status(
                    &run_id,
                    AutonomyRunStatus::Failed,
                    Some(&format!("run failed: {error}")),
                )
                .await
            {
                tracing::warn!(%finish_error, run_id = %run_id, "failed to record autonomy run failure");
            }
        }
    });
}

/// Execute a single autonomy run: consume pending wake events, assemble the
/// run briefing, drive the channel to completion under the soft/hard timeout,
/// and record the outcome.
pub async fn run_autonomy_channel(
    deps: &AgentDeps,
    run_id: String,
    config: AutonomyConfig,
) -> anyhow::Result<()> {
    // Consume pending wake events at run start. A crash after this point does
    // not replay events — crash semantics for tasks are handled by task
    // status, not event replay.
    let pending_events = deps
        .wake_event_store
        .pending(WAKE_EVENT_BATCH_LIMIT)
        .await?;
    let event_ids: Vec<String> = pending_events
        .iter()
        .map(|event| event.id.clone())
        .collect();
    if !event_ids.is_empty() {
        deps.wake_event_store.consume(&event_ids, &run_id).await?;
        deps.autonomy_run_store
            .set_wake_events(&run_id, &event_ids)
            .await?;
    }

    let briefing = build_run_briefing(deps, &config, &pending_events).await?;

    // Timeout prompts are rendered before the channel spawns so a template
    // failure surfaces immediately instead of mid-run. Config validation
    // guarantees warn < timeout.
    let remaining_secs = config.timeout_secs.saturating_sub(config.warn_secs).max(1);
    let (soft_warning_prompt, hard_timeout_prompt) = {
        let prompt_engine = deps.runtime_config.prompts.load();
        let soft = prompt_engine
            .render_system_autonomy_soft_warning(remaining_secs.div_ceil(60).max(1))
            .map_err(|error| anyhow::anyhow!("failed to render autonomy soft warning: {error}"))?;
        let hard = prompt_engine
            .render_system_autonomy_hard_timeout()
            .map_err(|error| anyhow::anyhow!("failed to render autonomy hard timeout: {error}"))?;
        (soft, hard)
    };

    let channel_id: crate::ChannelId = Arc::from(AUTONOMY_CONVERSATION_ID);
    let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<RoutedResponse>(32);
    // The autonomy channel has no delivery target — drain and drop anything
    // the channel tries to send so the bounded channel never backs up.
    tokio::spawn(async move { while response_rx.recv().await.is_some() {} });
    let event_rx = deps.event_tx.subscribe();

    // Direct tool access: the autonomy channel does not branch — it has no
    // user-facing context to protect, so it uses memory/execution tools
    // directly.
    let resolved_settings = ResolvedConversationSettings {
        delegation: DelegationMode::Direct,
        ..ResolvedConversationSettings::default()
    };

    let handle = AutonomyRunHandle::new(
        run_id.clone(),
        deps.autonomy_run_store.clone(),
        config.max_tasks_per_run,
    );

    let screenshot_dir = deps
        .runtime_config
        .workspace_dir
        .join(".spacebot")
        .join("screenshots");
    let logs_dir = deps
        .runtime_config
        .workspace_dir
        .join(".spacebot")
        .join("logs");

    let (channel, channel_tx) = Channel::new(
        channel_id.clone(),
        ChannelKind::Autonomy,
        deps.clone(),
        response_tx,
        event_rx,
        screenshot_dir,
        logs_dir,
        None, // autonomy channels don't capture prompt snapshots
        None, // autonomy channels don't share live transcript cache
        resolved_settings,
        None, // no cron outcome — delivery is not a concept here
        Some(handle.clone()),
    );

    let mut channel_handle = tokio::spawn(channel.run());

    let message = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "autonomy".into(),
        adapter: None,
        conversation_id: AUTONOMY_CONVERSATION_ID.to_string(),
        sender_id: "system".into(),
        agent_id: Some(deps.agent_id.clone()),
        content: MessageContent::Text(briefing),
        timestamp: chrono::Utc::now(),
        metadata: HashMap::new(),
        formatted_author: None,
    };

    if let Err(error) = channel_tx.send(message).await {
        channel_handle.abort();
        anyhow::bail!("failed to send autonomy briefing to channel: {error}");
    }

    // Soft warning at warn_secs, hard timeout at timeout_secs.
    let warn_after = Duration::from_secs(config.warn_secs.min(config.timeout_secs).max(1));
    let mut timed_out = false;
    let first_phase = tokio::time::timeout(warn_after, &mut channel_handle).await;
    let join_result = match first_phase {
        Ok(join_result) => Some(join_result),
        Err(_elapsed) => {
            tracing::info!(
                run_id = %run_id,
                remaining_secs,
                "autonomy run reached soft warning, injecting wrap-up notice"
            );
            if channel_tx
                .send(system_message(deps, soft_warning_prompt))
                .await
                .is_err()
            {
                tracing::debug!(
                    run_id = %run_id,
                    "soft warning not delivered; autonomy channel already exited"
                );
            }

            match tokio::time::timeout(Duration::from_secs(remaining_secs), &mut channel_handle)
                .await
            {
                Ok(join_result) => Some(join_result),
                Err(_elapsed) => {
                    // Hard timeout: give the LLM one direct turn to record the
                    // run, mirroring the cron wrap-up pattern.
                    timed_out = true;
                    tracing::warn!(run_id = %run_id, "autonomy run hit hard timeout, sending wrap-up prompt");
                    channel_tx
                        .send(system_message(deps, hard_timeout_prompt))
                        .await
                        .ok();
                    drop(channel_tx);

                    let grace = Duration::from_secs(HARD_TIMEOUT_GRACE_SECS);
                    match tokio::time::timeout(grace, &mut channel_handle).await {
                        Ok(join_result) => Some(join_result),
                        Err(_elapsed) => {
                            channel_handle.abort();
                            if let Err(join_error) = (&mut channel_handle).await
                                && !join_error.is_cancelled()
                            {
                                tracing::warn!(
                                    run_id = %run_id,
                                    %join_error,
                                    "autonomy channel task failed after abort"
                                );
                            }
                            None
                        }
                    }
                }
            }
        }
    };

    let channel_failed = match join_result {
        Some(Ok(Ok(()))) => None,
        Some(Ok(Err(error))) => Some(format!("autonomy channel failed: {error}")),
        Some(Err(join_error)) => Some(format!("autonomy channel join failed: {join_error}")),
        None => None,
    };

    // Record the run outcome if autonomy_complete didn't already.
    if !handle.completed() {
        if let Some(failure) = &channel_failed {
            deps.autonomy_run_store
                .finish_run_status(&run_id, AutonomyRunStatus::Failed, Some(failure))
                .await?;
        } else if timed_out {
            deps.autonomy_run_store
                .finish_run_status(
                    &run_id,
                    AutonomyRunStatus::Timeout,
                    Some(AUTONOMY_FALLBACK_SUMMARY),
                )
                .await?;
        } else {
            // The channel-side contract retries were exhausted without a
            // completion call — record the run with a synthesized summary.
            deps.autonomy_run_store
                .complete_run(&run_id, AUTONOMY_FALLBACK_SUMMARY, &[])
                .await?;
        }
    }

    // Goals whose linked work all landed during this run get flagged for the
    // user. The marker is deterministic rather than prompt-driven, and it
    // never changes goal status — closing a goal stays the user's call.
    match crate::goals::mark_goals_ready_for_review(&deps.goal_store).await {
        Ok(marked) => {
            for goal in marked {
                tracing::info!(goal_id = %goal.id, title = %goal.title, "goal flagged ready for review");
                deps.working_memory
                    .emit(
                        crate::memory::WorkingMemoryEventType::Outcome,
                        format!(
                            "Goal ready for review: {} — all linked tasks complete",
                            goal.title
                        ),
                    )
                    .importance(0.6)
                    .record();
                crate::wakes::emit_system_event(
                    deps,
                    crate::wakes::SystemEvent::GoalUpdated,
                    &format!("goal:{}", goal.id),
                    &serde_json::json!({
                        "goal_id": goal.id,
                        "title": goal.title,
                        "status": goal.status.to_string(),
                        "ready_for_review": true,
                    }),
                )
                .await;
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to flag goals ready for review");
        }
    }

    // Wake the ready-task pickup so side-effect tasks created during the run
    // (delegations, follow-ups) are noticed promptly. Fired after the channel
    // exits so the one-shot wake actually finds the rows.
    if let Some(wake_tx) = deps.wake_tx.as_ref() {
        crate::agent::wake::fire_wake(wake_tx, &deps.agent_id);
    }

    if let Err(error) = deps
        .wake_event_store
        .prune_consumed(WAKE_EVENT_RETENTION_DAYS)
        .await
    {
        tracing::warn!(%error, "failed to prune consumed wake events");
    }

    if let Some(failure) = channel_failed {
        anyhow::bail!(failure);
    }

    tracing::info!(run_id = %run_id, timed_out, completed = handle.completed(), "autonomy run finished");
    Ok(())
}

fn system_message(deps: &AgentDeps, text: String) -> InboundMessage {
    InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        source: "system".into(),
        adapter: None,
        conversation_id: AUTONOMY_CONVERSATION_ID.to_string(),
        sender_id: "system".into(),
        agent_id: Some(deps.agent_id.clone()),
        content: MessageContent::Text(text),
        timestamp: chrono::Utc::now(),
        metadata: HashMap::new(),
        formatted_author: None,
    }
}

/// Assemble the run briefing rendered from `autonomy_channel.md.j2`.
async fn build_run_briefing(
    deps: &AgentDeps,
    config: &AutonomyConfig,
    wake_events: &[crate::wakes::WakeEvent],
) -> anyhow::Result<String> {
    let agent_name = deps
        .agent_names
        .get(deps.agent_id.as_ref())
        .cloned()
        .unwrap_or_else(|| deps.agent_id.to_string());

    // Wake definitions supply each event's name and instructions.
    // Instructions apply only within the wake's min_level; events from a wake
    // above the current level are rendered as observations. An event whose
    // definition is gone falls back to its wake id.
    let wake_defs: HashMap<String, crate::wakes::WakeDef> = if wake_events.is_empty() {
        HashMap::new()
    } else {
        deps.wake_def_store
            .list()
            .await?
            .into_iter()
            .map(|def| (def.id.clone(), def))
            .collect()
    };

    let wake_event_views: Vec<AutonomyWakeEventView> = wake_events
        .iter()
        .map(|event| {
            let def = wake_defs.get(&event.wake_id);
            AutonomyWakeEventView {
                wake_id: event.wake_id.clone(),
                name: def
                    .map(|def| def.name.clone())
                    .unwrap_or_else(|| event.wake_id.clone()),
                instructions: def
                    .filter(|def| def.min_level <= config.level)
                    .map(|def| def.instructions.clone()),
                gated: def.is_some_and(|def| def.min_level > config.level),
                fired_at: event.fired_at.clone(),
                delivery_count: event.delivery_count,
                payload: compact_payload(&event.payload),
            }
        })
        .collect();

    // This run's own row already exists, so "the previous run" is the newest
    // one that is no longer running.
    let previous_runs: Vec<crate::wakes::AutonomyRun> = deps
        .autonomy_run_store
        .recent(config.run_history_count.max(1))
        .await?
        .into_iter()
        .filter(|run| run.status != AutonomyRunStatus::Running)
        .collect();
    let last_run_started_at = previous_runs
        .first()
        .and_then(|run| crate::wakes::parse_run_timestamp(&run.started_at));

    let run_history_views: Vec<AutonomyRunHistoryView> = previous_runs
        .into_iter()
        .map(|run| AutonomyRunHistoryView {
            started_at: run.started_at,
            status: run.status.as_str().to_string(),
            summary: run
                .summary
                .unwrap_or_else(|| "no summary recorded".to_string()),
            woken_by: run.wake_event_ids.len(),
        })
        .collect();

    let task_state = render_task_state(deps, config.claim_unowned).await?;
    let enrichment_queue = render_enrichment_queue(deps, config, last_run_started_at).await?;
    let active_goals = crate::goals::render_active_goals_extended(&deps.goal_store).await?;
    let active_workers = render_active_workers(deps).await?;

    let prompt_engine = deps.runtime_config.prompts.load();
    prompt_engine
        .render_autonomy_channel_prompt(
            &agent_name,
            config.level.as_str(),
            wake_event_views,
            run_history_views,
            &task_state,
            enrichment_queue.as_deref(),
            (!active_goals.is_empty()).then_some(active_goals.as_str()),
            active_workers.as_deref(),
            config.max_tasks_per_run,
            config.warn_secs.div_ceil(60).max(1),
            config.claim_unowned,
        )
        .map_err(|error| anyhow::anyhow!("failed to render autonomy channel prompt: {error}"))
}

/// Statuses the enrichment loop reasons about. `ready` and `in_progress` are
/// execution states, handled by the ready-task pickup rather than enrichment.
const ENRICHABLE_STATUSES: [TaskStatus; 2] = [TaskStatus::PendingApproval, TaskStatus::Backlog];

/// Comments rendered per task in the enrichment queue.
const QUEUE_COMMENT_LIMIT: i64 = 6;

/// Byte cap on a single rendered comment body.
const QUEUE_COMMENT_BYTES: usize = 400;

/// Staleness horizon: tasks untouched for this many intervals are back in play.
const STALE_INTERVAL_MULTIPLIER: i64 = 3;

/// Render the ordered enrichment selection for this run.
///
/// This is the deterministic half of task selection — the channel still
/// reasons about which of these are worth its time, but the ordering, the
/// staleness horizon, and the previous-run exclusion are settled in SQL
/// before the model ever sees them.
async fn render_enrichment_queue(
    deps: &AgentDeps,
    config: &AutonomyConfig,
    last_run_started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> anyhow::Result<Option<String>> {
    let now = chrono::Utc::now();
    let stale_after = chrono::Duration::seconds(
        (config.interval_secs as i64).saturating_mul(STALE_INTERVAL_MULTIPLIER),
    );
    let candidates = deps
        .task_store
        .select_for_enrichment(crate::tasks::EnrichmentSelection {
            agent_id: &deps.agent_id,
            claim_unowned: config.claim_unowned,
            statuses: &ENRICHABLE_STATUSES,
            last_run_started_at: last_run_started_at.map(crate::tasks::sqlite_timestamp),
            stale_before: crate::tasks::sqlite_timestamp(now - stale_after),
            // One extra so the channel can see there was more to choose from
            // than it is allowed to take.
            limit: i64::from(config.max_tasks_per_run) + 1,
        })
        .await?;

    if candidates.is_empty() {
        return Ok(None);
    }

    let mut output = String::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let task = &candidate.task;
        output.push_str(&format!(
            "{}. #{} [{}] {} — {}\n",
            index + 1,
            task.task_number,
            task.priority.as_str(),
            task.title,
            candidate.reason
        ));

        let comments = deps
            .task_store
            .recent_comments(task.task_number, QUEUE_COMMENT_LIMIT)
            .await?;
        for comment in comments {
            let author = match comment.author_id.as_deref() {
                Some(author_id) => format!("{} {}", comment.author_type, author_id),
                None => comment.author_type.to_string(),
            };
            output.push_str(&format!(
                "   - [{} {}] {}\n",
                author,
                comment.created_at,
                crate::tools::truncate_utf8_ellipsis(
                    &comment
                        .body
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    QUEUE_COMMENT_BYTES,
                )
            ));
        }
    }

    Ok(Some(output))
}

/// One-line JSON payload preview, truncated for prompt hygiene.
fn compact_payload(payload: &serde_json::Value) -> String {
    if payload.as_object().is_some_and(serde_json::Map::is_empty) {
        return String::new();
    }
    crate::tools::truncate_utf8_ellipsis(&payload.to_string(), 400)
}

/// Render the full task survey: pending_approval, ready, in_progress, backlog.
async fn render_task_state(deps: &AgentDeps, claim_unowned: bool) -> anyhow::Result<String> {
    let sections: [(TaskStatus, &str); 4] = [
        (
            TaskStatus::PendingApproval,
            "Pending approval (enrich these; never execute them)",
        ),
        (TaskStatus::Ready, "Ready (user-approved)"),
        (TaskStatus::InProgress, "In progress"),
        (TaskStatus::Backlog, "Backlog"),
    ];

    let comment_counts = deps.task_store.comment_counts().await?;

    let mut output = String::new();
    let mut any = false;
    for (status, label) in sections {
        let tasks = deps
            .task_store
            .list(TaskListFilter {
                status: Some(status),
                limit: Some(200),
                ..Default::default()
            })
            .await?;
        let visible: Vec<Task> = tasks
            .into_iter()
            .filter(|task| task_visible_to_agent(task, &deps.agent_id, claim_unowned))
            .collect();
        if visible.is_empty() {
            continue;
        }

        any = true;
        output.push_str(&format!("### {label}\n"));
        for task in visible {
            let comments = comment_counts.get(&task.id).copied().unwrap_or(0);
            output.push_str(&render_task_line(&task, &deps.agent_id, comments));
        }
        output.push('\n');
    }

    if !any {
        output.push_str("No active tasks.\n");
    }
    Ok(output)
}

fn render_task_line(task: &Task, agent_id: &str, comment_count: i64) -> String {
    let ownership = match task.assigned_agent_id.as_deref() {
        Some(assigned) if assigned == agent_id => String::new(),
        Some(assigned) => format!(" (assigned to {assigned})"),
        None => " (unowned)".to_string(),
    };
    let mut line = format!(
        "- #{} [{}] {}{}",
        task.task_number,
        task.priority.as_str(),
        task.title,
        ownership
    );
    if comment_count > 0 {
        line.push_str(&format!(
            " ({comment_count} comment{})",
            if comment_count == 1 { "" } else { "s" }
        ));
    }
    if let Some(enriched_at) = &task.last_enriched_at {
        line.push_str(&format!(" (last enriched {enriched_at})"));
    }
    if let Some(description) = task
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        line.push_str(" — ");
        line.push_str(&crate::tools::truncate_utf8_ellipsis(
            &description.split_whitespace().collect::<Vec<_>>().join(" "),
            300,
        ));
    }
    line.push('\n');
    line
}

/// Render currently running workers so the run doesn't duplicate in-flight work.
async fn render_active_workers(deps: &AgentDeps) -> anyhow::Result<Option<String>> {
    let logger = crate::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone());
    let (workers, _total) = logger
        .list_worker_runs(&deps.agent_id, 20, 0, Some("running"))
        .await?;
    if workers.is_empty() {
        return Ok(None);
    }

    let mut output = String::new();
    for worker in workers {
        let task_line = crate::summarize_first_non_empty_line(&worker.task, 160);
        output.push_str(&format!(
            "- {} [{}] {}\n",
            worker.id, worker.worker_type, task_line
        ));
    }
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn utc(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn due_requires_level_on() {
        assert!(!autonomy_run_due(
            AutonomyLevel::Off,
            utc(10_000),
            None,
            5,
            None,
            12,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Observe,
            utc(10_000),
            None,
            0,
            None,
            12,
            1800
        ));
    }

    #[test]
    fn due_respects_active_hours() {
        // Window 8-22: hour 3 is outside even with pending events.
        assert!(!autonomy_run_due(
            AutonomyLevel::Act,
            utc(10_000),
            None,
            5,
            Some((8, 22)),
            3,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            utc(10_000),
            None,
            5,
            Some((8, 22)),
            9,
            1800
        ));
        // Midnight-wrapping window 22-6: hour 23 is inside.
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            utc(10_000),
            None,
            0,
            Some((22, 6)),
            23,
            1800
        ));
    }

    #[test]
    fn pending_wake_events_pull_the_run_forward() {
        let now = utc(10_000);
        let recent_run = Some(utc(9_900)); // 100s ago, interval 1800s
        assert!(!autonomy_run_due(
            AutonomyLevel::Suggest,
            now,
            recent_run,
            0,
            None,
            12,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Suggest,
            now,
            recent_run,
            1,
            None,
            12,
            1800
        ));
    }

    #[test]
    fn interval_elapse_makes_the_run_due() {
        let now = utc(10_000);
        assert!(!autonomy_run_due(
            AutonomyLevel::Act,
            now,
            Some(utc(10_000 - 1799)),
            0,
            None,
            12,
            1800
        ));
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            now,
            Some(utc(10_000 - 1800)),
            0,
            None,
            12,
            1800
        ));
        // Never ran before: immediately due.
        assert!(autonomy_run_due(
            AutonomyLevel::Act,
            now,
            None,
            0,
            None,
            12,
            1800
        ));
    }

    #[test]
    fn ready_pickup_is_act_only() {
        assert!(!ready_pickup_allowed(AutonomyLevel::Off));
        assert!(!ready_pickup_allowed(AutonomyLevel::Observe));
        assert!(!ready_pickup_allowed(AutonomyLevel::Suggest));
        assert!(ready_pickup_allowed(AutonomyLevel::Act));
    }

    fn task_with_assignment(assigned: Option<&str>) -> Task {
        Task {
            id: "task-1".to_string(),
            task_number: 1,
            title: "test".to_string(),
            description: None,
            status: TaskStatus::Ready,
            priority: crate::tasks::TaskPriority::Medium,
            owner_agent_id: "owner".to_string(),
            assigned_agent_id: assigned.map(str::to_string),
            subtasks: Vec::new(),
            metadata: serde_json::json!({}),
            goal_id: None,
            source_memory_id: None,
            worker_id: None,
            created_by: "user".to_string(),
            approved_at: None,
            approved_by: None,
            last_enriched_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
        }
    }

    #[test]
    fn task_visibility_follows_assignment_and_claim_flag() {
        let mine = task_with_assignment(Some("agent-a"));
        let theirs = task_with_assignment(Some("agent-b"));
        let unowned = task_with_assignment(None);

        assert!(task_visible_to_agent(&mine, "agent-a", false));
        assert!(!task_visible_to_agent(&theirs, "agent-a", true));
        assert!(task_visible_to_agent(&unowned, "agent-a", true));
        assert!(!task_visible_to_agent(&unowned, "agent-a", false));
    }
}
