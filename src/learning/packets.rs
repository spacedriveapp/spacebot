//! Advisory packet store with multi-dimensional retrieval and effectiveness tracking.
//!
//! Advisory packets are gated advice units derived from insights and distillations.
//! Each packet carries targeting dimensions (intent family, tool name, phase) and
//! accumulates lifecycle counters (emit, deliver, helpful, unhelpful, noisy) that
//! feed a Bayesian effectiveness score.

use crate::learning::LearningStore;

use anyhow::{Context as _, Result};
use sqlx::Row as _;

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single unit of gated advisory advice with full lifecycle counters.
#[derive(Debug, Clone)]
pub struct AdvisoryPacket {
    pub id: String,

    // Targeting dimensions. All optional — NULL means "applies to any".
    pub intent_family: Option<String>,
    pub tool_name: Option<String>,
    pub phase: Option<String>,
    pub context_key: Option<String>,

    // Content and provenance.
    pub advice_text: String,
    pub source_id: String,
    pub source_type: String,
    pub authority_level: String,

    // Scoring and counters.
    pub score: f64,
    pub usage_count: i64,
    pub emit_count: i64,
    pub deliver_count: i64,
    pub helpful_count: i64,
    pub unhelpful_count: i64,
    pub noisy_count: i64,
    pub effectiveness_score: f64,

