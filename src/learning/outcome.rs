//! Episode lifecycle, step envelopes, and outcome prediction.
//!
//! Tracks active episodes (worker/branch runs) and their tool-call steps in
//! memory, persisting to `learning.db` on creation and completion. The outcome
//! predictor uses a Beta-prior counter table for lightweight success-rate
//! estimation.

use super::store::LearningStore;

use chrono::Utc;
use serde_json::Value as JsonValue;

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Active episode (in-memory)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct ActiveEpisode {
    pub episode_id: String,
    pub process_id: String,
    pub process_type: String,
    pub channel_id: Option<String>,
    pub trace_id: Option<String>,
    pub task: String,
    pub predicted_confidence: f64,
    pub started_at: chrono::DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Pending step (in-memory only)
// ---------------------------------------------------------------------------

struct PendingStep {
    step_id: String,
    episode_id: String,
    started_at: std::time::Instant,
}

// ---------------------------------------------------------------------------
// Episode tracker
// ---------------------------------------------------------------------------

/// Manages active episodes and step envelopes, writing to learning.db.
pub(crate) struct EpisodeTracker {
    /// episode_id → active episode
    active: HashMap<String, ActiveEpisode>,
    /// call_id → pending step (awaiting ToolCompleted)
    pending_steps: HashMap<String, PendingStep>,
    /// process_id → episode_id (quick lookup for tool events)
    process_to_episode: HashMap<String, String>,
}

impl EpisodeTracker {
    pub fn new() -> Self {
        Self {
            active: HashMap::new(),
            pending_steps: HashMap::new(),
            process_to_episode: HashMap::new(),
        }
    }

    // -- Worker lifecycle ---------------------------------------------------

    pub async fn on_worker_started(
        &mut self,
        store: &LearningStore,
        agent_id: &str,
        worker_id: &str,
        task: &str,
        channel_id: Option<&str>,
        trace_id: Option<&str>,
        process_id: &str,
    ) -> anyhow::Result<()> {
        let episode_id = format!("worker:{worker_id}");
        let predicted_confidence = OutcomePredictor::predict(store, None, None).await;

        sqlx::query(
            "INSERT INTO episodes (id, agent_id, trace_id, channel_id, process_id, process_type, \
             task, predicted_outcome, predicted_confidence, started_at) \
             VALUES (?, ?, ?, ?, ?, 'worker', ?, 'success', ?, datetime('now'))",
        )
        .bind(&episode_id)
        .bind(agent_id)
        .bind(trace_id)
        .bind(channel_id)
        .bind(process_id)
        .bind(task)
        .bind(predicted_confidence)
        .execute(store.pool())
        .await?;

        let episode = ActiveEpisode {
            episode_id: episode_id.clone(),
            process_id: process_id.to_owned(),
            process_type: "worker".into(),
            channel_id: channel_id.map(String::from),
            trace_id: trace_id.map(String::from),
            task: task.to_owned(),
            predicted_confidence,
            started_at: Utc::now(),
        };

        self.process_to_episode
            .insert(process_id.to_owned(), episode_id.clone());
        self.active.insert(episode_id, episode);
        Ok(())
    }

    pub async fn on_worker_complete(
        &mut self,
        store: &LearningStore,
        worker_id: &str,
        success: bool,
        duration_secs: f64,
    ) -> anyhow::Result<Option<String>> {
        let episode_id = format!("worker:{worker_id}");
        let Some(episode) = self.active.remove(&episode_id) else {
            // Fail-open: episode was never tracked (lag or restart).
            return Ok(None);
        };
        self.process_to_episode.remove(&episode.process_id);

        let actual_outcome = if success { "success" } else { "failure" };
        let actual_confidence: f64 = if success { 1.0 } else { 0.0 };
        let surprise_level = (episode.predicted_confidence - actual_confidence).abs();

        sqlx::query(
            "UPDATE episodes SET actual_outcome = ?, actual_confidence = ?, \
             surprise_level = ?, completed_at = datetime('now'), duration_secs = ? \
             WHERE id = ?",
        )
        .bind(actual_outcome)
        .bind(actual_confidence)
        .bind(surprise_level)
        .bind(duration_secs)
        .bind(&episode_id)
        .execute(store.pool())
        .await?;

        // Update outcome predictor counters.
        if let Err(error) = OutcomePredictor::update(store, None, None, success).await {
            tracing::warn!(%error, "outcome predictor update failed");
        }

        Ok(Some(episode_id))
    }

    // -- Branch lifecycle ---------------------------------------------------

    pub async fn on_branch_started(
        &mut self,
        store: &LearningStore,
        agent_id: &str,
        branch_id: &str,
        description: &str,
        channel_id: &str,
        trace_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let episode_id = format!("branch:{branch_id}");
        let predicted_confidence = OutcomePredictor::predict(store, None, None).await;

        sqlx::query(
            "INSERT INTO episodes (id, agent_id, trace_id, channel_id, process_id, process_type, \
             task, predicted_outcome, predicted_confidence, started_at) \
             VALUES (?, ?, ?, ?, ?, 'branch', ?, 'success', ?, datetime('now'))",
        )
        .bind(&episode_id)
        .bind(agent_id)
        .bind(trace_id)
        .bind(channel_id)
        .bind(branch_id)
        .bind(description)
        .bind(predicted_confidence)
        .execute(store.pool())
        .await?;

        let episode = ActiveEpisode {
            episode_id: episode_id.clone(),
            process_id: branch_id.to_owned(),
            process_type: "branch".into(),
            channel_id: Some(channel_id.to_owned()),
            trace_id: trace_id.map(String::from),
            task: description.to_owned(),
            predicted_confidence,
            started_at: Utc::now(),
        };

        self.process_to_episode
            .insert(branch_id.to_owned(), episode_id.clone());
        self.active.insert(episode_id, episode);
        Ok(())
    }

    pub async fn on_branch_result(
        &mut self,
        store: &LearningStore,
        branch_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let episode_id = format!("branch:{branch_id}");
        let Some(episode) = self.active.remove(&episode_id) else {
            return Ok(None);
        };
        self.process_to_episode.remove(&episode.process_id);

        // Branches that complete are successful by definition.
        let actual_confidence: f64 = 1.0;
        let surprise_level = (episode.predicted_confidence - actual_confidence).abs();
        let duration_secs = (Utc::now() - episode.started_at).num_milliseconds() as f64 / 1000.0;

        sqlx::query(
            "UPDATE episodes SET actual_outcome = 'success', actual_confidence = ?, \
             surprise_level = ?, completed_at = datetime('now'), duration_secs = ? \
             WHERE id = ?",
        )
        .bind(actual_confidence)
        .bind(surprise_level)
        .bind(duration_secs)
        .bind(&episode_id)
        .execute(store.pool())
        .await?;

        if let Err(error) = OutcomePredictor::update(store, None, None, true).await {
            tracing::warn!(%error, "outcome predictor update failed");
        }

        Ok(Some(episode_id))
    }

    // -- Tool step lifecycle ------------------------------------------------

    pub async fn on_tool_started(
        &mut self,
        store: &LearningStore,
        process_id: &str,
        call_id: &str,
        tool_name: &str,
        args_summary: Option<&JsonValue>,
        trace_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let episode_id = match self.process_to_episode.get(process_id) {
            Some(id) => id.clone(),
            None => return Ok(()), // no active episode for this process
        };

        let step_id = uuid::Uuid::new_v4().to_string();
        let args_json = args_summary.map(|v| v.to_string());

        sqlx::query(
            "INSERT INTO steps (id, episode_id, call_id, trace_id, tool_name, args_summary, \
             created_at) VALUES (?, ?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&step_id)
        .bind(&episode_id)
        .bind(call_id)
        .bind(trace_id)
        .bind(tool_name)
        .bind(&args_json)
        .execute(store.pool())
        .await?;

        self.pending_steps.insert(
            call_id.to_owned(),
            PendingStep {
                step_id,
                episode_id,
                started_at: std::time::Instant::now(),
            },
        );

        Ok(())
    }

    pub async fn on_tool_completed(
        &mut self,
        store: &LearningStore,
        _process_id: &str,
        call_id: &str,
        _tool_name: &str,
        success: bool,
    ) -> anyhow::Result<()> {
        let Some(pending) = self.pending_steps.remove(call_id) else {
            // Fail-open: ToolStarted was missed (lag/restart). Create placeholder.
            return Ok(());
        };

        let result_text = if success { "success" } else { "failure" };
        let evidence_gathered: i32 = if success { 1 } else { 0 };
        let progress_made: i32 = if success { 1 } else { 0 };

        sqlx::query(
            "UPDATE steps SET result = ?, completed_at = datetime('now'), \
             evidence_gathered = ?, progress_made = ? WHERE id = ?",
        )
        .bind(result_text)
        .bind(evidence_gathered)
        .bind(progress_made)
        .bind(&pending.step_id)
        .execute(store.pool())
        .await?;

        Ok(())
    }

    // -- Stale cleanup ------------------------------------------------------

    /// Close episodes that never received a completion event.
    pub async fn cleanup_stale(
        &mut self,
        store: &LearningStore,
        timeout_secs: u64,
    ) -> anyhow::Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::seconds(timeout_secs as i64);
        let mut stale_ids = Vec::new();

        for (episode_id, episode) in &self.active {
            if episode.started_at < cutoff {
                stale_ids.push(episode_id.clone());
            }
        }

        for episode_id in &stale_ids {
            if let Some(episode) = self.active.remove(episode_id) {
                self.process_to_episode.remove(&episode.process_id);
            }
            sqlx::query(
                "UPDATE episodes SET actual_outcome = 'abandoned', completed_at = datetime('now') \
                 WHERE id = ? AND completed_at IS NULL",
            )
            .bind(episode_id)
            .execute(store.pool())
            .await?;
        }

        let count = stale_ids.len();
        if count > 0 {
            tracing::info!(count, "cleaned up stale episodes");
        }
        Ok(count)
    }

    /// Return the IDs of all episodes currently tracked as active.
    ///
    /// Used by the evidence store to exempt in-progress episodes from expiry
    /// cleanup, ensuring evidence collected during an active episode is not
    /// deleted before distillation can run.
    pub fn active_episode_ids(&self) -> Vec<String> {
        self.active.keys().cloned().collect()
    }

    /// List episode IDs that completed but haven't been processed for distillation.
    pub async fn completed_episode_ids(
        store: &LearningStore,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM episodes WHERE completed_at IS NOT NULL \
             AND id NOT IN (SELECT DISTINCT source_episode_id FROM distillations WHERE source_episode_id IS NOT NULL) \
             ORDER BY completed_at ASC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(store.pool())
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Count total completed episodes (for cold-start checks).
    pub async fn completed_episode_count(store: &LearningStore) -> anyhow::Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM episodes WHERE completed_at IS NOT NULL",
        )
        .fetch_one(store.pool())
        .await?;
        Ok(row.0)
    }
}

