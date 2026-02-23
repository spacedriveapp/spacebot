//! Metrics calculation for the evolving intelligence system.

use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use sqlx::Row as _;

use crate::learning::LearningStore;

/// All current metric values as a single snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct MetricSnapshot {
    pub compounding_rate: f64,
    pub reuse_rate: f64,
    pub memory_effectiveness: f64,
    pub loop_suppression: f64,
    pub distillation_quality: f64,
    pub advisory_precision: f64,
    pub ralph_pass_rate: f64,
    pub emission_rate: f64,
    pub evidence_coverage: f64,
}

impl Default for MetricSnapshot {
    fn default() -> Self {
        Self {
            compounding_rate: 0.0,
            reuse_rate: 0.0,
            memory_effectiveness: 0.0,
            loop_suppression: 0.0,
            distillation_quality: 0.0,
            advisory_precision: 0.0,
            ralph_pass_rate: 0.0,
            emission_rate: 0.0,
            evidence_coverage: 0.0,
        }
    }
}

/// A single time-series data point from the metrics table.
#[derive(Debug, Clone, Serialize)]
pub struct MetricPoint {
    pub metric_name: String,
    pub metric_value: f64,
    pub recorded_at: String,
}

/// Calculates all learning system metrics from the learning database.
pub struct MetricsCalculator {
    store: Arc<LearningStore>,
}

impl MetricsCalculator {
    /// Create a new calculator backed by the given store.
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    /// Fraction of completed episodes where a retrieved distillation or insight
    /// was surfaced and trace-linked to a successful outcome.
    ///
    /// Numerator: distinct episodes with `actual_outcome = 'success'` that have
    /// at least one row in `implicit_feedback` linking them (episode_id IS NOT NULL).
    /// Denominator: total completed episodes (actual_outcome IS NOT NULL).
    pub async fn compounding_rate(&self) -> Result<f64> {
        let pool = self.store.pool();

        let total_row = sqlx::query(
            "SELECT COUNT(*) AS count FROM episodes WHERE actual_outcome IS NOT NULL",
        )
        .fetch_one(pool)
        .await?;
        let total_completed: i64 = total_row.get("count");

        if total_completed == 0 {
            return Ok(0.0);
        }

        let linked_row = sqlx::query(
            r#"
            SELECT COUNT(DISTINCT e.id) AS count
            FROM episodes e
            INNER JOIN implicit_feedback f ON f.episode_id = e.id
            WHERE e.actual_outcome = 'success'
              AND f.episode_id IS NOT NULL
            "#,
        )
        .fetch_one(pool)
        .await?;
        let linked_successes: i64 = linked_row.get("count");

        Ok(linked_successes as f64 / total_completed as f64)
    }

    /// Fraction of stored learnings that have been retrieved at least once.
    ///
    /// Numerator: distillations with `times_retrieved > 0` plus insights with
    /// `validation_count > 0`.
    /// Denominator: total count of distillations + insights.
    pub async fn reuse_rate(&self) -> Result<f64> {
        let pool = self.store.pool();

        let distillation_total_row =
            sqlx::query("SELECT COUNT(*) AS count FROM distillations")
                .fetch_one(pool)
                .await?;
        let distillation_total: i64 = distillation_total_row.get("count");

        let insight_total_row = sqlx::query("SELECT COUNT(*) AS count FROM insights")
            .fetch_one(pool)
            .await?;
        let insight_total: i64 = insight_total_row.get("count");

        let denominator = distillation_total + insight_total;
        if denominator == 0 {
            return Ok(0.0);
        }

        let distillation_reused_row = sqlx::query(
            "SELECT COUNT(*) AS count FROM distillations WHERE times_retrieved > 0",
        )
        .fetch_one(pool)
        .await?;
        let distillation_reused: i64 = distillation_reused_row.get("count");

        let insight_reused_row =
            sqlx::query("SELECT COUNT(*) AS count FROM insights WHERE validation_count > 0")
                .fetch_one(pool)
                .await?;
        let insight_reused: i64 = insight_reused_row.get("count");

        let numerator = distillation_reused + insight_reused;
        Ok(numerator as f64 / denominator as f64)
    }

