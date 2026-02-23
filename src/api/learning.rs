//! API endpoint handlers for the learning dashboard.
//!
//! All handlers read from the per-agent `LearningStore` (learning.db) via
//! `state.learning_stores`. Write endpoints (dismiss, correct, promote,
//! update) go through direct SQL so callers get immediate confirmation
//! without needing to go through the learning engine.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return a 404 JSON response for an unknown agent_id.
macro_rules! agent_not_found {
    () => {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "agent not found or learning disabled"})),
        )
            .into_response()
    };
}

/// Return a 500 JSON response for a query failure.
macro_rules! query_error {
    ($error:expr, $context:literal) => {{
        tracing::warn!(error = %$error, $context);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "database query failed"})),
        )
            .into_response()
    }};
}

fn humanize_quarantine_reason(reason: &str) -> String {
    match reason {
        "per_tool_10s" => "Same tool advice was shown recently (10s cooldown)".into(),
        "per_advice_600s" => "This specific advice was shown in the last 10 minutes".into(),
        "agreement_single_source" => "Only one source supports this — needs corroboration".into(),
        "budget_exceeded" => "Already showed max items this turn (emission budget)".into(),
        "context_obvious" => "You just did this action — no need to remind".into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// GET /learning/distillations
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct DistillationResponse {
    pub id: String,
    pub distillation_type: String,
    pub statement: String,
    pub confidence: f64,
    pub triggers: String,
    pub anti_triggers: String,
    pub domains: String,
    pub times_retrieved: i64,
    pub times_used: i64,
    pub times_helped: i64,
    pub validation_count: i64,
    pub contradiction_count: i64,
    pub source_episode_id: Option<String>,
    pub revalidate_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct DistillationsQuery {
    pub agent_id: String,
    pub distillation_type: Option<String>,
    pub min_confidence: Option<f64>,
}

#[derive(Serialize)]
pub(super) struct DistillationsResponse {
    pub distillations: Vec<DistillationResponse>,
}

pub(super) async fn get_distillations(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DistillationsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let min_confidence = query.min_confidence.unwrap_or(0.0);

    let rows = sqlx::query(
        r#"
        SELECT id, distillation_type, statement, confidence,
               triggers, anti_triggers, domains,
               COALESCE(times_retrieved, 0) as times_retrieved,
               COALESCE(times_used, 0) as times_used,
               COALESCE(times_helped, 0) as times_helped,
               COALESCE(validation_count, 0) as validation_count,
               COALESCE(contradiction_count, 0) as contradiction_count,
               source_episode_id, revalidate_by,
               created_at, updated_at
        FROM distillations
        WHERE confidence >= ?
        ORDER BY confidence DESC
        "#,
    )
    .bind(min_confidence)
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_distillations: fetch failed"),
    };

    let distillations: Vec<DistillationResponse> = rows
        .into_iter()
        .filter(|row| {
            let dtype: String = row.get("distillation_type");
            query
                .distillation_type
                .as_deref()
                .map_or(true, |t| dtype == t)
        })
        .map(|row| DistillationResponse {
            id: row.get("id"),
            distillation_type: row.get("distillation_type"),
            statement: row.get("statement"),
            confidence: row.get("confidence"),
            triggers: row.get("triggers"),
            anti_triggers: row.get("anti_triggers"),
            domains: row.get("domains"),
            times_retrieved: row.get("times_retrieved"),
            times_used: row.get("times_used"),
            times_helped: row.get("times_helped"),
            validation_count: row.get("validation_count"),
            contradiction_count: row.get("contradiction_count"),
            source_episode_id: row.get("source_episode_id"),
            revalidate_by: row.get("revalidate_by"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Json(DistillationsResponse { distillations }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/episodes
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct EpisodeResponse {
    pub id: String,
    pub agent_id: String,
    pub trace_id: Option<String>,
    pub channel_id: Option<String>,
    pub process_id: String,
    pub process_type: String,
    pub task: String,
    pub predicted_outcome: Option<String>,
    pub predicted_confidence: f64,
    pub actual_outcome: Option<String>,
    pub actual_confidence: Option<f64>,
    pub surprise_level: Option<f64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_secs: Option<f64>,
    pub phase: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct EpisodesQuery {
    pub agent_id: String,
    pub status: Option<String>,
    #[serde(default = "default_limit_50")]
    pub limit: i64,
}

fn default_limit_50() -> i64 {
    50
}

#[derive(Serialize)]
pub(super) struct EpisodesResponse {
    pub episodes: Vec<EpisodeResponse>,
}

pub(super) async fn get_episodes(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EpisodesQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let limit = query.limit.min(200);
    let completed_only = query.status.as_deref() == Some("completed");

    let sql = if completed_only {
        r#"
        SELECT id, agent_id, trace_id, channel_id, process_id, process_type,
               task, predicted_outcome, COALESCE(predicted_confidence, 0.0) as predicted_confidence,
               actual_outcome, actual_confidence, surprise_level,
               started_at, completed_at, duration_secs, phase, metadata
        FROM episodes
        WHERE completed_at IS NOT NULL
        ORDER BY started_at DESC
        LIMIT ?
        "#
    } else {
        r#"
        SELECT id, agent_id, trace_id, channel_id, process_id, process_type,
               task, predicted_outcome, COALESCE(predicted_confidence, 0.0) as predicted_confidence,
               actual_outcome, actual_confidence, surprise_level,
               started_at, completed_at, duration_secs, phase, metadata
        FROM episodes
        ORDER BY started_at DESC
        LIMIT ?
        "#
    };

    let rows = sqlx::query(sql)
        .bind(limit)
        .fetch_all(store.pool())
        .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_episodes: fetch failed"),
    };

    let episodes = rows
        .into_iter()
        .map(|row| EpisodeResponse {
            id: row.get("id"),
            agent_id: row.get("agent_id"),
            trace_id: row.get("trace_id"),
            channel_id: row.get("channel_id"),
            process_id: row.get("process_id"),
            process_type: row.get("process_type"),
            task: row.get("task"),
            predicted_outcome: row.get("predicted_outcome"),
            predicted_confidence: row.get("predicted_confidence"),
            actual_outcome: row.get("actual_outcome"),
            actual_confidence: row.get("actual_confidence"),
            surprise_level: row.get("surprise_level"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
            duration_secs: row.get("duration_secs"),
            phase: row.get("phase"),
            metadata: row.get("metadata"),
        })
        .collect();

    Json(EpisodesResponse { episodes }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/episode_steps
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct StepResponse {
    pub id: String,
    pub episode_id: String,
    pub call_id: String,
    pub trace_id: Option<String>,
    pub tool_name: Option<String>,
    pub args_summary: Option<String>,
    pub intent: Option<String>,
    pub hypothesis: Option<String>,
    pub prediction: Option<String>,
    pub confidence_before: Option<f64>,
    pub alternatives: Option<String>,
    pub assumptions: Option<String>,
    pub result: Option<String>,
    pub evaluation: Option<String>,
    pub surprise_level: Option<f64>,
    pub confidence_after: Option<f64>,
    pub lesson: Option<String>,
    pub evidence_gathered: i64,
    pub progress_made: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct EpisodeStepsQuery {
    pub agent_id: String,
    pub episode_id: String,
}

#[derive(Serialize)]
pub(super) struct EpisodeStepsResponse {
    pub steps: Vec<StepResponse>,
}

pub(super) async fn get_episode_steps(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EpisodeStepsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let rows = sqlx::query(
        r#"
        SELECT id, episode_id, call_id, trace_id, tool_name, args_summary,
               intent, hypothesis, prediction, confidence_before,
               alternatives, assumptions, result, evaluation, surprise_level,
               confidence_after, lesson,
               COALESCE(evidence_gathered, 0) as evidence_gathered,
               COALESCE(progress_made, 0) as progress_made,
               created_at, completed_at
        FROM steps
        WHERE episode_id = ?
        ORDER BY created_at
        "#,
    )
    .bind(&query.episode_id)
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_episode_steps: fetch failed"),
    };

    let steps = rows
        .into_iter()
        .map(|row| StepResponse {
            id: row.get("id"),
            episode_id: row.get("episode_id"),
            call_id: row.get("call_id"),
            trace_id: row.get("trace_id"),
            tool_name: row.get("tool_name"),
            args_summary: row.get("args_summary"),
            intent: row.get("intent"),
            hypothesis: row.get("hypothesis"),
            prediction: row.get("prediction"),
            confidence_before: row.get("confidence_before"),
            alternatives: row.get("alternatives"),
            assumptions: row.get("assumptions"),
            result: row.get("result"),
            evaluation: row.get("evaluation"),
            surprise_level: row.get("surprise_level"),
            confidence_after: row.get("confidence_after"),
            lesson: row.get("lesson"),
            evidence_gathered: row.get("evidence_gathered"),
            progress_made: row.get("progress_made"),
            created_at: row.get("created_at"),
            completed_at: row.get("completed_at"),
        })
        .collect();

    Json(EpisodeStepsResponse { steps }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/insights
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct InsightResponse {
    pub id: String,
    pub category: String,
    pub content: String,
    pub reliability: f64,
    pub confidence: f64,
    pub validation_count: i64,
    pub contradiction_count: i64,
    pub quality_score: Option<f64>,
    pub advisory_readiness: f64,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub promoted: bool,
    pub promoted_memory_id: Option<String>,
    pub last_validated_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct InsightsQuery {
    pub agent_id: String,
    pub category: Option<String>,
    pub min_reliability: Option<f64>,
}

#[derive(Serialize)]
pub(super) struct InsightsResponse {
    pub insights: Vec<InsightResponse>,
}

pub(super) async fn get_insights(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<InsightsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let min_reliability = query.min_reliability.unwrap_or(0.0);

    let rows = sqlx::query(
        r#"
        SELECT id, category, content, reliability, confidence,
               COALESCE(validation_count, 0) as validation_count,
               COALESCE(contradiction_count, 0) as contradiction_count,
               quality_score,
               COALESCE(advisory_readiness, 0.0) as advisory_readiness,
               source_type, source_id,
               COALESCE(promoted, 0) as promoted,
               promoted_memory_id, last_validated_at,
               created_at, updated_at
        FROM insights
        WHERE reliability >= ?
        ORDER BY reliability DESC
        "#,
    )
    .bind(min_reliability)
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_insights: fetch failed"),
    };

    let insights: Vec<InsightResponse> = rows
        .into_iter()
        .filter(|row| {
            let category: String = row.get("category");
            query
                .category
                .as_deref()
                .map_or(true, |c| category == c)
        })
        .map(|row| InsightResponse {
            id: row.get("id"),
            category: row.get("category"),
            content: row.get("content"),
            reliability: row.get("reliability"),
            confidence: row.get("confidence"),
            validation_count: row.get("validation_count"),
            contradiction_count: row.get("contradiction_count"),
            quality_score: row.get("quality_score"),
            advisory_readiness: row.get("advisory_readiness"),
            source_type: row.get("source_type"),
            source_id: row.get("source_id"),
            promoted: row.get::<i64, _>("promoted") != 0,
            promoted_memory_id: row.get("promoted_memory_id"),
            last_validated_at: row.get("last_validated_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Json(InsightsResponse { insights }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/chips
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct ChipResponse {
    pub chip_id: String,
    pub observation_count: i64,
    pub success_rate: f64,
    pub status: String,
    pub confidence: f64,
    pub last_triggered_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct ChipsQuery {
    pub agent_id: String,
}

#[derive(Serialize)]
pub(super) struct ChipsResponse {
    pub chips: Vec<ChipResponse>,
}

pub(super) async fn get_chips(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ChipsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let rows = sqlx::query(
        r#"
        SELECT chip_id,
               COALESCE(observation_count, 0) as observation_count,
               COALESCE(success_rate, 0.5) as success_rate,
               status,
               COALESCE(confidence, 0.5) as confidence,
               last_triggered_at, created_at, updated_at
        FROM chip_state
        ORDER BY observation_count DESC
        "#,
    )
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_chips: fetch failed"),
    };

    let chips = rows
        .into_iter()
        .map(|row| ChipResponse {
            chip_id: row.get("chip_id"),
            observation_count: row.get("observation_count"),
            success_rate: row.get("success_rate"),
            status: row.get("status"),
            confidence: row.get("confidence"),
            last_triggered_at: row.get("last_triggered_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Json(ChipsResponse { chips }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/chip_insights
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct ChipInsightResponse {
    pub id: String,
    pub chip_id: String,
    pub content: String,
    pub scores: String,
    pub total_score: f64,
    pub promotion_tier: Option<String>,
    pub merged: bool,
    pub created_at: String,
}

#[derive(Deserialize)]
pub(super) struct ChipInsightsQuery {
    pub agent_id: String,
    pub chip_id: String,
}

#[derive(Serialize)]
pub(super) struct ChipInsightsResponse {
    pub insights: Vec<ChipInsightResponse>,
}

pub(super) async fn get_chip_insights(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ChipInsightsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let rows = sqlx::query(
        r#"
        SELECT id, chip_id, content, scores, total_score,
               promotion_tier, COALESCE(merged, 0) as merged, created_at
        FROM chip_insights
        WHERE chip_id = ?
        ORDER BY total_score DESC
        "#,
    )
    .bind(&query.chip_id)
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_chip_insights: fetch failed"),
    };

    let insights = rows
        .into_iter()
        .map(|row| ChipInsightResponse {
            id: row.get("id"),
            chip_id: row.get("chip_id"),
            content: row.get("content"),
            scores: row.get("scores"),
            total_score: row.get("total_score"),
            promotion_tier: row.get("promotion_tier"),
            merged: row.get::<i64, _>("merged") != 0,
            created_at: row.get("created_at"),
        })
        .collect();

    Json(ChipInsightsResponse { insights }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/quarantine
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct QuarantineResponse {
    pub id: String,
    pub source_id: String,
    pub source_type: String,
    pub stage: String,
    pub reason: String,
    pub reason_human: String,
    pub quality_score: Option<f64>,
    pub readiness_score: Option<f64>,
    pub metadata: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize)]
pub(super) struct QuarantineQuery {
    pub agent_id: String,
    #[serde(default = "default_limit_50")]
    pub limit: i64,
}

#[derive(Serialize)]
pub(super) struct QuarantineListResponse {
    pub entries: Vec<QuarantineResponse>,
}

pub(super) async fn get_quarantine(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<QuarantineQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let limit = query.limit.min(200);

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
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_quarantine: fetch failed"),
    };

    let entries = rows
        .into_iter()
        .map(|row| {
            let reason: String = row.get("reason");
            let reason_human = humanize_quarantine_reason(&reason);
            QuarantineResponse {
                id: row.get("id"),
                source_id: row.get("source_id"),
                source_type: row.get("source_type"),
                stage: row.get("stage"),
                reason,
                reason_human,
                quality_score: row.get("quality_score"),
                readiness_score: row.get("readiness_score"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            }
        })
        .collect();

    Json(QuarantineListResponse { entries }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/metrics
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct MetricResponse {
    pub metric_name: String,
    pub metric_value: f64,
    pub recorded_at: String,
}

#[derive(Deserialize)]
pub(super) struct MetricsQuery {
    pub agent_id: String,
}

#[derive(Serialize)]
pub(super) struct MetricsResponse {
    pub metrics: Vec<MetricResponse>,
}

pub(super) async fn get_metrics(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MetricsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    // Latest value per metric_name using GROUP BY + MAX(recorded_at).
    let rows = sqlx::query(
        r#"
        SELECT metric_name, metric_value, recorded_at
        FROM metrics
        WHERE recorded_at = (
            SELECT MAX(m2.recorded_at)
            FROM metrics m2
            WHERE m2.metric_name = metrics.metric_name
        )
        GROUP BY metric_name
        ORDER BY metric_name
        "#,
    )
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_metrics: fetch failed"),
    };

    let metrics = rows
        .into_iter()
        .map(|row| MetricResponse {
            metric_name: row.get("metric_name"),
            metric_value: row.get("metric_value"),
            recorded_at: row.get("recorded_at"),
        })
        .collect();

    Json(MetricsResponse { metrics }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/metrics/history
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct MetricPointResponse {
    pub metric_name: String,
    pub metric_value: f64,
    pub recorded_at: String,
}

#[derive(Deserialize)]
pub(super) struct MetricsHistoryQuery {
    pub agent_id: String,
    pub metric: Option<String>,
    #[serde(default = "default_days_30")]
    pub days: i64,
}

fn default_days_30() -> i64 {
    30
}

#[derive(Serialize)]
pub(super) struct MetricsHistoryResponse {
    pub points: Vec<MetricPointResponse>,
}

pub(super) async fn get_metrics_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MetricsHistoryQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let days = query.days.max(1).min(365);
    let cutoff = format!("-{days} days");

    let rows = if let Some(metric_name) = query.metric.as_deref() {
        sqlx::query(
            r#"
            SELECT metric_name, metric_value, recorded_at
            FROM metrics
            WHERE recorded_at > datetime('now', ?)
              AND metric_name = ?
            ORDER BY recorded_at ASC
            "#,
        )
        .bind(&cutoff)
        .bind(metric_name)
        .fetch_all(store.pool())
        .await
    } else {
        sqlx::query(
            r#"
            SELECT metric_name, metric_value, recorded_at
            FROM metrics
            WHERE recorded_at > datetime('now', ?)
            ORDER BY recorded_at ASC
            "#,
        )
        .bind(&cutoff)
        .fetch_all(store.pool())
        .await
    };

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_metrics_history: fetch failed"),
    };

    let points = rows
        .into_iter()
        .map(|row| MetricPointResponse {
            metric_name: row.get("metric_name"),
            metric_value: row.get("metric_value"),
            recorded_at: row.get("recorded_at"),
        })
        .collect();

    Json(MetricsHistoryResponse { points }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/contradictions
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct ContradictionResponse {
    pub id: String,
    pub insight_a_id: String,
    pub insight_b_id: String,
    pub contradiction_type: String,
    pub resolution: Option<String>,
    pub similarity_score: Option<f64>,
    pub created_at: String,
}

#[derive(Deserialize)]
pub(super) struct ContradictionsQuery {
    pub agent_id: String,
    #[serde(default = "default_limit_50")]
    pub limit: i64,
}

#[derive(Serialize)]
pub(super) struct ContradictionsResponse {
    pub contradictions: Vec<ContradictionResponse>,
}

pub(super) async fn get_contradictions(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ContradictionsQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let limit = query.limit.min(200);

    let rows = sqlx::query(
        r#"
        SELECT id, insight_a_id, insight_b_id, contradiction_type,
               resolution, similarity_score, created_at
        FROM contradictions
        ORDER BY created_at DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_contradictions: fetch failed"),
    };

    let contradictions = rows
        .into_iter()
        .map(|row| ContradictionResponse {
            id: row.get("id"),
            insight_a_id: row.get("insight_a_id"),
            insight_b_id: row.get("insight_b_id"),
            contradiction_type: row.get("contradiction_type"),
            resolution: row.get("resolution"),
            similarity_score: row.get("similarity_score"),
            created_at: row.get("created_at"),
        })
        .collect();

    Json(ContradictionsResponse { contradictions }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/evidence
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct EvidenceResponse {
    pub id: String,
    pub evidence_type: String,
    pub content: String,
    pub episode_id: Option<String>,
    pub tool_name: Option<String>,
    pub retention_hours: i64,
    pub compressed: bool,
    pub expires_at: String,
    pub created_at: String,
}

#[derive(Deserialize)]
pub(super) struct EvidenceQuery {
    pub agent_id: String,
    pub evidence_type: Option<String>,
    pub episode_id: Option<String>,
}

#[derive(Serialize)]
pub(super) struct EvidenceListResponse {
    pub evidence: Vec<EvidenceResponse>,
}

pub(super) async fn get_evidence(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EvidenceQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let rows = sqlx::query(
        r#"
        SELECT id, evidence_type, content, episode_id, tool_name,
               retention_hours, COALESCE(compressed, 0) as compressed,
               expires_at, created_at
        FROM evidence
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_evidence: fetch failed"),
    };

    let evidence: Vec<EvidenceResponse> = rows
        .into_iter()
        .filter(|row| {
            let etype: String = row.get("evidence_type");
            let epid: Option<String> = row.get("episode_id");
            let type_ok = query
                .evidence_type
                .as_deref()
                .map_or(true, |t| etype == t);
            let episode_ok = query
                .episode_id
                .as_deref()
                .map_or(true, |e| epid.as_deref() == Some(e));
            type_ok && episode_ok
        })
        .map(|row| EvidenceResponse {
            id: row.get("id"),
            evidence_type: row.get("evidence_type"),
            content: row.get("content"),
            episode_id: row.get("episode_id"),
            tool_name: row.get("tool_name"),
            retention_hours: row.get("retention_hours"),
            compressed: row.get::<i64, _>("compressed") != 0,
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
        })
        .collect();

    Json(EvidenceListResponse { evidence }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/truth
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct TruthResponse {
    pub id: String,
    pub claim: String,
    pub status: String,
    pub evidence_level: String,
    pub reference_count: i64,
    pub source_insight_id: Option<String>,
    pub last_validated_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct TruthQuery {
    pub agent_id: String,
    pub status: Option<String>,
    pub evidence_level: Option<String>,
}

#[derive(Serialize)]
pub(super) struct TruthListResponse {
    pub entries: Vec<TruthResponse>,
}

pub(super) async fn get_truth(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TruthQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let rows = sqlx::query(
        r#"
        SELECT id, claim, status, evidence_level,
               COALESCE(reference_count, 0) as reference_count,
               source_insight_id, last_validated_at,
               created_at, updated_at
        FROM truth_ledger
        ORDER BY reference_count DESC
        "#,
    )
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_truth: fetch failed"),
    };

    let entries: Vec<TruthResponse> = rows
        .into_iter()
        .filter(|row| {
            let status: String = row.get("status");
            let evidence_level: String = row.get("evidence_level");
            let status_ok = query.status.as_deref().map_or(true, |s| status == s);
            let evidence_ok = query
                .evidence_level
                .as_deref()
                .map_or(true, |e| evidence_level == e);
            status_ok && evidence_ok
        })
        .map(|row| TruthResponse {
            id: row.get("id"),
            claim: row.get("claim"),
            status: row.get("status"),
            evidence_level: row.get("evidence_level"),
            reference_count: row.get("reference_count"),
            source_insight_id: row.get("source_insight_id"),
            last_validated_at: row.get("last_validated_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Json(TruthListResponse { entries }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/tuneables
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct TuneableResponse {
    pub key: String,
    pub value: String,
    pub description: Option<String>,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub(super) struct TuneablesQuery {
    pub agent_id: String,
}

#[derive(Serialize)]
pub(super) struct TuneablesResponse {
    pub tuneables: Vec<TuneableResponse>,
}

pub(super) async fn get_tuneables(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TuneablesQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let rows = sqlx::query(
        r#"
        SELECT key, value, description, updated_at
        FROM tuneables
        ORDER BY key
        "#,
    )
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_tuneables: fetch failed"),
    };

    let tuneables = rows
        .into_iter()
        .map(|row| TuneableResponse {
            key: row.get("key"),
            value: row.get("value"),
            description: row.get("description"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Json(TuneablesResponse { tuneables }).into_response()
}

// ---------------------------------------------------------------------------
// GET /learning/tuneables/history
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct TuneableSnapshotResponse {
    pub id: String,
    pub snapshot: String,
    pub reason: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize)]
pub(super) struct TuneablesHistoryQuery {
    pub agent_id: String,
    #[serde(default = "default_limit_50")]
    pub limit: i64,
}

#[derive(Serialize)]
pub(super) struct TuneablesHistoryResponse {
    pub snapshots: Vec<TuneableSnapshotResponse>,
}

pub(super) async fn get_tuneables_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TuneablesHistoryQuery>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&query.agent_id) else {
        return agent_not_found!();
    };

    let limit = query.limit.min(200);

    let rows = sqlx::query(
        r#"
        SELECT id, snapshot, reason, created_at
        FROM tuneables_snapshots
        ORDER BY created_at DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(store.pool())
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(error) => return query_error!(error, "get_tuneables_history: fetch failed"),
    };

    let snapshots = rows
        .into_iter()
        .map(|row| TuneableSnapshotResponse {
            id: row.get("id"),
            snapshot: row.get("snapshot"),
            reason: row.get("reason"),
            created_at: row.get("created_at"),
        })
        .collect();

    Json(TuneablesHistoryResponse { snapshots }).into_response()
}

// ---------------------------------------------------------------------------
// POST /learning/insights/dismiss
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct DismissInsightRequest {
    pub agent_id: String,
    pub insight_id: String,
}

#[derive(Serialize)]
pub(super) struct SuccessResponse {
    pub success: bool,
}

pub(super) async fn dismiss_insight(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<DismissInsightRequest>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&body.agent_id) else {
        return agent_not_found!();
    };

    let result = sqlx::query("DELETE FROM insights WHERE id = ?")
        .bind(&body.insight_id)
        .execute(store.pool())
        .await;

    match result {
        Ok(_) => Json(SuccessResponse { success: true }).into_response(),
        Err(error) => query_error!(error, "dismiss_insight: delete failed"),
    }
}

// ---------------------------------------------------------------------------
// PUT /learning/insights/correct
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct CorrectInsightRequest {
    pub agent_id: String,
    pub insight_id: String,
    pub content: String,
}

pub(super) async fn correct_insight(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<CorrectInsightRequest>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&body.agent_id) else {
        return agent_not_found!();
    };

    let result = sqlx::query(
        r#"
        UPDATE insights
        SET content = ?,
            reliability = 0.5,
            validation_count = 0,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(&body.content)
    .bind(&body.insight_id)
    .execute(store.pool())
    .await;

    match result {
        Ok(_) => Json(SuccessResponse { success: true }).into_response(),
        Err(error) => query_error!(error, "correct_insight: update failed"),
    }
}

// ---------------------------------------------------------------------------
// PUT /learning/insights/promote
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct PromoteInsightRequest {
    pub agent_id: String,
    pub insight_id: String,
}

pub(super) async fn promote_insight(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<PromoteInsightRequest>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&body.agent_id) else {
        return agent_not_found!();
    };

    let result = sqlx::query(
        r#"
        UPDATE insights
        SET promoted = 1,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(&body.insight_id)
    .execute(store.pool())
    .await;

    match result {
        Ok(_) => Json(SuccessResponse { success: true }).into_response(),
        Err(error) => query_error!(error, "promote_insight: update failed"),
    }
}

// ---------------------------------------------------------------------------
// POST /learning/distillations/dismiss
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct DismissDistillationRequest {
    pub agent_id: String,
    pub distillation_id: String,
}

pub(super) async fn dismiss_distillation(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<DismissDistillationRequest>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&body.agent_id) else {
        return agent_not_found!();
    };

    let result = sqlx::query("DELETE FROM distillations WHERE id = ?")
        .bind(&body.distillation_id)
        .execute(store.pool())
        .await;

    match result {
        Ok(_) => Json(SuccessResponse { success: true }).into_response(),
        Err(error) => query_error!(error, "dismiss_distillation: delete failed"),
    }
}

// ---------------------------------------------------------------------------
// PUT /learning/distillations/correct
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct CorrectDistillationRequest {
    pub agent_id: String,
    pub distillation_id: String,
    pub statement: String,
}

pub(super) async fn correct_distillation(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<CorrectDistillationRequest>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&body.agent_id) else {
        return agent_not_found!();
    };

    let result = sqlx::query(
        r#"
        UPDATE distillations
        SET statement = ?,
            confidence = 0.5,
            validation_count = 0,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(&body.statement)
    .bind(&body.distillation_id)
    .execute(store.pool())
    .await;

    match result {
        Ok(_) => Json(SuccessResponse { success: true }).into_response(),
        Err(error) => query_error!(error, "correct_distillation: update failed"),
    }
}

// ---------------------------------------------------------------------------
// PUT /learning/tuneables
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct UpdateTuneableRequest {
    pub agent_id: String,
    pub key: String,
    pub value: String,
}

pub(super) async fn update_tuneable(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UpdateTuneableRequest>,
) -> impl IntoResponse {
    let stores = state.learning_stores.load();
    let Some(store) = stores.get(&body.agent_id) else {
        return agent_not_found!();
    };

    let result = sqlx::query(
        r#"
        INSERT INTO tuneables (key, value, updated_at)
        VALUES (?, ?, datetime('now'))
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&body.key)
    .bind(&body.value)
    .execute(store.pool())
    .await;

    match result {
        Ok(_) => Json(SuccessResponse { success: true }).into_response(),
        Err(error) => query_error!(error, "update_tuneable: upsert failed"),
    }
}
