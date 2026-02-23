//! Auto-tuner for closed-loop self-optimization.
//!
//! `AutoTuner` observes per-source effectiveness signals and nudges
//! tuneable weights toward values that reinforce high-performing sources
//! while softening the influence of weaker ones. Each run is bounded by
//! `TunerMode` to limit the maximum single-cycle change, and every
//! adjustment is preceded by an automatic snapshot for easy rollback.

use crate::learning::LearningStore;
use crate::learning::tuneables::TuneableStore;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Minimum value the tuner will write to any weight tuneable.
const WEIGHT_MIN: f64 = 0.5;

/// Maximum value the tuner will write to any weight tuneable.
const WEIGHT_MAX: f64 = 2.0;

/// Controls how aggressively the tuner adjusts tuneable weights in a single
/// cycle.
///
/// The mode determines the symmetric window around the current value within
/// which an adjustment may land. `Suggest` logs proposals without writing
/// anything, making it safe for observation periods.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunerMode {
    /// Compute and log proposed adjustments only — no writes to tuneables.
    Suggest,
    /// Limit each tuneable to a 5% change per cycle.
    Conservative,
    /// Limit each tuneable to a 15% change per cycle.
    Moderate,
    /// Limit each tuneable to a 25% change per cycle.
    Aggressive,
}

impl TunerMode {
    /// Maximum percentage change this mode permits per tuneable per cycle.
    ///
    /// Returns `0` for `Suggest` to signal that no writes should occur even
    /// though the algorithm still computes proposals for logging.
    pub fn max_change_pct(&self) -> u8 {
        match self {
            TunerMode::Suggest => 0,
            TunerMode::Conservative => 5,
            TunerMode::Moderate => 15,
            TunerMode::Aggressive => 25,
        }
    }

    /// Parse a mode name case-insensitively, defaulting to `Conservative` for
    /// unrecognised strings rather than returning an error.
    pub fn from_str_lossy(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "suggest" => TunerMode::Suggest,
            "conservative" => TunerMode::Conservative,
            "moderate" => TunerMode::Moderate,
            "aggressive" => TunerMode::Aggressive,
            _ => {
                tracing::warn!(value, "unknown tuner mode, defaulting to conservative");
                TunerMode::Conservative
            }
        }
    }
}

impl fmt::Display for TunerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunerMode::Suggest => write!(f, "suggest"),
            TunerMode::Conservative => write!(f, "conservative"),
            TunerMode::Moderate => write!(f, "moderate"),
            TunerMode::Aggressive => write!(f, "aggressive"),
        }
    }
}

/// A single weight adjustment proposed or applied by the auto-tuner during one
/// cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningAdjustment {
    /// The tuneable key that was (or would be) adjusted.
    pub key: String,
    /// String representation of the value before adjustment.
    pub old_value: String,
    /// String representation of the value after adjustment.
    pub new_value: String,
    /// Percentage change applied (positive = increase, negative = decrease).
    pub change_pct: f64,
}

/// Summary returned after a completed tuning cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningResult {
    /// All adjustments computed during this cycle. Empty when no numeric
    /// tuneables matched the provided source keys, or when the global average
    /// was zero.
    pub adjustments: Vec<TuningAdjustment>,
    /// ID of the pre-flight snapshot captured before any writes were made.
    pub snapshot_id: String,
    /// Mode that governed this cycle.
    pub mode: TunerMode,
}

/// A row from the `tuneables_snapshots` audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub id: String,
    pub reason: Option<String>,
    pub created_at: String,
}

/// Closed-loop optimizer that adjusts tuneable weights based on observed
/// per-source effectiveness signals.
///
/// All writes are preceded by an automatic snapshot, so any cycle can be
/// rolled back by calling [`restore_snapshot`](AutoTuner::restore_snapshot)
/// with the `snapshot_id` from the returned [`TuningResult`].
pub struct AutoTuner {
    store: Arc<LearningStore>,
    mode: TunerMode,
}

impl AutoTuner {
    pub fn new(store: Arc<LearningStore>, mode: TunerMode) -> Self {
        Self { store, mode }
    }