    /// Ratio of helpful retrievals to total retrievals across distillations.
    ///
    /// Only distillations that have been retrieved at least once are included.
    /// Returns 0.0 when no distillations have been retrieved.
    pub async fn memory_effectiveness(&self) -> Result<f64> {
        let pool = self.store.pool();

        let row = sqlx::query(
            r#"
            SELECT
                COALESCE(SUM(times_helped), 0)    AS total_helped,
                COALESCE(SUM(times_retrieved), 0) AS total_retrieved
            FROM distillations
            WHERE times_retrieved > 0
            "#,
        )
        .fetch_one(pool)
        .await?;

        let total_helped: i64 = row.get("total_helped");
        let total_retrieved: i64 = row.get("total_retrieved");

        if total_retrieved == 0 {
            return Ok(0.0);
        }

        Ok(total_helped as f64 / total_retrieved as f64)
    }

    /// Placeholder — loop suppression requires inter-episode comparison logic
    /// that is not yet implemented.
    pub async fn loop_suppression(&self) -> Result<f64> {
        Ok(0.0)
    }

    /// Ratio of helpful uses to total uses across distillations.
    ///
    /// Only distillations that have been used at least once are included.
    pub async fn distillation_quality(&self) -> Result<f64> {
        let pool = self.store.pool();

        let row = sqlx::query(
            r#"
            SELECT
                COALESCE(SUM(times_helped), 0) AS total_helped,
                COALESCE(SUM(times_used), 0)   AS total_used
            FROM distillations
            WHERE times_used > 0
            "#,
        )
        .fetch_one(pool)
        .await?;

        let total_helped: i64 = row.get("total_helped");
        let total_used: i64 = row.get("total_used");

        if total_used == 0 {
            return Ok(0.0);
        }

        Ok(total_helped as f64 / total_used as f64)
    }

    /// Ratio of helpful deliveries to total feedback-bearing deliveries across
    /// advisory packets.
    ///
    /// Only packets where at least one of helpful_count, unhelpful_count, or
    /// noisy_count is greater than zero are included.
    pub async fn advisory_precision(&self) -> Result<f64> {
        let pool = self.store.pool();

        let row = sqlx::query(
            r#"
            SELECT
                COALESCE(SUM(helpful_count), 0)   AS total_helpful,
                COALESCE(SUM(helpful_count + unhelpful_count + noisy_count), 0) AS total_feedback
            FROM advisory_packets
            WHERE helpful_count > 0
               OR unhelpful_count > 0
               OR noisy_count > 0
            "#,
        )
        .fetch_one(pool)
        .await?;

        let total_helpful: i64 = row.get("total_helpful");
        let total_feedback: i64 = row.get("total_feedback");

        if total_feedback == 0 {
            return Ok(0.0);
        }

        Ok(total_helpful as f64 / total_feedback as f64)
    }

    /// Fraction of Meta-Ralph verdicts that were rated QUALITY.
    pub async fn ralph_pass_rate(&self) -> Result<f64> {
        let pool = self.store.pool();

        let total_row = sqlx::query("SELECT COUNT(*) AS count FROM ralph_verdicts")
            .fetch_one(pool)
            .await?;
        let total_verdicts: i64 = total_row.get("count");

        if total_verdicts == 0 {
            return Ok(0.0);
        }

        let quality_row =
            sqlx::query("SELECT COUNT(*) AS count FROM ralph_verdicts WHERE verdict = 'QUALITY'")
                .fetch_one(pool)
                .await?;
        let quality_verdicts: i64 = quality_row.get("count");

        Ok(quality_verdicts as f64 / total_verdicts as f64)
    }

    /// Advisory packets emitted per completed episode, capped at 1.0.
    ///
    /// Numerator: sum of `emit_count` across all advisory packets.
    /// Denominator: count of completed episodes (`actual_outcome IS NOT NULL`).
    pub async fn emission_rate(&self) -> Result<f64> {
        let pool = self.store.pool();

        let episode_row = sqlx::query(
            "SELECT COUNT(*) AS count FROM episodes WHERE actual_outcome IS NOT NULL",
        )
        .fetch_one(pool)
        .await?;
        let completed_episodes: i64 = episode_row.get("count");

        if completed_episodes == 0 {
            return Ok(0.0);
        }

        let emit_row =
            sqlx::query("SELECT COALESCE(SUM(emit_count), 0) AS total FROM advisory_packets")
                .fetch_one(pool)
                .await?;
        let total_emitted: i64 = emit_row.get("total");

        let rate = total_emitted as f64 / completed_episodes as f64;
        Ok(rate.min(1.0))
    }