    // Timestamps.
    pub last_surfaced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Internal sqlx row type
// ---------------------------------------------------------------------------

/// Mirrors the `advisory_packets` table exactly so sqlx can deserialize rows.
///
/// Extra computed columns in a query result (e.g. a ranking alias) are silently
/// ignored by `FromRow` — only fields declared here are extracted.
#[derive(Debug, sqlx::FromRow)]
struct PacketRow {
    id: String,
    intent_family: Option<String>,
    tool_name: Option<String>,
    phase: Option<String>,
    context_key: Option<String>,
    advice_text: String,
    source_id: String,
    source_type: String,
    authority_level: String,
    score: f64,
    usage_count: i64,
    emit_count: i64,
    deliver_count: i64,
    helpful_count: i64,
    unhelpful_count: i64,
    noisy_count: i64,
    effectiveness_score: f64,
    last_surfaced_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<PacketRow> for AdvisoryPacket {
    fn from(row: PacketRow) -> Self {
        Self {
            id: row.id,
            intent_family: row.intent_family,
            tool_name: row.tool_name,
            phase: row.phase,
            context_key: row.context_key,
            advice_text: row.advice_text,
            source_id: row.source_id,
            source_type: row.source_type,
            authority_level: row.authority_level,
            score: row.score,
            usage_count: row.usage_count,
            emit_count: row.emit_count,
            deliver_count: row.deliver_count,
            helpful_count: row.helpful_count,
            unhelpful_count: row.unhelpful_count,
            noisy_count: row.noisy_count,
            effectiveness_score: row.effectiveness_score,
            last_surfaced_at: row.last_surfaced_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// PacketStore
// ---------------------------------------------------------------------------

/// CRUD and analytics operations on the `advisory_packets` table.
pub(crate) struct PacketStore {
    store: Arc<LearningStore>,
}

impl PacketStore {
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    // -----------------------------------------------------------------------
    // Writes
    // -----------------------------------------------------------------------

    /// Persist a packet. Replaces an existing row with the same `id`.
    ///
    /// The caller is responsible for setting `created_at` and `updated_at` to
    /// `datetime('now')` when building a new packet.
    pub async fn save_packet(&self, packet: &AdvisoryPacket) -> Result<()> {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO advisory_packets (
                id, intent_family, tool_name, phase, context_key,
                advice_text, source_id, source_type, authority_level, score,
                usage_count, emit_count, deliver_count,
                helpful_count, unhelpful_count, noisy_count,
                effectiveness_score, last_surfaced_at, created_at, updated_at
            ) VALUES (
                ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?,
                ?, ?, ?,
                ?, ?, ?,
                ?, ?, ?, ?
            )
            "#,
        )
        .bind(&packet.id)
        .bind(&packet.intent_family)
        .bind(&packet.tool_name)
        .bind(&packet.phase)
        .bind(&packet.context_key)
        .bind(&packet.advice_text)
        .bind(&packet.source_id)
        .bind(&packet.source_type)
        .bind(&packet.authority_level)
        .bind(packet.score)
        .bind(packet.usage_count)
        .bind(packet.emit_count)
        .bind(packet.deliver_count)
        .bind(packet.helpful_count)
        .bind(packet.unhelpful_count)
        .bind(packet.noisy_count)
        .bind(packet.effectiveness_score)
        .bind(&packet.last_surfaced_at)
        .bind(&packet.created_at)
        .bind(&packet.updated_at)
        .execute(self.store.pool())
        .await
        .context("failed to save advisory packet")?;

        Ok(())
    }

    /// Increment `emit_count` and stamp `last_surfaced_at` when a packet is
    /// included in an advisory payload heading to the LLM.
    pub async fn record_emit(&self, id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE advisory_packets
            SET emit_count      = emit_count + 1,
                last_surfaced_at = datetime('now'),
                updated_at       = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to record packet emit")?;

        Ok(())
    }

    /// Increment `deliver_count` once the advice has been confirmed delivered
    /// to (acknowledged by) the model context.
    pub async fn record_delivery(&self, id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE advisory_packets
            SET deliver_count = deliver_count + 1,
                updated_at    = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to record packet delivery")?;

        Ok(())
    }

    /// Increment the appropriate feedback counter and recompute effectiveness.
    ///
    /// `helpful` and `noisy` are independent: advice can be both accurate and
    /// overly verbose. `helpful = false` with `noisy = false` increments
    /// `unhelpful_count`.
    pub async fn record_feedback(&self, id: &str, helpful: bool, noisy: bool) -> Result<()> {
        // Increment the relevant counter(s).
        if helpful {
            sqlx::query(
                "UPDATE advisory_packets SET helpful_count = helpful_count + 1, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(id)
            .execute(self.store.pool())
            .await
            .context("failed to increment helpful_count")?;
        } else {
            sqlx::query(
                "UPDATE advisory_packets SET unhelpful_count = unhelpful_count + 1, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(id)
            .execute(self.store.pool())
            .await
            .context("failed to increment unhelpful_count")?;
        }

        if noisy {
            sqlx::query(
                "UPDATE advisory_packets SET noisy_count = noisy_count + 1, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(id)
            .execute(self.store.pool())
            .await
            .context("failed to increment noisy_count")?;
        }

        // Recompute effectiveness now that counters have changed.
        self.update_effectiveness(id).await
    }

    /// Recompute and persist `effectiveness_score` using Bayesian smoothing.
    ///
    /// Formula: `(helpful + 1) / (helpful + unhelpful + noisy + 2)`.
    ///
    /// With zero feedback this yields 0.5 (neutral prior). As feedback
    /// accumulates the score converges toward the true signal ratio. Noisy
    /// counts penalise equally to unhelpful because noisy advice degrades the
    /// user experience even when technically accurate.
    pub async fn update_effectiveness(&self, id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE advisory_packets
            SET effectiveness_score =
                    CAST(helpful_count + 1 AS REAL)
                    / CAST(helpful_count + unhelpful_count + noisy_count + 2 AS REAL),
                updated_at = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(self.store.pool())
        .await
        .context("failed to update effectiveness score")?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// Retrieve packets matching the supplied dimensions, ranked by a
    /// multi-dimensional score.
    ///
    /// Each non-`None` dimension generates a WHERE condition that admits both
    /// exact matches *and* packets with a `NULL` value for that column (general
    /// advisories apply everywhere). The ORDER BY expression then rewards
    /// packets whose columns specifically match the requested dimension:
    ///
    /// | Dimension     | Exact-match bonus |
    /// |---------------|-------------------|
    /// | `tool_name`   | 4.0               |
    /// | `intent_family` | 3.0             |
    /// | `phase`       | 2.0               |
    /// | Effectiveness | × 2.0 multiplier  |
    ///
    /// Packets with `effectiveness_score < 0.3` receive a −2.0 penalty so that
    /// consistently unhelpful advisories are deprioritised even when they
    /// happen to match on multiple dimensions.
    pub async fn retrieve_packets(
        &self,
        intent_family: Option<&str>,
        tool_name: Option<&str>,
        phase: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AdvisoryPacket>> {
        // Build WHERE conditions. Each non-None filter includes NULL rows so
        // that general (untargeted) packets surface alongside specific ones.
        let mut where_conditions: Vec<&str> = Vec::new();
        if intent_family.is_some() {
            where_conditions.push("(intent_family = ? OR intent_family IS NULL)");
        }
        if tool_name.is_some() {
            where_conditions.push("(tool_name = ? OR tool_name IS NULL)");
        }
        if phase.is_some() {
            where_conditions.push("(phase = ? OR phase IS NULL)");
        }

        let where_clause = if where_conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_conditions.join(" AND "))
        };

        // Build the ORDER BY scoring expression. Values are embedded as escaped
        // SQL literals rather than bound parameters so the expression can be
        // composed dynamically without duplicating bindings.
        let score_expression = build_score_expression(intent_family, tool_name, phase);

        let sql = format!(
            r#"
            SELECT *
            FROM advisory_packets
            {where_clause}
            ORDER BY ({score_expression}) DESC
            LIMIT ?
            "#,
        );

        // Bind WHERE parameters in the same order the conditions were added.
        let mut query = sqlx::query_as::<_, PacketRow>(&sql);
        if let Some(family) = intent_family {
            query = query.bind(family);
        }
        if let Some(tool) = tool_name {
            query = query.bind(tool);
        }
        if let Some(p) = phase {
            query = query.bind(p);
        }
        query = query.bind(limit);

        let rows = query
            .fetch_all(self.store.pool())
            .await
            .context("failed to retrieve advisory packets")?;

        Ok(rows.into_iter().map(AdvisoryPacket::from).collect())
    }

    /// Average effectiveness grouped by `source_id`, sorted descending.
    ///
    /// Callers can use this to identify which upstream sources consistently
    /// produce high- or low-quality advice.
    pub async fn get_per_source_effectiveness(&self) -> Result<Vec<(String, f64)>> {
        let rows = sqlx::query(
            r#"
            SELECT source_id, AVG(effectiveness_score) AS avg_effectiveness
            FROM advisory_packets
            GROUP BY source_id
            ORDER BY avg_effectiveness DESC
            "#,
        )
        .fetch_all(self.store.pool())
        .await
        .context("failed to query per-source effectiveness")?;

        let results = rows
            .into_iter()
            .map(|row| {
                let source_id: String = row.get("source_id");
                let avg_effectiveness: f64 = row.get("avg_effectiveness");
                (source_id, avg_effectiveness)
            })
            .collect();

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Compose the ORDER BY scoring expression from the active filter dimensions.
///
/// Values are embedded as single-quote–escaped SQL string literals so the
/// expression can vary structurally without extra bind slots. The input strings
/// originate from internal Rust code paths (not raw HTTP input), and single
/// quotes are escaped before embedding.
fn build_score_expression(
    intent_family: Option<&str>,
    tool_name: Option<&str>,
    phase: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(family) = intent_family {
        let escaped = family.replace('\'', "''");
        parts.push(format!(
            "CASE WHEN intent_family = '{escaped}' THEN 3.0 ELSE 0.0 END"
        ));
    }

    if let Some(tool) = tool_name {
        let escaped = tool.replace('\'', "''");
        parts.push(format!(
            "CASE WHEN tool_name = '{escaped}' THEN 4.0 ELSE 0.0 END"
        ));
    }

    if let Some(p) = phase {
        let escaped = p.replace('\'', "''");
        parts.push(format!(
            "CASE WHEN phase = '{escaped}' THEN 2.0 ELSE 0.0 END"
        ));
    }

    // Effectiveness contribution — always applied.
    parts.push("(effectiveness_score * 2.0)".to_string());

    // Penalise consistently low-performing packets so they rank below general
    // advisories even when they match multiple targeting dimensions.
    parts.push("(CASE WHEN effectiveness_score < 0.3 THEN -2.0 ELSE 0.0 END)".to_string());

    parts.join(" + ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// Pure effectiveness formula used in `update_effectiveness`.
    ///
    /// Mirrors the SQL expression so tests don't require a live database.
    fn compute_effectiveness(helpful: i64, unhelpful: i64, noisy: i64) -> f64 {
        (helpful as f64 + 1.0) / (helpful as f64 + unhelpful as f64 + noisy as f64 + 2.0)
    }

    #[test]
    fn test_effectiveness_neutral_prior_with_no_feedback() {
        // Zero feedback should yield 0.5 — the uninformed prior.
        let score = compute_effectiveness(0, 0, 0);
        assert!(
            (score - 0.5).abs() < f64::EPSILON,
            "expected 0.5 prior, got {score}"
        );
    }

    #[test]
    fn test_effectiveness_converges_high_on_all_helpful() {
        // 10 helpful, 0 negative → (10+1)/(10+0+0+2) = 11/12 ≈ 0.917
        let score = compute_effectiveness(10, 0, 0);
        let expected = 11.0 / 12.0;
        assert!(
            (score - expected).abs() < f64::EPSILON,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn test_effectiveness_converges_low_on_all_unhelpful() {
        // 0 helpful, 10 unhelpful → 1/12 ≈ 0.083
        let score = compute_effectiveness(0, 10, 0);
        let expected = 1.0 / 12.0;
        assert!(
            (score - expected).abs() < f64::EPSILON,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn test_effectiveness_noisy_penalises_equally_to_unhelpful() {
        // Noisy and unhelpful feedback should produce identical scores when
        // counts are equal, because both signal degraded user experience.
        let all_noisy = compute_effectiveness(0, 0, 10);
        let all_unhelpful = compute_effectiveness(0, 10, 0);
        assert!(
            (all_noisy - all_unhelpful).abs() < f64::EPSILON,
            "noisy={all_noisy}, unhelpful={all_unhelpful}: should be equal"
        );
    }

    #[test]
    fn test_effectiveness_mixed_feedback_is_bounded() {
        // Score must remain strictly within (0.0, 1.0) for any input.
        let cases = [
            (0, 0, 0),
            (100, 0, 0),
            (0, 100, 0),
            (0, 0, 100),
            (50, 50, 0),
            (1, 1, 1),
            (1000, 1, 1),
        ];
        for (helpful, unhelpful, noisy) in cases {
            let score = compute_effectiveness(helpful, unhelpful, noisy);
            assert!(
                score > 0.0 && score < 1.0,
                "score {score} out of (0,1) for helpful={helpful} unhelpful={unhelpful} noisy={noisy}"
            );
        }
    }

    #[test]
    fn test_effectiveness_helpful_and_noisy_can_coexist() {
        // Advice can be correct but verbose; both helpful and noisy can be
        // true. The score should still be above 0.5 when helpful dominates.
        let score = compute_effectiveness(5, 0, 2);
        assert!(
            score > 0.5,
            "5 helpful + 2 noisy should still score above neutral, got {score}"
        );
    }

    #[test]
    fn test_score_expression_includes_all_dimensions() {
        use super::build_score_expression;

        let expression =
            build_score_expression(Some("coding"), Some("shell"), Some("pre_tool"));

        assert!(expression.contains("intent_family = 'coding'"));
        assert!(expression.contains("3.0"));
        assert!(expression.contains("tool_name = 'shell'"));
        assert!(expression.contains("4.0"));
        assert!(expression.contains("phase = 'pre_tool'"));
        assert!(expression.contains("2.0"));
        assert!(expression.contains("effectiveness_score * 2.0"));
        assert!(expression.contains("-2.0"));
    }

    #[test]
    fn test_score_expression_omits_missing_dimensions() {
        use super::build_score_expression;

        // With only tool_name supplied, intent and phase weights should not appear.
        let expression = build_score_expression(None, Some("shell"), None);

        assert!(!expression.contains("intent_family"));
        assert!(!expression.contains("phase"));
        assert!(expression.contains("tool_name = 'shell'"));
    }

    #[test]
    fn test_score_expression_escapes_single_quotes() {
        use super::build_score_expression;

        // An input containing a single quote must not break the SQL literal.
        let expression = build_score_expression(None, Some("it's_a_tool"), None);
        assert!(expression.contains("it''s_a_tool"), "single quotes should be escaped");
    }
}
