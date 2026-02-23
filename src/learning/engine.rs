//! LearningEngine: async event loop coordinator for the learning system.
//!
//! Subscribes to the `ProcessEvent` broadcast channel and routes events through
//! Layer 1 (outcome tracking, step envelopes, distillation) and Layer 2
//! (cognitive signals, Meta-Ralph, insights, contradiction detection,
//! auto-promotion). All processing is fail-open: errors are logged and
//! swallowed to avoid disrupting the hot path.

use super::config::LearningConfig;
use super::feedback::FeedbackTracker;
use super::outcome::EpisodeTracker;
use super::patches::ErrorCounter;
use super::ralph;
use super::retriever::DistillationCache;
use super::store::LearningStore;
use crate::{AgentDeps, ProcessEvent};

use tokio::sync::broadcast;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Spawn the learning engine as a background task.
///
/// Follows the `spawn_bulletin_loop` / `spawn_association_loop` pattern from
/// the cortex. The engine subscribes to the process event bus, filters for
/// owner-only traces, and routes events through the full learning pipeline.
pub fn spawn_learning_loop(
    deps: AgentDeps,
    learning_store: Arc<LearningStore>,
    tuneables: Arc<super::tuneables::TuneableStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_learning_loop(&deps, &learning_store, tuneables).await {
            tracing::error!(%error, "learning loop exited with error");
        }
    })
}

// ---------------------------------------------------------------------------
// Engine state
// ---------------------------------------------------------------------------

/// All mutable state for the learning engine, kept together so the event loop
/// can pass a single reference around.
struct EngineState {
    // M1: outcome tracking
    episode_tracker: EpisodeTracker,
    feedback_tracker: FeedbackTracker,
    error_counter: ErrorCounter,
    distillation_cache: DistillationCache,
    ralph_hashes: HashSet<String>,
    policy_patches: Vec<super::patches::PolicyPatch>,
    non_owner_traces: HashSet<String>,
    last_batch_run: Instant,
    last_stale_cleanup: Instant,
    last_promotion_check: Instant,
    // M3: subsystems
    phase_state: super::phase::PhaseState,
    control_plane: super::control::ControlPlane,
    learning_state: super::control::LearningState,
    evidence_store: super::evidence::EvidenceStore,
    escape_protocol: super::escape::EscapeProtocol,
    cooldown_manager: super::cooldowns::CooldownManager,
    chip_runtime: super::chips::runtime::ChipRuntime,
    tuneables: Arc<super::tuneables::TuneableStore>,
    packet_store: super::packets::PacketStore,
    quarantine: super::quarantine::QuarantineStore,
    truth_ledger: super::truth::TruthLedger,
    last_evidence_cleanup: Instant,
    last_truth_stale_check: Instant,
    last_tuner_run: Instant,
}

