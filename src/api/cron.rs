use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub(super) struct CronQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct CronExecutionsQuery {
    agent_id: String,
    #[serde(default)]
    cron_id: Option<String>,
    #[serde(default = "default_cron_executions_limit")]
    limit: i64,
}

fn default_cron_executions_limit() -> i64 {
    50
}

#[derive(Deserialize, Debug)]
pub(super) struct CreateCronRequest {
    agent_id: String,
    id: String,
    prompt: String,
    #[serde(default = "default_interval")]
    interval_secs: u64,
    delivery_target: String,
    #[serde(default)]
    active_start_hour: Option<u8>,
    #[serde(default)]
    active_end_hour: Option<u8>,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    run_once: bool,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

fn default_interval() -> u64 {
    3600
}

fn default_enabled() -> bool {
    true
}

#[derive(Deserialize)]
pub(super) struct DeleteCronRequest {
    agent_id: String,
    cron_id: String,
}

#[derive(Deserialize)]
pub(super) struct TriggerCronRequest {
    agent_id: String,
    cron_id: String,
}

#[derive(Deserialize)]
pub(super) struct ToggleCronRequest {
    agent_id: String,
    cron_id: String,
    enabled: bool,
}

#[derive(Serialize)]
struct CronJobWithStats {
    id: String,
    prompt: String,
    interval_secs: u64,
    delivery_target: String,
    enabled: bool,
    run_once: bool,
    active_hours: Option<(u8, u8)>,
    timeout_secs: Option<u64>,
    success_count: u64,
    failure_count: u64,
    last_executed_at: Option<String>,
}

#[derive(Serialize)]
pub(super) struct CronListResponse {
    jobs: Vec<CronJobWithStats>,
    timezone: String,
}

#[derive(Serialize)]
pub(super) struct CronExecutionsResponse {
    executions: Vec<crate::cron::CronExecutionEntry>,
}

#[derive(Serialize)]
pub(super) struct CronActionResponse {
    success: bool,
    message: String,
}

/// List all cron jobs for an agent with execution statistics.
pub(super) async fn list_cron_jobs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CronQuery>,
) -> Result<Json<CronListResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let schedulers = state.cron_schedulers.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let scheduler = schedulers
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let configs = store.load_all_unfiltered().await.map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, "failed to load cron jobs");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut jobs = Vec::new();
    for config in configs {
        let stats = store
            .get_execution_stats(&config.id)
            .await
            .unwrap_or_default();

        jobs.push(CronJobWithStats {
            id: config.id,
            prompt: config.prompt,
            interval_secs: config.interval_secs,
            delivery_target: config.delivery_target,
            enabled: config.enabled,
            run_once: config.run_once,
            active_hours: config.active_hours,
            timeout_secs: config.timeout_secs,
            success_count: stats.success_count,
            failure_count: stats.failure_count,
            last_executed_at: stats.last_executed_at,
        });
    }

    Ok(Json(CronListResponse {
        jobs,
        timezone: scheduler.cron_timezone_label(),
    }))
}

/// Get execution history for cron jobs.
pub(super) async fn cron_executions(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CronExecutionsQuery>,
) -> Result<Json<CronExecutionsResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let executions = if let Some(cron_id) = query.cron_id {
        store
            .load_executions(&cron_id, query.limit)
            .await
            .map_err(|error| {
                tracing::warn!(%error, agent_id = %query.agent_id, cron_id = %cron_id, "failed to load cron executions");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    } else {
        store
            .load_all_executions(query.limit)
            .await
            .map_err(|error| {
                tracing::warn!(%error, agent_id = %query.agent_id, "failed to load cron executions");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    };

    Ok(Json(CronExecutionsResponse { executions }))
}

/// Create or update a cron job.
pub(super) async fn create_or_update_cron(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateCronRequest>,
) -> Result<Json<CronActionResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let schedulers = state.cron_schedulers.load();

    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let scheduler = schedulers
        .get(&request.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let active_hours = match (request.active_start_hour, request.active_end_hour) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => None,
    };

    let config = crate::cron::CronConfig {
        id: request.id.clone(),
        prompt: request.prompt,
        interval_secs: request.interval_secs,
        delivery_target: request.delivery_target,
        active_hours,
        enabled: request.enabled,
        run_once: request.run_once,
        timeout_secs: request.timeout_secs,
    };

    store.save(&config).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.id, "failed to save cron job");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    scheduler.register(config).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.id, "failed to register cron job");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(CronActionResponse {
        success: true,
        message: format!("Cron job '{}' saved successfully", request.id),
    }))
}

/// Delete a cron job.
pub(super) async fn delete_cron(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeleteCronRequest>,
) -> Result<Json<CronActionResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let schedulers = state.cron_schedulers.load();
    let scheduler = schedulers
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    scheduler.unregister(&query.cron_id).await;

    store.delete(&query.cron_id).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, cron_id = %query.cron_id, "failed to delete cron job");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(CronActionResponse {
        success: true,
        message: format!("Cron job '{}' deleted successfully", query.cron_id),
    }))
}

/// Trigger a cron job immediately.
pub(super) async fn trigger_cron(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TriggerCronRequest>,
) -> Result<Json<CronActionResponse>, StatusCode> {
    let schedulers = state.cron_schedulers.load();
    let scheduler = schedulers
        .get(&request.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    scheduler.trigger_now(&request.cron_id).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.cron_id, "failed to trigger cron job");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(CronActionResponse {
        success: true,
        message: format!("Cron job '{}' triggered", request.cron_id),
    }))
}

/// Enable or disable a cron job.
pub(super) async fn toggle_cron(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ToggleCronRequest>,
) -> Result<Json<CronActionResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let schedulers = state.cron_schedulers.load();
    let scheduler = schedulers
        .get(&request.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    store.update_enabled(&request.cron_id, request.enabled).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.cron_id, "failed to update cron job enabled state");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    scheduler.set_enabled(&request.cron_id, request.enabled).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.cron_id, "failed to update scheduler enabled state");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let status = if request.enabled {
        "enabled"
    } else {
        "disabled"
    };
    Ok(Json(CronActionResponse {
        success: true,
        message: format!("Cron job '{}' {}", request.cron_id, status),
    }))
}
