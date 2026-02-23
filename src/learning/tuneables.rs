//! Runtime-tuneable key-value configuration for the learning system.
//!
//! `TuneableStore` wraps the `tuneables` table in learning.db and keeps a
//! local read cache so the hot advisory path never waits on a DB round-trip.
//! Writes go to the database first, then update the cache atomically.
//! Out-of-band changes (e.g. from a CLI or another process) are picked up by
//! calling `refresh`.

use crate::learning::LearningStore;

use anyhow::Result;
use sqlx::Row as _;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Hot-reloadable configuration backed by learning.db's `tuneables` table.
///
/// All reads are served from an in-memory `HashMap` protected by a
/// `RwLock`. Multiple readers can proceed concurrently without touching
/// SQLite. Writers hold the lock only long enough to swap the value in;
/// the DB write happens before the lock is acquired so the cache is never
/// updated if persistence fails.
pub struct TuneableStore {
    store: Arc<LearningStore>,
    cache: Arc<RwLock<HashMap<String, String>>>,
}

impl TuneableStore {
    /// Build a `TuneableStore` and eagerly warm the cache from the database.
    pub async fn new(store: Arc<LearningStore>) -> Result<Self> {
        let tuneable_store = Self {
            store,
            cache: Arc::new(RwLock::new(HashMap::new())),
        };
        tuneable_store.refresh().await?;
        Ok(tuneable_store)
    }

    /// Return a cached string value, or `None` if the key is absent.
    pub async fn get(&self, key: &str) -> Option<String> {
        self.cache.read().await.get(key).cloned()
    }

