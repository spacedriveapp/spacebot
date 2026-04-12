//! Token usage API endpoints.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::sync::Arc;

// ── Response types ──

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct UsageTotals {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    reasoning_tokens: i64,
    request_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_usd: Option<f64>,
    cost_status: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct UsageByModel {
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    reasoning_tokens: i64,
    request_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct UsageByDay {
    date: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    reasoning_tokens: i64,
    request_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct UsageByAgent {
    agent_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    reasoning_tokens: i64,
    request_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct UsageResponse {
    total: UsageTotals,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_model: Vec<UsageByModel>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_day: Vec<UsageByDay>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    by_agent: Vec<UsageByAgent>,
}

// ── Query params ──

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct UsageQuery {
    /// Filter to one agent.
    #[serde(default)]
    agent_id: Option<String>,
    /// ISO 8601 lower bound (default: 30 days ago).
    #[serde(default)]
    since: Option<String>,
    /// ISO 8601 upper bound.
    #[serde(default)]
    until: Option<String>,
    /// Group by: day, agent, model (comma-separated for multiple).
    #[serde(default)]
    group_by: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct ConversationUsageQuery {
    /// Filter to one agent.
    #[serde(default)]
    agent_id: Option<String>,
}

// ── Helpers ──

fn default_since() -> String {
    let thirty_days_ago = chrono::Utc::now() - chrono::Duration::days(30);
    thirty_days_ago.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Aggregate cost_status: most conservative across all rows.
fn aggregate_cost_status(rows: &[sqlx::sqlite::SqliteRow]) -> String {
    let mut has_unknown = false;
    let mut has_estimated = false;
    for row in rows {
        let status: String = row.get("cost_status");
        match status.as_str() {
            "unknown" => has_unknown = true,
            "estimated" => has_estimated = true,
            _ => {}
        }
    }
    if has_unknown {
        "unknown".to_string()
    } else if has_estimated {
        "estimated".to_string()
    } else {
        "included".to_string()
    }
}

// ── Endpoints ──

/// Aggregated token usage for the instance.
#[utoipa::path(
    get,
    path = "/usage",
    params(UsageQuery),
    responses(
        (status = 200, body = UsageResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "usage",
)]
pub(super) async fn get_usage(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<UsageResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let since = query
        .since
        .as_deref()
        .unwrap_or(&default_since())
        .to_string();
    let until = query.until.clone();
    let group_by = query
        .group_by
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    // Determine which pools to query.
    let target_pools: Vec<(&str, &sqlx::SqlitePool)> = if let Some(ref agent_id) = query.agent_id {
        pools
            .get(agent_id.as_str())
            .map(|pool| vec![(agent_id.as_str(), pool)])
            .unwrap_or_default()
    } else {
        pools.iter().map(|(k, v)| (k.as_str(), v)).collect()
    };

    if target_pools.is_empty() {
        return Ok(Json(UsageResponse {
            total: UsageTotals {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                request_count: 0,
                estimated_cost_usd: None,
                cost_status: "unknown".to_string(),
            },
            by_model: vec![],
            by_day: vec![],
            by_agent: vec![],
        }));
    }

    // Collect totals across all agent pools.
    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut total_cache_read: i64 = 0;
    let mut total_cache_write: i64 = 0;
    let mut total_reasoning: i64 = 0;
    let mut total_requests: i64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut has_unknown = false;
    let mut has_estimated = false;

    let mut all_by_model: std::collections::HashMap<String, UsageByModel> =
        std::collections::HashMap::new();
    let mut all_by_day: std::collections::HashMap<String, UsageByDay> =
        std::collections::HashMap::new();
    let mut all_by_agent: Vec<UsageByAgent> = Vec::new();

    for (agent_id, pool) in &target_pools {
        // Total query
        let mut total_sql = String::from(
            "SELECT COALESCE(SUM(input_tokens), 0) as input_tokens, \
             COALESCE(SUM(output_tokens), 0) as output_tokens, \
             COALESCE(SUM(cache_read_tokens), 0) as cache_read_tokens, \
             COALESCE(SUM(cache_write_tokens), 0) as cache_write_tokens, \
             COALESCE(SUM(reasoning_tokens), 0) as reasoning_tokens, \
             COALESCE(SUM(request_count), 0) as request_count, \
             COALESCE(SUM(estimated_cost_usd), 0.0) as estimated_cost_usd \
             FROM token_usage WHERE recorded_at >= ?",
        );
        let mut bind_values: Vec<String> = vec![since.clone()];

        if let Some(ref u) = until {
            total_sql.push_str(" AND recorded_at <= ?");
            bind_values.push(u.clone());
        }

        let mut q = sqlx::query(&total_sql);
        for v in &bind_values {
            q = q.bind(v);
        }

        let row = q.fetch_one(*pool).await.map_err(|error| {
            tracing::error!(%error, agent_id, "failed to query token usage totals");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let input: i64 = row.get("input_tokens");
        let output: i64 = row.get("output_tokens");
        let cache_read: i64 = row.get("cache_read_tokens");
        let cache_write: i64 = row.get("cache_write_tokens");
        let reasoning: i64 = row.get("reasoning_tokens");
        let requests: i64 = row.get("request_count");
        let cost: f64 = row.get("estimated_cost_usd");

        total_input += input;
        total_output += output;
        total_cache_read += cache_read;
        total_cache_write += cache_write;
        total_reasoning += reasoning;
        total_requests += requests;
        total_cost += cost;

        // Check cost status
        let status_rows =
            sqlx::query("SELECT DISTINCT cost_status FROM token_usage WHERE recorded_at >= ?")
                .bind(&since)
                .fetch_all(*pool)
                .await
                .map_err(|error| {
                    tracing::error!(%error, agent_id, "failed to query cost status");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
        let status = aggregate_cost_status(&status_rows);
        if status == "unknown" {
            has_unknown = true;
        } else if status == "estimated" {
            has_estimated = true;
        }

        // Collect per-agent data
        if group_by.contains(&"agent") && requests > 0 {
            all_by_agent.push(UsageByAgent {
                agent_id: agent_id.to_string(),
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cache_read,
                cache_write_tokens: cache_write,
                reasoning_tokens: reasoning,
                request_count: requests,
                estimated_cost_usd: Some(cost),
            });
        }

        // By model
        if group_by.contains(&"model") {
            let mut model_sql = String::from(
                "SELECT model, \
                 COALESCE(SUM(input_tokens), 0) as input_tokens, \
                 COALESCE(SUM(output_tokens), 0) as output_tokens, \
                 COALESCE(SUM(cache_read_tokens), 0) as cache_read_tokens, \
                 COALESCE(SUM(cache_write_tokens), 0) as cache_write_tokens, \
                 COALESCE(SUM(reasoning_tokens), 0) as reasoning_tokens, \
                 COALESCE(SUM(request_count), 0) as request_count, \
                 COALESCE(SUM(estimated_cost_usd), 0.0) as estimated_cost_usd \
                 FROM token_usage WHERE recorded_at >= ?",
            );
            if until.is_some() {
                model_sql.push_str(" AND recorded_at <= ?");
            }
            model_sql.push_str(" GROUP BY model ORDER BY request_count DESC");

            let mut q = sqlx::query(&model_sql).bind(&since);
            if let Some(ref u) = until {
                q = q.bind(u);
            }

            let rows = q.fetch_all(*pool).await.map_err(|error| {
                tracing::error!(%error, agent_id, "failed to query usage by model");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            for row in rows {
                let model: String = row.get("model");
                let entry = all_by_model.entry(model.clone()).or_insert(UsageByModel {
                    model,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    request_count: 0,
                    estimated_cost_usd: Some(0.0),
                });
                entry.input_tokens += row.get::<i64, _>("input_tokens");
                entry.output_tokens += row.get::<i64, _>("output_tokens");
                entry.cache_read_tokens += row.get::<i64, _>("cache_read_tokens");
                entry.cache_write_tokens += row.get::<i64, _>("cache_write_tokens");
                entry.reasoning_tokens += row.get::<i64, _>("reasoning_tokens");
                entry.request_count += row.get::<i64, _>("request_count");
                if let Some(cost) = entry.estimated_cost_usd.as_mut() {
                    *cost += row.get::<f64, _>("estimated_cost_usd");
                }
            }
        }

        // By day
        if group_by.contains(&"day") {
            let mut day_sql = String::from(
                "SELECT date(recorded_at) as date, \
                 COALESCE(SUM(input_tokens), 0) as input_tokens, \
                 COALESCE(SUM(output_tokens), 0) as output_tokens, \
                 COALESCE(SUM(cache_read_tokens), 0) as cache_read_tokens, \
                 COALESCE(SUM(cache_write_tokens), 0) as cache_write_tokens, \
                 COALESCE(SUM(reasoning_tokens), 0) as reasoning_tokens, \
                 COALESCE(SUM(request_count), 0) as request_count, \
                 COALESCE(SUM(estimated_cost_usd), 0.0) as estimated_cost_usd \
                 FROM token_usage WHERE recorded_at >= ?",
            );
            if until.is_some() {
                day_sql.push_str(" AND recorded_at <= ?");
            }
            day_sql.push_str(" GROUP BY date ORDER BY date");

            let mut q = sqlx::query(&day_sql).bind(&since);
            if let Some(ref u) = until {
                q = q.bind(u);
            }

            let rows = q.fetch_all(*pool).await.map_err(|error| {
                tracing::error!(%error, agent_id, "failed to query usage by day");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            for row in rows {
                let date: String = row.get("date");
                let entry = all_by_day.entry(date.clone()).or_insert(UsageByDay {
                    date,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    request_count: 0,
                    estimated_cost_usd: Some(0.0),
                });
                entry.input_tokens += row.get::<i64, _>("input_tokens");
                entry.output_tokens += row.get::<i64, _>("output_tokens");
                entry.cache_read_tokens += row.get::<i64, _>("cache_read_tokens");
                entry.cache_write_tokens += row.get::<i64, _>("cache_write_tokens");
                entry.reasoning_tokens += row.get::<i64, _>("reasoning_tokens");
                entry.request_count += row.get::<i64, _>("request_count");
                if let Some(cost) = entry.estimated_cost_usd.as_mut() {
                    *cost += row.get::<f64, _>("estimated_cost_usd");
                }
            }
        }
    }

    let cost_status = if has_unknown {
        "unknown"
    } else if has_estimated {
        "estimated"
    } else {
        "included"
    };

    let mut by_model: Vec<UsageByModel> = all_by_model.into_values().collect();
    by_model.sort_by(|a, b| b.request_count.cmp(&a.request_count));

    let mut by_day: Vec<UsageByDay> = all_by_day.into_values().collect();
    by_day.sort_by(|a, b| a.date.cmp(&b.date));

    Ok(Json(UsageResponse {
        total: UsageTotals {
            input_tokens: total_input,
            output_tokens: total_output,
            cache_read_tokens: total_cache_read,
            cache_write_tokens: total_cache_write,
            reasoning_tokens: total_reasoning,
            request_count: total_requests,
            estimated_cost_usd: if cost_status == "unknown" {
                None
            } else {
                Some(total_cost)
            },
            cost_status: cost_status.to_string(),
        },
        by_model,
        by_day,
        by_agent: all_by_agent,
    }))
}

/// Aggregated token usage for a single conversation.
#[utoipa::path(
    get,
    path = "/usage/conversation/{conversation_id}",
    params(
        ("conversation_id" = String, Path, description = "Conversation ID"),
        ConversationUsageQuery,
    ),
    responses(
        (status = 200, body = UsageTotals),
        (status = 500, description = "Internal server error"),
    ),
    tag = "usage",
)]
pub(super) async fn get_conversation_usage(
    State(state): State<Arc<ApiState>>,
    Path(conversation_id): Path<String>,
    Query(query): Query<ConversationUsageQuery>,
) -> Result<Json<UsageTotals>, StatusCode> {
    let pools = state.agent_pools.load();

    let target_pools: Vec<&sqlx::SqlitePool> = if let Some(ref agent_id) = query.agent_id {
        pools.get(agent_id.as_str()).into_iter().collect()
    } else {
        pools.values().collect()
    };

    let mut total_input: i64 = 0;
    let mut total_output: i64 = 0;
    let mut total_cache_read: i64 = 0;
    let mut total_cache_write: i64 = 0;
    let mut total_reasoning: i64 = 0;
    let mut total_requests: i64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut has_unknown = false;
    let mut has_estimated = false;

    for pool in &target_pools {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(input_tokens), 0) as input_tokens, \
             COALESCE(SUM(output_tokens), 0) as output_tokens, \
             COALESCE(SUM(cache_read_tokens), 0) as cache_read_tokens, \
             COALESCE(SUM(cache_write_tokens), 0) as cache_write_tokens, \
             COALESCE(SUM(reasoning_tokens), 0) as reasoning_tokens, \
             COALESCE(SUM(request_count), 0) as request_count, \
             COALESCE(SUM(estimated_cost_usd), 0.0) as estimated_cost_usd \
             FROM token_usage WHERE conversation_id = ?",
        )
        .bind(&conversation_id)
        .fetch_one(*pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to query conversation usage");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        total_input += row.get::<i64, _>("input_tokens");
        total_output += row.get::<i64, _>("output_tokens");
        total_cache_read += row.get::<i64, _>("cache_read_tokens");
        total_cache_write += row.get::<i64, _>("cache_write_tokens");
        total_reasoning += row.get::<i64, _>("reasoning_tokens");
        total_requests += row.get::<i64, _>("request_count");
        total_cost += row.get::<f64, _>("estimated_cost_usd");

        let status_rows = sqlx::query(
            "SELECT DISTINCT cost_status FROM token_usage WHERE conversation_id = ?",
        )
        .bind(&conversation_id)
        .fetch_all(*pool)
        .await
        .map_err(|error| {
            tracing::error!(%error, %conversation_id, "failed to query conversation cost status");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let status = aggregate_cost_status(&status_rows);
        if status == "unknown" {
            has_unknown = true;
        } else if status == "estimated" {
            has_estimated = true;
        }
    }

    let cost_status = if has_unknown {
        "unknown"
    } else if has_estimated {
        "estimated"
    } else {
        "included"
    };

    Ok(Json(UsageTotals {
        input_tokens: total_input,
        output_tokens: total_output,
        cache_read_tokens: total_cache_read,
        cache_write_tokens: total_cache_write,
        reasoning_tokens: total_reasoning,
        request_count: total_requests,
        estimated_cost_usd: if cost_status == "unknown" {
            None
        } else {
            Some(total_cost)
        },
        cost_status: cost_status.to_string(),
    }))
}