    /// Fraction of completed episodes that have at least one evidence record.
    pub async fn evidence_coverage(&self) -> Result<f64> {
        let pool = self.store.pool();

        let episode_row = sqlx::query(
            "SELECT COUNT(*) AS count FROM episodes WHERE actual_outcome IS NOT NULL",
        )
        .fetch_one(pool)
        .await?;
        let completed_episodes: i64 = episode_row.get("count");

        if completed_episodes == 0 {
            return Ok(0.0);
        }

        let evidence_row = sqlx::query(
            "SELECT COUNT(DISTINCT episode_id) AS count FROM evidence WHERE episode_id IS NOT NULL",
        )
        .fetch_one(pool)
        .await?;
        let distinct_episodes_with_evidence: i64 = evidence_row.get("count");

        Ok(distinct_episodes_with_evidence as f64 / completed_episodes as f64)
    }

    /// Calculate all metrics and write each one to the `metrics` table.
    pub async fn record_all_metrics(&self) -> Result<()> {
        let compounding_rate = self.compounding_rate().await?;
        let reuse_rate = self.reuse_rate().await?;
        let memory_effectiveness = self.memory_effectiveness().await?;
        let loop_suppression = self.loop_suppression().await?;
        let distillation_quality = self.distillation_quality().await?;
        let advisory_precision = self.advisory_precision().await?;
        let ralph_pass_rate = self.ralph_pass_rate().await?;
        let emission_rate = self.emission_rate().await?;
        let evidence_coverage = self.evidence_coverage().await?;

        self.store.record_metric("compounding_rate", compounding_rate).await?;
        self.store.record_metric("reuse_rate", reuse_rate).await?;
        self.store.record_metric("memory_effectiveness", memory_effectiveness).await?;
        self.store.record_metric("loop_suppression", loop_suppression).await?;
        self.store.record_metric("distillation_quality", distillation_quality).await?;
        self.store.record_metric("advisory_precision", advisory_precision).await?;
        self.store.record_metric("ralph_pass_rate", ralph_pass_rate).await?;
        self.store.record_metric("emission_rate", emission_rate).await?;
        self.store.record_metric("evidence_coverage", evidence_coverage).await?;

        Ok(())
    }

    /// Return time-series data points for a named metric over the last N days.
    ///
    /// Results are ordered oldest-first so they can be plotted directly.
    pub async fn get_metric_history(
        &self,
        metric_name: &str,
        days: u32,
    ) -> Result<Vec<MetricPoint>> {
        let pool = self.store.pool();

        let cutoff = format!("-{days} days");
        let rows = sqlx::query(
            r#"
            SELECT metric_name, metric_value, recorded_at
            FROM metrics
            WHERE metric_name = ?
              AND recorded_at >= datetime('now', ?)
            ORDER BY recorded_at ASC
            "#,
        )
        .bind(metric_name)
        .bind(&cutoff)
        .fetch_all(pool)
        .await?;

        let points = rows
            .into_iter()
            .map(|row| MetricPoint {
                metric_name: row.get("metric_name"),
                metric_value: row.get("metric_value"),
                recorded_at: row.get("recorded_at"),
            })
            .collect();

        Ok(points)
    }

    /// Calculate and return all current metric values without writing to the
    /// database.
    pub async fn get_current_metrics(&self) -> Result<MetricSnapshot> {
        let compounding_rate = self.compounding_rate().await?;
        let reuse_rate = self.reuse_rate().await?;
        let memory_effectiveness = self.memory_effectiveness().await?;
        let loop_suppression = self.loop_suppression().await?;
        let distillation_quality = self.distillation_quality().await?;
        let advisory_precision = self.advisory_precision().await?;
        let ralph_pass_rate = self.ralph_pass_rate().await?;
        let emission_rate = self.emission_rate().await?;
        let evidence_coverage = self.evidence_coverage().await?;

        Ok(MetricSnapshot {
            compounding_rate,
            reuse_rate,
            memory_effectiveness,
            loop_suppression,
            distillation_quality,
            advisory_precision,
            ralph_pass_rate,
            emission_rate,
            evidence_coverage,
        })
    }
}
