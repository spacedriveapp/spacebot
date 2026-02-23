//! Quarantine audit trail for dropped and suppressed advisories.
//!
//! Every advisory that fails quality gating, is held in cooldown, suppressed by
//! budget limits, or lacks sufficient cross-source agreement is logged here
//! before being discarded. The trail lets the tuner understand why advisories
//! never reach agents and informs threshold calibration.

use crate::learning::LearningStore;

use serde_json::Value as JsonValue;
use sqlx::Row as _;
use uuid::Uuid;

use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The pipeline stage at which an advisory was dropped or suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineStage {
    /// Failed quality scoring during primitive extraction.
    Primitive,
    /// Held back by the per-source emission cooldown.
    Cooldown,
    /// Suppressed due to historical ineffectiveness or noise signals.
    Suppression,
    /// Dropped because the advisory budget for the context window was exhausted.
    Budget,
    /// Insufficient cross-source agreement to promote to an advisory packet.
    Agreement,
}

impl fmt::Display for QuarantineStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            QuarantineStage::Primitive => "primitive",
            QuarantineStage::Cooldown => "cooldown",
            QuarantineStage::Suppression => "suppression",
            QuarantineStage::Budget => "budget",
            QuarantineStage::Agreement => "agreement",
        };
        f.write_str(label)
    }
}

/// A row from the `advisory_quarantine` table.
#[derive(Debug, Clone)]
pub struct QuarantineEntry {
    pub id: String,
    pub source_id: String,
    pub source_type: String,
    /// Stage name as stored in the database (lowercase, matches `QuarantineStage` display).
    pub stage: String,
    pub reason: String,
    pub quality_score: Option<f64>,
    pub readiness_score: Option<f64>,
    pub metadata: Option<String>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Writes and queries the advisory quarantine audit trail.
pub(crate) struct QuarantineStore {
    store: Arc<LearningStore>,
}

impl QuarantineStore {
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    /// Record a dropped advisory in the quarantine trail.
    ///
    /// Errors are logged and swallowed — a failed audit write must never
    /// propagate to the caller or stall the gating pipeline.
    pub async fn log_quarantine(
        &self,
        source_id: &str,
        source_type: &str,
        stage: QuarantineStage,
        reason: &str,
        quality_score: Option<f64>,
        readiness_score: Option<f64>,
        metadata: Option<&JsonValue>,
    ) {
        if let Err(error) = self
            .write_quarantine(
                source_id,
                source_type,
                stage,
                reason,
                quality_score,
                readiness_score,
                metadata,
            )
            .await
        {
            tracing::warn!(%error, %source_id, "failed to write quarantine entry");
        }
    }