    /// Parse a cached value as `f64`, returning `None` on a missing or
    /// unparseable key rather than propagating an error. Advisory code can
    /// gracefully fall back to hard-coded defaults when a tuneable is absent.
    pub async fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).await?.parse().ok()
    }

    /// Parse a cached value as `i64`.
    pub async fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).await?.parse().ok()
    }

    /// Persist a value to the database and update the cache.
    ///
    /// When `description` is `None` and the row already exists, the stored
    /// description is preserved via `COALESCE`. A non-`None` description
    /// always overwrites.
    pub async fn set(&self, key: &str, value: &str, description: Option<&str>) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tuneables (key, value, description, updated_at)
            VALUES (?, ?, ?, datetime('now'))
            ON CONFLICT(key) DO UPDATE SET
                value       = excluded.value,
                description = COALESCE(excluded.description, tuneables.description),
                updated_at  = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(value)
        .bind(description)
        .execute(self.store.pool())
        .await?;

        self.cache
            .write()
            .await
            .insert(key.to_string(), value.to_string());

        Ok(())
    }

    /// Reload all rows from the database, replacing the in-memory cache.
    ///
    /// Call this on a background timer or after receiving an external signal
    /// that the table was modified outside this process.
    pub async fn refresh(&self) -> Result<()> {
        let rows = sqlx::query("SELECT key, value FROM tuneables")
            .fetch_all(self.store.pool())
            .await?;

        let mut cache = self.cache.write().await;
        cache.clear();
        for row in rows {
            let key: String = row.get("key");
            let value: String = row.get("value");
            cache.insert(key, value);
        }

        Ok(())
    }

    /// Return a point-in-time clone of the entire cache.
    ///
    /// Useful for the auto-tuner, which needs a stable snapshot to compute
    /// diffs without holding the lock across an async boundary.
    pub async fn snapshot(&self) -> HashMap<String, String> {
        self.cache.read().await.clone()
    }

    /// Insert system defaults using `INSERT OR IGNORE` so that any value
    /// already present in the database is left untouched. Safe to call on
    /// every startup.
    pub async fn populate_defaults(&self) -> Result<()> {
        // Each entry: (key, value, description).
        let defaults: &[(&str, &str, &str)] = &[
            // --- Advisory scoring weights ---
            (
                "advisory_score_context_weight",
                "0.45",
                "Weight for context signals in the composite advisory score",
            ),
            (
                "advisory_score_confidence_weight",
                "0.25",
                "Weight for source confidence in the composite advisory score",
            ),
            (
                "advisory_score_base_floor",
                "0.15",
                "Minimum base score before any boost is applied",
            ),
            // --- Advisory boost factors ---
            (
                "advisory_negative_boost",
                "1.3",
                "Score multiplier when the prior outcome for this context was negative",
            ),
            (
                "advisory_failure_boost",
                "1.5",
                "Score multiplier applied after a confirmed task failure",
            ),
            // --- Cold-start ramp ---
            // The boost fades linearly from start_episode to end_episode so
            // early episodes get extra advisory coverage while the system
            // accumulates enough signal to self-regulate.
            (
                "advisory_cold_start_boost",
                "0.20",
                "Score bonus added during the cold-start window",
            ),
            (
                "advisory_cold_start_end_episode",
                "50",
                "Episode number at which the cold-start bonus reaches zero",
            ),
            (
                "advisory_cold_start_start_episode",
                "25",
                "Episode number at which the cold-start bonus begins ramping down",
            ),
            // --- Emission rate limits ---
            (
                "advisory_emission_max_per_event",
                "2",
                "Maximum number of advisory packets emitted per tool event",
            ),
            (
                "advisory_emission_target_rate_min",
                "0.05",
                "Lower bound of the target advisory emission rate (fraction of steps)",
            ),
            (
                "advisory_emission_target_rate_max",
                "0.15",
                "Upper bound of the target advisory emission rate (fraction of steps)",
            ),
            // --- Cooldown windows ---
            (
                "advisory_cooldown_per_tool_secs",
                "10",
                "Seconds before the same tool can receive another advisory",
            ),
            (
                "advisory_cooldown_per_advice_secs",
                "600",
                "Seconds before the same advice text may re-surface",
            ),
            // --- Tier-2 fallback ---
            (
                "advisory_tier2_budget_ms",
                "4000",
                "Millisecond budget for a tier-2 advisory lookup before it is skipped",
            ),
            (
                "advisory_tier2_fallback_threshold",
                "0.55",
                "Minimum score for a tier-2 fallback advisory to be accepted",
            ),
            (
                "advisory_tier2_fallback_window",
                "80",
                "Episode lookback window used for tier-2 advisory selection",
            ),
            // --- Phase detection ---
            (
                "phase_halt_timeout_secs",
                "300",
                "Seconds of inactivity before a phase is considered halted",
            ),
            (
                "phase_tool_dominance_threshold",
                "0.60",
                "Fraction of steps a single tool must occupy to be considered dominant",
            ),
            (
                "phase_consecutive_failures_diagnose",
                "2",
                "Consecutive step failures before phase diagnosis is triggered",
            ),
            // --- General thresholds ---
            (
                "importance_threshold",
                "0.5",
                "Minimum importance score for an insight to be eligible for promotion",
            ),
            (
                "evidence_cleanup_interval_secs",
                "3600",
                "How often the evidence table is scanned for expired rows",
            ),
            // --- Truth ledger ---
            (
                "truth_stale_days",
                "60",
                "Days before an unvalidated claim is marked stale",
            ),
            (
                "truth_fact_references",
                "3",
                "Reference count needed to promote a claim to fact status",
            ),
            (
                "truth_rule_episodes",
                "3",
                "Observed episodes needed to promote a claim to rule status",
            ),
            (
                "truth_rule_references",
                "5",
                "Reference count needed to promote a claim to rule status",
            ),
            // --- Domain chip lifecycle ---
            (
                "chip_deprecation_threshold",
                "0.3",
                "Chip success rate below which the chip is marked deprecated",
            ),
            (
                "chip_provisional_min_insights",
                "10",
                "Minimum accumulated insights before a chip leaves provisional status",
            ),
            (
                "chip_provisional_min_confidence",
                "0.7",
                "Minimum confidence before a chip leaves provisional status",
            ),
            // --- Auto-tuner ---
            (
                "tuner_mode",
                "conservative",
                "Tuner aggressiveness: conservative | moderate | aggressive",
            ),
            (
                "tuner_interval_secs",
                "86400",
                "How often the auto-tuner evaluates and adjusts tuneables (seconds)",
            ),
            (
                "tuner_max_change_pct",
                "5",
                "Maximum percentage change the tuner may apply to any single tuneable per run",
            ),
            // --- Control plane ---
            (
                "control_repeat_error_threshold",
                "2",
                "Identical consecutive errors before the control plane intervenes",
            ),
            (
                "control_diff_thrash_threshold",
                "3",
                "Alternating diffs before the control plane intervenes",
            ),
        ];

        for (key, value, description) in defaults {
            sqlx::query(
                r#"
                INSERT OR IGNORE INTO tuneables (key, value, description, updated_at)
                VALUES (?, ?, ?, datetime('now'))
                "#,
            )
            .bind(key)
            .bind(value)
            .bind(description)
            .execute(self.store.pool())
            .await?;
        }

        // Reload so the cache reflects any rows that were just inserted.
        self.refresh().await?;

        Ok(())
    }
}

