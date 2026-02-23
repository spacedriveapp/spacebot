//! Truth ledger for tracking claim lifecycle from raw assertion to validated rule.
//!
//! Claims are inserted via [`TruthLedger::record_claim`], accumulate evidence
//! through [`TruthLedger::add_reference`], and promote automatically:
//!
//! - **Claim → Fact** when reference count reaches 3 (Strong evidence).
//! - **Fact → Rule** when the entry also spans ≥3 independent episodes and
//!   has ≥5 total references.
//!
//! Staleness detection, contradiction marking, and revalidation complete the
//! lifecycle.

use crate::learning::{EvidenceLevel, LearningStore, TruthStatus};

use anyhow::{Context as _, Result};
use uuid::Uuid;

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single entry from the truth ledger.
#[derive(Debug, Clone)]
pub struct TruthEntry {
    pub id: String,
    pub claim: String,
    pub status: TruthStatus,
    pub evidence_level: EvidenceLevel,
    pub reference_count: i64,
    pub source_insight_id: Option<String>,
    pub last_validated_at: Option<String>,
}

// ---------------------------------------------------------------------------
// TruthLedger
// ---------------------------------------------------------------------------

/// Manages the truth ledger — claim lifecycle tracking from raw assertion to
/// validated rule.
#[derive(Debug)]
pub struct TruthLedger {
    store: Arc<LearningStore>,
}

impl TruthLedger {
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    /// Insert a new claim into the ledger. Returns the generated ID.
    pub async fn record_claim(
        &self,
        claim: impl Into<String>,
        source_insight_id: Option<String>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let claim = claim.into();

        sqlx::query(
            r#"
            INSERT INTO truth_ledger
                (id, claim, status, evidence_level, reference_count,
                 source_insight_id, last_validated_at, created_at, updated_at)
            VALUES
                (?, ?, 'claim', 'none', 0, ?, NULL, datetime('now'), datetime('now'))
            "#,
        )
        .bind(&id)
        .bind(&claim)
        .bind(&source_insight_id)
        .execute(self.store.pool())
        .await
        .context("failed to insert claim into truth_ledger")?;

        Ok(id)
    }