// ---------------------------------------------------------------------------
// Outcome Predictor
// ---------------------------------------------------------------------------

/// Lightweight counter-based predictor with Beta-prior smoothing.
pub(crate) struct OutcomePredictor;

impl OutcomePredictor {
    /// Predict success rate for the given key components.
    ///
    /// Fallback chain: exact `"phase:tool"` → `"*:tool"` → `"phase:*"` → prior (0.75).
    pub async fn predict(
        store: &LearningStore,
        phase: Option<&str>,
        tool: Option<&str>,
    ) -> f64 {
        let keys = Self::fallback_keys(phase, tool);
        for key in &keys {
            if let Ok(Some(rate)) = Self::lookup(store, key).await {
                return rate;
            }
        }
        0.75 // prior
    }

    /// Update counts after an episode completes.
    pub async fn update(
        store: &LearningStore,
        phase: Option<&str>,
        tool: Option<&str>,
        success: bool,
    ) -> anyhow::Result<()> {
        let key = Self::primary_key(phase, tool);

        if success {
            sqlx::query(
                "INSERT INTO outcome_predictions (key, success_count, failure_count, last_updated) \
                 VALUES (?, 4.0, 1.0, datetime('now')) \
                 ON CONFLICT(key) DO UPDATE SET \
                 success_count = outcome_predictions.success_count + 1.0, \
                 last_updated = datetime('now')",
            )
            .bind(&key)
            .execute(store.pool())
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO outcome_predictions (key, success_count, failure_count, last_updated) \
                 VALUES (?, 3.0, 2.0, datetime('now')) \
                 ON CONFLICT(key) DO UPDATE SET \
                 failure_count = outcome_predictions.failure_count + 1.0, \
                 last_updated = datetime('now')",
            )
            .bind(&key)
            .execute(store.pool())
            .await?;
        }