    /// Evaluate source effectiveness signals and nudge matching tuneable
    /// weights within the bounds permitted by the current mode.
    ///
    /// ## Algorithm
    ///
    /// 1. Compute the global average effectiveness across all provided sources.
    /// 2. For each source, derive `ideal_boost = effectiveness / global_average`.
    ///    A value above 1.0 means the source outperforms the average; below 1.0
    ///    means it underperforms.
    /// 3. Clamp `ideal_boost` to a symmetric window `[1 - max_pct, 1 + max_pct]`
    ///    so a single cycle cannot change a weight by more than the mode allows.
    ///    `Suggest` mode clamps to exactly `[1.0, 1.0]` (no change).
    /// 4. Multiply the current tuneable value by the clamped multiplier, then
    ///    clamp the result to `[WEIGHT_MIN, WEIGHT_MAX]`.
    /// 5. Snapshot the current state, then (if not `Suggest`) write the new
    ///    values.
    pub async fn run_tuning_cycle(
        &self,
        tuneables: &TuneableStore,
        source_effectiveness: &[(String, f64)],
    ) -> Result<TuningResult> {
        if source_effectiveness.is_empty() {
            let snapshot_id = self
                .save_snapshot(tuneables, "tuning cycle (no sources)")
                .await?;
            return Ok(TuningResult {
                adjustments: Vec::new(),
                snapshot_id,
                mode: self.mode.clone(),
            });
        }

        let global_average = {
            let total: f64 = source_effectiveness.iter().map(|(_, effectiveness)| effectiveness).sum();
            total / source_effectiveness.len() as f64
        };

        // A zero average (all sources reported 0.0) leaves ideal_boost undefined.
        // Snapshot and return without writing anything.
        if global_average == 0.0 {
            let snapshot_id = self
                .save_snapshot(tuneables, "tuning cycle (zero effectiveness average)")
                .await?;
            return Ok(TuningResult {
                adjustments: Vec::new(),
                snapshot_id,
                mode: self.mode.clone(),
            });
        }

        let max_change_fraction = self.mode.max_change_pct() as f64 / 100.0;

        let mut adjustments = Vec::new();

        for (source_key, effectiveness) in source_effectiveness {
            let Some(old_value) = tuneables.get_f64(source_key).await else {
                // No matching numeric tuneable — not an error, just not tunable.
                tracing::debug!(key = %source_key, "no numeric tuneable for source, skipping");
                continue;
            };

            let ideal_boost = effectiveness / global_average;

            // Clamp to the mode's window so no single cycle changes a weight
            // by more than max_change_pct. For Suggest, max_change_fraction is
            // 0.0, so the multiplier is forced to exactly 1.0.
            let actual_multiplier =
                ideal_boost.clamp(1.0 - max_change_fraction, 1.0 + max_change_fraction);

            let new_value = (old_value * actual_multiplier).clamp(WEIGHT_MIN, WEIGHT_MAX);

            let change_pct = if old_value == 0.0 {
                0.0
            } else {
                (new_value - old_value) / old_value * 100.0
            };

            adjustments.push(TuningAdjustment {
                key: source_key.clone(),
                old_value: format!("{old_value:.6}"),
                new_value: format!("{new_value:.6}"),
                change_pct,
            });
        }

        // Snapshot before any writes so the pre-change state is always
        // recoverable regardless of whether subsequent writes succeed.
        let snapshot_id = self
            .save_snapshot(tuneables, "pre-tuning-cycle snapshot")
            .await?;

        if self.mode != TunerMode::Suggest {
            for adjustment in &adjustments {
                tuneables
                    .set(&adjustment.key, &adjustment.new_value, None)
                    .await
                    .with_context(|| {
                        format!("failed to write adjusted tuneable '{}'", adjustment.key)
                    })?;
            }
        }

        tracing::info!(
            mode = %self.mode,
            adjustment_count = adjustments.len(),
            %snapshot_id,
            "tuning cycle complete",
        );

        Ok(TuningResult {
            adjustments,
            snapshot_id,
            mode: self.mode.clone(),
        })
    }

    /// Capture a named snapshot of the current tuneable state.
    ///
    /// The full cache is serialized as a JSON object and written to the
    /// `tuneables_snapshots` table. Returns the UUID assigned to the row.
    pub async fn save_snapshot(&self, tuneables: &TuneableStore, reason: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let state = tuneables.snapshot().await;
        let snapshot_json =
            serde_json::to_string(&state).context("failed to serialize tuneable snapshot")?;

        sqlx::query(
            r#"
            INSERT INTO tuneables_snapshots (id, snapshot, reason, created_at)
            VALUES (?, ?, ?, datetime('now'))
            "#,
        )
        .bind(&id)
        .bind(&snapshot_json)
        .bind(reason)
        .execute(self.store.pool())
        .await
        .context("failed to insert tuneable snapshot")?;

        tracing::debug!(%id, %reason, "tuneable snapshot saved");

        Ok(id)
    }