    /// Increment the reference count, update evidence level, stamp
    /// `last_validated_at`, and auto-promote Claim → Fact when evidence
    /// becomes Strong (≥3 references).
    pub async fn add_reference(&self, id: &str) -> Result<()> {
        let entry = self
            .get_entry(id)
            .await?
            .with_context(|| format!("truth_ledger entry {id} not found"))?;

        let new_reference_count = entry.reference_count + 1;
        let new_evidence_level = evidence_level_from_count(new_reference_count);

        // Claim → Fact only happens on the transition to Strong.
        // Fact, Rule, Stale, and Contradicted entries are not touched here.
        let new_status = if entry.status == TruthStatus::Claim
            && new_evidence_level == EvidenceLevel::Strong
        {
            TruthStatus::Fact
        } else {
            entry.status
        };

        sqlx::query(
            r#"
            UPDATE truth_ledger
            SET reference_count   = ?,
                evidence_level    = ?,
                status            = ?,
                last_validated_at = datetime('now'),
                updated_at        = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(new_reference_count)
        .bind(new_evidence_level.to_string())
        .bind(new_status.to_string())
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to update truth_ledger reference")?;

        Ok(())
    }

    /// Promote a Fact with Strong evidence to a Rule when it has been observed
    /// across enough independent episodes.
    ///
    /// Promotion requires all four conditions simultaneously:
    /// - status is Fact
    /// - evidence_level is Strong
    /// - `episode_count` ≥ 3
    /// - `reference_count` ≥ 5
    ///
    /// Entries that do not meet every condition are left unchanged.
    pub async fn validate_across_episodes(&self, id: &str, episode_count: i64) -> Result<()> {
        let entry = self
            .get_entry(id)
            .await?
            .with_context(|| format!("truth_ledger entry {id} not found"))?;

        let qualifies = entry.status == TruthStatus::Fact
            && entry.evidence_level == EvidenceLevel::Strong
            && episode_count >= 3
            && entry.reference_count >= 5;

        if qualifies {
            sqlx::query(
                r#"
                UPDATE truth_ledger
                SET status     = 'rule',
                    updated_at = datetime('now')
                WHERE id = ?
                "#,
            )
            .bind(id)
            .execute(self.store.pool())
            .await
            .context("failed to promote truth_ledger entry to rule")?;
        }

        Ok(())
    }

    /// Mark an entry as Contradicted. Contradicted entries are retained for
    /// audit but excluded from fact/rule retrieval.
    pub async fn mark_contradicted(&self, id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE truth_ledger
            SET status     = 'contradicted',
                updated_at = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to mark truth_ledger entry as contradicted")?;

        Ok(())
    }

    /// Return IDs of Fact or Rule entries whose `last_validated_at` is older
    /// than `stale_days` days, or NULL (never validated after creation).
    pub async fn check_stale(&self, stale_days: i64) -> Result<Vec<String>> {
        // Build the datetime modifier as a string so it can be passed as a
        // single bind parameter — SQLite does not support dynamic modifiers
        // built entirely from bind values.
        let cutoff_modifier = format!("-{stale_days} days");

        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            SELECT id
            FROM   truth_ledger
            WHERE  status IN ('fact', 'rule')
              AND  (
                       last_validated_at IS NULL
                    OR last_validated_at < datetime('now', ?)
                   )
            "#,
        )
        .bind(&cutoff_modifier)
        .fetch_all(self.store.pool())
        .await
        .context("failed to query stale truth_ledger entries")?;

        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Mark an entry as Stale.
    pub async fn mark_stale(&self, id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE truth_ledger
            SET status     = 'stale',
                updated_at = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to mark truth_ledger entry as stale")?;

        Ok(())
    }

    /// Revalidate a Stale entry — restores it to Fact and stamps
    /// `last_validated_at`. Entries with any other status are left unchanged.
    pub async fn revalidate(&self, id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE truth_ledger
            SET status            = 'fact',
                last_validated_at = datetime('now'),
                updated_at        = datetime('now')
            WHERE id     = ?
              AND status = 'stale'
            "#,
        )
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to revalidate truth_ledger entry")?;

        Ok(())
    }

    /// Fetch a single entry by ID. Returns `None` if not found.
    pub async fn get_entry(&self, id: &str) -> Result<Option<TruthEntry>> {
        let row: Option<(
            String,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT id, claim, status, evidence_level, reference_count,
                   source_insight_id, last_validated_at
            FROM   truth_ledger
            WHERE  id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(self.store.pool())
        .await
        .context("failed to fetch truth_ledger entry")?;

        Ok(row.map(
            |(id, claim, status, evidence_level, reference_count, source_insight_id, last_validated_at)| {
                TruthEntry {
                    id,
                    claim,
                    status: TruthStatus::from_str_lossy(&status),
                    evidence_level: EvidenceLevel::from_str_lossy(&evidence_level),
                    reference_count,
                    source_insight_id,
                    last_validated_at,
                }
            },
        ))
    }

    /// Return all Fact and Rule entries backed by Strong evidence, ordered by
    /// reference count descending.
    pub async fn get_facts_and_rules(&self) -> Result<Vec<TruthEntry>> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT id, claim, status, evidence_level, reference_count,
                   source_insight_id, last_validated_at
            FROM   truth_ledger
            WHERE  status         IN ('fact', 'rule')
              AND  evidence_level  = 'strong'
            ORDER BY reference_count DESC
            "#,
        )
        .fetch_all(self.store.pool())
        .await
        .context("failed to fetch facts and rules from truth_ledger")?;

