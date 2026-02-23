//! /learned chat command handler and formatters.

use crate::learning::LearningStore;

use sqlx::Row as _;

use anyhow::Result;

// ---------------------------------------------------------------------------
// Command enum
// ---------------------------------------------------------------------------

/// Sub-commands available under `/learned`.
pub enum LearningCommand {
    Summary,
    Distillations,
    Episodes,
    Insights,
    Chips,
    Metrics,
    Contradictions,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a raw chat message into a `LearningCommand`.
///
/// Returns `None` if the text does not begin with `/learned` (case-insensitive).
pub fn parse_learning_command(text: &str) -> Option<LearningCommand> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();

    if !lower.starts_with("/learned") {
        return None;
    }

    let remainder = trimmed["/learned".len()..].trim();

    let command = match remainder {
        "" => LearningCommand::Summary,
        "distillations" => LearningCommand::Distillations,
        "episodes" => LearningCommand::Episodes,
        "insights" => LearningCommand::Insights,
        "chips" => LearningCommand::Chips,
        "metrics" => LearningCommand::Metrics,
        "contradictions" => LearningCommand::Contradictions,
        _ => return None,
    };

    Some(command)
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Handle a parsed `LearningCommand`, query the store, and return a formatted
/// string ready for delivery to the user.
///
/// The `platform` argument drives truncation: discord (≤1950 chars), telegram
/// (≤4000 chars), or unlimited for anything else.
pub async fn handle_learning_command(
    command: LearningCommand,
    store: &LearningStore,
    platform: &str,
) -> Result<String> {
    let body = match command {
        LearningCommand::Summary => format_summary(store).await?,
        LearningCommand::Distillations => format_distillations(store).await?,
        LearningCommand::Episodes => format_episodes(store).await?,
        LearningCommand::Insights => format_insights(store).await?,
        LearningCommand::Chips => format_chips(store).await?,
        LearningCommand::Metrics => format_metrics(store).await?,
        LearningCommand::Contradictions => format_contradictions(store).await?,
    };

    Ok(truncate_for_platform(&body, platform))
}

// ---------------------------------------------------------------------------
// Sub-handlers
// ---------------------------------------------------------------------------

async fn format_summary(store: &LearningStore) -> Result<String> {
    let episode_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM episodes")
        .fetch_one(store.pool())
        .await?
        .try_get("count")?;

    let distillation_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM distillations")
        .fetch_one(store.pool())
        .await?
        .try_get("count")?;

    let insight_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM insights")
        .fetch_one(store.pool())
        .await?
        .try_get("count")?;

    let active_chip_count: i64 = sqlx::query(
        "SELECT COUNT(*) AS count FROM chip_state WHERE status = 'active'",
    )
    .fetch_one(store.pool())
    .await?
    .try_get("count")?;

    let recent_contradiction_count: i64 = sqlx::query(
        "SELECT COUNT(*) AS count FROM contradictions \
         WHERE created_at >= datetime('now', '-30 days')",
    )
    .fetch_one(store.pool())
    .await?
    .try_get("count")?;

    let compounding_rate = latest_metric(store, "compounding_rate").await?;
    let ralph_pass_rate = latest_metric(store, "ralph_pass_rate").await?;
    let advisory_precision = latest_metric(store, "advisory_precision").await?;

    let top_distillations = sqlx::query(
        "SELECT distillation_type, statement, confidence \
         FROM distillations ORDER BY confidence DESC LIMIT 3",
    )
    .fetch_all(store.pool())
    .await?;

    let mut output = String::new();

    output.push_str("**Learning Summary**\n\n");

    output.push_str("**Counts**\n");
    output.push_str(&format!("• Episodes: `{episode_count}`\n"));
    output.push_str(&format!("• Distillations: `{distillation_count}`\n"));
    output.push_str(&format!("• Insights: `{insight_count}`\n"));
    output.push_str(&format!("• Active chips: `{active_chip_count}`\n"));
    output.push_str(&format!(
        "• Recent contradictions (30d): `{recent_contradiction_count}`\n"
    ));
    output.push('\n');

    output.push_str("**Metrics**\n");
    output.push_str(&format!(
        "• Compounding rate: `{}`\n",
        format_optional_metric(compounding_rate)
    ));
    output.push_str(&format!(
        "• Ralph pass rate: `{}`\n",
        format_optional_metric(ralph_pass_rate)
    ));
    output.push_str(&format!(
        "• Advisory precision: `{}`\n",
        format_optional_metric(advisory_precision)
    ));
    output.push('\n');

    if !top_distillations.is_empty() {
        output.push_str("**Top Distillations**\n");
        for row in &top_distillations {
            let distillation_type: String = row.try_get("distillation_type")?;
            let statement: String = row.try_get("statement")?;
            let confidence: f64 = row.try_get("confidence")?;
            output.push_str(&format!(
                "• [{distillation_type}] {statement} (confidence: `{:.2}`)\n",
                confidence
            ));
        }
    }

    Ok(output)
}

async fn format_distillations(store: &LearningStore) -> Result<String> {
    let rows = sqlx::query(
        "SELECT distillation_type, statement, confidence, \
         times_retrieved, times_used, times_helped \
         FROM distillations ORDER BY distillation_type, confidence DESC",
    )
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        return Ok("No distillations recorded yet.".to_string());
    }

