//! Autonomy status, run-history, and instance-ceiling endpoints for the
//! control interface.
//!
//! Status reads ride the per-agent autonomy stores threaded through
//! `AgentDeps` (via the wake registry) and the hot-reloaded `RuntimeConfig`.
//! Per-agent level and tuning writes ride the agent-config path in
//! `api::config`; the instance-wide ceiling is written here, to the top-level
//! `[autonomy]` table in config.toml.

use super::state::ApiState;
use crate::config::AutonomyLevel;
use crate::wakes::{AutonomyRun, AutonomyRunStatus};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// How many recent runs to inspect when deriving a status snapshot. Enough to
/// find the running row and the most recent finished run.
const STATUS_RUN_WINDOW: u32 = 10;

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct AutonomyStatusQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct AutonomyRunsQuery {
    /// Agent to list runs for. Omit to aggregate runs across all agents.
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default = "default_runs_limit")]
    limit: u32,
}

fn default_runs_limit() -> u32 {
    20
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyCurrentRun {
    pub started_at: String,
}

/// Where this agent's proactive messages go when no wake overrides it.
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct HomeChannelStatus {
    /// Canonical `adapter:target` string.
    pub target: String,
    /// Set deliberately, rather than adopted on the first completed turn.
    pub explicit: bool,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyStatusResponse {
    pub agent_id: String,
    pub level: AutonomyLevel,
    /// The agent's dial capped by the instance ceiling — what the agent
    /// actually runs at.
    pub effective_level: AutonomyLevel,
    pub interval_secs: u64,
    pub active_hours: Option<(u8, u8)>,
    pub max_tasks_per_run: u32,
    /// When the most recent finished run started.
    pub last_run_at: Option<String>,
    /// Summary of the most recent finished run.
    pub last_run_summary: Option<String>,
    /// Interval anchor: last run start + interval, clamped to now when
    /// overdue. `null` when the level is `off`.
    pub next_run_at: Option<String>,
    /// The in-flight run, when one is active.
    pub current_run: Option<AutonomyCurrentRun>,
    pub pending_wake_events: i64,
    /// Resolved home channel, or `null` when the agent has nowhere to speak
    /// on its own.
    pub home_channel: Option<HomeChannelStatus>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyFleetResponse {
    /// Instance-wide autonomy ceiling applied to every agent.
    pub ceiling: AutonomyLevel,
    pub agents: Vec<AutonomyStatusResponse>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AutonomyCeilingUpdateRequest {
    ceiling: AutonomyLevel,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyRunEntry {
    pub agent_id: String,
    #[serde(flatten)]
    pub run: AutonomyRun,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutonomyRunsResponse {
    pub runs: Vec<AutonomyRunEntry>,
}

/// Look up an agent's deps in the wake registry.
async fn agent_deps(state: &ApiState, agent_id: &str) -> Option<crate::AgentDeps> {
    let key: crate::AgentId = Arc::from(agent_id);
    state.wake_registry.read().await.get(&key).cloned()
}

/// Build a status snapshot for one agent from its stores and live config.
pub(crate) async fn build_status(
    agent_id: &str,
    deps: &crate::AgentDeps,
    ceiling: AutonomyLevel,
) -> crate::error::Result<AutonomyStatusResponse> {
    let config = **deps.runtime_config.autonomy.load();
    let effective_level = config.level.min(ceiling);
    let recent = deps.autonomy_run_store.recent(STATUS_RUN_WINDOW).await?;
    let pending_wake_events = deps.wake_event_store.pending_count().await?;

    let current_run = recent
        .iter()
        .find(|run| run.status == AutonomyRunStatus::Running)
        .map(|run| AutonomyCurrentRun {
            started_at: run.started_at.clone(),
        });
    let last_finished = recent
        .iter()
        .find(|run| run.status != AutonomyRunStatus::Running);

    let next_run_at = if effective_level == AutonomyLevel::Off {
        None
    } else {
        let now = chrono::Utc::now();
        // The newest run (running or finished) anchors the interval, matching
        // the driver's `last_run_started_at` semantics. A never-run agent is
        // immediately due.
        let next = recent
            .first()
            .and_then(|run| crate::wakes::parse_run_timestamp(&run.started_at))
            .map(|started| {
                (started + chrono::Duration::seconds(config.interval_secs as i64)).max(now)
            })
            .unwrap_or(now);
        Some(next.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    };

    Ok(AutonomyStatusResponse {
        agent_id: agent_id.to_string(),
        level: config.level,
        effective_level,
        interval_secs: config.interval_secs,
        active_hours: config.active_hours,
        max_tasks_per_run: config.max_tasks_per_run,
        last_run_at: last_finished.map(|run| run.started_at.clone()),
        last_run_summary: last_finished.and_then(|run| run.summary.clone()),
        next_run_at,
        current_run,
        pending_wake_events,
        home_channel: deps
            .runtime_config
            .settings
            .load()
            .as_ref()
            .as_ref()
            .and_then(|settings| settings.home_channel())
            .map(|home| HomeChannelStatus {
                target: home.target,
                explicit: home.explicit,
            }),
    })
}

/// Get the autonomy status for a single agent.
#[utoipa::path(
    get,
    path = "/agents/autonomy",
    params(AutonomyStatusQuery),
    responses(
        (status = 200, body = AutonomyStatusResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "autonomy",
)]
pub(super) async fn autonomy_status(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AutonomyStatusQuery>,
) -> Result<Json<AutonomyStatusResponse>, StatusCode> {
    let deps = agent_deps(&state, &query.agent_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let ceiling = **state.autonomy_ceiling.load();

    let status = build_status(&query.agent_id, &deps, ceiling)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to build autonomy status");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(status))
}

/// Get the autonomy status for every agent, in agent-list order.
#[utoipa::path(
    get,
    path = "/agents/autonomy/fleet",
    responses(
        (status = 200, body = AutonomyFleetResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "autonomy",
)]
pub(super) async fn autonomy_fleet(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<AutonomyFleetResponse>, StatusCode> {
    let agent_ids: Vec<String> = state
        .agent_configs
        .load()
        .iter()
        .map(|info| info.id.clone())
        .collect();
    let ceiling = **state.autonomy_ceiling.load();

    let mut agents = Vec::with_capacity(agent_ids.len());
    for agent_id in agent_ids {
        // Agents mid-removal may be absent from the registry; skip them
        // rather than failing the whole fleet snapshot.
        let Some(deps) = agent_deps(&state, &agent_id).await else {
            continue;
        };
        let status = build_status(&agent_id, &deps, ceiling)
            .await
            .map_err(|error| {
                tracing::warn!(%error, %agent_id, "failed to build autonomy status");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        agents.push(status);
    }

    Ok(Json(AutonomyFleetResponse { ceiling, agents }))
}

/// Set the instance-wide autonomy ceiling.
///
/// Persists to the top-level `[autonomy]` table in config.toml, then stores
/// the new level into the shared ArcSwap so every agent picks it up
/// immediately. Returns the resulting fleet snapshot.
#[utoipa::path(
    put,
    path = "/agents/autonomy/ceiling",
    request_body = AutonomyCeilingUpdateRequest,
    responses(
        (status = 200, body = AutonomyFleetResponse),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "autonomy",
)]
pub(super) async fn update_autonomy_ceiling(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<AutonomyCeilingUpdateRequest>,
) -> Result<Json<AutonomyFleetResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Hold the config write mutex across the read-modify-write so concurrent
    // config.toml editors cannot clobber each other.
    let config_guard = state.config_write_mutex.lock().await;

    let config_content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut doc = config_content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            tracing::warn!(%error, "failed to parse config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if doc.get("autonomy").is_none() {
        doc["autonomy"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let table = doc["autonomy"]
        .as_table_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    table["ceiling"] = toml_edit::value(request.ceiling.as_str());

    let updated_content = doc.to_string();
    if let Err(error) = crate::config::Config::validate_toml(&updated_content) {
        tracing::warn!(%error, "rejected ceiling update due to invalid resulting TOML");
        return Err(StatusCode::BAD_REQUEST);
    }

    tokio::fs::write(&config_path, updated_content)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Store while still holding the config write mutex so concurrent updates
    // apply to the in-memory ceiling in the same order as the file writes.
    state.autonomy_ceiling.store(Arc::new(request.ceiling));
    drop(config_guard);

    tracing::info!(ceiling = %request.ceiling, "instance autonomy ceiling updated via API");

    autonomy_fleet(State(state)).await
}

/// Clear an agent's home channel, returning it to sending nothing on its own.
///
/// There is no set-from-here counterpart: a home is claimed from the chat that
/// should receive it, so the only action this surface can offer is giving it
/// up.
#[utoipa::path(
    delete,
    path = "/agents/autonomy/home",
    params(AutonomyStatusQuery),
    responses(
        (status = 200, body = AutonomyStatusResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "autonomy",
)]
pub(super) async fn clear_home_channel(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AutonomyStatusQuery>,
) -> Result<Json<AutonomyStatusResponse>, StatusCode> {
    let deps = agent_deps(&state, &query.agent_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let settings = deps
        .runtime_config
        .settings
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    settings.clear_home_channel().map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, "failed to clear home channel");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    tracing::info!(agent_id = %query.agent_id, "home channel cleared via API");

    autonomy_status(State(state), Query(query)).await
}

/// List recent autonomy runs, newest first. Scoped to one agent when
/// `agent_id` is given, aggregated across all agents otherwise.
#[utoipa::path(
    get,
    path = "/agents/autonomy/runs",
    params(AutonomyRunsQuery),
    responses(
        (status = 200, body = AutonomyRunsResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "autonomy",
)]
pub(super) async fn autonomy_runs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AutonomyRunsQuery>,
) -> Result<Json<AutonomyRunsResponse>, StatusCode> {
    let limit = query.limit.clamp(1, 100);

    let mut runs: Vec<AutonomyRunEntry> = Vec::new();
    match &query.agent_id {
        Some(agent_id) => {
            let deps = agent_deps(&state, agent_id)
                .await
                .ok_or(StatusCode::NOT_FOUND)?;
            let agent_runs = deps
                .autonomy_run_store
                .recent(limit)
                .await
                .map_err(|error| {
                    tracing::warn!(%error, %agent_id, "failed to list autonomy runs");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            runs.extend(agent_runs.into_iter().map(|run| AutonomyRunEntry {
                agent_id: agent_id.clone(),
                run,
            }));
        }
        None => {
            let registry: Vec<(String, crate::AgentDeps)> = state
                .wake_registry
                .read()
                .await
                .iter()
                .map(|(id, deps)| (id.to_string(), deps.clone()))
                .collect();

            for (agent_id, deps) in registry {
                let agent_runs = deps
                    .autonomy_run_store
                    .recent(limit)
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, %agent_id, "failed to list autonomy runs");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;
                runs.extend(agent_runs.into_iter().map(|run| AutonomyRunEntry {
                    agent_id: agent_id.clone(),
                    run,
                }));
            }
            runs.sort_by(|a, b| {
                b.run
                    .started_at
                    .cmp(&a.run.started_at)
                    .then_with(|| b.run.id.cmp(&a.run.id))
            });
            runs.truncate(limit as usize);
        }
    }

    Ok(Json(AutonomyRunsResponse { runs }))
}