        Ok(rows
            .into_iter()
            .map(
                |(id, claim, status, evidence_level, reference_count, source_insight_id, last_validated_at)| {
                    TruthEntry {
                        id,
                        claim,
                        status: TruthStatus::from_str_lossy(&status),
                        evidence_level: EvidenceLevel::from_str_lossy(&evidence_level),
                        reference_count,
                        source_insight_id,
                        last_validated_at,
                    }
                },
            )
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a reference count to an evidence level.
///
/// References 1–2 are Weak — a single corroborating source may be
/// coincidence. Three or more constitute consistent, independent corroboration
/// and are classified as Strong.
fn evidence_level_from_count(reference_count: i64) -> EvidenceLevel {
    match reference_count {
        0 => EvidenceLevel::None,
        1..=2 => EvidenceLevel::Weak,
        _ => EvidenceLevel::Strong,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> TruthLedger {
        let path = std::env::temp_dir()
            .join(format!("spacebot_truth_test_{}.db", Uuid::new_v4()));
        let store = LearningStore::connect(&path).await.unwrap();
        TruthLedger::new(store)
    }

    #[tokio::test]
    async fn new_claim_has_no_evidence() {
        let ledger = setup().await;
        let id = ledger
            .record_claim("API calls should be retried on 503", None)
            .await
            .unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Claim);
        assert_eq!(entry.evidence_level, EvidenceLevel::None);
        assert_eq!(entry.reference_count, 0);
        assert!(entry.last_validated_at.is_none());
    }

    #[tokio::test]
    async fn two_references_reach_weak_evidence_without_promotion() {
        let ledger = setup().await;
        let id = ledger.record_claim("Prefer small commits", None).await.unwrap();

        ledger.add_reference(&id).await.unwrap();
        ledger.add_reference(&id).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.evidence_level, EvidenceLevel::Weak);
        // Weak evidence is not enough to leave Claim status.
        assert_eq!(entry.status, TruthStatus::Claim);
        assert_eq!(entry.reference_count, 2);
        assert!(entry.last_validated_at.is_some());
    }

    #[tokio::test]
    async fn third_reference_promotes_claim_to_fact() {
        let ledger = setup().await;
        let id = ledger
            .record_claim("Run tests before pushing", None)
            .await
            .unwrap();

        ledger.add_reference(&id).await.unwrap();
        ledger.add_reference(&id).await.unwrap();
        ledger.add_reference(&id).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.evidence_level, EvidenceLevel::Strong);
        assert_eq!(entry.status, TruthStatus::Fact);
        assert_eq!(entry.reference_count, 3);
        assert!(entry.last_validated_at.is_some());
    }

    #[tokio::test]
    async fn fact_with_enough_episodes_and_references_becomes_rule() {
        let ledger = setup().await;
        let id = ledger
            .record_claim("Validate user input at the boundary", None)
            .await
            .unwrap();

        for _ in 0..5 {
            ledger.add_reference(&id).await.unwrap();
        }

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Fact);