        // LRU eviction: keep at most 2000 keys.
        Self::evict_if_needed(store).await?;
        Ok(())
    }

    // -- helpers ------------------------------------------------------------

    async fn lookup(store: &LearningStore, key: &str) -> anyhow::Result<Option<f64>> {
        let row: Option<(f64, f64)> = sqlx::query_as(
            "SELECT success_count, failure_count FROM outcome_predictions WHERE key = ?",
        )
        .bind(key)
        .fetch_optional(store.pool())
        .await?;

        Ok(row.map(|(success, failure)| success / (success + failure)))
    }

    fn primary_key(phase: Option<&str>, tool: Option<&str>) -> String {
        let phase_part = phase.unwrap_or("*");
        let tool_part = tool.unwrap_or("*");
        format!("{phase_part}:{tool_part}")
    }

    fn fallback_keys(phase: Option<&str>, tool: Option<&str>) -> Vec<String> {
        let mut keys = Vec::with_capacity(4);
        // Exact
        keys.push(Self::primary_key(phase, tool));
        // Wildcard phase
        if phase.is_some() {
            keys.push(format!("*:{}", tool.unwrap_or("*")));
        }
        // Wildcard tool
        if tool.is_some() {
            keys.push(format!("{}:*", phase.unwrap_or("*")));
        }
        // Full wildcard (should match prior default row if it exists)
        if phase.is_some() || tool.is_some() {
            keys.push("*:*".into());
        }
        keys
    }

    async fn evict_if_needed(store: &LearningStore) -> anyhow::Result<()> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM outcome_predictions")
                .fetch_one(store.pool())
                .await?;

        if count.0 > 2000 {
            let excess = count.0 - 2000;
            sqlx::query(
                "DELETE FROM outcome_predictions WHERE key IN \
                 (SELECT key FROM outcome_predictions ORDER BY last_updated ASC LIMIT ?)",
            )
            .bind(excess)
            .execute(store.pool())
            .await?;
        }
        Ok(())
    }
}
