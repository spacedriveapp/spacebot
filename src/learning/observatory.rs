//! Obsidian Observatory: markdown vault generator for learning system visualization.

use crate::learning::LearningStore;

use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the Obsidian Observatory vault generator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservatoryConfig {
    /// Whether the observatory is active. When false, `sync_observatory` is a no-op.
    pub enabled: bool,
    /// Whether to auto-sync the vault on a timer.
    pub auto_sync: bool,
    /// Minimum seconds between auto-sync runs (cooldown).
    pub sync_cooldown_secs: u64,
    /// Directory where the vault is written.
    pub vault_dir: PathBuf,
    /// Whether to generate the Mermaid canvas in the flow dashboard.
    pub generate_canvas: bool,
    /// Maximum number of recent items shown in dashboard summaries.
    pub max_recent_items: usize,
    /// Maximum items per type written to the explore/ directory.
    pub explore_max_per_type: usize,
}

impl Default for ObservatoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_sync: true,
            sync_cooldown_secs: 120,
            vault_dir: PathBuf::from("observatory"),
            generate_canvas: true,
            max_recent_items: 20,
            explore_max_per_type: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Summary of a completed observatory sync run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservatoryReport {
    /// Number of markdown files written (created or overwritten).
    pub files_written: usize,
    /// Wall-clock time spent generating the vault, in milliseconds.
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// Internal row types (for SQL results)
// ---------------------------------------------------------------------------

struct InsightRow {
    id: String,
    category: String,
    content: String,
    reliability: f64,
    confidence: f64,
    validation_count: i64,
    contradiction_count: i64,
    promoted: i64,
    created_at: String,
}

struct DistillationRow {
    id: String,
    distillation_type: String,
    statement: String,
    confidence: f64,
    times_retrieved: i64,
    times_used: i64,
    times_helped: i64,
    created_at: String,
}

struct EpisodeRow {
    id: String,
    task: String,
    predicted_outcome: Option<String>,
    predicted_confidence: f64,
    actual_outcome: Option<String>,
    duration_secs: Option<f64>,
    completed_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Generate (or refresh) the Obsidian-compatible markdown vault from
/// `learning.db` data.
///
/// When `config.enabled` is `false` this function returns immediately with
/// zero files written. All file I/O uses `tokio::fs` and is fully async.
pub async fn sync_observatory(
    store: &LearningStore,
    config: &ObservatoryConfig,
) -> anyhow::Result<ObservatoryReport> {
    if !config.enabled {
        return Ok(ObservatoryReport {
            files_written: 0,
            duration_ms: 0,
        });
    }

    let started_at = std::time::Instant::now();
    let mut files_written: usize = 0;

    // Ensure the top-level vault directory exists.
    tokio::fs::create_dir_all(&config.vault_dir).await?;

    // _observatory/ sub-tree
    let observatory_dir = config.vault_dir.join("_observatory");
    tokio::fs::create_dir_all(&observatory_dir).await?;
    let stages_dir = observatory_dir.join("stages");
    tokio::fs::create_dir_all(&stages_dir).await?;

    // explore/ sub-trees
    let explore_dir = config.vault_dir.join("explore");
    tokio::fs::create_dir_all(explore_dir.join("insights")).await?;
    tokio::fs::create_dir_all(explore_dir.join("distillations")).await?;
    tokio::fs::create_dir_all(explore_dir.join("episodes")).await?;

    // --- flow.md
    files_written +=
        generate_flow_dashboard(store, config, &observatory_dir).await?;

    // --- stages/*.md
    files_written +=
        generate_stage_pages(store, config, &stages_dir).await?;

    // --- explore/**/*.md
    files_written +=
        generate_explorer_pages(store, config, &explore_dir).await?;

    // --- Dashboard.md
    files_written +=
        generate_dashboard(config, &config.vault_dir).await?;

    let duration_ms = started_at.elapsed().as_millis() as u64;

    Ok(ObservatoryReport {
        files_written,
        duration_ms,
    })
}

// ---------------------------------------------------------------------------
// flow.md
// ---------------------------------------------------------------------------

async fn generate_flow_dashboard(
    store: &LearningStore,
    config: &ObservatoryConfig,
    observatory_dir: &Path,
) -> anyhow::Result<usize> {
    // Pull a handful of live stats to populate the dashboard.
    let pool = store.pool();

    let episode_count: i64 = sqlx::query("SELECT COUNT(*) FROM episodes")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let insight_count: i64 = sqlx::query("SELECT COUNT(*) FROM insights")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let distillation_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM distillations")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let promoted_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM insights WHERE promoted = 1")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let chip_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM chip_state WHERE status = 'active'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let quarantine_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM advisory_quarantine")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    // Current phase from learning_state
    let current_phase: String =
        sqlx::query("SELECT value FROM learning_state WHERE key = 'current_phase'")
            .fetch_optional(pool)
            .await
            .map(|maybe_row| {
                maybe_row
                    .map(|row| row.get::<String, _>(0))
                    .unwrap_or_else(|| "unknown".into())
            })
            .unwrap_or_else(|_| "unknown".into());

    let mermaid_block = if config.generate_canvas {
        r#"```mermaid
graph TD
    A[Episodes<br>Layer 1: Outcome Tracking] --> B[Insights<br>Layer 2: Meta-Learning]
    B --> C[Advisory Gate<br>Layer 3: Gated Advice]
    C --> D[Domain Chips<br>Layer 4: Specialization]
    D --> E[Memory Promotion<br>Long-term knowledge]

    style A fill:#1e3a5f,color:#fff
    style B fill:#1a4731,color:#fff
    style C fill:#4a2c00,color:#fff
    style D fill:#3d1a4f,color:#fff
    style E fill:#4a0e0e,color:#fff
```"#
        .to_owned()
    } else {
        String::new()
    };

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: dashboard
---

# 🔭 Observatory Flow

{mermaid_block}

## Current Status

| Signal | Value |
|--------|-------|
| Active Phase | `{current_phase}` |
| Episodes | {episode_count} |
| Insights | {insight_count} |
| Distillations | {distillation_count} |
| Promoted to Memory | {promoted_count} |
| Active Chips | {chip_count} |
| Quarantined | {quarantine_count} |

## Pipeline Layers

- [[stages/outcome-tracking]] — Layer 1: Episode recording & outcome prediction
- [[stages/meta-learning]] — Layer 2: Insight generation & validation
- [[stages/advisory-gating]] — Layer 3: Gated advisory packets
- [[stages/domain-chips]] — Layer 4: Specialised domain chips
- [[stages/metrics]] — Live metric snapshot
- [[stages/tuneables]] — Hot-reloadable configuration

## Explorer

- [[../explore/insights/]] — All cognitive insights
- [[../explore/distillations/]] — Distilled patterns & policies
- [[../explore/episodes/]] — Completed episodes

---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = observatory_dir.join("flow.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

// ---------------------------------------------------------------------------
// stages/*.md
// ---------------------------------------------------------------------------

async fn generate_stage_pages(
    store: &LearningStore,
    _config: &ObservatoryConfig,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let mut files_written: usize = 0;

    files_written += generate_outcome_tracking_page(store, stages_dir).await?;
    files_written += generate_meta_learning_page(store, stages_dir).await?;
    files_written += generate_advisory_gating_page(store, stages_dir).await?;
    files_written += generate_domain_chips_page(store, stages_dir).await?;
    files_written += generate_metrics_page(store, stages_dir).await?;
    files_written += generate_tuneables_page(store, stages_dir).await?;

    Ok(files_written)
}

async fn generate_outcome_tracking_page(
    store: &LearningStore,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();

    let total_episodes: i64 = sqlx::query("SELECT COUNT(*) FROM episodes")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let completed_episodes: i64 =
        sqlx::query("SELECT COUNT(*) FROM episodes WHERE actual_outcome IS NOT NULL")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let success_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM episodes WHERE actual_outcome = 'success'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let failure_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM episodes WHERE actual_outcome = 'failure'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let partial_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM episodes WHERE actual_outcome = 'partial'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let avg_surprise: f64 =
        sqlx::query("SELECT AVG(surprise_level) FROM episodes WHERE surprise_level IS NOT NULL")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<f64, _>(0))
            .unwrap_or(0.0);

    let success_rate = if completed_episodes > 0 {
        success_count as f64 / completed_episodes as f64
    } else {
        0.0
    };
    let success_rate_pct = success_rate * 100.0;

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: stage-health
layer: 1
stage: outcome-tracking
total_episodes: {total_episodes}
completed_episodes: {completed_episodes}
success_rate: {success_rate:.3}
---

# Layer 1: Outcome Tracking

## Health Summary

| Metric | Value |
|--------|-------|
| Total Episodes | {total_episodes} |
| Completed | {completed_episodes} |
| Success | {success_count} |
| Failure | {failure_count} |
| Partial | {partial_count} |
| Success Rate | {success_rate_pct:.1}% |
| Avg Surprise Level | {avg_surprise:.3} |

## Purpose

Layer 1 records every agent episode — the task, predicted outcome, actual
outcome, and duration. The surprise level drives meta-learning priority in
Layer 2.

---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = stages_dir.join("outcome-tracking.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

async fn generate_meta_learning_page(
    store: &LearningStore,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();

    let total_insights: i64 = sqlx::query("SELECT COUNT(*) FROM insights")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let promoted_insights: i64 =
        sqlx::query("SELECT COUNT(*) FROM insights WHERE promoted = 1")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let avg_reliability: f64 =
        sqlx::query("SELECT AVG(reliability) FROM insights")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<f64, _>(0))
            .unwrap_or(0.0);

    let avg_confidence: f64 =
        sqlx::query("SELECT AVG(confidence) FROM insights")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<f64, _>(0))
            .unwrap_or(0.0);

    let total_distillations: i64 =
        sqlx::query("SELECT COUNT(*) FROM distillations")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let contradictions: i64 = sqlx::query("SELECT COUNT(*) FROM contradictions")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let ralph_pass_count: i64 =
        sqlx::query("SELECT COUNT(*) FROM ralph_verdicts WHERE verdict = 'pass'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let ralph_total: i64 = sqlx::query("SELECT COUNT(*) FROM ralph_verdicts")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let ralph_pass_rate = if ralph_total > 0 {
        ralph_pass_count as f64 / ralph_total as f64
    } else {
        0.0
    };
    let ralph_pass_rate_pct = ralph_pass_rate * 100.0;

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: stage-health
layer: 2
stage: meta-learning
total_insights: {total_insights}
promoted_insights: {promoted_insights}
avg_reliability: {avg_reliability:.3}
avg_confidence: {avg_confidence:.3}
ralph_pass_rate: {ralph_pass_rate:.3}
---

# Layer 2: Meta-Learning

## Health Summary

| Metric | Value |
|--------|-------|
| Total Insights | {total_insights} |
| Promoted to Memory | {promoted_insights} |
| Avg Reliability | {avg_reliability:.3} |
| Avg Confidence | {avg_confidence:.3} |
| Total Distillations | {total_distillations} |
| Contradictions | {contradictions} |
| Ralph Pass Rate | {ralph_pass_rate_pct:.1}% ({ralph_pass_count}/{ralph_total}) |

## Purpose

Layer 2 synthesises raw episode data into structured cognitive insights across
8 categories. Meta-Ralph scores each candidate and only high-quality insights
reach Layer 3. Contradictions are tracked and resolved automatically.

---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = stages_dir.join("meta-learning.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

async fn generate_advisory_gating_page(
    store: &LearningStore,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();

    let total_packets: i64 =
        sqlx::query("SELECT COUNT(*) FROM advisory_packets")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let total_emitted: i64 =
        sqlx::query("SELECT SUM(emit_count) FROM advisory_packets")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let total_delivered: i64 =
        sqlx::query("SELECT SUM(deliver_count) FROM advisory_packets")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let total_helpful: i64 =
        sqlx::query("SELECT SUM(helpful_count) FROM advisory_packets")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let avg_effectiveness: f64 =
        sqlx::query("SELECT AVG(effectiveness_score) FROM advisory_packets")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<f64, _>(0))
            .unwrap_or(0.0);

    let quarantine_total: i64 =
        sqlx::query("SELECT COUNT(*) FROM advisory_quarantine")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let precision = if total_delivered > 0 {
        total_helpful as f64 / total_delivered as f64
    } else {
        0.0
    };
    let precision_pct = precision * 100.0;

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: stage-health
layer: 3
stage: advisory-gating
total_packets: {total_packets}
avg_effectiveness: {avg_effectiveness:.3}
precision: {precision:.3}
quarantine_total: {quarantine_total}
---

# Layer 3: Advisory Gating

## Health Summary

| Metric | Value |
|--------|-------|
| Advisory Packets | {total_packets} |
| Total Emitted | {total_emitted} |
| Total Delivered | {total_delivered} |
| Total Helpful | {total_helpful} |
| Precision | {precision_pct:.1}% |
| Avg Effectiveness | {avg_effectiveness:.3} |
| Quarantined Items | {quarantine_total} |

## Purpose

Layer 3 gates insights through Ralph quality scoring and readiness checks
before converting them into advisory packets. Packets are surfaced to the
agent at the right moment based on intent family, tool name, and phase.
Ineffective advisories are automatically suppressed.

---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = stages_dir.join("advisory-gating.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

async fn generate_domain_chips_page(
    store: &LearningStore,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();

    let active_chips: i64 =
        sqlx::query("SELECT COUNT(*) FROM chip_state WHERE status = 'active'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let total_chips: i64 = sqlx::query("SELECT COUNT(*) FROM chip_state")
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i64, _>(0))
        .unwrap_or(0);

    let total_observations: i64 =
        sqlx::query("SELECT SUM(observation_count) FROM chip_state")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let avg_success_rate: f64 =
        sqlx::query("SELECT AVG(success_rate) FROM chip_state WHERE status = 'active'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<f64, _>(0))
            .unwrap_or(0.0);

    let total_chip_insights: i64 =
        sqlx::query("SELECT COUNT(*) FROM chip_insights")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let promoted_chip_insights: i64 =
        sqlx::query("SELECT COUNT(*) FROM chip_insights WHERE promotion_tier = 'long_term'")
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i64, _>(0))
            .unwrap_or(0);

    let avg_success_rate_pct = avg_success_rate * 100.0;

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: stage-health
layer: 4
stage: domain-chips
active_chips: {active_chips}
total_chips: {total_chips}
avg_success_rate: {avg_success_rate:.3}
---

# Layer 4: Domain Chips

## Health Summary

| Metric | Value |
|--------|-------|
| Active Chips | {active_chips} |
| Total Chips | {total_chips} |
| Total Observations | {total_observations} |
| Avg Success Rate | {avg_success_rate_pct:.1}% |
| Chip Insights | {total_chip_insights} |
| Promoted (long-term) | {promoted_chip_insights} |

## Purpose

Layer 4 maintains specialised domain chips — pluggable observers that
accumulate observations for specific tool families or task domains. Chips
score their own insights and promote the highest-quality ones into the
advisory pipeline.

---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = stages_dir.join("domain-chips.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

async fn generate_metrics_page(
    store: &LearningStore,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();

    // Fetch the most recent value for each distinct metric name.
    let rows = sqlx::query(
        r#"
        SELECT metric_name, metric_value, recorded_at
        FROM metrics
        WHERE id IN (
            SELECT MAX(id)
            FROM metrics
            GROUP BY metric_name
        )
        ORDER BY metric_name
        "#,
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut metric_rows = String::new();
    for row in &rows {
        let metric_name: String = row.get("metric_name");
        let metric_value: f64 = row.get("metric_value");
        let recorded_at: String = row.get("recorded_at");
        metric_rows.push_str(&format!(
            "| `{metric_name}` | {metric_value:.4} | {recorded_at} |\n"
        ));
    }
    if metric_rows.is_empty() {
        metric_rows.push_str("| — | — | — |\n");
    }

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: stage-health
stage: metrics
---

# Metrics Snapshot

| Metric | Latest Value | Recorded At |
|--------|-------------|-------------|
{metric_rows}
## Notes

Metrics are appended to `learning.db` on every heartbeat tick. The table
above shows the most recent value per named metric. Open a Dataview query
on `explore/` for trend history.

---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = stages_dir.join("metrics.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

async fn generate_tuneables_page(
    store: &LearningStore,
    stages_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();

    let rows = sqlx::query(
        "SELECT key, value, description, updated_at FROM tuneables ORDER BY key",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut tuneable_rows = String::new();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        let description: String = row
            .try_get::<Option<String>, _>("description")
            .ok()
            .flatten()
            .unwrap_or_default();
        let updated_at: String = row.get("updated_at");
        tuneable_rows.push_str(&format!(
            "| `{key}` | `{value}` | {description} | {updated_at} |\n"
        ));
    }
    if tuneable_rows.is_empty() {
        tuneable_rows.push_str("| — | — | — | — |\n");
    }

    let now = chrono_now_utc();
    let content = format!(
        r#"---
generated: {now}
type: stage-health
stage: tuneables
---

# Tuneables

Hot-reloadable configuration values from `learning.db`. These can be changed
at runtime without restarting the agent; the advisory path picks them up on
the next tick.

| Key | Value | Description | Updated At |
|-----|-------|-------------|------------|
{tuneable_rows}
---
*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = stages_dir.join("tuneables.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

// ---------------------------------------------------------------------------
// explore/**/*.md
// ---------------------------------------------------------------------------

async fn generate_explorer_pages(
    store: &LearningStore,
    config: &ObservatoryConfig,
    explore_dir: &Path,
) -> anyhow::Result<usize> {
    let mut files_written: usize = 0;

    files_written +=
        generate_insight_pages(store, config, &explore_dir.join("insights")).await?;
    files_written +=
        generate_distillation_pages(store, config, &explore_dir.join("distillations")).await?;
    files_written +=
        generate_episode_pages(store, config, &explore_dir.join("episodes")).await?;

    Ok(files_written)
}

async fn generate_insight_pages(
    store: &LearningStore,
    config: &ObservatoryConfig,
    insights_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();
    let limit = config.explore_max_per_type as i64;

    let rows = sqlx::query(
        r#"
        SELECT id, category, content, reliability, confidence,
               validation_count, contradiction_count, promoted, created_at
        FROM insights
        ORDER BY updated_at DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut insight_rows: Vec<InsightRow> = Vec::with_capacity(rows.len());
    for row in rows {
        insight_rows.push(InsightRow {
            id: row.get("id"),
            category: row.get("category"),
            content: row.get("content"),
            reliability: row.get("reliability"),
            confidence: row.get("confidence"),
            validation_count: row.get("validation_count"),
            contradiction_count: row.get("contradiction_count"),
            promoted: row.get("promoted"),
            created_at: row.get("created_at"),
        });
    }

    let mut files_written: usize = 0;
    for insight in &insight_rows {
        let content_first_line = first_line(&insight.content);
        let promoted_label = if insight.promoted != 0 {
            "✅ Promoted to memory"
        } else {
            "⏳ Pending promotion"
        };

        let filename = sanitize_filename(&format!("{}-{}", insight.id, content_first_line));
        let path = insights_dir.join(format!("{filename}.md"));

        let file_content = format!(
            r#"---
type: cognitive-insight
category: {category}
reliability: {reliability:.3}
confidence: {confidence:.3}
promoted: {promoted}
created: {created_at}
---

# {content_first_line}

**Category:** {category}
**Reliability:** {reliability:.3} | **Confidence:** {confidence:.3}
**Validations:** {validation_count} | **Contradictions:** {contradiction_count}
**Status:** {promoted_label}

{content}
"#,
            category = insight.category,
            reliability = insight.reliability,
            confidence = insight.confidence,
            promoted = insight.promoted != 0,
            created_at = insight.created_at,
            content_first_line = content_first_line,
            validation_count = insight.validation_count,
            contradiction_count = insight.contradiction_count,
            promoted_label = promoted_label,
            content = insight.content,
        );

        tokio::fs::write(&path, file_content.as_bytes()).await?;
        files_written += 1;
    }

    Ok(files_written)
}

async fn generate_distillation_pages(
    store: &LearningStore,
    config: &ObservatoryConfig,
    distillations_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();
    let limit = config.explore_max_per_type as i64;

    let rows = sqlx::query(
        r#"
        SELECT id, distillation_type, statement, confidence,
               times_retrieved, times_used, times_helped, created_at
        FROM distillations
        ORDER BY updated_at DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut distillation_rows: Vec<DistillationRow> = Vec::with_capacity(rows.len());
    for row in rows {
        distillation_rows.push(DistillationRow {
            id: row.get("id"),
            distillation_type: row.get("distillation_type"),
            statement: row.get("statement"),
            confidence: row.get("confidence"),
            times_retrieved: row.get("times_retrieved"),
            times_used: row.get("times_used"),
            times_helped: row.get("times_helped"),
            created_at: row.get("created_at"),
        });
    }

    let mut files_written: usize = 0;
    for distillation in &distillation_rows {
        let statement_first_line = first_line(&distillation.statement);
        let filename =
            sanitize_filename(&format!("{}-{}", distillation.id, statement_first_line));
        let path = distillations_dir.join(format!("{filename}.md"));

        let file_content = format!(
            r#"---
type: distillation
distillation_type: {distillation_type}
confidence: {confidence:.3}
times_used: {times_used}
created: {created_at}
---

# {statement_first_line}

**Type:** {distillation_type}
**Confidence:** {confidence:.3}
**Usage:** Retrieved {times_retrieved}x, Used {times_used}x, Helped {times_helped}x

{statement}
"#,
            distillation_type = distillation.distillation_type,
            confidence = distillation.confidence,
            times_used = distillation.times_used,
            created_at = distillation.created_at,
            statement_first_line = statement_first_line,
            times_retrieved = distillation.times_retrieved,
            times_helped = distillation.times_helped,
            statement = distillation.statement,
        );

        tokio::fs::write(&path, file_content.as_bytes()).await?;
        files_written += 1;
    }

    Ok(files_written)
}

async fn generate_episode_pages(
    store: &LearningStore,
    config: &ObservatoryConfig,
    episodes_dir: &Path,
) -> anyhow::Result<usize> {
    let pool = store.pool();
    let limit = config.explore_max_per_type as i64;

    let rows = sqlx::query(
        r#"
        SELECT id, task, predicted_outcome, predicted_confidence,
               actual_outcome, duration_secs, completed_at
        FROM episodes
        WHERE actual_outcome IS NOT NULL
        ORDER BY completed_at DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut episode_rows: Vec<EpisodeRow> = Vec::with_capacity(rows.len());
    for row in rows {
        episode_rows.push(EpisodeRow {
            id: row.get("id"),
            task: row.get("task"),
            predicted_outcome: row.get("predicted_outcome"),
            predicted_confidence: row.get("predicted_confidence"),
            actual_outcome: row.get("actual_outcome"),
            duration_secs: row.get("duration_secs"),
            completed_at: row.get("completed_at"),
        });
    }

    let mut files_written: usize = 0;
    for episode in &episode_rows {
        let id_short = episode.id.get(..8).unwrap_or(&episode.id);
        let task_truncated = truncate_str(&episode.task, 60);
        let predicted_outcome = episode
            .predicted_outcome
            .as_deref()
            .unwrap_or("unknown");
        let actual_outcome = episode
            .actual_outcome
            .as_deref()
            .unwrap_or("unknown");
        let completed_at = episode
            .completed_at
            .as_deref()
            .unwrap_or("ongoing");
        let duration = episode
            .duration_secs
            .map(|seconds| format!("{seconds:.1}s"))
            .unwrap_or_else(|| "—".into());

        let filename = sanitize_filename(&format!("{}-{}", id_short, task_truncated));
        let path = episodes_dir.join(format!("{filename}.md"));

        let file_content = format!(
            r#"---
type: episode
task: {task_truncated}
predicted: {predicted_outcome}
actual: {actual_outcome}
completed: {completed_at}
---

# Episode: {id_short}

**Task:** {task}
**Predicted:** {predicted_outcome} ({predicted_confidence:.2})
**Actual:** {actual_outcome}
**Duration:** {duration}
"#,
            task_truncated = task_truncated,
            predicted_outcome = predicted_outcome,
            actual_outcome = actual_outcome,
            completed_at = completed_at,
            id_short = id_short,
            task = episode.task,
            predicted_confidence = episode.predicted_confidence,
            duration = duration,
        );

        tokio::fs::write(&path, file_content.as_bytes()).await?;
        files_written += 1;
    }

    Ok(files_written)
}

// ---------------------------------------------------------------------------
// Dashboard.md
// ---------------------------------------------------------------------------

async fn generate_dashboard(
    config: &ObservatoryConfig,
    vault_dir: &Path,
) -> anyhow::Result<usize> {
    let recent_limit = config.max_recent_items;
    let now = chrono_now_utc();

    let content = format!(
        r#"---
generated: {now}
type: vault-dashboard
---

# 🔭 Observatory Dashboard

> Live view of the evolving intelligence learning system.
> All queries powered by [Obsidian Dataview](https://github.com/blacksmithgu/obsidian-dataview).

---

## Pipeline

- [[_observatory/flow|Flow Dashboard]] — Architecture & live status
- [[_observatory/stages/outcome-tracking|Layer 1: Outcome Tracking]]
- [[_observatory/stages/meta-learning|Layer 2: Meta-Learning]]
- [[_observatory/stages/advisory-gating|Layer 3: Advisory Gating]]
- [[_observatory/stages/domain-chips|Layer 4: Domain Chips]]
- [[_observatory/stages/metrics|Metrics]]
- [[_observatory/stages/tuneables|Tuneables]]

---

## Recent Insights

```dataview
TABLE category, reliability, confidence
FROM "explore/insights"
SORT created DESC
LIMIT {recent_limit}
```

---

## Top Distillations

```dataview
TABLE distillation_type, confidence, times_used
FROM "explore/distillations"
SORT confidence DESC
LIMIT {recent_limit}
```

---

## Recent Episodes

```dataview
TABLE task, predicted, actual, completed
FROM "explore/episodes"
SORT completed DESC
LIMIT {recent_limit}
```

---

## Promoted Insights

```dataview
TABLE category, reliability, confidence
FROM "explore/insights"
WHERE promoted = true
SORT reliability DESC
LIMIT {recent_limit}
```

---

*Generated by Obsidian Observatory · {now}*
"#
    );

    let path = vault_dir.join("Dashboard.md");
    tokio::fs::write(&path, content.as_bytes()).await?;
    Ok(1)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the first non-empty line of a string, trimmed.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(empty)")
}

/// Truncate a string to at most `max_chars` characters, appending `…` if truncated.
fn truncate_str(text: &str, max_chars: usize) -> String {
    let mut characters = text.chars();
    let collected: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        format!("{collected}…")
    } else {
        collected
    }
}

/// Sanitize a string for use as a filename.
///
/// Replaces whitespace and unsafe characters with hyphens, lowercases the
/// result, and truncates to 80 characters.
fn sanitize_filename(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    // Collapse consecutive hyphens.
    let mut previous_was_hyphen = false;
    let collapsed: String = sanitized
        .chars()
        .filter(|character| {
            if *character == '-' {
                if previous_was_hyphen {
                    return false;
                }
                previous_was_hyphen = true;
            } else {
                previous_was_hyphen = false;
            }
            true
        })
        .collect();

    // Trim leading/trailing hyphens, then truncate.
    let trimmed = collapsed.trim_matches('-');
    truncate_str(trimmed, 80)
}

/// Return the current UTC time as an ISO 8601 string, second-precision.
fn chrono_now_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as YYYY-MM-DDTHH:MM:SSZ using integer arithmetic to avoid
    // pulling in a full datetime library dependency.
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days_since_epoch = hours / 24;
    let second_of_minute = seconds % 60;
    let minute_of_hour = minutes % 60;
    let hour_of_day = hours % 24;

    // Gregorian calendar conversion from days since 1970-01-01.
    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{year:04}-{month:02}-{day:02}T{hour_of_day:02}:{minute_of_hour:02}:{second_of_minute:02}Z"
    )
}

/// Convert a count of days since the Unix epoch (1970-01-01) into a
/// (year, month, day) triple using the proleptic Gregorian calendar.
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    // (civil_from_days, shifted to 1970 epoch).
    days += 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}