    /// Overwrite the current tuneable values with the contents of a previously
    /// saved snapshot.
    ///
    /// Every key in the snapshot is written back via `TuneableStore::set`,
    /// which updates both the database and the in-memory cache atomically.
    pub async fn restore_snapshot(
        &self,
        snapshot_id: &str,
        tuneables: &TuneableStore,
    ) -> Result<()> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT snapshot FROM tuneables_snapshots WHERE id = ?")
                .bind(snapshot_id)
                .fetch_optional(self.store.pool())
                .await
                .context("failed to query tuneable snapshot")?;

        let Some((snapshot_json,)) = row else {
            anyhow::bail!("snapshot '{snapshot_id}' not found");
        };

        let state: HashMap<String, String> = serde_json::from_str(&snapshot_json)
            .context("failed to deserialize tuneable snapshot")?;

        for (key, value) in &state {
            tuneables
                .set(key, value, None)
                .await
                .with_context(|| format!("failed to restore tuneable '{key}'"))?;
        }

        tracing::info!(snapshot_id, key_count = state.len(), "tuneable snapshot restored");

        Ok(())
    }

    /// Return recent snapshots in reverse-chronological order.
    pub async fn get_snapshots(&self, limit: i64) -> Result<Vec<SnapshotEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, reason, created_at
            FROM tuneables_snapshots
            ORDER BY created_at DESC, rowid DESC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(self.store.pool())
        .await
        .context("failed to query tuneable snapshots")?;

        let entries = rows
            .into_iter()
            .map(|row| SnapshotEntry {
                id: row.get("id"),
                reason: row.get("reason"),
                created_at: row.get("created_at"),
            })
            .collect();

        Ok(entries)
    }
}

impl fmt::Debug for AutoTuner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AutoTuner")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Helpers shared between implementation and tests
// ---------------------------------------------------------------------------