    async fn write_quarantine(
        &self,
        source_id: &str,
        source_type: &str,
        stage: QuarantineStage,
        reason: &str,
        quality_score: Option<f64>,
        readiness_score: Option<f64>,
        metadata: Option<&JsonValue>,
    ) -> Result<(), crate::learning::LearningError> {
        let id = Uuid::new_v4().to_string();
        let stage_str = stage.to_string();
        let metadata_str = metadata.map(|value| value.to_string());

        sqlx::query(
            r#"
            INSERT INTO advisory_quarantine
                (id, source_id, source_type, stage, reason, quality_score, readiness_score, metadata, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
            "#,
        )
        .bind(&id)
        .bind(source_id)
        .bind(source_type)
        .bind(&stage_str)
        .bind(reason)
        .bind(quality_score)
        .bind(readiness_score)
        .bind(&metadata_str)
        .execute(self.store.pool())
        .await?;

        Ok(())
    }

    /// Count quarantined entries grouped by stage, ordered alphabetically by stage name.
    pub async fn get_quarantine_stats(
        &self,
    ) -> Result<Vec<(String, i64)>, crate::learning::LearningError> {
        let rows = sqlx::query(
            "SELECT stage, COUNT(*) AS count FROM advisory_quarantine GROUP BY stage ORDER BY stage",
        )
        .fetch_all(self.store.pool())
        .await?;

        let stats = rows
            .into_iter()
            .map(|row| {
                let stage: String = row.get("stage");
                let count: i64 = row.get("count");
                (stage, count)
            })
            .collect::<Vec<_>>();

        Ok(stats)
    }

    /// Fetch the most recently quarantined entries, newest first.
    pub async fn get_recent(
        &self,
        limit: i64,
    ) -> Result<Vec<QuarantineEntry>, crate::learning::LearningError> {
        let rows = sqlx::query(
            r#"
            SELECT id, source_id, source_type, stage, reason,
                   quality_score, readiness_score, metadata, created_at
            FROM advisory_quarantine
            ORDER BY created_at DESC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(self.store.pool())
        .await?;

        let entries = rows
            .into_iter()
            .map(|row| QuarantineEntry {
                id: row.get("id"),
                source_id: row.get("source_id"),
                source_type: row.get("source_type"),
                stage: row.get("stage"),
                reason: row.get("reason"),
                quality_score: row.get("quality_score"),
                readiness_score: row.get("readiness_score"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect::<Vec<_>>();

        Ok(entries)
    }
}

impl fmt::Debug for QuarantineStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuarantineStore").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Spin up an isolated `QuarantineStore` backed by a throwaway SQLite file.
    async fn setup() -> QuarantineStore {
        let path = std::env::temp_dir().join(format!(
            "spacebot_test_quarantine_{}.db",
            uuid::Uuid::new_v4()
        ));
        let store = LearningStore::connect(&path).await.unwrap();
        QuarantineStore::new(store)
    }

    #[tokio::test]
    async fn test_log_and_retrieve() {
        let quarantine = setup().await;

        quarantine
            .log_quarantine(
                "insight-abc",
                "insight",
                QuarantineStage::Budget,
                "advisory budget exhausted for this context window",
                Some(0.72),
                Some(0.45),
                None,
            )
            .await;

        let entries = quarantine.get_recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry.source_id, "insight-abc");
        assert_eq!(entry.source_type, "insight");
        assert_eq!(entry.stage, "budget");
        assert_eq!(
            entry.reason,
            "advisory budget exhausted for this context window"
        );
        assert!(
            (entry.quality_score.unwrap() - 0.72).abs() < f64::EPSILON,
            "quality score mismatch: got {:?}",
            entry.quality_score
        );
        assert!(
            (entry.readiness_score.unwrap() - 0.45).abs() < f64::EPSILON,
            "readiness score mismatch: got {:?}",
            entry.readiness_score
        );
        assert!(entry.metadata.is_none());
    }

    #[tokio::test]
    async fn test_log_with_metadata() {
        let quarantine = setup().await;

        let metadata =
            serde_json::json!({"trigger": "score_below_threshold", "threshold": 0.5});

        quarantine
            .log_quarantine(
                "distill-xyz",
                "distillation",
                QuarantineStage::Agreement,
                "only one source supports this advisory",
                Some(0.31),
                None,
                Some(&metadata),
            )
            .await;

        let entries = quarantine.get_recent(5).await.unwrap();
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry.stage, "agreement");
        assert!(entry.readiness_score.is_none());

        let stored_metadata = entry.metadata.as_deref().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(stored_metadata).unwrap();
        assert_eq!(parsed["trigger"], "score_below_threshold");
    }

    #[tokio::test]
    async fn test_quarantine_stats() {
        let quarantine = setup().await;

        quarantine
            .log_quarantine("a", "insight", QuarantineStage::Primitive, "low score", Some(0.1), None, None)
            .await;
        quarantine
            .log_quarantine("b", "insight", QuarantineStage::Primitive, "low score", Some(0.2), None, None)
            .await;
        quarantine
            .log_quarantine("c", "distillation", QuarantineStage::Cooldown, "still hot", None, None, None)
            .await;

        let stats = quarantine.get_quarantine_stats().await.unwrap();
        assert_eq!(stats.len(), 2);

        let cooldown_count = stats.iter().find(|(stage, _)| stage == "cooldown").unwrap().1;
        assert_eq!(cooldown_count, 1);

        let primitive_count = stats.iter().find(|(stage, _)| stage == "primitive").unwrap().1;
        assert_eq!(primitive_count, 2);
    }

    #[tokio::test]
    async fn test_get_recent_respects_limit() {
        let quarantine = setup().await;

        for index in 0..5 {
            quarantine
                .log_quarantine(
                    &format!("source-{index}"),
                    "insight",
                    QuarantineStage::Suppression,
                    "suppressed by effectiveness history",
                    Some(0.4),
                    Some(0.3),
                    None,
                )
                .await;
        }

        let entries = quarantine.get_recent(3).await.unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_stage_display() {
        assert_eq!(QuarantineStage::Primitive.to_string(), "primitive");
        assert_eq!(QuarantineStage::Cooldown.to_string(), "cooldown");
        assert_eq!(QuarantineStage::Suppression.to_string(), "suppression");
        assert_eq!(QuarantineStage::Budget.to_string(), "budget");
        assert_eq!(QuarantineStage::Agreement.to_string(), "agreement");
    }
}