        // 5 references + 3 episodes satisfies all promotion conditions.
        ledger.validate_across_episodes(&id, 3).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Rule);
    }

    #[tokio::test]
    async fn insufficient_episodes_prevents_rule_promotion() {
        let ledger = setup().await;
        let id = ledger.record_claim("Avoid early optimisation", None).await.unwrap();

        for _ in 0..5 {
            ledger.add_reference(&id).await.unwrap();
        }

        // 2 episodes is one short of the required 3.
        ledger.validate_across_episodes(&id, 2).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Fact);
    }

    #[tokio::test]
    async fn insufficient_references_prevents_rule_promotion() {
        let ledger = setup().await;
        let id = ledger.record_claim("Keep functions short", None).await.unwrap();

        // 4 references = Strong, but not the required ≥5.
        for _ in 0..4 {
            ledger.add_reference(&id).await.unwrap();
        }

        ledger.validate_across_episodes(&id, 5).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Fact);
    }

    #[tokio::test]
    async fn mark_contradicted_sets_status() {
        let ledger = setup().await;
        let id = ledger
            .record_claim("Use tabs for indentation", None)
            .await
            .unwrap();

        ledger.mark_contradicted(&id).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Contradicted);
    }

    #[tokio::test]
    async fn stale_detection_marks_and_revalidation_restores_fact() {
        let ledger = setup().await;
        let id = ledger
            .record_claim("Deploy on Fridays is fine", None)
            .await
            .unwrap();

        // Promote to Fact.
        for _ in 0..3 {
            ledger.add_reference(&id).await.unwrap();
        }

        // Back-date last_validated_at to simulate an entry that has not been
        // seen in 10 days, past the 7-day staleness threshold.
        sqlx::query(
            "UPDATE truth_ledger SET last_validated_at = datetime('now', '-10 days') WHERE id = ?",
        )
        .bind(&id)
        .execute(ledger.store.pool())
        .await
        .unwrap();

        let stale_ids = ledger.check_stale(7).await.unwrap();
        assert!(stale_ids.contains(&id), "entry should appear in stale list");

        ledger.mark_stale(&id).await.unwrap();
        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Stale);

        ledger.revalidate(&id).await.unwrap();
        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Fact);
        assert!(entry.last_validated_at.is_some());
    }

    #[tokio::test]
    async fn recently_validated_fact_does_not_appear_as_stale() {
        let ledger = setup().await;
        let id = ledger
            .record_claim("Prefer immutable data structures", None)
            .await
            .unwrap();

        // Promote to Fact — this stamps last_validated_at to now.
        for _ in 0..3 {
            ledger.add_reference(&id).await.unwrap();
        }

        // With a 7-day threshold the freshly-validated entry should be absent.
        let stale_ids = ledger.check_stale(7).await.unwrap();
        assert!(!stale_ids.contains(&id));
    }

    #[tokio::test]
    async fn get_facts_and_rules_excludes_weak_claims_and_contradicted() {
        let ledger = setup().await;

        // Claim with no evidence — excluded.
        let bare_claim_id = ledger.record_claim("Maybe cache this", None).await.unwrap();

        // Claim with Weak evidence — excluded.
        let weak_id = ledger
            .record_claim("Possibly use a queue", None)
            .await
            .unwrap();
        ledger.add_reference(&weak_id).await.unwrap();

        // Fact with Strong evidence — included.
        let fact_id = ledger
            .record_claim("Cache read-heavy endpoints", None)
            .await
            .unwrap();
        for _ in 0..3 {
            ledger.add_reference(&fact_id).await.unwrap();
        }

        // Rule — included.
        let rule_id = ledger
            .record_claim("Wrap all external calls in a timeout", None)
            .await
            .unwrap();
        for _ in 0..5 {
            ledger.add_reference(&rule_id).await.unwrap();
        }
        ledger.validate_across_episodes(&rule_id, 3).await.unwrap();

        // Contradicted Fact — excluded.
        let contra_id = ledger
            .record_claim("Never use async in handlers", None)
            .await
            .unwrap();
        for _ in 0..3 {
            ledger.add_reference(&contra_id).await.unwrap();
        }
        ledger.mark_contradicted(&contra_id).await.unwrap();

        let results = ledger.get_facts_and_rules().await.unwrap();
        let ids: Vec<&str> = results.iter().map(|entry| entry.id.as_str()).collect();

        assert!(ids.contains(&fact_id.as_str()), "fact should be present");
        assert!(ids.contains(&rule_id.as_str()), "rule should be present");
        assert!(!ids.contains(&bare_claim_id.as_str()), "bare claim should be absent");
        assert!(!ids.contains(&weak_id.as_str()), "weak claim should be absent");
        assert!(!ids.contains(&contra_id.as_str()), "contradicted entry should be absent");
    }

    #[tokio::test]
    async fn source_insight_id_is_persisted() {
        let ledger = setup().await;
        let source = "insight-abc-123".to_string();
        let id = ledger
            .record_claim("Limit batch sizes to 100", Some(source))
            .await
            .unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.source_insight_id.as_deref(), Some("insight-abc-123"));
    }

    #[tokio::test]
    async fn revalidate_is_idempotent_for_non_stale_entries() {
        let ledger = setup().await;
        let id = ledger.record_claim("Write tests first", None).await.unwrap();

        // Entry is a Claim, not Stale — revalidate should be a no-op.
        ledger.revalidate(&id).await.unwrap();

        let entry = ledger.get_entry(&id).await.unwrap().unwrap();
        assert_eq!(entry.status, TruthStatus::Claim);
    }
}