/// Compute the adjusted value for a single tuneable given the observed
/// effectiveness, global average, and tuner mode.
///
/// Extracted so the core arithmetic can be unit-tested without a database.
fn compute_adjusted_value(
    old_value: f64,
    effectiveness: f64,
    global_average: f64,
    mode: &TunerMode,
) -> f64 {
    let max_change_fraction = mode.max_change_pct() as f64 / 100.0;
    let ideal_boost = effectiveness / global_average;
    let actual_multiplier =
        ideal_boost.clamp(1.0 - max_change_fraction, 1.0 + max_change_fraction);
    (old_value * actual_multiplier).clamp(WEIGHT_MIN, WEIGHT_MAX)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Bounded adjustment arithmetic
    // -----------------------------------------------------------------------

    #[test]
    fn test_suggest_mode_never_changes_value() {
        // max_change_pct = 0, so the multiplier is clamped to exactly 1.0.
        // Use 0.80 — safely above WEIGHT_MIN so the clamp doesn't interfere.
        let result = compute_adjusted_value(0.80, 2.0, 1.0, &TunerMode::Suggest);
        assert!(
            (result - 0.80).abs() < 1e-9,
            "suggest mode must not change the value, got {result}"
        );
    }

    #[test]
    fn test_conservative_caps_large_upward_boost() {
        // ideal_boost = 1.5, conservative window = [0.95, 1.05]
        let result = compute_adjusted_value(1.0, 1.5, 1.0, &TunerMode::Conservative);
        let expected = 1.0 * 1.05;
        assert!(
            (result - expected).abs() < 1e-9,
            "expected {expected}, got {result}"
        );
    }

    #[test]
    fn test_conservative_caps_large_downward_boost() {
        // ideal_boost = 0.5, conservative window = [0.95, 1.05]
        let result = compute_adjusted_value(1.0, 0.5, 1.0, &TunerMode::Conservative);
        let expected = 1.0 * 0.95;
        assert!(
            (result - expected).abs() < 1e-9,
            "expected {expected}, got {result}"
        );
    }

    #[test]
    fn test_moderate_caps_at_fifteen_pct() {
        // ideal_boost = 1.5, moderate window = [0.85, 1.15]
        let result = compute_adjusted_value(1.0, 1.5, 1.0, &TunerMode::Moderate);
        let expected = 1.0 * 1.15;
        assert!(
            (result - expected).abs() < 1e-9,
            "expected {expected}, got {result}"
        );
    }

    #[test]
    fn test_aggressive_passes_through_boost_within_cap() {
        // ideal_boost = 1.2 fits inside aggressive window [0.75, 1.25]
        let result = compute_adjusted_value(1.0, 1.2, 1.0, &TunerMode::Aggressive);
        let expected = 1.0 * 1.2;
        assert!(
            (result - expected).abs() < 1e-9,
            "expected {expected}, got {result}"
        );
    }

    #[test]
    fn test_average_source_produces_no_change() {
        // effectiveness == global_average → ideal_boost = 1.0 → no movement.
        // Use 0.80 — safely above WEIGHT_MIN so the clamp doesn't interfere.
        let result = compute_adjusted_value(0.80, 1.0, 1.0, &TunerMode::Moderate);
        assert!(
            (result - 0.80).abs() < 1e-9,
            "average-effectiveness source should produce no change, got {result}"
        );
    }

    #[test]
    fn test_weight_clamped_to_min() {
        // old_value = 0.6, aggressive -25% → 0.6 * 0.75 = 0.45 < WEIGHT_MIN
        let result = compute_adjusted_value(0.6, 0.0, 1.0, &TunerMode::Aggressive);
        assert!(
            (result - WEIGHT_MIN).abs() < 1e-9,
            "result {result} should be clamped to WEIGHT_MIN={WEIGHT_MIN}"
        );
    }

    #[test]
    fn test_weight_clamped_to_max() {
        // old_value = 1.9, aggressive +25% → 1.9 * 1.25 = 2.375 > WEIGHT_MAX
        let result = compute_adjusted_value(1.9, 2.0, 1.0, &TunerMode::Aggressive);
        assert!(
            (result - WEIGHT_MAX).abs() < 1e-9,
            "result {result} should be clamped to WEIGHT_MAX={WEIGHT_MAX}"
        );
    }

    #[test]
    fn test_global_average_normalises_boost() {
        // With global_average = 2.0 and effectiveness = 2.0, ideal_boost = 1.0
        // — the source is exactly average, so no change.
        // Use 0.80 — safely above WEIGHT_MIN so the clamp doesn't interfere.
        let result = compute_adjusted_value(0.80, 2.0, 2.0, &TunerMode::Moderate);
        assert!(
            (result - 0.80).abs() < 1e-9,
            "source at global average should produce no change, got {result}"
        );
    }

    // -----------------------------------------------------------------------
    // TunerMode helpers
    // -----------------------------------------------------------------------

    #[test]
    fn test_max_change_pct_values() {
        assert_eq!(TunerMode::Suggest.max_change_pct(), 0);
        assert_eq!(TunerMode::Conservative.max_change_pct(), 5);
        assert_eq!(TunerMode::Moderate.max_change_pct(), 15);
        assert_eq!(TunerMode::Aggressive.max_change_pct(), 25);
    }

    #[test]
    fn test_display_round_trips() {
        assert_eq!(TunerMode::Suggest.to_string(), "suggest");
        assert_eq!(TunerMode::Conservative.to_string(), "conservative");
        assert_eq!(TunerMode::Moderate.to_string(), "moderate");
        assert_eq!(TunerMode::Aggressive.to_string(), "aggressive");
    }

    #[test]
    fn test_from_str_lossy_case_insensitive() {
        assert_eq!(TunerMode::from_str_lossy("suggest"), TunerMode::Suggest);
        assert_eq!(TunerMode::from_str_lossy("CONSERVATIVE"), TunerMode::Conservative);
        assert_eq!(TunerMode::from_str_lossy("Moderate"), TunerMode::Moderate);
        assert_eq!(TunerMode::from_str_lossy("AGGRESSIVE"), TunerMode::Aggressive);
    }

    #[test]
    fn test_from_str_lossy_unknown_defaults_to_conservative() {
        assert_eq!(TunerMode::from_str_lossy("turbo"), TunerMode::Conservative);
        assert_eq!(TunerMode::from_str_lossy(""), TunerMode::Conservative);
        assert_eq!(TunerMode::from_str_lossy("YOLO"), TunerMode::Conservative);
    }

    // -----------------------------------------------------------------------
    // Integration: snapshot + tuning cycle (in-memory SQLite)
    // -----------------------------------------------------------------------

    async fn setup() -> (Arc<LearningStore>, TuneableStore) {
        let path = std::env::temp_dir().join(format!(
            "spacebot_test_tuner_{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = LearningStore::connect(&path).await.unwrap();
        let tuneables = TuneableStore::new(store.clone()).await.unwrap();
        (store, tuneables)
    }

    #[tokio::test]
    async fn test_suggest_cycle_does_not_write_tuneables() {
        let (store, tuneables) = setup().await;
        tuneables.set("advisory_score_context_weight", "0.45", None).await.unwrap();

        let tuner = AutoTuner::new(store, TunerMode::Suggest);
        let sources = vec![("advisory_score_context_weight".to_string(), 0.9)];
        let result = tuner.run_tuning_cycle(&tuneables, &sources).await.unwrap();

        // Suggest mode: one adjustment logged but value unchanged.
        assert_eq!(result.mode, TunerMode::Suggest);
        assert_eq!(result.adjustments.len(), 1);
        let value = tuneables.get_f64("advisory_score_context_weight").await.unwrap();
        assert!(
            (value - 0.45).abs() < 1e-6,
            "suggest mode must not mutate tuneable, got {value}"
        );
    }

    #[tokio::test]
    async fn test_conservative_cycle_writes_bounded_value() {
        let (store, tuneables) = setup().await;
        // Use 1.0 — safely within [WEIGHT_MIN, WEIGHT_MAX] so the clamp doesn't
        // interfere with verifying the +5% cap.
        tuneables.set("advisory_score_context_weight", "1.0", None).await.unwrap();

        let tuner = AutoTuner::new(store, TunerMode::Conservative);
        // Source well above average (2.0 vs global avg 1.5) → ideal_boost 1.33,
        // capped to +5% by Conservative mode.
        let sources = vec![
            ("advisory_score_context_weight".to_string(), 2.0),
            ("advisory_score_confidence_weight".to_string(), 1.0),
        ];
        // advisory_score_confidence_weight is not seeded, so it should be skipped.
        let result = tuner.run_tuning_cycle(&tuneables, &sources).await.unwrap();

        assert_eq!(result.adjustments.len(), 1);
        let written = tuneables.get_f64("advisory_score_context_weight").await.unwrap();
        let expected = 1.0 * 1.05;
        assert!(
            (written - expected).abs() < 1e-5,
            "expected {expected:.6}, got {written:.6}"
        );
    }

    #[tokio::test]
    async fn test_empty_sources_returns_empty_adjustments() {
        let (store, tuneables) = setup().await;
        let tuner = AutoTuner::new(store, TunerMode::Moderate);
        let result = tuner.run_tuning_cycle(&tuneables, &[]).await.unwrap();
        assert!(result.adjustments.is_empty());
        assert!(!result.snapshot_id.is_empty());
    }

    #[tokio::test]
    async fn test_snapshot_and_restore_roundtrip() {
        let (store, tuneables) = setup().await;
        tuneables.set("some_weight", "0.60", None).await.unwrap();

        let tuner = AutoTuner::new(store, TunerMode::Aggressive);
        let snapshot_id = tuner.save_snapshot(&tuneables, "test snapshot").await.unwrap();

        // Mutate after snapshot.
        tuneables.set("some_weight", "1.99", None).await.unwrap();
        assert!((tuneables.get_f64("some_weight").await.unwrap() - 1.99).abs() < 1e-6);

        // Restore should bring back 0.60.
        tuner.restore_snapshot(&snapshot_id, &tuneables).await.unwrap();
        let restored = tuneables.get_f64("some_weight").await.unwrap();
        assert!(
            (restored - 0.60).abs() < 1e-6,
            "expected 0.60 after restore, got {restored}"
        );
    }

    #[tokio::test]
    async fn test_get_snapshots_returns_entries_in_reverse_order() {
        let (store, tuneables) = setup().await;
        let tuner = AutoTuner::new(store, TunerMode::Conservative);

        tuner.save_snapshot(&tuneables, "first").await.unwrap();
        tuner.save_snapshot(&tuneables, "second").await.unwrap();
        tuner.save_snapshot(&tuneables, "third").await.unwrap();

        let snapshots = tuner.get_snapshots(10).await.unwrap();
        assert_eq!(snapshots.len(), 3);
        // Most recent first.
        assert_eq!(snapshots[0].reason.as_deref(), Some("third"));
        assert_eq!(snapshots[2].reason.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn test_restore_nonexistent_snapshot_returns_error() {
        let (store, tuneables) = setup().await;
        let tuner = AutoTuner::new(store, TunerMode::Conservative);
        let result = tuner.restore_snapshot("does-not-exist", &tuneables).await;
        assert!(result.is_err());
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("does-not-exist"),
            "error should name the missing snapshot id, got: {message}"
        );
    }
}