    let mut output = String::new();
    output.push_str("**Distillations**\n\n");

    let mut current_type = String::new();

    for row in &rows {
        let distillation_type: String = row.try_get("distillation_type")?;
        let statement: String = row.try_get("statement")?;
        let confidence: f64 = row.try_get("confidence")?;
        let times_retrieved: i64 = row.try_get("times_retrieved")?;
        let times_used: i64 = row.try_get("times_used")?;
        let times_helped: i64 = row.try_get("times_helped")?;

        if distillation_type != current_type {
            if !current_type.is_empty() {
                output.push('\n');
            }
            output.push_str(&format!("**{distillation_type}**\n"));
            current_type = distillation_type.clone();
        }

        output.push_str(&format!(
            "• {statement}\n  confidence: `{:.2}` · retrieved: `{times_retrieved}` · used: `{times_used}` · helped: `{times_helped}`\n",
            confidence
        ));
    }

    Ok(output)
}

async fn format_episodes(store: &LearningStore) -> Result<String> {
    let rows = sqlx::query(
        "SELECT task, predicted_outcome, actual_outcome, duration_secs, started_at \
         FROM episodes ORDER BY started_at DESC LIMIT 10",
    )
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        return Ok("No episodes recorded yet.".to_string());
    }

    let mut output = String::new();
    output.push_str("**Recent Episodes** (last 10)\n\n");

    for (index, row) in rows.iter().enumerate() {
        let task: String = row.try_get("task")?;
        let predicted_outcome: Option<String> = row.try_get("predicted_outcome")?;
        let actual_outcome: Option<String> = row.try_get("actual_outcome")?;
        let duration_secs: Option<f64> = row.try_get("duration_secs")?;
        let started_at: String = row.try_get("started_at")?;

        let predicted = predicted_outcome
            .as_deref()
            .unwrap_or("—");
        let actual = actual_outcome.as_deref().unwrap_or("—");
        let duration = duration_secs
            .map(|seconds| format!("{:.2}s", seconds))
            .unwrap_or_else(|| "—".to_string());

        output.push_str(&format!(
            "**{}. {}**\n  started: `{started_at}` · duration: `{duration}`\n  predicted: `{predicted}` → actual: `{actual}`\n\n",
            index + 1,
            task
        ));
    }

    Ok(output.trim_end().to_string())
}

async fn format_insights(store: &LearningStore) -> Result<String> {
    let rows = sqlx::query(
        "SELECT category, content, reliability \
         FROM insights ORDER BY category, reliability DESC",
    )
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        return Ok("No insights recorded yet.".to_string());
    }

    let mut output = String::new();
    output.push_str("**Insights**\n\n");

    let mut current_category = String::new();

    for row in &rows {
        let category: String = row.try_get("category")?;
        let content: String = row.try_get("content")?;
        let reliability: f64 = row.try_get("reliability")?;

        if category != current_category {
            if !current_category.is_empty() {
                output.push('\n');
            }
            output.push_str(&format!("**{category}**\n"));
            current_category = category.clone();
        }

        output.push_str(&format!(
            "• {content}\n  reliability: `{:.2}`\n",
            reliability
        ));
    }

    Ok(output)
}