impl std::fmt::Debug for TuneableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuneableStore").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spin up an isolated `TuneableStore` backed by a throwaway SQLite file.
    async fn setup() -> TuneableStore {
        let path = std::env::temp_dir().join(format!(
            "spacebot_test_tuneables_{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = LearningStore::connect(&path).await.unwrap();
        TuneableStore::new(store).await.unwrap()
    }

    #[tokio::test]
    async fn test_set_and_get_roundtrip() {
        let tuneables = setup().await;

        tuneables
            .set("my_threshold", "0.75", Some("test key"))
            .await
            .unwrap();

        let value = tuneables.get("my_threshold").await.unwrap();
        assert_eq!(value, "0.75");
    }

    #[tokio::test]
    async fn test_get_f64_parses_correctly() {
        let tuneables = setup().await;

        tuneables.set("some_weight", "0.42", None).await.unwrap();

        let parsed = tuneables.get_f64("some_weight").await.unwrap();
        assert!((parsed - 0.42).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_get_i64_parses_correctly() {
        let tuneables = setup().await;

        tuneables.set("some_count", "99", None).await.unwrap();

        let parsed = tuneables.get_i64("some_count").await.unwrap();
        assert_eq!(parsed, 99);
    }

    #[tokio::test]
    async fn test_missing_key_returns_none() {
        let tuneables = setup().await;

        assert!(tuneables.get("nonexistent_key").await.is_none());
        assert!(tuneables.get_f64("nonexistent_key").await.is_none());
        assert!(tuneables.get_i64("nonexistent_key").await.is_none());
    }

    #[tokio::test]
    async fn test_set_overwrites_existing_value() {
        let tuneables = setup().await;

        tuneables.set("overwrite_me", "first", None).await.unwrap();
        tuneables
            .set("overwrite_me", "second", None)
            .await
            .unwrap();

        assert_eq!(tuneables.get("overwrite_me").await.unwrap(), "second");
    }

    #[tokio::test]
    async fn test_description_is_preserved_when_none() {
        let tuneables = setup().await;

        tuneables
            .set("described_key", "initial", Some("my description"))
            .await
            .unwrap();

        // Overwrite value only — description should survive.
        tuneables
            .set("described_key", "updated", None)
            .await
            .unwrap();

        // Verify the stored description is still present via a direct query.
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT description FROM tuneables WHERE key = 'described_key'",
        )
        .fetch_optional(tuneables.store.pool())
        .await
        .unwrap();

        assert_eq!(row.unwrap().0, "my description");
    }

    #[tokio::test]
    async fn test_refresh_picks_up_out_of_band_database_changes() {
        let path = std::env::temp_dir().join(format!(
            "spacebot_test_refresh_{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = LearningStore::connect(&path).await.unwrap();
        let tuneables = TuneableStore::new(store.clone()).await.unwrap();

        // Write directly to the DB, bypassing the cache.
        sqlx::query(
            "INSERT INTO tuneables (key, value, updated_at) VALUES ('direct_write', 'injected', datetime('now'))",
        )
        .execute(store.pool())
        .await
        .unwrap();

        assert!(
            tuneables.get("direct_write").await.is_none(),
            "cache should not yet know about the out-of-band write"
        );

        tuneables.refresh().await.unwrap();

        assert_eq!(
            tuneables.get("direct_write").await.unwrap(),
            "injected"
        );
    }

    #[tokio::test]
    async fn test_populate_defaults_does_not_overwrite_existing_values() {
        let tuneables = setup().await;

        // Pin a custom value for a key that populate_defaults will touch.
        tuneables
            .set("importance_threshold", "0.99", None)
            .await
            .unwrap();

        tuneables.populate_defaults().await.unwrap();

        assert_eq!(
            tuneables.get("importance_threshold").await.unwrap(),
            "0.99",
            "custom value must survive populate_defaults"
        );
    }

    #[tokio::test]
    async fn test_populate_defaults_seeds_all_expected_keys() {
        let tuneables = setup().await;

        tuneables.populate_defaults().await.unwrap();

        let snapshot = tuneables.snapshot().await;

        // Spot-check a representative sample from each group.
        for key in [
            "advisory_score_context_weight",
            "advisory_score_confidence_weight",
            "advisory_score_base_floor",
            "advisory_negative_boost",
            "advisory_failure_boost",
            "advisory_cold_start_boost",
            "advisory_cold_start_end_episode",
            "advisory_cold_start_start_episode",
            "advisory_emission_max_per_event",
            "advisory_emission_target_rate_min",
            "advisory_emission_target_rate_max",
            "advisory_cooldown_per_tool_secs",
            "advisory_cooldown_per_advice_secs",
            "advisory_tier2_budget_ms",
            "advisory_tier2_fallback_threshold",
            "advisory_tier2_fallback_window",
            "phase_halt_timeout_secs",
            "phase_tool_dominance_threshold",
            "phase_consecutive_failures_diagnose",
            "importance_threshold",
            "evidence_cleanup_interval_secs",
            "truth_stale_days",
            "truth_fact_references",
            "truth_rule_episodes",
            "truth_rule_references",
            "chip_deprecation_threshold",
            "chip_provisional_min_insights",
            "chip_provisional_min_confidence",
            "tuner_mode",
            "tuner_interval_secs",
            "tuner_max_change_pct",
            "control_repeat_error_threshold",
            "control_diff_thrash_threshold",
        ] {
            assert!(
                snapshot.contains_key(key),
                "missing expected default key: {key}"
            );
        }

        // Verify a handful of specific default values.
        assert_eq!(snapshot["advisory_score_context_weight"], "0.45");
        assert_eq!(snapshot["advisory_score_confidence_weight"], "0.25");
        assert_eq!(snapshot["tuner_mode"], "conservative");
        assert_eq!(snapshot["tuner_max_change_pct"], "5");
        assert_eq!(snapshot["control_diff_thrash_threshold"], "3");
    }

    #[tokio::test]
    async fn test_snapshot_is_independent_of_subsequent_writes() {
        let tuneables = setup().await;

        tuneables
            .set("snap_key", "original", None)
            .await
            .unwrap();
        let snapshot = tuneables.snapshot().await;

        tuneables
            .set("snap_key", "modified", None)
            .await
            .unwrap();

        // The snapshot captured before the second write must be unaffected.
        assert_eq!(snapshot["snap_key"], "original");
        assert_eq!(tuneables.get("snap_key").await.unwrap(), "modified");
    }
}