impl EngineState {
    fn new(
        ralph_hashes: HashSet<String>,
        policy_patches: Vec<super::patches::PolicyPatch>,
        store: Arc<LearningStore>,
        tuneables: Arc<super::tuneables::TuneableStore>,
    ) -> Self {
        let now = Instant::now();
        Self {
            episode_tracker: EpisodeTracker::new(),
            feedback_tracker: FeedbackTracker::new(300), // 5-min TTL
            error_counter: ErrorCounter::new(),
            distillation_cache: DistillationCache::new(),
            ralph_hashes,
            policy_patches,
            non_owner_traces: HashSet::new(),
            last_batch_run: now,
            last_stale_cleanup: now,
            last_promotion_check: now,
            phase_state: super::phase::PhaseState::new(),
            control_plane: super::control::ControlPlane::new(),
            learning_state: super::control::LearningState::default(),
            evidence_store: super::evidence::EvidenceStore::new(store.clone()),
            escape_protocol: super::escape::EscapeProtocol::new(),
            cooldown_manager: super::cooldowns::CooldownManager::new(),
            chip_runtime: super::chips::runtime::ChipRuntime::new(store.clone()),
            tuneables,
            packet_store: super::packets::PacketStore::new(store.clone()),
            quarantine: super::quarantine::QuarantineStore::new(store.clone()),
            truth_ledger: super::truth::TruthLedger::new(store),
            last_evidence_cleanup: now,
            last_truth_stale_check: now,
            last_tuner_run: now,
        }
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

async fn run_learning_loop(
    deps: &AgentDeps,
    store: &Arc<LearningStore>,
    tuneables: Arc<super::tuneables::TuneableStore>,
) -> anyhow::Result<()> {
    let mut event_rx = deps.event_tx.subscribe();

    tracing::info!(agent_id = %deps.agent_id, "learning engine started");

    // One-time init: ensure default policy patches exist.
    if let Err(error) = super::patches::ensure_defaults(store).await {
        tracing::warn!(%error, "failed to ensure default policy patches");
    }

    // Load initial state.
    let ralph_hashes = ralph::load_existing_hashes(store).await.unwrap_or_default();
    let policy_patches = super::patches::load_enabled(store).await.unwrap_or_default();
    let mut state = EngineState::new(ralph_hashes, policy_patches, store.clone(), tuneables);

    // Load domain chips from the chips/ directory. Fail-open: missing or
    // partially-broken chip directories are non-fatal.
    let chips_dir = std::path::Path::new("chips");
    if chips_dir.exists() {
        match state.chip_runtime.load_chips_from_dir(chips_dir) {
            Ok(count) => tracing::info!(count, "loaded domain chips"),
            Err(error) => tracing::warn!(%error, "failed to load chips directory"),
        }
    } else {
        tracing::debug!("chips/ directory not found, skipping chip loading");
    }

    // Populate tuneable defaults (no-op if values are already set).
    if let Err(error) = state.tuneables.populate_defaults().await {
        tracing::warn!(%error, "failed to populate tuneable defaults");
    }

    loop {
        let config = load_config(deps);
        if !config.enabled {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        let tick_interval = Duration::from_secs(config.tick_interval_secs);
        let mut heartbeat = tokio::time::interval(tick_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            let config = load_config(deps);
            if !config.enabled {
                break;
            }

            tokio::select! {
                _ = heartbeat.tick() => {
                    run_tick(deps, store, &mut state, &config).await;
                }
                event = event_rx.recv() => {
                    match event {
                        Ok(process_event) => {
                            if !should_process_event(&config, &mut state.non_owner_traces, &process_event) {
                                continue;
                            }
                            if let Err(error) = handle_event(deps, store, &mut state, &config, &process_event).await {
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

// ---------------------------------------------------------------------------
// Tick handlers (timers)
// ---------------------------------------------------------------------------

/// Periodic work: heartbeat, stale cleanup, batch distillation, promotion.
async fn run_tick(
    deps: &AgentDeps,
    store: &Arc<LearningStore>,
    state: &mut EngineState,
    config: &LearningConfig,
) {
    // Heartbeat
    if let Err(error) = store
        .set_state("learning_heartbeat", chrono::Utc::now().to_rfc3339())
        .await
    {
        tracing::warn!(%error, "failed to update learning heartbeat");
    }

    // Purge expired feedback
    state.feedback_tracker.purge_expired();

    // Stale episode cleanup (every stale_episode_timeout_secs / 2)
    let stale_interval = Duration::from_secs(config.stale_episode_timeout_secs / 2);
    if state.last_stale_cleanup.elapsed() >= stale_interval {
        if let Err(error) = state
            .episode_tracker
            .cleanup_stale(store, config.stale_episode_timeout_secs)
            .await
        {
            tracing::warn!(%error, "stale episode cleanup failed");
        }
        state.last_stale_cleanup = Instant::now();
    }

    // Batch distillation extraction
    let batch_interval = Duration::from_secs(config.batch_interval_secs);
    if state.last_batch_run.elapsed() >= batch_interval {
        run_batch_distillation(store, config).await;
        state.last_batch_run = Instant::now();
    }

    // Auto-promotion check
    let promotion_interval = Duration::from_secs(config.promotion_interval_secs);
    if state.last_promotion_check.elapsed() >= promotion_interval {
        run_promotion_check(deps, store, config).await;
        state.last_promotion_check = Instant::now();
    }

    // Evidence cleanup (hourly): remove expired records, skipping active episodes.
    if state.last_evidence_cleanup.elapsed() >= Duration::from_secs(3600) {
        let active_ids = state.episode_tracker.active_episode_ids();
        let active_refs: Vec<&str> = active_ids.iter().map(|s| s.as_str()).collect();
        if let Err(error) = state.evidence_store.cleanup_expired(&active_refs).await {
            tracing::warn!(%error, "evidence cleanup failed");
        }
        state.last_evidence_cleanup = Instant::now();
    }

    // Truth stale check (daily): find claims not validated in 30 days and mark them.
    if state.last_truth_stale_check.elapsed() >= Duration::from_secs(86_400) {
        match state.truth_ledger.check_stale(30).await {
            Ok(stale_ids) => {
                for stale_id in &stale_ids {
                    if let Err(error) = state.truth_ledger.mark_stale(stale_id).await {
                        tracing::warn!(%error, stale_id, "failed to mark truth entry stale");
                    }
                }
                if !stale_ids.is_empty() {
                    tracing::debug!(count = stale_ids.len(), "marked stale truth entries");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "truth stale check failed");
            }
        }
        state.last_truth_stale_check = Instant::now();
    }

    // Tuneables refresh (on tuner_interval_secs): reload cached values from DB.
    let tuner_interval_secs = state
        .tuneables
        .get_i64("tuner_interval_secs")
        .await
        .unwrap_or(3600) as u64;
    if state.last_tuner_run.elapsed() >= Duration::from_secs(tuner_interval_secs) {
        if let Err(error) = state.tuneables.refresh().await {
            tracing::warn!(%error, "tuneables refresh failed");
        }
        state.last_tuner_run = Instant::now();
    }
}

/// Extract distillations from recently completed episodes.
async fn run_batch_distillation(store: &Arc<LearningStore>, config: &LearningConfig) {
    let episode_ids =
        match EpisodeTracker::completed_episode_ids(store, config.distillation_batch_size).await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, "failed to fetch completed episodes for distillation");
                return;
            }
        };

    for episode_id in &episode_ids {
        match super::distillation::extract_from_episode(store, episode_id).await {
            Ok(distillations) => {
                tracing::debug!(
                    episode_id,
                    count = distillations.len(),
                    "extracted distillations"
                );
            }
            Err(error) => {
                tracing::warn!(%error, episode_id, "distillation extraction failed");
            }
        }
    }
}

/// Check for insights eligible for promotion to the memory graph, and demote
/// any that have fallen below the reliability threshold.
async fn run_promotion_check(deps: &AgentDeps, store: &Arc<LearningStore>, config: &LearningConfig) {
    let completed_episodes =
        EpisodeTracker::completed_episode_count(store).await.unwrap_or(0);
    let cold_factor =
        super::meta::cold_start_factor(completed_episodes, config.cold_start_episodes);

    // Promote
    match super::meta::load_promotable(store, cold_factor).await {
        Ok(insights) => {
            for insight in &insights {
                if let Err(error) = promote_insight(deps, store, insight).await {
                    tracing::warn!(%error, insight_id = %insight.id, "insight promotion failed");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to load promotable insights");
        }
    }

    // Demote
    match super::meta::load_demotable(store).await {
        Ok(insights) => {
            for insight in &insights {
                if let Err(error) = demote_insight(deps, store, insight).await {
                    tracing::warn!(%error, insight_id = %insight.id, "insight demotion failed");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to load demotable insights");
        }
    }
}

async fn promote_insight(
    deps: &AgentDeps,
    store: &LearningStore,
    insight: &super::meta::Insight,
) -> anyhow::Result<()> {
    use crate::memory::types::{Memory, MemoryType};

    let mapping = super::meta::promotion_mapping(
        insight.source_type.as_deref(),
        &insight.category,
    );

    let memory_type = match mapping.memory_type {
        "Observation" => MemoryType::Observation,
        "Decision" => MemoryType::Decision,
        "Preference" => MemoryType::Preference,
        _ => MemoryType::Observation,
    };

    let memory = Memory::new(insight.content.clone(), memory_type)
        .with_importance(mapping.importance as f32)
        .with_source("learning_engine".to_owned());

    let memory_store = deps.memory_search.store();
    memory_store.save(&memory).await?;

    // Embed in vector store for similarity search.
    match deps.memory_search.embedding_model_arc().embed_one(&memory.content).await {
        Ok(embedding) => {
            if let Err(error) = deps
                .memory_search
                .embedding_table()
                .store(&memory.id, &memory.content, &embedding)
                .await
            {
                tracing::warn!(%error, memory_id = %memory.id, "embedding store failed during promotion");
            }
        }
        Err(error) => {
            tracing::warn!(%error, memory_id = %memory.id, "embedding generation failed during promotion");
        }
    }

    super::meta::mark_promoted(store, &insight.id, &memory.id).await?;

    store.log_event(
        "insight_promoted",
        &format!(
            "insight {} → memory {} ({})",
            insight.id, memory.id, mapping.memory_type
        ),
        None,
    ).await?;

    tracing::info!(
        insight_id = %insight.id,
        memory_id = %memory.id,
        memory_type = mapping.memory_type,
        "promoted insight to memory graph"
    );
    Ok(())
}

async fn demote_insight(
    deps: &AgentDeps,
    store: &LearningStore,
    insight: &super::meta::Insight,
) -> anyhow::Result<()> {
    if let Some(memory_id) = &insight.promoted_memory_id {
        let memory_store = deps.memory_search.store();
        memory_store.forget(memory_id).await?;

        super::meta::mark_demoted(store, &insight.id).await?;

        store.log_event(
            "insight_demoted",
            &format!("insight {} demoted, memory {} removed", insight.id, memory_id),
            None,
        ).await?;

        tracing::info!(
            insight_id = %insight.id,
            memory_id = %memory_id,
            "demoted insight, removed from memory graph"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Event handler
// ---------------------------------------------------------------------------

/// Route a process event through the full learning pipeline.
///
/// Order: audit log → Layer 1 (episodes/steps) → Layer 2 (signals/insights).
async fn handle_event(
    deps: &AgentDeps,
    store: &Arc<LearningStore>,
    state: &mut EngineState,
    config: &LearningConfig,
    event: &ProcessEvent,
) -> anyhow::Result<()> {
    // Always log to audit trail (M1 behavior).
    let (event_type, summary) = summarize_event(event);
    let details = serde_json::to_value(event).ok();
    store.log_event(&event_type, &summary, details.as_ref()).await?;

    // --- Layer 1: Outcome Tracking ---
    match event {
        ProcessEvent::WorkerStarted {
            worker_id,
            task,
            channel_id,
            trace_id,
            ..
        } => {
            let worker_id_str = worker_id.to_string();
            state
                .episode_tracker
                .on_worker_started(
                    store,
                    &deps.agent_id,
                    &worker_id_str,
                    task,
                    channel_id.as_deref(),
                    trace_id.as_deref(),
                    &worker_id_str,
                )
                .await?;
        }
        ProcessEvent::WorkerComplete {
            worker_id,
            success,
            duration_secs,
            ..
        } => {
            let worker_id_str = worker_id.to_string();

            // Update error counter for policy patches.
            if *success {
                state.error_counter.record_success(&worker_id_str);
            } else {
                state.error_counter.record_error(&worker_id_str);
            }

            if let Some(_episode_id) = state
                .episode_tracker
                .on_worker_complete(store, &worker_id_str, *success, *duration_secs)
                .await?
            {
                // Resolve any pending feedback for this worker.
                state
                    .feedback_tracker
                    .resolve_outcome(store, &worker_id_str, *success)
                    .await?;
            }

            // Check policy patches.
            let actions = super::patches::evaluate_patches(
                &state.policy_patches,
                &state.error_counter,
                &worker_id_str,
                None,
                None,
            );
            for action in &actions {
                tracing::info!(
                    patch_id = %action.patch_id,
                    action_type = %action.action_type,
                    "policy patch triggered"
                );
                store.log_event(
                    "policy_patch_fired",
                    &format!("{}: {}", action.action_type, action.action_config),
                    None,
                ).await?;
            }
        }
        ProcessEvent::BranchStarted {
            branch_id,
            description,
            channel_id,
            trace_id,
            ..
        } => {
            let branch_id_str = branch_id.to_string();
            state
                .episode_tracker
                .on_branch_started(
                    store,
                    &deps.agent_id,
                    &branch_id_str,
                    description,
                    channel_id,
                    trace_id.as_deref(),
                )
                .await?;
        }
        ProcessEvent::BranchResult { branch_id, .. } => {
            let branch_id_str = branch_id.to_string();
            state
                .episode_tracker
                .on_branch_result(store, &branch_id_str)
                .await?;
        }
        ProcessEvent::ToolStarted {
            process_id,
            call_id,
            tool_name,
            args_summary,
            trace_id,
            ..
        } => {
            let process_id_str = process_id.to_string();
            state
                .episode_tracker
                .on_tool_started(
                    store,
                    &process_id_str,
                    call_id,
                    tool_name,
                    args_summary.as_ref(),
                    trace_id.as_deref(),
                )
                .await?;
        }
        ProcessEvent::ToolCompleted {
            process_id,
            call_id,
            tool_name,
            result,
            ..
        } => {
            let process_id_str = process_id.to_string();
            // Infer success from result text (ToolCompleted doesn't have a success bool).
            let success = !result.starts_with("Error") && !result.starts_with("error");
            state
                .episode_tracker
                .on_tool_completed(store, &process_id_str, call_id, tool_name, success)
                .await?;

            // Resolve implicit feedback for this call.
            state
                .feedback_tracker
                .resolve_outcome(store, call_id, success)
                .await?;
        }

        // --- Layer 2: Cognitive Signals ---
        ProcessEvent::UserMessage {
            content, ..
        } => {
            if let Some(signal) = super::signals::analyze_message(content) {
                // Run through Meta-Ralph quality gate.
                if let Some(candidate) = &signal.extracted_candidate {
                    let result = ralph::evaluate(candidate, &state.ralph_hashes);
                    let verdict_str = result.verdict.to_string();

                    // Save the ralph verdict.
                    if let Err(error) =
                        ralph::save_verdict(store, candidate, &result, Some("cognitive_signal")).await
                    {
                        tracing::warn!(%error, "failed to save ralph verdict");
                    }
                    state.ralph_hashes.insert(result.input_hash.clone());

                    // Save cognitive signal.
                    if let Err(error) =
                        super::signals::save_signal(store, &signal, Some(&verdict_str)).await
                    {
                        tracing::warn!(%error, "failed to save cognitive signal");
                    }

                    // If quality or needs_work, create an insight.
                    if result.verdict == ralph::RalphVerdict::Quality
                        || result.verdict == ralph::RalphVerdict::NeedsWork
                    {
                        let mut text = candidate.clone();

                        // One auto-refinement attempt for NeedsWork.
                        if result.verdict == ralph::RalphVerdict::NeedsWork {
                            if let Some(refined) = ralph::attempt_refinement(&text) {
                                text = refined;
                            }
                        }

                        let category = category_from_signal(&signal);
                        let insight = super::meta::Insight {
                            id: uuid::Uuid::new_v4().to_string(),
                            category: category.clone(),
                            content: text.clone(),
                            reliability: 0.5,
                            confidence: 0.3,
                            validation_count: 0,
                            contradiction_count: 0,
                            quality_score: Some(result.total_score),
                            advisory_readiness: 0.0,
                            source_type: Some("cognitive_signal".into()),
                            source_id: Some(signal.id.clone()),
                            promoted: false,
                            promoted_memory_id: None,
                        };

                        if let Err(error) = super::meta::save_insight(store, &insight).await {
                            tracing::warn!(%error, "failed to save insight from cognitive signal");
                        } else {
                            // Check for contradictions.
                            let category_str = category.to_string();
                            match super::contradiction::find_contradictions_for_insight(
                                store,
                                &insight.id,
                                &text,
                                &category_str,
                            )
                            .await
                            {
                                Ok(contradictions) => {
                                    for contradiction in &contradictions {
                                        if let Err(error) =
                                            super::contradiction::save_contradiction(
                                                store,
                                                contradiction,
                                            )
                                            .await
                                        {
                                            tracing::warn!(%error, "failed to save contradiction");
                                        }

                                        // Update contradiction counts.
                                        if contradiction.contradiction_type
                                            == super::contradiction::ContradictionType::Temporal
                                        {
                                            let _ = super::meta::increment_contradiction(
                                                store,
                                                &contradiction.insight_b_id,
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "contradiction detection failed");
                                }
                            }
                        }
                    }
                } else {
                    // No extracted candidate, still save the signal.
                    if let Err(error) =
                        super::signals::save_signal(store, &signal, None).await
                    {
                        tracing::warn!(%error, "failed to save cognitive signal");
                    }
                }
            }
        }

        // Remaining events: already logged to audit trail above.
        _ => {}
    }

    // --- Layer 3: Phase Tracking, Evidence Capture, Control Plane ---
    //
    // All processing below is fail-open: errors are logged and execution
    // continues. M1/M2 logic above is intentionally left untouched.
    match event {
        ProcessEvent::ToolStarted {
            tool_name,
            args_summary,
            ..
        } => {
            let args_str: Option<String> = args_summary.as_ref().map(|v| v.to_string());
            let task_text = state.learning_state.original_task.clone();

            state.phase_state.detect_phase(
                tool_name,
                args_str.as_deref(),
                task_text.as_deref(),
                false,
                false,
                0,
            );

            let evidence_type = super::evidence::EvidenceStore::classify_evidence(
                tool_name,
                args_str.as_deref(),
                false,
            );
            if let Err(error) = state
                .evidence_store
                .store_evidence(&evidence_type, "", None, Some(tool_name))
                .await
            {
                tracing::warn!(%error, "evidence capture failed for ToolStarted");
            }

            state.learning_state.phase = state.phase_state.current();

            let watcher_actions = state.control_plane.evaluate(&state.learning_state);
            for action in &watcher_actions {
                state.learning_state.watcher_firing_count += 1;
                state.escape_protocol.update_metrics(
                    state.learning_state.watcher_firing_count,
                    0.5,
                    state.learning_state.steps_without_new_evidence,
                    0.0,
                );
                if action.severity == super::WatcherSeverity::Block {
                    deps.runtime_config
                        .control_plane_block
                        .store(Arc::new(Some(action.message.clone())));
                }
            }
        }

        ProcessEvent::ToolCompleted {
            tool_name,
            args_summary,
            result,
            ..
        } => {
            let success = !result.starts_with("Error") && !result.starts_with("error");
            let is_failure = !success;
            let args_str: Option<String> = args_summary.as_ref().map(|v| v.to_string());
            let task_text = state.learning_state.original_task.clone();

            state.phase_state.detect_phase(
                tool_name,
                args_str.as_deref(),
                task_text.as_deref(),
                is_failure,
                false,
                0,
            );

            let evidence_type = super::evidence::EvidenceStore::classify_evidence(
                tool_name,
                args_str.as_deref(),
                is_failure,
            );
            if let Err(error) = state
                .evidence_store
                .store_evidence(&evidence_type, result, None, Some(tool_name))
                .await
            {
                tracing::warn!(%error, "evidence capture failed");
            }

            state.learning_state.phase = state.phase_state.current();

            let watcher_actions = state.control_plane.evaluate(&state.learning_state);
            for action in &watcher_actions {
                state.learning_state.watcher_firing_count += 1;
                state.escape_protocol.update_metrics(
                    state.learning_state.watcher_firing_count,
                    0.5,
                    state.learning_state.steps_without_new_evidence,
                    0.0,
                );
                if action.severity == super::WatcherSeverity::Block {
                    state
                        .quarantine
                        .log_quarantine(
                            action.watcher_name,
                            "watcher",
                            super::quarantine::QuarantineStage::Suppression,
                            &action.message,
                            None,
                            None,
                            None,
                        )
                        .await;
                    // Signal the SpacebotHook to block the next tool call.
                    deps.runtime_config
                        .control_plane_block
                        .store(Arc::new(Some(action.message.clone())));
                }
            }

            // Track file edits for the diff-thrash watcher.
            if tool_name == "file" {
                if let Some(args) = args_summary {
                    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                        *state
                            .learning_state
                            .file_edit_counts
                            .entry(path.to_owned())
                            .or_insert(0) += 1;
                    }
                }
            }

            // Track consecutive errors and update phase failure counter.
            if !success {
                state.learning_state.consecutive_same_errors += 1;
                state.phase_state.record_failure();
            } else {
                state.learning_state.consecutive_same_errors = 0;
                state.phase_state.clear_failures();
                if tool_name.contains("test") {
                    state.learning_state.has_validation = true;
                }
            }

            // Track file reads so the cooldown manager can suppress
            // redundant read-before-write advisories.
            if tool_name == "file" {
                if let Some(args) = args_summary {
                    if args.get("operation").and_then(|v| v.as_str()) == Some("read") {
                        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                            state.cooldown_manager.record_file_read(path);
                        }
                    }
                }
            }

            // Route to domain chip runtime.
            let domain = detect_domain_from_tool(tool_name);
            let matched_chip_ids: Vec<String> = state
                .chip_runtime
                .route_event("tool_event", domain.as_deref())
                .iter()
                .map(|chip| chip.id.clone())
                .collect();
            for chip_id in matched_chip_ids {
                if let Err(error) = state
                    .chip_runtime
                    .record_observation(&chip_id, tool_name, "tool_event", result, None)
                    .await
                {
                    tracing::warn!(%error, chip_id, "chip observation recording failed");
                }
            }
        }

        _ => {}
    }

    Ok(())
}

/// Map cognitive signal patterns to insight categories.
fn category_from_signal(signal: &super::signals::CognitiveSignal) -> super::meta::InsightCategory {
    use super::signals::CognitivePattern;

    if let Some(pattern) = signal.detected_patterns.first() {
        return match pattern {
            CognitivePattern::Preference => super::meta::InsightCategory::UserModel,
            CognitivePattern::Decision => super::meta::InsightCategory::Reasoning,
            CognitivePattern::Correction => super::meta::InsightCategory::SelfAwareness,
            CognitivePattern::Reasoning => super::meta::InsightCategory::Reasoning,
            CognitivePattern::Remember => super::meta::InsightCategory::Context,
        };
    }

    // Fall back to domain detection.
    if signal.detected_domains.contains(&"coding".to_owned()) {
        return super::meta::InsightCategory::DomainExpertise;
    }
    if signal.detected_domains.contains(&"communication".to_owned()) {
        return super::meta::InsightCategory::Communication;
    }

    super::meta::InsightCategory::Context
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn load_config(deps: &AgentDeps) -> LearningConfig {
    (**deps.runtime_config.learning.load()).clone()
}

// ---------------------------------------------------------------------------
// Event summarization (audit log)
// ---------------------------------------------------------------------------

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
        ProcessEvent::WorkerStarted {
            task, worker_id, ..
        } => (
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
            format!(
                "branch {} started: {}",
                branch_id,
                truncate(description, 120)
            ),
        ),
        ProcessEvent::BranchResult { branch_id, .. } => {
            ("branch_result".into(), format!("branch {} completed", branch_id))
        }
        ProcessEvent::ToolStarted {
            tool_name, call_id, ..
        } => ("tool_started".into(), format!("{}[{}]", tool_name, call_id)),
        ProcessEvent::ToolCompleted {
            tool_name, call_id, ..
        } => (
            "tool_completed".into(),
            format!("{}[{}]", tool_name, call_id),
        ),
        ProcessEvent::MemorySaved { memory_id, .. } => {
            ("memory_saved".into(), format!("memory {memory_id}"))
        }
        ProcessEvent::CompactionTriggered {
            channel_id,
            threshold_reached,
            ..
        } => (
            "compaction_triggered".into(),
            format!(
                "channel {} at {:.0}%",
                channel_id,
                threshold_reached * 100.0
            ),
        ),
        ProcessEvent::StatusUpdate {
            process_id,
            status,
            ..
        } => (
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

// ---------------------------------------------------------------------------
// Owner-only filtering
// ---------------------------------------------------------------------------

fn should_process_event(
    config: &LearningConfig,
    non_owner_traces: &mut HashSet<String>,
    event: &ProcessEvent,
) -> bool {
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
        _ => {
            if let Some(trace_id) = extract_trace_id(event) {
                !non_owner_traces.contains(trace_id)
            } else {
                true
            }
        }
    }
}

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a tool name to a domain label for chip routing.
///
/// Returns `None` for tools that don't map cleanly to a single domain,
/// allowing chips that have no domain constraint to match unconditionally.
fn detect_domain_from_tool(tool_name: &str) -> Option<String> {
    match tool_name {
        "shell" | "exec" | "file" => Some("coding".into()),
        "web_search" | "browser" => Some("research".into()),
        _ => None,
    }
}

fn truncate(value: &str, max: usize) -> &str {
    if value.len() <= max {
        value
    } else {
        let end = value.floor_char_boundary(max);
        &value[..end]
    }
}