async fn format_chips(store: &LearningStore) -> Result<String> {
    let rows = sqlx::query(
        "SELECT chip_id, observation_count, success_rate, status, confidence \
         FROM chip_state ORDER BY observation_count DESC",
    )
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        return Ok("No domain chips recorded yet.".to_string());
    }

    let mut output = String::new();
    output.push_str("**Domain Chips**\n\n");

    for row in &rows {
        let chip_id: String = row.try_get("chip_id")?;
        let observation_count: i64 = row.try_get("observation_count")?;
        let success_rate: f64 = row.try_get("success_rate")?;
        let status: String = row.try_get("status")?;
        let confidence: f64 = row.try_get("confidence")?;

        output.push_str(&format!(
            "**{chip_id}** [{status}]\n  observations: `{observation_count}` · success rate: `{:.2}` · confidence: `{:.2}`\n\n",
            success_rate, confidence
        ));
    }

    Ok(output.trim_end().to_string())
}

async fn format_metrics(store: &LearningStore) -> Result<String> {
    let rows = sqlx::query(
        "SELECT metric_name, metric_value, recorded_at \
         FROM metrics \
         WHERE id IN (SELECT MAX(id) FROM metrics GROUP BY metric_name) \
         ORDER BY metric_name",
    )
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        return Ok("No metrics recorded yet.".to_string());
    }

    let mut output = String::new();
    output.push_str("**Metrics** (latest value per metric)\n\n");

    for row in &rows {
        let metric_name: String = row.try_get("metric_name")?;
        let metric_value: f64 = row.try_get("metric_value")?;
        let recorded_at: String = row.try_get("recorded_at")?;

        output.push_str(&format!(
            "• **{metric_name}**: `{:.2}` (recorded `{recorded_at}`)\n",
            metric_value
        ));
    }

    Ok(output)
}

async fn format_contradictions(store: &LearningStore) -> Result<String> {
    let rows = sqlx::query(
        "SELECT insight_a_id, insight_b_id, contradiction_type, resolution, created_at \
         FROM contradictions ORDER BY created_at DESC LIMIT 10",
    )
    .fetch_all(store.pool())
    .await?;

    if rows.is_empty() {
        return Ok("No contradictions recorded yet.".to_string());
    }

    let mut output = String::new();
    output.push_str("**Contradictions** (last 10)\n\n");

    for (index, row) in rows.iter().enumerate() {
        let insight_a_id: String = row.try_get("insight_a_id")?;
        let insight_b_id: String = row.try_get("insight_b_id")?;
        let contradiction_type: String = row.try_get("contradiction_type")?;
        let resolution: Option<String> = row.try_get("resolution")?;
        let created_at: String = row.try_get("created_at")?;

        let resolution_text = resolution
            .as_deref()
            .unwrap_or("unresolved");

        output.push_str(&format!(
            "**{}. {contradiction_type}** ({created_at})\n  `{insight_a_id}` ↔ `{insight_b_id}`\n  resolution: {resolution_text}\n\n",
            index + 1
        ));
    }

    Ok(output.trim_end().to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fetch the most recently recorded value for a named metric. Returns `None`
/// if no data point exists for that metric name.
async fn latest_metric(store: &LearningStore, metric_name: &str) -> Result<Option<f64>> {
    let row = sqlx::query(
        "SELECT metric_value FROM metrics WHERE metric_name = ? ORDER BY recorded_at DESC LIMIT 1",
    )
    .bind(metric_name)
    .fetch_optional(store.pool())
    .await?;

    match row {
        Some(row) => Ok(Some(row.try_get("metric_value")?)),
        None => Ok(None),
    }
}

/// Format an optional metric value for display.
fn format_optional_metric(value: Option<f64>) -> String {
    match value {
        Some(number) => format!("{:.2}", number),
        None => "—".to_string(),
    }
}

/// Truncate formatted output to fit within platform message limits.
///
/// discord caps at ~2000 chars; leaving a 50-char buffer gives 1950.
/// telegram caps at ~4096 chars; leaving a ~100-char buffer gives 4000.
fn truncate_for_platform(text: &str, platform: &str) -> String {
    let suffix = "\n\n_...truncated, use sub-commands for detail_";

    let limit = match platform {
        "discord" => Some(1950usize),
        "telegram" => Some(4000usize),
        _ => None,
    };

    let Some(max_chars) = limit else {
        return text.to_string();
    };

    if text.len() <= max_chars {
        return text.to_string();
    }

    // Trim to leave room for the suffix.
    let cut_at = max_chars.saturating_sub(suffix.len());
    let trimmed = &text[..cut_at];

    // Break at the last newline so we don't cut mid-word.
    let break_at = trimmed.rfind('\n').unwrap_or(cut_at);

    format!("{}{}", &text[..break_at], suffix)
}
