//! HTTP server setup: router, static file serving, and API routes.

use super::state::{AgentInfo, ApiEvent, ApiState};
use crate::agent::cortex::{CortexEvent, CortexLogger};
use crate::agent::cortex_chat::{CortexChatEvent, CortexChatMessage, CortexChatStore};
use crate::conversation::channels::ChannelStore;
use crate::conversation::history::{ProcessRunLogger, TimelineItem};
use crate::memory::search::{SearchConfig, SearchMode, SearchSort};
use crate::memory::types::{Association, Memory, MemorySearchResult, MemoryType};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Json, Response, Sse};
use axum::routing::{delete, get, post, put};
use futures::stream::Stream;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use tower_http::cors::{Any, CorsLayer};

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

/// Embedded frontend assets from the Vite build output.
#[derive(Embed)]
#[folder = "interface/dist/"]
#[allow(unused)]
struct InterfaceAssets;

// -- Response types --

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct StatusResponse {
    status: &'static str,
    version: &'static str,
    pid: u32,
    uptime_seconds: u64,
}

#[derive(Serialize)]
struct ChannelResponse {
    agent_id: String,
    id: String,
    platform: String,
    display_name: Option<String>,
    is_active: bool,
    last_activity_at: String,
    created_at: String,
}

#[derive(Serialize)]
struct ChannelsResponse {
    channels: Vec<ChannelResponse>,
}

#[derive(Serialize)]
struct MessagesResponse {
    items: Vec<TimelineItem>,
    has_more: bool,
}

#[derive(Serialize)]
struct AgentsResponse {
    agents: Vec<AgentInfo>,
}

#[derive(Serialize)]
struct AgentOverviewResponse {
    /// Memory count by type.
    memory_counts: HashMap<String, i64>,
    /// Total memory count.
    memory_total: i64,
    /// Active channel count for this agent.
    channel_count: usize,
    /// Cron jobs (all, not just enabled).
    cron_jobs: Vec<CronJobInfo>,
    /// Last cortex bulletin event time, if any.
    last_bulletin_at: Option<String>,
    /// Recent cortex events (last 5).
    recent_cortex_events: Vec<CortexEvent>,
    /// Daily memory creation counts for the last 30 days.
    memory_daily: Vec<DayCount>,
    /// Daily activity counts (branches, workers) for the last 30 days.
    activity_daily: Vec<ActivityDayCount>,
    /// Activity heatmap: messages per day-of-week/hour.
    activity_heatmap: Vec<HeatmapCell>,
    /// Latest cortex bulletin text, if any.
    latest_bulletin: Option<String>,
}

#[derive(Serialize)]
struct DayCount {
    date: String,
    count: i64,
}

#[derive(Serialize)]
struct ActivityDayCount {
    date: String,
    branches: i64,
    workers: i64,
}

#[derive(Serialize)]
struct HeatmapCell {
    day: i64,
    hour: i64,
    count: i64,
}

#[derive(Serialize)]
struct CronJobInfo {
    id: String,
    prompt: String,
    interval_secs: u64,
    delivery_target: String,
    enabled: bool,
    active_hours: Option<(u8, u8)>,
}

/// Instance-wide overview response for the main dashboard.
#[derive(Serialize)]
struct InstanceOverviewResponse {
    version: &'static str,
    uptime_seconds: u64,
    pid: u32,
    agents: Vec<AgentSummary>,
}

/// Summary of a single agent for the dashboard.
#[derive(Serialize)]
struct AgentSummary {
    id: String,
    /// Number of active channels.
    channel_count: usize,
    /// Total memory count.
    memory_total: i64,
    /// Number of cron jobs.
    cron_job_count: usize,
    /// 14-day activity sparkline (messages per day).
    activity_sparkline: Vec<i64>,
    /// Most recent activity across all channels.
    last_activity_at: Option<String>,
    /// Last bulletin generation time.
    last_bulletin_at: Option<String>,
    /// Cortex-generated agent profile (display name, status, bio).
    profile: Option<crate::agent::cortex::AgentProfile>,
}

#[derive(Serialize)]
struct MemoriesListResponse {
    memories: Vec<Memory>,
    total: usize,
}

#[derive(Serialize)]
struct MemoriesSearchResponse {
    results: Vec<MemorySearchResult>,
}

#[derive(Serialize)]
struct MemoryGraphResponse {
    nodes: Vec<Memory>,
    edges: Vec<Association>,
    total: usize,
}

#[derive(Serialize)]
struct MemoryGraphNeighborsResponse {
    nodes: Vec<Memory>,
    edges: Vec<Association>,
}

#[derive(Serialize)]
struct CortexEventsResponse {
    events: Vec<CortexEvent>,
    total: i64,
}

#[derive(Serialize)]
struct CortexChatMessagesResponse {
    messages: Vec<CortexChatMessage>,
    thread_id: String,
}

#[derive(Serialize)]
struct IdentityResponse {
    soul: Option<String>,
    identity: Option<String>,
    user: Option<String>,
}

#[derive(Deserialize)]
struct IdentityQuery {
    agent_id: String,
}

#[derive(Deserialize)]
struct IdentityUpdateRequest {
    agent_id: String,
    soul: Option<String>,
    identity: Option<String>,
    user: Option<String>,
}

#[derive(Deserialize)]
struct CortexChatSendRequest {
    agent_id: String,
    thread_id: String,
    message: String,
    channel_id: Option<String>,
}

// -- Ingest Types --

#[derive(Serialize)]
struct IngestFileInfo {
    content_hash: String,
    filename: String,
    file_size: i64,
    total_chunks: i64,
    chunks_completed: i64,
    status: String,
    started_at: String,
    completed_at: Option<String>,
}

#[derive(Serialize)]
struct IngestFilesResponse {
    files: Vec<IngestFileInfo>,
}

#[derive(Serialize)]
struct IngestUploadResponse {
    uploaded: Vec<String>,
}

#[derive(Serialize)]
struct IngestDeleteResponse {
    success: bool,
}

// -- Agent Config Types --

#[derive(Serialize, Debug)]
struct RoutingSection {
    channel: String,
    branch: String,
    worker: String,
    compactor: String,
    cortex: String,
    rate_limit_cooldown_secs: u64,
}

#[derive(Serialize, Debug)]
struct TuningSection {
    max_concurrent_branches: usize,
    max_concurrent_workers: usize,
    max_turns: usize,
    branch_max_turns: usize,
    context_window: usize,
    history_backfill_count: usize,
}

#[derive(Serialize, Debug)]
struct CompactionSection {
    background_threshold: f32,
    aggressive_threshold: f32,
    emergency_threshold: f32,
}

#[derive(Serialize, Debug)]
struct CortexSection {
    tick_interval_secs: u64,
    worker_timeout_secs: u64,
    branch_timeout_secs: u64,
    circuit_breaker_threshold: u8,
    bulletin_interval_secs: u64,
    bulletin_max_words: usize,
    bulletin_max_turns: usize,
}

#[derive(Serialize, Debug)]
struct CoalesceSection {
    enabled: bool,
    debounce_ms: u64,
    max_wait_ms: u64,
    min_messages: usize,
    multi_user_only: bool,
}

#[derive(Serialize, Debug)]
struct MemoryPersistenceSection {
    enabled: bool,
    message_interval: usize,
}

#[derive(Serialize, Debug)]
struct BrowserSection {
    enabled: bool,
    headless: bool,
    evaluate_enabled: bool,
}

#[derive(Serialize, Debug)]
struct DiscordSection {
    enabled: bool,
    allow_bot_messages: bool,
}

#[derive(Serialize, Debug)]
struct AgentConfigResponse {
    routing: RoutingSection,
    tuning: TuningSection,
    compaction: CompactionSection,
    cortex: CortexSection,
    coalesce: CoalesceSection,
    memory_persistence: MemoryPersistenceSection,
    browser: BrowserSection,
    discord: DiscordSection,
}

#[derive(Deserialize)]
struct AgentConfigQuery {
    agent_id: String,
}

#[derive(Deserialize, Debug, Default)]
struct AgentConfigUpdateRequest {
    agent_id: String,
    #[serde(default)]
    routing: Option<RoutingUpdate>,
    #[serde(default)]
    tuning: Option<TuningUpdate>,
    #[serde(default)]
    compaction: Option<CompactionUpdate>,
    #[serde(default)]
    cortex: Option<CortexUpdate>,
    #[serde(default)]
    coalesce: Option<CoalesceUpdate>,
    #[serde(default)]
    memory_persistence: Option<MemoryPersistenceUpdate>,
    #[serde(default)]
    browser: Option<BrowserUpdate>,
    #[serde(default)]
    discord: Option<DiscordUpdate>,
}

#[derive(Deserialize, Debug)]
struct RoutingUpdate {
    channel: Option<String>,
    branch: Option<String>,
    worker: Option<String>,
    compactor: Option<String>,
    cortex: Option<String>,
    rate_limit_cooldown_secs: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct TuningUpdate {
    max_concurrent_branches: Option<usize>,
    max_concurrent_workers: Option<usize>,
    max_turns: Option<usize>,
    branch_max_turns: Option<usize>,
    context_window: Option<usize>,
    history_backfill_count: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct CompactionUpdate {
    background_threshold: Option<f32>,
    aggressive_threshold: Option<f32>,
    emergency_threshold: Option<f32>,
}

#[derive(Deserialize, Debug)]
struct CortexUpdate {
    tick_interval_secs: Option<u64>,
    worker_timeout_secs: Option<u64>,
    branch_timeout_secs: Option<u64>,
    circuit_breaker_threshold: Option<u8>,
    bulletin_interval_secs: Option<u64>,
    bulletin_max_words: Option<usize>,
    bulletin_max_turns: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct CoalesceUpdate {
    enabled: Option<bool>,
    debounce_ms: Option<u64>,
    max_wait_ms: Option<u64>,
    min_messages: Option<usize>,
    multi_user_only: Option<bool>,
}

#[derive(Deserialize, Debug)]
struct MemoryPersistenceUpdate {
    enabled: Option<bool>,
    message_interval: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct BrowserUpdate {
    enabled: Option<bool>,
    headless: Option<bool>,
    evaluate_enabled: Option<bool>,
}

#[derive(Deserialize, Debug)]
struct DiscordUpdate {
    allow_bot_messages: Option<bool>,
}

/// Start the HTTP server on the given address.
///
/// The caller provides a pre-built `ApiState` so agent event streams and
/// DB pools can be registered after startup.
pub async fn start_http_server(
    bind: SocketAddr,
    state: Arc<ApiState>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let api_routes = Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/overview", get(instance_overview))
        .route("/events", get(events_sse))
        .route("/agents", get(list_agents))
        .route("/agents/overview", get(agent_overview))
        .route("/channels", get(list_channels))
        .route("/channels/messages", get(channel_messages))
        .route("/channels/status", get(channel_status))
        .route("/agents/memories", get(list_memories))
        .route("/agents/memories/search", get(search_memories))
        .route("/agents/memories/graph", get(memory_graph))
        .route(
            "/agents/memories/graph/neighbors",
            get(memory_graph_neighbors),
        )
        .route("/cortex/events", get(cortex_events))
        .route("/cortex-chat/messages", get(cortex_chat_messages))
        .route("/cortex-chat/send", post(cortex_chat_send))
        .route("/agents/profile", get(get_agent_profile))
        .route("/agents/identity", get(get_identity).put(update_identity))
        .route(
            "/agents/config",
            get(get_agent_config).put(update_agent_config),
        )
        .route(
            "/agents/cron",
            get(list_cron_jobs)
                .post(create_or_update_cron)
                .delete(delete_cron),
        )
        .route("/agents/cron/executions", get(cron_executions))
        .route("/agents/cron/trigger", post(trigger_cron))
        .route("/agents/cron/toggle", put(toggle_cron))
        .route("/channels/cancel", post(cancel_process))
        .route(
            "/agents/ingest/files",
            get(list_ingest_files).delete(delete_ingest_file),
        )
        .route("/agents/ingest/upload", post(upload_ingest_file))
        .route("/providers", get(get_providers).put(update_provider))
        .route("/providers/{provider}", delete(delete_provider))
        .route("/models", get(get_models))
        .route("/models/refresh", post(refresh_models))
        .route("/messaging/status", get(messaging_status))
        .route(
            "/bindings",
            get(list_bindings)
                .post(create_binding)
                .put(update_binding)
                .delete(delete_binding),
        )
        .route(
            "/settings",
            get(get_global_settings).put(update_global_settings),
        )
        .route("/config/raw", get(get_raw_config).put(update_raw_config))
        .route("/update/check", get(update_check).post(update_check_now))
        .route("/update/apply", post(update_apply));

    let app = Router::new()
        .nest("/api", api_routes)
        .fallback(static_handler)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "HTTP server listening");

    let handle = tokio::spawn(async move {
        let mut shutdown = shutdown_rx;
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.wait_for(|v| *v).await;
            })
            .await
        {
            tracing::error!(%error, "HTTP server exited with error");
        }
    });

    Ok(handle)
}

// -- API handlers --

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn status(State(state): State<Arc<ApiState>>) -> Json<StatusResponse> {
    let uptime = state.started_at.elapsed();
    Json(StatusResponse {
        status: "running",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        uptime_seconds: uptime.as_secs(),
    })
}

/// List all configured agents with their config summaries.
async fn list_agents(State(state): State<Arc<ApiState>>) -> Json<AgentsResponse> {
    let agents = state.agent_configs.load();
    Json(AgentsResponse {
        agents: agents.as_ref().clone(),
    })
}

/// Get overview stats for an agent: memory breakdown, channels, cron, cortex.
async fn agent_overview(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentOverviewQuery>,
) -> Result<Json<AgentOverviewResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Memory counts by type
    let memory_rows = sqlx::query(
        "SELECT memory_type, COUNT(*) as count FROM memories WHERE forgotten = 0 GROUP BY memory_type",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, "failed to count memories");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut memory_counts: HashMap<String, i64> = HashMap::new();
    let mut memory_total: i64 = 0;
    for row in &memory_rows {
        let memory_type: String = row.get("memory_type");
        let count: i64 = row.get("count");
        memory_total += count;
        memory_counts.insert(memory_type, count);
    }

    // Channel count
    let channel_store = ChannelStore::new(pool.clone());
    let channels = channel_store.list_active().await.unwrap_or_default();
    let channel_count = channels.len();

    // Cron jobs
    let cron_rows = sqlx::query(
        "SELECT id, prompt, interval_secs, delivery_target, active_start_hour, active_end_hour, enabled FROM cron_jobs ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let cron_jobs: Vec<CronJobInfo> = cron_rows
        .into_iter()
        .map(|row| {
            let active_start: Option<i64> = row.try_get("active_start_hour").ok();
            let active_end: Option<i64> = row.try_get("active_end_hour").ok();
            CronJobInfo {
                id: row.get("id"),
                prompt: row.get("prompt"),
                interval_secs: row.get::<i64, _>("interval_secs") as u64,
                delivery_target: row.get("delivery_target"),
                enabled: row.get::<i64, _>("enabled") != 0,
                active_hours: match (active_start, active_end) {
                    (Some(s), Some(e)) => Some((s as u8, e as u8)),
                    _ => None,
                },
            }
        })
        .collect();

    // Last bulletin time
    let cortex_logger = CortexLogger::new(pool.clone());
    let bulletin_events = cortex_logger
        .load_events(1, 0, Some("bulletin_generated"))
        .await
        .unwrap_or_default();
    let last_bulletin_at = bulletin_events.first().map(|e| e.created_at.clone());

    // Recent cortex events
    let recent_cortex_events = cortex_logger
        .load_events(5, 0, None)
        .await
        .unwrap_or_default();

    // Latest bulletin text
    let latest_bulletin = bulletin_events.first().and_then(|e| {
        e.details.as_ref().and_then(|d| {
            d.get("bulletin_text")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
    });

    // Memory daily counts for last 30 days
    let memory_daily_rows = sqlx::query(
        "SELECT date(created_at) as date, COUNT(*) as count FROM memories WHERE forgotten = 0 AND created_at > date('now', '-30 days') GROUP BY date ORDER BY date",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let memory_daily: Vec<DayCount> = memory_daily_rows
        .into_iter()
        .map(|row| DayCount {
            date: row.get("date"),
            count: row.get("count"),
        })
        .collect();

    // Activity daily counts (branches + workers) for last 30 days
    let activity_window = chrono::Utc::now() - chrono::Duration::days(30);

    let branch_activity = sqlx::query(
        "SELECT date(started_at) as date, COUNT(*) as count FROM branch_runs WHERE started_at > ? GROUP BY date ORDER BY date",
    )
    .bind(activity_window.to_rfc3339())
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let worker_activity = sqlx::query(
        "SELECT date(started_at) as date, COUNT(*) as count FROM worker_runs WHERE started_at > ? GROUP BY date ORDER BY date",
    )
    .bind(activity_window.to_rfc3339())
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let activity_daily: Vec<ActivityDayCount> = {
        let mut map: HashMap<String, ActivityDayCount> = HashMap::new();
        for row in branch_activity {
            let date: String = row.get("date");
            let count: i64 = row.get("count");
            map.entry(date.clone())
                .or_insert_with(|| ActivityDayCount {
                    date,
                    branches: 0,
                    workers: 0,
                })
                .branches = count;
        }
        for row in worker_activity {
            let date: String = row.get("date");
            let count: i64 = row.get("count");
            map.entry(date.clone())
                .or_insert_with(|| ActivityDayCount {
                    date,
                    branches: 0,
                    workers: 0,
                })
                .workers = count;
        }
        let mut days: Vec<_> = map.into_values().collect();
        days.sort_by(|a, b| a.date.cmp(&b.date));
        days
    };

    // Activity heatmap: messages per day-of-week/hour
    let heatmap_rows = sqlx::query(
        "SELECT CAST(strftime('%w', created_at) AS INTEGER) as day, CAST(strftime('%H', created_at) AS INTEGER) as hour, COUNT(*) as count FROM conversation_messages WHERE created_at > date('now', '-90 days') GROUP BY day, hour",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let activity_heatmap: Vec<HeatmapCell> = heatmap_rows
        .into_iter()
        .map(|row| HeatmapCell {
            day: row.get("day"),
            hour: row.get("hour"),
            count: row.get("count"),
        })
        .collect();

    Ok(Json(AgentOverviewResponse {
        memory_counts,
        memory_total,
        channel_count,
        cron_jobs,
        last_bulletin_at,
        recent_cortex_events,
        memory_daily,
        activity_daily,
        activity_heatmap,
        latest_bulletin,
    }))
}

#[derive(Deserialize)]
struct AgentOverviewQuery {
    agent_id: String,
}

/// Get instance-wide overview for the main dashboard.
async fn instance_overview(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<InstanceOverviewResponse>, StatusCode> {
    let uptime = state.started_at.elapsed();
    let pools = state.agent_pools.load();
    let configs = state.agent_configs.load();

    let mut agents: Vec<AgentSummary> = Vec::new();

    for agent_config in configs.iter() {
        let agent_id = agent_config.id.clone();

        let Some(pool) = pools.get(&agent_id) else {
            continue;
        };

        // Channel count
        let channel_store = ChannelStore::new(pool.clone());
        let channels = channel_store.list_active().await.unwrap_or_default();
        let channel_count = channels.len();

        // Last activity from channels
        let last_activity_at = channels
            .iter()
            .map(|c| &c.last_activity_at)
            .max()
            .map(|dt| dt.to_rfc3339());

        // Memory count
        let memory_total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM memories WHERE forgotten = 0")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        // Cron job count
        let cron_job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cron_jobs")
            .fetch_one(pool)
            .await
            .unwrap_or(0);

        // 14-day activity sparkline
        let activity_window = chrono::Utc::now() - chrono::Duration::days(14);
        let activity_rows = sqlx::query(
            "SELECT date(created_at) as date, COUNT(*) as count FROM conversation_messages WHERE created_at > ? GROUP BY date ORDER BY date",
        )
        .bind(activity_window.to_rfc3339())
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        // Build sparkline (14 values, one per day, 0 for missing days)
        let mut activity_map: HashMap<String, i64> = HashMap::new();
        for row in &activity_rows {
            let date: String = row.get("date");
            let count: i64 = row.get("count");
            activity_map.insert(date, count);
        }

        let mut activity_sparkline: Vec<i64> = Vec::with_capacity(14);
        for i in 0..14 {
            let date = (chrono::Utc::now() - chrono::Duration::days(13 - i as i64))
                .format("%Y-%m-%d")
                .to_string();
            activity_sparkline.push(*activity_map.get(&date).unwrap_or(&0));
        }

        // Last bulletin time
        let cortex_logger = CortexLogger::new(pool.clone());
        let bulletin_events = cortex_logger
            .load_events(1, 0, Some("bulletin_generated"))
            .await
            .unwrap_or_default();
        let last_bulletin_at = bulletin_events.first().map(|e| e.created_at.clone());

        // Agent profile
        let profile = crate::agent::cortex::load_profile(pool, &agent_id).await;

        agents.push(AgentSummary {
            id: agent_id,
            channel_count,
            memory_total,
            cron_job_count: cron_job_count as usize,
            activity_sparkline,
            last_activity_at,
            last_bulletin_at,
            profile,
        });
    }

    Ok(Json(InstanceOverviewResponse {
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: uptime.as_secs(),
        pid: std::process::id(),
        agents,
    }))
}

/// SSE endpoint streaming all agent events to connected clients.
async fn events_sse(
    State(state): State<Arc<ApiState>>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>> {
    let mut rx = state.event_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let event_type = match &event {
                            ApiEvent::InboundMessage { .. } => "inbound_message",
                            ApiEvent::OutboundMessage { .. } => "outbound_message",
                            ApiEvent::TypingState { .. } => "typing_state",
                            ApiEvent::WorkerStarted { .. } => "worker_started",
                            ApiEvent::WorkerStatusUpdate { .. } => "worker_status",
                            ApiEvent::WorkerCompleted { .. } => "worker_completed",
                            ApiEvent::BranchStarted { .. } => "branch_started",
                            ApiEvent::BranchCompleted { .. } => "branch_completed",
                            ApiEvent::BranchFailed { .. } => "branch_failed",
                            ApiEvent::ToolStarted { .. } => "tool_started",
                            ApiEvent::ToolCompleted { .. } => "tool_completed",
                        };
                        yield Ok(axum::response::sse::Event::default()
                            .event(event_type)
                            .data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    tracing::debug!(count, "SSE client lagged");
                    yield Ok(axum::response::sse::Event::default()
                        .event("lagged")
                        .data(format!("{{\"skipped\":{count}}}")));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

/// List active channels across all agents.
async fn list_channels(State(state): State<Arc<ApiState>>) -> Json<ChannelsResponse> {
    let pools = state.agent_pools.load();
    let mut all_channels = Vec::new();

    for (agent_id, pool) in pools.iter() {
        let store = ChannelStore::new(pool.clone());
        match store.list_active().await {
            Ok(channels) => {
                for channel in channels {
                    all_channels.push(ChannelResponse {
                        agent_id: agent_id.clone(),
                        id: channel.id,
                        platform: channel.platform,
                        display_name: channel.display_name,
                        is_active: channel.is_active,
                        last_activity_at: channel.last_activity_at.to_rfc3339(),
                        created_at: channel.created_at.to_rfc3339(),
                    });
                }
            }
            Err(error) => {
                tracing::warn!(%error, agent_id, "failed to list channels");
            }
        }
    }

    Json(ChannelsResponse {
        channels: all_channels,
    })
}

#[derive(Deserialize)]
struct MessagesQuery {
    channel_id: String,
    #[serde(default = "default_message_limit")]
    limit: i64,
    before: Option<String>,
}

fn default_message_limit() -> i64 {
    20
}

/// Get the unified timeline for a channel: messages, branch runs, and worker runs
/// interleaved chronologically.
async fn channel_messages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MessagesQuery>,
) -> Json<MessagesResponse> {
    let pools = state.agent_pools.load();
    let limit = query.limit.min(100);
    // Fetch one extra to determine if there are more pages
    let fetch_limit = limit + 1;

    for (_agent_id, pool) in pools.iter() {
        let logger = ProcessRunLogger::new(pool.clone());
        match logger
            .load_channel_timeline(&query.channel_id, fetch_limit, query.before.as_deref())
            .await
        {
            Ok(items) if !items.is_empty() => {
                let has_more = items.len() as i64 > limit;
                let items = if has_more {
                    items[items.len() - limit as usize..].to_vec()
                } else {
                    items
                };
                return Json(MessagesResponse { items, has_more });
            }
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(%error, channel_id = %query.channel_id, "failed to load timeline");
                continue;
            }
        }
    }

    Json(MessagesResponse {
        items: vec![],
        has_more: false,
    })
}

/// Get live status (active workers, branches, completed items) for all channels.
///
/// Returns the StatusBlock directly -- it already derives Serialize.
async fn channel_status(
    State(state): State<Arc<ApiState>>,
) -> Json<HashMap<String, serde_json::Value>> {
    // Snapshot the map under the outer lock, then release it so
    // register/unregister calls aren't blocked during serialization.
    let snapshot: Vec<_> = {
        let blocks = state.channel_status_blocks.read().await;
        blocks.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };

    let mut result = HashMap::new();
    for (channel_id, status_block) in snapshot {
        let block = status_block.read().await;
        if let Ok(value) = serde_json::to_value(&*block) {
            result.insert(channel_id, value);
        }
    }

    Json(result)
}

#[derive(Deserialize)]
struct MemoriesListQuery {
    agent_id: String,
    #[serde(default = "default_memories_limit")]
    limit: i64,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default = "default_memories_sort")]
    sort: String,
}

fn default_memories_limit() -> i64 {
    50
}

fn default_memories_sort() -> String {
    "recent".into()
}

fn parse_sort(sort: &str) -> SearchSort {
    match sort {
        "importance" => SearchSort::Importance,
        "most_accessed" => SearchSort::MostAccessed,
        _ => SearchSort::Recent,
    }
}

fn parse_memory_type(type_str: &str) -> Option<MemoryType> {
    match type_str {
        "fact" => Some(MemoryType::Fact),
        "preference" => Some(MemoryType::Preference),
        "decision" => Some(MemoryType::Decision),
        "identity" => Some(MemoryType::Identity),
        "event" => Some(MemoryType::Event),
        "observation" => Some(MemoryType::Observation),
        "goal" => Some(MemoryType::Goal),
        "todo" => Some(MemoryType::Todo),
        _ => None,
    }
}

/// List memories for an agent with sorting, filtering, and pagination.
async fn list_memories(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoriesListQuery>,
) -> Result<Json<MemoriesListResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let limit = query.limit.min(200);
    let sort = parse_sort(&query.sort);
    let memory_type = query.memory_type.as_deref().and_then(parse_memory_type);

    // Fetch limit + offset so we can paginate, then slice
    let fetch_limit = limit + query.offset as i64;
    let all = store
        .get_sorted(sort, fetch_limit, memory_type)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list memories");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = all.len();
    let memories = all.into_iter().skip(query.offset).collect();

    Ok(Json(MemoriesListResponse { memories, total }))
}

#[derive(Deserialize)]
struct MemoriesSearchQuery {
    agent_id: String,
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default)]
    memory_type: Option<String>,
}

fn default_search_limit() -> usize {
    20
}

/// Search memories using hybrid search (vector + FTS + graph).
async fn search_memories(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoriesSearchQuery>,
) -> Result<Json<MemoriesSearchResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let config = SearchConfig {
        mode: SearchMode::Hybrid,
        memory_type: query.memory_type.as_deref().and_then(parse_memory_type),
        max_results: query.limit.min(100),
        ..SearchConfig::default()
    };

    let results = memory_search.search(&query.q, &config)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, query = %query.q, "memory search failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoriesSearchResponse { results }))
}

// -- Memory graph handlers --

#[derive(Deserialize)]
struct MemoryGraphQuery {
    agent_id: String,
    #[serde(default = "default_graph_limit")]
    limit: i64,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    memory_type: Option<String>,
    #[serde(default = "default_memories_sort")]
    sort: String,
}

fn default_graph_limit() -> i64 {
    200
}

/// Get a subgraph of memories: nodes + all edges between them.
/// Uses the same sort/filter params as the list endpoint, then fetches
/// all associations that connect the returned nodes.
async fn memory_graph(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoryGraphQuery>,
) -> Result<Json<MemoryGraphResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let limit = query.limit.min(500);
    let sort = parse_sort(&query.sort);
    let memory_type = query.memory_type.as_deref().and_then(parse_memory_type);

    let fetch_limit = limit + query.offset as i64;
    let all = store
        .get_sorted(sort, fetch_limit, memory_type)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load graph nodes");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = all.len();
    let nodes: Vec<Memory> = all.into_iter().skip(query.offset).collect();
    let node_ids: Vec<String> = nodes.iter().map(|m| m.id.clone()).collect();

    let edges = store
        .get_associations_between(&node_ids)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load graph edges");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoryGraphResponse {
        nodes,
        edges,
        total,
    }))
}

#[derive(Deserialize)]
struct MemoryGraphNeighborsQuery {
    agent_id: String,
    memory_id: String,
    #[serde(default = "default_neighbor_depth")]
    depth: u32,
    /// Comma-separated list of memory IDs the client already has.
    #[serde(default)]
    exclude: Option<String>,
}

fn default_neighbor_depth() -> u32 {
    1
}

/// Get the neighbors of a specific memory node. Returns new nodes
/// and edges not already present in the client's graph.
async fn memory_graph_neighbors(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MemoryGraphNeighborsQuery>,
) -> Result<Json<MemoryGraphNeighborsResponse>, StatusCode> {
    let searches = state.memory_searches.load();
    let memory_search = searches.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = memory_search.store();

    let depth = query.depth.min(3);
    let exclude_ids: Vec<String> = query
        .exclude
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let (nodes, edges) = store.get_neighbors(&query.memory_id, depth, &exclude_ids)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, memory_id = %query.memory_id, "failed to load neighbors");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MemoryGraphNeighborsResponse { nodes, edges }))
}

// -- Cortex chat handlers --

#[derive(Deserialize)]
struct CortexChatMessagesQuery {
    agent_id: String,
    /// If omitted, loads the latest thread.
    thread_id: Option<String>,
    #[serde(default = "default_cortex_chat_limit")]
    limit: i64,
}

fn default_cortex_chat_limit() -> i64 {
    50
}

/// Load persisted cortex chat history for a thread.
/// If no thread_id is provided, loads the latest thread.
/// If no threads exist, returns an empty list with a fresh thread_id.
async fn cortex_chat_messages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CortexChatMessagesQuery>,
) -> Result<Json<CortexChatMessagesResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = CortexChatStore::new(pool.clone());

    // Resolve thread_id: explicit > latest > generate new
    let thread_id = if let Some(tid) = query.thread_id {
        tid
    } else {
        store
            .latest_thread_id()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    };

    let messages = store
        .load_history(&thread_id, query.limit.min(200))
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load cortex chat history");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(CortexChatMessagesResponse {
        messages,
        thread_id,
    }))
}

/// Send a message to cortex chat. Returns an SSE stream with activity events.
///
/// Send a message to cortex chat. Returns an SSE stream with activity events.
///
/// The stream emits:
/// - `thinking` — cortex is processing
/// - `tool_started` — a tool call began
/// - `tool_completed` — a tool call finished (with result preview)
/// - `done` — full response text
/// - `error` — if something went wrong
async fn cortex_chat_send(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<CortexChatSendRequest>,
) -> Result<Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>>, StatusCode> {
    let sessions = state.cortex_chat_sessions.load();
    let session = sessions
        .get(&request.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    let thread_id = request.thread_id;
    let message = request.message;
    let channel_id = request.channel_id;

    // Start the agent and get an event receiver
    let channel_ref = channel_id.as_deref();
    let mut event_rx = session
        .send_message_with_events(&thread_id, &message, channel_ref)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to start cortex chat send");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let stream = async_stream::stream! {
        // Send thinking event
        yield Ok(axum::response::sse::Event::default()
            .event("thinking")
            .data("{}"));

        // Forward events from the agent task
        while let Some(event) = event_rx.recv().await {
            let event_name = match &event {
                CortexChatEvent::Thinking => "thinking",
                CortexChatEvent::ToolStarted { .. } => "tool_started",
                CortexChatEvent::ToolCompleted { .. } => "tool_completed",
                CortexChatEvent::Done { .. } => "done",
                CortexChatEvent::Error { .. } => "error",
            };
            if let Ok(json) = serde_json::to_string(&event) {
                yield Ok(axum::response::sse::Event::default()
                    .event(event_name)
                    .data(json));
            }
        }
    };

    Ok(Sse::new(stream))
}

// -- Agent profile handler --

/// Get the cortex-generated profile for an agent.
async fn get_agent_profile(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentOverviewQuery>,
) -> Result<Json<AgentProfileResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let profile = crate::agent::cortex::load_profile(pool, &query.agent_id).await;

    Ok(Json(AgentProfileResponse { profile }))
}

#[derive(Serialize)]
struct AgentProfileResponse {
    profile: Option<crate::agent::cortex::AgentProfile>,
}

// -- Identity file handlers --

/// Get identity files (SOUL.md, IDENTITY.md, USER.md) for an agent.
async fn get_identity(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IdentityQuery>,
) -> Result<Json<IdentityResponse>, StatusCode> {
    let workspaces = state.agent_workspaces.load();
    let workspace = workspaces
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let identity = crate::identity::Identity::load(workspace).await;

    Ok(Json(IdentityResponse {
        soul: identity.soul,
        identity: identity.identity,
        user: identity.user,
    }))
}

/// Update identity files for an agent. Only writes files for fields that are present.
/// The file watcher will pick up changes and hot-reload identity into RuntimeConfig.
async fn update_identity(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<IdentityUpdateRequest>,
) -> Result<Json<IdentityResponse>, StatusCode> {
    let workspaces = state.agent_workspaces.load();
    let workspace = workspaces
        .get(&request.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(soul) = &request.soul {
        tokio::fs::write(workspace.join("SOUL.md"), soul)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write SOUL.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    if let Some(identity) = &request.identity {
        tokio::fs::write(workspace.join("IDENTITY.md"), identity)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write IDENTITY.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    if let Some(user) = &request.user {
        tokio::fs::write(workspace.join("USER.md"), user)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write USER.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    // Read back the current state after writes
    let updated = crate::identity::Identity::load(workspace).await;

    Ok(Json(IdentityResponse {
        soul: updated.soul,
        identity: updated.identity,
        user: updated.user,
    }))
}

// -- Agent config handlers --

/// Get the resolved configuration for an agent.
/// Reads live values from the agent's RuntimeConfig (hot-reloaded via ArcSwap).
async fn get_agent_config(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentConfigQuery>,
) -> Result<Json<AgentConfigResponse>, StatusCode> {
    let runtime_configs = state.runtime_configs.load();
    let rc = runtime_configs
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let routing = rc.routing.load();
    let compaction = rc.compaction.load();
    let cortex = rc.cortex.load();
    let coalesce = rc.coalesce.load();
    let memory_persistence = rc.memory_persistence.load();
    let browser = rc.browser_config.load();

    let response = AgentConfigResponse {
        routing: RoutingSection {
            channel: routing.channel.clone(),
            branch: routing.branch.clone(),
            worker: routing.worker.clone(),
            compactor: routing.compactor.clone(),
            cortex: routing.cortex.clone(),
            rate_limit_cooldown_secs: routing.rate_limit_cooldown_secs,
        },
        tuning: TuningSection {
            max_concurrent_branches: **rc.max_concurrent_branches.load(),
            max_concurrent_workers: **rc.max_concurrent_workers.load(),
            max_turns: **rc.max_turns.load(),
            branch_max_turns: **rc.branch_max_turns.load(),
            context_window: **rc.context_window.load(),
            history_backfill_count: **rc.history_backfill_count.load(),
        },
        compaction: CompactionSection {
            background_threshold: compaction.background_threshold,
            aggressive_threshold: compaction.aggressive_threshold,
            emergency_threshold: compaction.emergency_threshold,
        },
        cortex: CortexSection {
            tick_interval_secs: cortex.tick_interval_secs,
            worker_timeout_secs: cortex.worker_timeout_secs,
            branch_timeout_secs: cortex.branch_timeout_secs,
            circuit_breaker_threshold: cortex.circuit_breaker_threshold,
            bulletin_interval_secs: cortex.bulletin_interval_secs,
            bulletin_max_words: cortex.bulletin_max_words,
            bulletin_max_turns: cortex.bulletin_max_turns,
        },
        coalesce: CoalesceSection {
            enabled: coalesce.enabled,
            debounce_ms: coalesce.debounce_ms,
            max_wait_ms: coalesce.max_wait_ms,
            min_messages: coalesce.min_messages,
            multi_user_only: coalesce.multi_user_only,
        },
        memory_persistence: MemoryPersistenceSection {
            enabled: memory_persistence.enabled,
            message_interval: memory_persistence.message_interval,
        },
        browser: BrowserSection {
            enabled: browser.enabled,
            headless: browser.headless,
            evaluate_enabled: browser.evaluate_enabled,
        },
        discord: {
            let perms = state.discord_permissions.read().await;
            match perms.as_ref() {
                Some(arc_swap) => {
                    let snapshot = arc_swap.load();
                    DiscordSection {
                        enabled: true,
                        allow_bot_messages: snapshot.allow_bot_messages,
                    }
                }
                None => DiscordSection {
                    enabled: false,
                    allow_bot_messages: false,
                },
            }
        },
    };

    Ok(Json(response))
}

/// Update agent configuration by editing config.toml with toml_edit.
/// This preserves formatting and comments while writing the new values.
async fn update_agent_config(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<AgentConfigUpdateRequest>,
) -> Result<Json<AgentConfigResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Read the config file
    let config_content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Parse with toml_edit to preserve formatting
    let mut doc = config_content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            tracing::warn!(%error, "failed to parse config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Find or create the agent table
    let agent_idx = find_or_create_agent_table(&mut doc, &request.agent_id)?;

    // Apply updates to the correct agent entry
    if let Some(routing) = &request.routing {
        update_routing_table(&mut doc, agent_idx, routing)?;
    }
    if let Some(tuning) = &request.tuning {
        update_tuning_table(&mut doc, agent_idx, tuning)?;
    }
    if let Some(compaction) = &request.compaction {
        update_compaction_table(&mut doc, agent_idx, compaction)?;
    }
    if let Some(cortex) = &request.cortex {
        update_cortex_table(&mut doc, agent_idx, cortex)?;
    }
    if let Some(coalesce) = &request.coalesce {
        update_coalesce_table(&mut doc, agent_idx, coalesce)?;
    }
    if let Some(memory_persistence) = &request.memory_persistence {
        update_memory_persistence_table(&mut doc, agent_idx, memory_persistence)?;
    }
    if let Some(browser) = &request.browser {
        update_browser_table(&mut doc, agent_idx, browser)?;
    }
    if let Some(discord) = &request.discord {
        update_discord_table(&mut doc, discord)?;
    }

    // Write the updated config back
    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(agent_id = %request.agent_id, "config.toml updated via API");

    // Immediately reload RuntimeConfig and DiscordPermissions so the response
    // reflects the new values (the file watcher will also pick this up, but has a 2s debounce)
    match crate::config::Config::load_from_path(&config_path) {
        Ok(new_config) => {
            let runtime_configs = state.runtime_configs.load();
            if let Some(rc) = runtime_configs.get(&request.agent_id) {
                rc.reload_config(&new_config, &request.agent_id);
            }
            if request.discord.is_some() {
                if let Some(discord_config) = &new_config.messaging.discord {
                    let new_perms = crate::config::DiscordPermissions::from_config(
                        discord_config,
                        &new_config.bindings,
                    );
                    let perms = state.discord_permissions.read().await;
                    if let Some(arc_swap) = perms.as_ref() {
                        arc_swap.store(std::sync::Arc::new(new_perms));
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "config.toml written but failed to reload immediately");
        }
    }

    get_agent_config(
        State(state),
        Query(AgentConfigQuery {
            agent_id: request.agent_id,
        }),
    )
    .await
}

/// Find the index of an agent table in the [[agents]] array, or create a new one.
fn find_or_create_agent_table(
    doc: &mut toml_edit::DocumentMut,
    agent_id: &str,
) -> Result<usize, StatusCode> {
    // Create agents array if it doesn't exist
    if doc.get("agents").is_none() {
        doc["agents"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }

    // Get the agents array
    let agents = doc
        .get_mut("agents")
        .and_then(|a| a.as_array_of_tables_mut())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    // Find existing agent
    for (idx, table) in agents.iter().enumerate() {
        if let Some(id) = table.get("id").and_then(|v| v.as_str()) {
            if id == agent_id {
                return Ok(idx);
            }
        }
    }

    // Create new agent table
    let mut new_agent = toml_edit::Table::new();
    new_agent["id"] = toml_edit::value(agent_id);
    agents.push(new_agent);

    Ok(agents.len() - 1)
}

/// Get a mutable reference to an agent's table in the [[agents]] array.
fn get_agent_table_mut(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
) -> Result<&mut toml_edit::Table, StatusCode> {
    doc.get_mut("agents")
        .and_then(|a| a.as_array_of_tables_mut())
        .and_then(|arr| arr.get_mut(agent_idx))
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Get or create a subtable within an agent's config (e.g., [agents.routing]).
fn get_or_create_subtable<'a>(
    agent: &'a mut toml_edit::Table,
    key: &str,
) -> &'a mut toml_edit::Table {
    if !agent.contains_key(key) {
        agent[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    agent[key].as_table_mut().expect("just created as table")
}

fn update_routing_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    routing: &RoutingUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "routing");
    if let Some(ref v) = routing.channel {
        table["channel"] = toml_edit::value(v.as_str());
    }
    if let Some(ref v) = routing.branch {
        table["branch"] = toml_edit::value(v.as_str());
    }
    if let Some(ref v) = routing.worker {
        table["worker"] = toml_edit::value(v.as_str());
    }
    if let Some(ref v) = routing.compactor {
        table["compactor"] = toml_edit::value(v.as_str());
    }
    if let Some(ref v) = routing.cortex {
        table["cortex"] = toml_edit::value(v.as_str());
    }
    if let Some(v) = routing.rate_limit_cooldown_secs {
        table["rate_limit_cooldown_secs"] = toml_edit::value(v as i64);
    }
    Ok(())
}

fn update_tuning_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    tuning: &TuningUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    if let Some(v) = tuning.max_concurrent_branches {
        agent["max_concurrent_branches"] = toml_edit::value(v as i64);
    }
    if let Some(v) = tuning.max_concurrent_workers {
        agent["max_concurrent_workers"] = toml_edit::value(v as i64);
    }
    if let Some(v) = tuning.max_turns {
        agent["max_turns"] = toml_edit::value(v as i64);
    }
    if let Some(v) = tuning.branch_max_turns {
        agent["branch_max_turns"] = toml_edit::value(v as i64);
    }
    if let Some(v) = tuning.context_window {
        agent["context_window"] = toml_edit::value(v as i64);
    }
    if let Some(v) = tuning.history_backfill_count {
        agent["history_backfill_count"] = toml_edit::value(v as i64);
    }
    Ok(())
}

fn update_compaction_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    compaction: &CompactionUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "compaction");
    if let Some(v) = compaction.background_threshold {
        table["background_threshold"] = toml_edit::value(v as f64);
    }
    if let Some(v) = compaction.aggressive_threshold {
        table["aggressive_threshold"] = toml_edit::value(v as f64);
    }
    if let Some(v) = compaction.emergency_threshold {
        table["emergency_threshold"] = toml_edit::value(v as f64);
    }
    Ok(())
}

fn update_cortex_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    cortex: &CortexUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "cortex");
    if let Some(v) = cortex.tick_interval_secs {
        table["tick_interval_secs"] = toml_edit::value(v as i64);
    }
    if let Some(v) = cortex.worker_timeout_secs {
        table["worker_timeout_secs"] = toml_edit::value(v as i64);
    }
    if let Some(v) = cortex.branch_timeout_secs {
        table["branch_timeout_secs"] = toml_edit::value(v as i64);
    }
    if let Some(v) = cortex.circuit_breaker_threshold {
        table["circuit_breaker_threshold"] = toml_edit::value(v as i64);
    }
    if let Some(v) = cortex.bulletin_interval_secs {
        table["bulletin_interval_secs"] = toml_edit::value(v as i64);
    }
    if let Some(v) = cortex.bulletin_max_words {
        table["bulletin_max_words"] = toml_edit::value(v as i64);
    }
    if let Some(v) = cortex.bulletin_max_turns {
        table["bulletin_max_turns"] = toml_edit::value(v as i64);
    }
    Ok(())
}

fn update_coalesce_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    coalesce: &CoalesceUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "coalesce");
    if let Some(v) = coalesce.enabled {
        table["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = coalesce.debounce_ms {
        table["debounce_ms"] = toml_edit::value(v as i64);
    }
    if let Some(v) = coalesce.max_wait_ms {
        table["max_wait_ms"] = toml_edit::value(v as i64);
    }
    if let Some(v) = coalesce.min_messages {
        table["min_messages"] = toml_edit::value(v as i64);
    }
    if let Some(v) = coalesce.multi_user_only {
        table["multi_user_only"] = toml_edit::value(v);
    }
    Ok(())
}

fn update_memory_persistence_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    memory_persistence: &MemoryPersistenceUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "memory_persistence");
    if let Some(v) = memory_persistence.enabled {
        table["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = memory_persistence.message_interval {
        table["message_interval"] = toml_edit::value(v as i64);
    }
    Ok(())
}

fn update_browser_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    browser: &BrowserUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "browser");
    if let Some(v) = browser.enabled {
        table["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = browser.headless {
        table["headless"] = toml_edit::value(v);
    }
    if let Some(v) = browser.evaluate_enabled {
        table["evaluate_enabled"] = toml_edit::value(v);
    }
    Ok(())
}

/// Update instance-level Discord config at [messaging.discord].
fn update_discord_table(
    doc: &mut toml_edit::DocumentMut,
    discord: &DiscordUpdate,
) -> Result<(), StatusCode> {
    let messaging = doc
        .get_mut("messaging")
        .and_then(|m| m.as_table_mut())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let discord_table = messaging
        .get_mut("discord")
        .and_then(|d| d.as_table_mut())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(allow_bot_messages) = discord.allow_bot_messages {
        discord_table["allow_bot_messages"] = toml_edit::value(allow_bot_messages);
    }

    Ok(())
}

// -- Cortex events handlers --

#[derive(Deserialize)]
struct CortexEventsQuery {
    agent_id: String,
    #[serde(default = "default_cortex_events_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    event_type: Option<String>,
}

fn default_cortex_events_limit() -> i64 {
    50
}

/// List cortex events for an agent with optional type filter, newest first.
async fn cortex_events(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CortexEventsQuery>,
) -> Result<Json<CortexEventsResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let logger = CortexLogger::new(pool.clone());

    let limit = query.limit.min(200);
    let event_type_ref = query.event_type.as_deref();

    let events = logger
        .load_events(limit, query.offset, event_type_ref)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to load cortex events");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = logger.count_events(event_type_ref).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %query.agent_id, "failed to count cortex events");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(CortexEventsResponse { events, total }))
}

// -- Cron handlers --

#[derive(Deserialize)]
struct CronQuery {
    agent_id: String,
}

#[derive(Deserialize)]
struct CronExecutionsQuery {
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
struct CreateCronRequest {
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
}

fn default_interval() -> u64 {
    3600
}

fn default_enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct DeleteCronRequest {
    agent_id: String,
    cron_id: String,
}

#[derive(Deserialize)]
struct TriggerCronRequest {
    agent_id: String,
    cron_id: String,
}

#[derive(Deserialize)]
struct ToggleCronRequest {
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
    active_hours: Option<(u8, u8)>,
    success_count: u64,
    failure_count: u64,
    last_executed_at: Option<String>,
}

#[derive(Serialize)]
struct CronListResponse {
    jobs: Vec<CronJobWithStats>,
}

#[derive(Serialize)]
struct CronExecutionsResponse {
    executions: Vec<crate::cron::CronExecutionEntry>,
}

#[derive(Serialize)]
struct CronActionResponse {
    success: bool,
    message: String,
}

/// List all cron jobs for an agent with execution statistics.
async fn list_cron_jobs(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CronQuery>,
) -> Result<Json<CronListResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

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
            active_hours: config.active_hours,
            success_count: stats.success_count,
            failure_count: stats.failure_count,
            last_executed_at: stats.last_executed_at,
        });
    }

    Ok(Json(CronListResponse { jobs }))
}

/// Get execution history for cron jobs.
async fn cron_executions(
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
async fn create_or_update_cron(
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
    };

    // Save to database
    store.save(&config).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.id, "failed to save cron job");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Register or update in scheduler
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
async fn delete_cron(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeleteCronRequest>,
) -> Result<Json<CronActionResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let schedulers = state.cron_schedulers.load();
    let scheduler = schedulers
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    // Unregister from scheduler first
    scheduler.unregister(&query.cron_id).await;

    // Delete from database
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
async fn trigger_cron(
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
async fn toggle_cron(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ToggleCronRequest>,
) -> Result<Json<CronActionResponse>, StatusCode> {
    let stores = state.cron_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let schedulers = state.cron_schedulers.load();
    let scheduler = schedulers
        .get(&request.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    // Update in database first
    store.update_enabled(&request.cron_id, request.enabled).await.map_err(|error| {
        tracing::warn!(%error, agent_id = %request.agent_id, cron_id = %request.cron_id, "failed to update cron job enabled state");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Update in scheduler (this will start/stop the timer as needed)
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

// -- Process cancellation --

#[derive(Deserialize)]
struct CancelProcessRequest {
    channel_id: String,
    process_type: String,
    process_id: String,
}

#[derive(Serialize)]
struct CancelProcessResponse {
    success: bool,
    message: String,
}

/// Cancel a running worker or branch via the API.
async fn cancel_process(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CancelProcessRequest>,
) -> Result<Json<CancelProcessResponse>, StatusCode> {
    let states = state.channel_states.read().await;
    let channel_state = states
        .get(&request.channel_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    match request.process_type.as_str() {
        "worker" => {
            let worker_id: crate::WorkerId = request
                .process_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            channel_state
                .cancel_worker(worker_id)
                .await
                .map_err(|_| StatusCode::NOT_FOUND)?;
            Ok(Json(CancelProcessResponse {
                success: true,
                message: format!("Worker {} cancelled", request.process_id),
            }))
        }
        "branch" => {
            let branch_id: crate::BranchId = request
                .process_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            channel_state
                .cancel_branch(branch_id)
                .await
                .map_err(|_| StatusCode::NOT_FOUND)?;
            Ok(Json(CancelProcessResponse {
                success: true,
                message: format!("Branch {} cancelled", request.process_id),
            }))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

// -- Provider management --

#[derive(Serialize)]
struct ProviderStatus {
    anthropic: bool,
    openai: bool,
    openrouter: bool,
    ollama: bool,
    zhipu: bool,
    groq: bool,
    together: bool,
    fireworks: bool,
    deepseek: bool,
    xai: bool,
    mistral: bool,
    opencode_zen: bool,
}

#[derive(Serialize)]
struct ProvidersResponse {
    providers: ProviderStatus,
    has_any: bool,
}

#[derive(Deserialize)]
struct ProviderUpdateRequest {
    provider: String,
    api_key: String,
}

#[derive(Serialize)]
struct ProviderUpdateResponse {
    success: bool,
    message: String,
}

async fn get_providers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ProvidersResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    // Check which providers have keys by reading the config
    let (
        anthropic,
        openai,
        openrouter,
        ollama,
        zhipu,
        groq,
        together,
        fireworks,
        deepseek,
        xai,
        mistral,
        opencode_zen,
    ) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let has_key = |key: &str, env_var: &str| -> bool {
            // Check the TOML [llm] table
            if let Some(llm) = doc.get("llm") {
                if let Some(val) = llm.get(key) {
                    if let Some(s) = val.as_str() {
                        // If it's an env reference, check if the env var is set
                        if let Some(var_name) = s.strip_prefix("env:") {
                            return std::env::var(var_name).is_ok();
                        }
                        return !s.is_empty();
                    }
                }
            }
            // Fall back to checking env vars directly
            std::env::var(env_var).is_ok()
        };

        (
            has_key("anthropic_key", "ANTHROPIC_API_KEY"),
            has_key("openai_key", "OPENAI_API_KEY"),
            has_key("openrouter_key", "OPENROUTER_API_KEY"),
            has_key("ollama_key", "OLLAMA_API_KEY"),
            has_key("zhipu_key", "ZHIPU_API_KEY"),
            has_key("groq_key", "GROQ_API_KEY"),
            has_key("together_key", "TOGETHER_API_KEY"),
            has_key("fireworks_key", "FIREWORKS_API_KEY"),
            has_key("deepseek_key", "DEEPSEEK_API_KEY"),
            has_key("xai_key", "XAI_API_KEY"),
            has_key("mistral_key", "MISTRAL_API_KEY"),
            has_key("opencode_zen_key", "OPENCODE_ZEN_API_KEY"),
        )
    } else {
        // No config file — check env vars only
        (
            std::env::var("ANTHROPIC_API_KEY").is_ok(),
            std::env::var("OPENAI_API_KEY").is_ok(),
            std::env::var("OPENROUTER_API_KEY").is_ok(),
            std::env::var("OLLAMA_API_KEY").is_ok(),
            std::env::var("ZHIPU_API_KEY").is_ok(),
            std::env::var("GROQ_API_KEY").is_ok(),
            std::env::var("TOGETHER_API_KEY").is_ok(),
            std::env::var("FIREWORKS_API_KEY").is_ok(),
            std::env::var("DEEPSEEK_API_KEY").is_ok(),
            std::env::var("XAI_API_KEY").is_ok(),
            std::env::var("MISTRAL_API_KEY").is_ok(),
            std::env::var("OPENCODE_ZEN_API_KEY").is_ok(),
        )
    };

    let providers = ProviderStatus {
        anthropic,
        openai,
        openrouter,
        ollama,
        zhipu,
        groq,
        together,
        fireworks,
        deepseek,
        xai,
        mistral,
        opencode_zen,
    };
    let has_any = providers.anthropic
        || providers.openai
        || providers.openrouter
        || providers.ollama
        || providers.zhipu
        || providers.groq
        || providers.together
        || providers.fireworks
        || providers.deepseek
        || providers.xai
        || providers.mistral
        || providers.opencode_zen;

    Ok(Json(ProvidersResponse { providers, has_any }))
}

async fn update_provider(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProviderUpdateRequest>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let key_name = match request.provider.as_str() {
        "anthropic" => "anthropic_key",
        "openai" => "openai_key",
        "openrouter" => "openrouter_key",
        "ollama" => "ollama_key",
        "zhipu" => "zhipu_key",
        "groq" => "groq_key",
        "together" => "together_key",
        "fireworks" => "fireworks_key",
        "deepseek" => "deepseek_key",
        "xai" => "xai_key",
        "mistral" => "mistral_key",
        "opencode-zen" => "opencode_zen_key",
        _ => {
            return Ok(Json(ProviderUpdateResponse {
                success: false,
                message: format!("Unknown provider: {}", request.provider),
            }));
        }
    };

    if request.api_key.trim().is_empty() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "API key cannot be empty".into(),
        }));
    }

    let config_path = state.config_path.read().await.clone();

    // Read existing config or create a new document
    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Ensure [llm] table exists
    if doc.get("llm").is_none() {
        doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
    }

    // Set the key
    doc["llm"][key_name] = toml_edit::value(request.api_key);

    // Auto-set routing defaults if the current routing points to a provider
    // the user doesn't have a key for. This prevents the common case where
    // someone sets up OpenRouter but routing still defaults to anthropic/*.
    let should_set_routing = {
        let current_channel = doc
            .get("defaults")
            .and_then(|d| d.get("routing"))
            .and_then(|r| r.get("channel"))
            .and_then(|v| v.as_str())
            .unwrap_or("anthropic/claude-sonnet-4-20250514");

        let current_provider = crate::llm::routing::provider_from_model(current_channel);

        // Check if the current routing provider has a key configured
        let has_key_for_current = match current_provider {
            "anthropic" => doc
                .get("llm")
                .and_then(|l| l.get("anthropic_key"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "openai" => doc
                .get("llm")
                .and_then(|l| l.get("openai_key"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "openrouter" => doc
                .get("llm")
                .and_then(|l| l.get("openrouter_key"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "ollama" => doc
                .get("llm")
                .and_then(|l| l.get("ollama_key"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "zhipu" => doc
                .get("llm")
                .and_then(|l| l.get("zhipu_key"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "opencode-zen" => doc
                .get("llm")
                .and_then(|l| l.get("opencode_zen_key"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            _ => false,
        };

        !has_key_for_current
    };

    if should_set_routing {
        let routing = crate::llm::routing::defaults_for_provider(&request.provider);

        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(defaults) = doc.get_mut("defaults").and_then(|d| d.as_table_mut()) {
            if defaults.get("routing").is_none() {
                defaults["routing"] = toml_edit::Item::Table(toml_edit::Table::new());
            }

            if let Some(routing_table) = defaults.get_mut("routing").and_then(|r| r.as_table_mut())
            {
                routing_table["channel"] = toml_edit::value(&routing.channel);
                routing_table["branch"] = toml_edit::value(&routing.branch);
                routing_table["worker"] = toml_edit::value(&routing.worker);
                routing_table["compactor"] = toml_edit::value(&routing.compactor);
                routing_table["cortex"] = toml_edit::value(&routing.cortex);
            }
        }
    }

    // Write back to disk
    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Signal the main loop that providers have been configured
    state
        .provider_setup_tx
        .try_send(crate::ProviderSetupEvent::ProvidersConfigured)
        .ok();

    let routing_note = if should_set_routing {
        format!(
            " Model routing updated to use {} defaults.",
            request.provider
        )
    } else {
        String::new()
    };

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!(
            "Provider '{}' configured.{}",
            request.provider, routing_note
        ),
    }))
}

async fn delete_provider(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(provider): axum::extract::Path<String>,
) -> Result<Json<ProviderUpdateResponse>, StatusCode> {
    let key_name = match provider.as_str() {
        "anthropic" => "anthropic_key",
        "openai" => "openai_key",
        "openrouter" => "openrouter_key",
        "ollama" => "ollama_key",
        "zhipu" => "zhipu_key",
        "groq" => "groq_key",
        "together" => "together_key",
        "fireworks" => "fireworks_key",
        "deepseek" => "deepseek_key",
        "xai" => "xai_key",
        "mistral" => "mistral_key",
        "opencode-zen" => "opencode_zen_key",
        _ => {
            return Ok(Json(ProviderUpdateResponse {
                success: false,
                message: format!("Unknown provider: {}", provider),
            }));
        }
    };

    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(ProviderUpdateResponse {
            success: false,
            message: "No config file found".into(),
        }));
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Remove the key from [llm]
    if let Some(llm) = doc.get_mut("llm") {
        if let Some(table) = llm.as_table_mut() {
            table.remove(key_name);
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProviderUpdateResponse {
        success: true,
        message: format!("Provider '{}' removed", provider),
    }))
}

// -- Model listing --

#[derive(Serialize, Clone)]
struct ModelInfo {
    /// Full routing string (e.g. "openrouter/anthropic/claude-sonnet-4-20250514")
    id: String,
    /// Human-readable name
    name: String,
    /// Provider ID ("anthropic", "openrouter", "openai", "ollama", "zhipu")
    provider: String,
    /// Context window size in tokens, if known
    context_window: Option<u64>,
    /// Whether this is a curated/recommended model
    curated: bool,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<ModelInfo>,
}

/// Static curated model list — the "known good" models we recommend.
fn curated_models() -> Vec<ModelInfo> {
    vec![
        // Anthropic (direct)
        ModelInfo {
            id: "anthropic/claude-sonnet-4-20250514".into(),
            name: "Claude Sonnet 4".into(),
            provider: "anthropic".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "anthropic/claude-haiku-4.5-20250514".into(),
            name: "Claude Haiku 4.5".into(),
            provider: "anthropic".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "anthropic/claude-opus-4-20250514".into(),
            name: "Claude Opus 4".into(),
            provider: "anthropic".into(),
            context_window: Some(200_000),
            curated: true,
        },
        // OpenRouter
        ModelInfo {
            id: "openrouter/anthropic/claude-sonnet-4-20250514".into(),
            name: "Claude Sonnet 4".into(),
            provider: "openrouter".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/anthropic/claude-haiku-4.5-20250514".into(),
            name: "Claude Haiku 4.5".into(),
            provider: "openrouter".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/anthropic/claude-opus-4-20250514".into(),
            name: "Claude Opus 4".into(),
            provider: "openrouter".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/google/gemini-2.5-pro-preview".into(),
            name: "Gemini 2.5 Pro".into(),
            provider: "openrouter".into(),
            context_window: Some(1_048_576),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/google/gemini-2.5-flash-preview".into(),
            name: "Gemini 2.5 Flash".into(),
            provider: "openrouter".into(),
            context_window: Some(1_048_576),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/deepseek/deepseek-r1".into(),
            name: "DeepSeek R1".into(),
            provider: "openrouter".into(),
            context_window: Some(163_840),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/deepseek/deepseek-chat-v3-0324".into(),
            name: "DeepSeek V3".into(),
            provider: "openrouter".into(),
            context_window: Some(163_840),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/moonshotai/kimi-k2".into(),
            name: "Kimi K2".into(),
            provider: "openrouter".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/moonshotai/kimi-k2.5".into(),
            name: "Kimi K2.5".into(),
            provider: "openrouter".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/openai/gpt-4.1".into(),
            name: "GPT-4.1".into(),
            provider: "openrouter".into(),
            context_window: Some(1_047_576),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/openai/gpt-4.1-mini".into(),
            name: "GPT-4.1 Mini".into(),
            provider: "openrouter".into(),
            context_window: Some(1_047_576),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/x-ai/grok-3-mini".into(),
            name: "Grok 3 Mini".into(),
            provider: "openrouter".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "openrouter/mistralai/mistral-medium-3".into(),
            name: "Mistral Medium 3".into(),
            provider: "openrouter".into(),
            context_window: Some(131_072),
            curated: true,
        },
        // OpenAI (direct)
        ModelInfo {
            id: "openai/gpt-4.1".into(),
            name: "GPT-4.1".into(),
            provider: "openai".into(),
            context_window: Some(1_047_576),
            curated: true,
        },
        ModelInfo {
            id: "openai/gpt-4.1-mini".into(),
            name: "GPT-4.1 Mini".into(),
            provider: "openai".into(),
            context_window: Some(1_047_576),
            curated: true,
        },
        ModelInfo {
            id: "openai/gpt-4.1-nano".into(),
            name: "GPT-4.1 Nano".into(),
            provider: "openai".into(),
            context_window: Some(1_047_576),
            curated: true,
        },
        ModelInfo {
            id: "openai/o3".into(),
            name: "o3".into(),
            provider: "openai".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "openai/o3-mini".into(),
            name: "o3 Mini".into(),
            provider: "openai".into(),
            context_window: Some(200_000),
            curated: true,
        },
        ModelInfo {
            id: "openai/o4-mini".into(),
            name: "o4 Mini".into(),
            provider: "openai".into(),
            context_window: Some(200_000),
            curated: true,
        },
        // Ollama Cloud (OpenAI-compatible)
        ModelInfo {
            id: "ollama/gpt-oss:120b".into(),
            name: "gpt-oss 120B".into(),
            provider: "ollama".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "ollama/gpt-oss:20b".into(),
            name: "gpt-oss 20B".into(),
            provider: "ollama".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "ollama/qwen3:30b-a3b".into(),
            name: "Qwen3 30B A3B".into(),
            provider: "ollama".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "ollama/devstral:24b".into(),
            name: "Devstral 24B".into(),
            provider: "ollama".into(),
            context_window: None,
            curated: true,
        },
        // Z.ai (GLM)
        ModelInfo {
            id: "zhipu/glm-4-plus".into(),
            name: "GLM-4 Plus".into(),
            provider: "zhipu".into(),
            context_window: Some(128_000),
            curated: true,
        },
        ModelInfo {
            id: "zhipu/glm-4-flash".into(),
            name: "GLM-4 Flash".into(),
            provider: "zhipu".into(),
            context_window: Some(128_000),
            curated: true,
        },
        ModelInfo {
            id: "zhipu/glm-4-flashx".into(),
            name: "GLM-4 FlashX".into(),
            provider: "zhipu".into(),
            context_window: Some(128_000),
            curated: true,
        },
        // Groq
        ModelInfo {
            id: "groq/llama-3.3-70b-versatile".into(),
            name: "Llama 3.3 70B Versatile".into(),
            provider: "groq".into(),
            context_window: Some(32_768),
            curated: true,
        },
        ModelInfo {
            id: "groq/llama-3.3-70b-specdec".into(),
            name: "Llama 3.3 70B Speculative Decoding".into(),
            provider: "groq".into(),
            context_window: Some(8192),
            curated: true,
        },
        ModelInfo {
            id: "groq/llama-3.1-70b-versatile".into(),
            name: "Llama 3.1 70B Versatile".into(),
            provider: "groq".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "groq/mixtral-8x7b-32768".into(),
            name: "Mixtral 8x7B".into(),
            provider: "groq".into(),
            context_window: Some(32_768),
            curated: true,
        },
        ModelInfo {
            id: "groq/gemma2-9b-it".into(),
            name: "Gemma 2 9B".into(),
            provider: "groq".into(),
            context_window: Some(8192),
            curated: true,
        },
        // Together AI
        ModelInfo {
            id: "together/meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo".into(),
            name: "Llama 3.1 405B Instruct Turbo".into(),
            provider: "together".into(),
            context_window: Some(130_815),
            curated: true,
        },
        ModelInfo {
            id: "together/meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo".into(),
            name: "Llama 3.1 70B Instruct Turbo".into(),
            provider: "together".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "together/meta-llama/Meta-Llama-3.3-70B-Instruct-Turbo".into(),
            name: "Llama 3.3 70B Instruct Turbo".into(),
            provider: "together".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "together/Qwen/Qwen2.5-72B-Instruct-Turbo".into(),
            name: "Qwen 2.5 72B Instruct Turbo".into(),
            provider: "together".into(),
            context_window: Some(32_768),
            curated: true,
        },
        ModelInfo {
            id: "together/deepseek-ai/DeepSeek-V3".into(),
            name: "DeepSeek V3".into(),
            provider: "together".into(),
            context_window: Some(65_536),
            curated: true,
        },
        // Fireworks AI
        ModelInfo {
            id: "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct".into(),
            name: "Llama 3.3 70B Instruct".into(),
            provider: "fireworks".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "fireworks/accounts/fireworks/models/llama-v3p1-405b-instruct".into(),
            name: "Llama 3.1 405B Instruct".into(),
            provider: "fireworks".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct".into(),
            name: "Llama 3.1 70B Instruct".into(),
            provider: "fireworks".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct".into(),
            name: "Llama 3.1 8B Instruct".into(),
            provider: "fireworks".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "fireworks/accounts/fireworks/models/qwen2p5-72b-instruct".into(),
            name: "Qwen 2.5 72B Instruct".into(),
            provider: "fireworks".into(),
            context_window: Some(32_768),
            curated: true,
        },
        // DeepSeek
        ModelInfo {
            id: "deepseek/deepseek-chat".into(),
            name: "DeepSeek Chat".into(),
            provider: "deepseek".into(),
            context_window: Some(65_536),
            curated: true,
        },
        ModelInfo {
            id: "deepseek/deepseek-reasoner".into(),
            name: "DeepSeek Reasoner".into(),
            provider: "deepseek".into(),
            context_window: Some(65_536),
            curated: true,
        },
        // xAI
        ModelInfo {
            id: "xai/grok-2-latest".into(),
            name: "Grok 2".into(),
            provider: "xai".into(),
            context_window: Some(131_072),
            curated: true,
        },
        ModelInfo {
            id: "xai/grok-2-vision-latest".into(),
            name: "Grok 2 Vision".into(),
            provider: "xai".into(),
            context_window: Some(32_768),
            curated: true,
        },
        ModelInfo {
            id: "xai/grok-beta".into(),
            name: "Grok Beta".into(),
            provider: "xai".into(),
            context_window: Some(131_072),
            curated: true,
        },
        // Mistral AI
        ModelInfo {
            id: "mistral/mistral-large-latest".into(),
            name: "Mistral Large".into(),
            provider: "mistral".into(),
            context_window: Some(128_000),
            curated: true,
        },
        ModelInfo {
            id: "mistral/mistral-small-latest".into(),
            name: "Mistral Small".into(),
            provider: "mistral".into(),
            context_window: Some(128_000),
            curated: true,
        },
        ModelInfo {
            id: "mistral/mistral-medium-latest".into(),
            name: "Mistral Medium".into(),
            provider: "mistral".into(),
            context_window: Some(128_000),
            curated: true,
        },
        ModelInfo {
            id: "mistral/codestral-latest".into(),
            name: "Codestral".into(),
            provider: "mistral".into(),
            context_window: Some(32_000),
            curated: true,
        },
        ModelInfo {
            id: "mistral/pixtral-large-latest".into(),
            name: "Pixtral Large".into(),
            provider: "mistral".into(),
            context_window: Some(128_000),
            curated: true,
        },
        // OpenCode Zen (OpenAI-compatible)
        ModelInfo {
            id: "opencode-zen/kimi-k2.5".into(),
            name: "Kimi K2.5".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "opencode-zen/kimi-k2".into(),
            name: "Kimi K2".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "opencode-zen/kimi-k2-thinking".into(),
            name: "Kimi K2 Thinking".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "opencode-zen/glm-5".into(),
            name: "GLM 5".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "opencode-zen/minimax-m2.5".into(),
            name: "MiniMax M2.5".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "opencode-zen/qwen3-coder".into(),
            name: "Qwen3 Coder 480B".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
        ModelInfo {
            id: "opencode-zen/big-pickle".into(),
            name: "Big Pickle".into(),
            provider: "opencode-zen".into(),
            context_window: None,
            curated: true,
        },
    ]
}

/// In-memory cache for dynamically fetched models.
static DYNAMIC_MODELS: std::sync::LazyLock<
    tokio::sync::RwLock<(Vec<ModelInfo>, std::time::Instant)>,
> = std::sync::LazyLock::new(|| tokio::sync::RwLock::new((Vec::new(), std::time::Instant::now())));

const MODEL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

async fn get_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ModelsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    // Determine which providers are configured
    let configured = configured_providers(&config_path).await;

    // Start with curated models, filtered to configured providers
    let mut models: Vec<ModelInfo> = curated_models()
        .into_iter()
        .filter(|m| configured.contains(&m.provider.as_str()))
        .collect();

    // Add cached dynamic models if fresh
    let cache = DYNAMIC_MODELS.read().await;
    if !cache.0.is_empty() && cache.1.elapsed() < MODEL_CACHE_TTL {
        for model in &cache.0 {
            if configured.contains(&model.provider.as_str()) {
                // Skip if already in curated list
                if !models.iter().any(|m| m.id == model.id) {
                    models.push(model.clone());
                }
            }
        }
    }
    drop(cache);

    Ok(Json(ModelsResponse { models }))
}

async fn refresh_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ModelsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    let configured = configured_providers(&config_path).await;

    let mut dynamic = Vec::new();

    // Query OpenRouter if configured
    if configured.contains(&"openrouter") {
        if let Ok(models) = fetch_openrouter_models(&config_path).await {
            dynamic.extend(models);
        }
    }

    // Cache the results
    let mut cache = DYNAMIC_MODELS.write().await;
    *cache = (dynamic, std::time::Instant::now());
    drop(cache);

    // Return full list (curated + dynamic)
    get_models(State(state)).await
}

/// Helper: which providers have keys configured
async fn configured_providers(config_path: &std::path::Path) -> Vec<&'static str> {
    let mut providers = Vec::new();

    let content = match tokio::fs::read_to_string(config_path).await {
        Ok(c) => c,
        Err(_) => return providers,
    };
    let doc: toml_edit::DocumentMut = match content.parse() {
        Ok(d) => d,
        Err(_) => return providers,
    };

    let has_key = |key: &str, env_var: &str| -> bool {
        if let Some(llm) = doc.get("llm") {
            if let Some(val) = llm.get(key) {
                if let Some(s) = val.as_str() {
                    if let Some(var_name) = s.strip_prefix("env:") {
                        return std::env::var(var_name).is_ok();
                    }
                    return !s.is_empty();
                }
            }
        }
        std::env::var(env_var).is_ok()
    };

    if has_key("anthropic_key", "ANTHROPIC_API_KEY") {
        providers.push("anthropic");
    }
    if has_key("openai_key", "OPENAI_API_KEY") {
        providers.push("openai");
    }
    if has_key("openrouter_key", "OPENROUTER_API_KEY") {
        providers.push("openrouter");
    }
    if has_key("ollama_key", "OLLAMA_API_KEY") {
        providers.push("ollama");
    }
    if has_key("zhipu_key", "ZHIPU_API_KEY") {
        providers.push("zhipu");
    }
    if has_key("groq_key", "GROQ_API_KEY") {
        providers.push("groq");
    }
    if has_key("together_key", "TOGETHER_API_KEY") {
        providers.push("together");
    }
    if has_key("fireworks_key", "FIREWORKS_API_KEY") {
        providers.push("fireworks");
    }
    if has_key("deepseek_key", "DEEPSEEK_API_KEY") {
        providers.push("deepseek");
    }
    if has_key("xai_key", "XAI_API_KEY") {
        providers.push("xai");
    }
    if has_key("mistral_key", "MISTRAL_API_KEY") {
        providers.push("mistral");
    }
    if has_key("opencode_zen_key", "OPENCODE_ZEN_API_KEY") {
        providers.push("opencode-zen");
    }

    providers
}

/// Fetch available models from OpenRouter's API.
async fn fetch_openrouter_models(config_path: &std::path::Path) -> anyhow::Result<Vec<ModelInfo>> {
    let content = tokio::fs::read_to_string(config_path).await?;
    let doc: toml_edit::DocumentMut = content.parse()?;

    let api_key = doc
        .get("llm")
        .and_then(|l| l.get("openrouter_key"))
        .and_then(|v| v.as_str())
        .map(|s| {
            if let Some(var) = s.strip_prefix("env:") {
                std::env::var(var).unwrap_or_default()
            } else {
                s.to_string()
            }
        })
        .unwrap_or_default();

    if api_key.is_empty() {
        anyhow::bail!("no openrouter key");
    }

    let client = reqwest::Client::new();
    let response = client
        .get("https://openrouter.ai/api/v1/models")
        .header("Authorization", format!("Bearer {api_key}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?;

    #[derive(Deserialize)]
    struct OpenRouterModelsResponse {
        data: Vec<OpenRouterModel>,
    }
    #[derive(Deserialize)]
    struct OpenRouterModel {
        id: String,
        name: Option<String>,
        context_length: Option<u64>,
    }

    let body: OpenRouterModelsResponse = response.json().await?;

    Ok(body
        .data
        .into_iter()
        .map(|m| ModelInfo {
            id: format!("openrouter/{}", m.id),
            name: m.name.unwrap_or_else(|| m.id.clone()),
            provider: "openrouter".into(),
            context_window: m.context_length,
            curated: false,
        })
        .collect())
}

// -- Ingest handlers --

#[derive(Deserialize)]
struct IngestQuery {
    agent_id: String,
}

#[derive(Deserialize)]
struct IngestDeleteQuery {
    agent_id: String,
    content_hash: String,
}

/// List ingested files with progress info for in-progress ones.
async fn list_ingest_files(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestQuery>,
) -> Result<Json<IngestFilesResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let rows = sqlx::query(
        r#"
        SELECT f.content_hash, f.filename, f.file_size, f.total_chunks, f.status,
               f.started_at, f.completed_at,
               COALESCE(p.done, 0) as chunks_completed
        FROM ingestion_files f
        LEFT JOIN (
            SELECT content_hash, COUNT(*) as done
            FROM ingestion_progress
            GROUP BY content_hash
        ) p ON f.content_hash = p.content_hash
        ORDER BY f.started_at DESC
        LIMIT 100
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to list ingest files");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let files = rows
        .into_iter()
        .map(|row| IngestFileInfo {
            content_hash: row.get("content_hash"),
            filename: row.get("filename"),
            file_size: row.get("file_size"),
            total_chunks: row.get("total_chunks"),
            chunks_completed: row.get("chunks_completed"),
            status: row.get("status"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
        })
        .collect();

    Ok(Json(IngestFilesResponse { files }))
}

/// Upload one or more files to the agent's ingest directory.
async fn upload_ingest_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestQuery>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<IngestUploadResponse>, StatusCode> {
    let workspaces = state.agent_workspaces.load();
    let workspace = workspaces
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let ingest_dir = workspace.join("ingest");

    tokio::fs::create_dir_all(&ingest_dir)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to create ingest directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut uploaded = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("upload-{}.txt", uuid::Uuid::new_v4()));

        let data = field.bytes().await.map_err(|error| {
            tracing::warn!(%error, "failed to read upload field");
            StatusCode::BAD_REQUEST
        })?;

        if data.is_empty() {
            continue;
        }

        // Sanitize filename to prevent path traversal
        let safe_name = Path::new(&filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload.txt");

        let target = ingest_dir.join(safe_name);

        // If file already exists, append a uuid suffix to avoid overwriting
        let target = if target.exists() {
            let stem = Path::new(safe_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("upload");
            let ext = Path::new(safe_name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("txt");
            let unique = format!(
                "{}-{}.{}",
                stem,
                &uuid::Uuid::new_v4().to_string()[..8],
                ext
            );
            ingest_dir.join(unique)
        } else {
            target
        };

        tokio::fs::write(&target, &data).await.map_err(|error| {
            tracing::warn!(%error, path = %target.display(), "failed to write uploaded file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        // Insert a queued record so the file appears in the UI immediately
        if let Ok(content) = std::str::from_utf8(&data) {
            let hash = crate::agent::ingestion::content_hash(content);
            let pools = state.agent_pools.load();
            if let Some(pool) = pools.get(&query.agent_id) {
                let file_size = data.len() as i64;
                let _ = sqlx::query(
                    r#"
                    INSERT OR IGNORE INTO ingestion_files (content_hash, filename, file_size, total_chunks, status)
                    VALUES (?, ?, ?, 0, 'queued')
                    "#,
                )
                .bind(&hash)
                .bind(safe_name)
                .bind(file_size)
                .execute(pool)
                .await;
            }
        }

        tracing::info!(
            agent_id = %query.agent_id,
            filename = %safe_name,
            bytes = data.len(),
            "file uploaded to ingest directory"
        );

        uploaded.push(safe_name.to_string());
    }

    Ok(Json(IngestUploadResponse { uploaded }))
}

/// Delete a completed ingestion file record from history.
async fn delete_ingest_file(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IngestDeleteQuery>,
) -> Result<Json<IngestDeleteResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    sqlx::query("DELETE FROM ingestion_files WHERE content_hash = ?")
        .bind(&query.content_hash)
        .execute(pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to delete ingest file record");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(IngestDeleteResponse { success: true }))
}

// -- Messaging / Bindings --

#[derive(Serialize, Clone)]
struct PlatformStatus {
    configured: bool,
    enabled: bool,
}

#[derive(Serialize)]
struct MessagingStatusResponse {
    discord: PlatformStatus,
    slack: PlatformStatus,
    webhook: PlatformStatus,
}

/// Get which messaging platforms are configured and enabled.
async fn messaging_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<MessagingStatusResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (discord, slack, webhook) = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to read config.toml for messaging status");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        let doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
            tracing::warn!(%error, "failed to parse config.toml for messaging status");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let discord_status = doc
            .get("messaging")
            .and_then(|m| m.get("discord"))
            .map(|d| {
                let has_token = d
                    .get("token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.is_empty());
                let enabled = d.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                PlatformStatus {
                    configured: has_token,
                    enabled: has_token && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let slack_status = doc
            .get("messaging")
            .and_then(|m| m.get("slack"))
            .map(|s| {
                let has_bot_token = s
                    .get("bot_token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| !t.is_empty());
                let has_app_token = s
                    .get("app_token")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| !t.is_empty());
                let enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                PlatformStatus {
                    configured: has_bot_token && has_app_token,
                    enabled: has_bot_token && has_app_token && enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        let webhook_status = doc
            .get("messaging")
            .and_then(|m| m.get("webhook"))
            .map(|w| {
                let enabled = w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                PlatformStatus {
                    configured: true,
                    enabled,
                }
            })
            .unwrap_or(PlatformStatus {
                configured: false,
                enabled: false,
            });

        (discord_status, slack_status, webhook_status)
    } else {
        let default = PlatformStatus {
            configured: false,
            enabled: false,
        };
        (default.clone(), default.clone(), default)
    };

    Ok(Json(MessagingStatusResponse {
        discord,
        slack,
        webhook,
    }))
}

#[derive(Serialize)]
struct BindingResponse {
    agent_id: String,
    channel: String,
    guild_id: Option<String>,
    workspace_id: Option<String>,
    chat_id: Option<String>,
    channel_ids: Vec<String>,
    dm_allowed_users: Vec<String>,
}

#[derive(Serialize)]
struct BindingsListResponse {
    bindings: Vec<BindingResponse>,
}

#[derive(Deserialize)]
struct BindingsQuery {
    #[serde(default)]
    agent_id: Option<String>,
}

/// List all bindings, optionally filtered by agent_id.
async fn list_bindings(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<BindingsQuery>,
) -> Json<BindingsListResponse> {
    let bindings_guard = state.bindings.read().await;
    let bindings = match bindings_guard.as_ref() {
        Some(arc_swap) => {
            let loaded = arc_swap.load();
            loaded.as_ref().clone()
        }
        None => Vec::new(),
    };
    drop(bindings_guard);

    let filtered: Vec<BindingResponse> = bindings
        .into_iter()
        .filter(|b| query.agent_id.as_ref().map_or(true, |id| &b.agent_id == id))
        .map(|b| BindingResponse {
            agent_id: b.agent_id,
            channel: b.channel,
            guild_id: b.guild_id,
            workspace_id: b.workspace_id,
            chat_id: b.chat_id,
            channel_ids: b.channel_ids,
            dm_allowed_users: b.dm_allowed_users,
        })
        .collect();

    Json(BindingsListResponse { bindings: filtered })
}

#[derive(Deserialize)]
struct CreateBindingRequest {
    agent_id: String,
    channel: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    channel_ids: Vec<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    /// Optional: set platform credentials if not yet configured.
    #[serde(default)]
    platform_credentials: Option<PlatformCredentials>,
}

#[derive(Deserialize)]
struct PlatformCredentials {
    /// Discord bot token.
    #[serde(default)]
    discord_token: Option<String>,
    /// Slack bot token.
    #[serde(default)]
    slack_bot_token: Option<String>,
    /// Slack app token.
    #[serde(default)]
    slack_app_token: Option<String>,
    /// Telegram bot token.
    #[serde(default)]
    telegram_token: Option<String>,
}

#[derive(Serialize)]
struct CreateBindingResponse {
    success: bool,
    /// True if platform credentials were added/changed (adapter needs restart).
    restart_required: bool,
    message: String,
}

/// Create a new binding (and optionally configure platform credentials).
async fn create_binding(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<CreateBindingRequest>,
) -> Result<Json<CreateBindingResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to read config.toml");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Track adapters that need to be started at runtime
    let mut new_discord_token: Option<String> = None;
    let mut new_slack_tokens: Option<(String, String)> = None;
    let mut new_telegram_token: Option<String> = None;

    // Write platform credentials if provided
    if let Some(credentials) = &request.platform_credentials {
        if let Some(token) = &credentials.discord_token {
            if !token.is_empty() {
                if doc.get("messaging").is_none() {
                    doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let messaging = doc["messaging"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                if !messaging.contains_key("discord") {
                    messaging["discord"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let discord = messaging["discord"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                discord["enabled"] = toml_edit::value(true);
                discord["token"] = toml_edit::value(token.as_str());
                new_discord_token = Some(token.clone());
            }
        }
        if let Some(bot_token) = &credentials.slack_bot_token {
            let app_token = credentials.slack_app_token.as_deref().unwrap_or("");
            if !bot_token.is_empty() && !app_token.is_empty() {
                if doc.get("messaging").is_none() {
                    doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let messaging = doc["messaging"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                if !messaging.contains_key("slack") {
                    messaging["slack"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let slack = messaging["slack"]
                    .as_table_mut()
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                slack["enabled"] = toml_edit::value(true);
                slack["bot_token"] = toml_edit::value(bot_token.as_str());
                slack["app_token"] = toml_edit::value(app_token);
                new_slack_tokens = Some((bot_token.clone(), app_token.to_string()));
            }
        }
        if let Some(token) = &credentials.telegram_token {
            if !token.is_empty() {
                if doc.get("messaging").is_none() {
                    doc["messaging"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let messaging = doc["messaging"].as_table_mut().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                if !messaging.contains_key("telegram") {
                    messaging["telegram"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let telegram = messaging["telegram"].as_table_mut().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                telegram["enabled"] = toml_edit::value(true);
                telegram["token"] = toml_edit::value(token.as_str());
                new_telegram_token = Some(token.clone());
            }
        }
    }

    // Add the binding to the [[bindings]] array
    if doc.get("bindings").is_none() {
        doc["bindings"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let bindings_array = doc["bindings"]
        .as_array_of_tables_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut binding_table = toml_edit::Table::new();
    binding_table["agent_id"] = toml_edit::value(&request.agent_id);
    binding_table["channel"] = toml_edit::value(&request.channel);
    if let Some(guild_id) = &request.guild_id {
        binding_table["guild_id"] = toml_edit::value(guild_id.as_str());
    }
    if let Some(workspace_id) = &request.workspace_id {
        binding_table["workspace_id"] = toml_edit::value(workspace_id.as_str());
    }
    if let Some(chat_id) = &request.chat_id {
        binding_table["chat_id"] = toml_edit::value(chat_id.as_str());
    }
    if !request.channel_ids.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.channel_ids {
            arr.push(id.as_str());
        }
        binding_table["channel_ids"] = toml_edit::value(arr);
    }
    if !request.dm_allowed_users.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.dm_allowed_users {
            arr.push(id.as_str());
        }
        binding_table["dm_allowed_users"] = toml_edit::value(arr);
    }
    bindings_array.push(binding_table);

    // Write back to disk
    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding created via API"
    );

    // Hot-reload bindings and permissions
    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
        drop(bindings_guard);

        // Rebuild Discord permissions
        if let Some(discord_config) = &new_config.messaging.discord {
            let new_perms = crate::config::DiscordPermissions::from_config(
                discord_config,
                &new_config.bindings,
            );
            let perms = state.discord_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        // Rebuild Slack permissions
        if let Some(slack_config) = &new_config.messaging.slack {
            let new_perms =
                crate::config::SlackPermissions::from_config(slack_config, &new_config.bindings);
            let perms = state.slack_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        // Hot-start new adapters if platform credentials were provided
        let manager_guard = state.messaging_manager.read().await;
        if let Some(manager) = manager_guard.as_ref() {
            if let Some(token) = new_discord_token {
                // Ensure Discord permissions exist for the new adapter
                let discord_perms = {
                    let perms_guard = state.discord_permissions.read().await;
                    match perms_guard.as_ref() {
                        Some(existing) => existing.clone(),
                        None => {
                            drop(perms_guard);
                            let perms = crate::config::DiscordPermissions::from_config(
                                new_config
                                    .messaging
                                    .discord
                                    .as_ref()
                                    .expect("discord config exists when token is provided"),
                                &new_config.bindings,
                            );
                            let arc_swap =
                                std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                            state.set_discord_permissions(arc_swap.clone()).await;
                            arc_swap
                        }
                    }
                };
                let adapter = crate::messaging::discord::DiscordAdapter::new(&token, discord_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start discord adapter");
                }
            }

            if let Some((bot_token, app_token)) = new_slack_tokens {
                let slack_perms = {
                    let perms_guard = state.slack_permissions.read().await;
                    match perms_guard.as_ref() {
                        Some(existing) => existing.clone(),
                        None => {
                            drop(perms_guard);
                            let perms = crate::config::SlackPermissions::from_config(
                                new_config
                                    .messaging
                                    .slack
                                    .as_ref()
                                    .expect("slack config exists when tokens are provided"),
                                &new_config.bindings,
                            );
                            let arc_swap =
                                std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms));
                            state.set_slack_permissions(arc_swap.clone()).await;
                            arc_swap
                        }
                    }
                };
                let adapter =
                    crate::messaging::slack::SlackAdapter::new(&bot_token, &app_token, slack_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start slack adapter");
                }
            }

            if let Some(token) = new_telegram_token {
                let telegram_perms = {
                    let perms = crate::config::TelegramPermissions::from_config(
                        new_config.messaging.telegram.as_ref().expect("telegram config exists when token is provided"),
                        &new_config.bindings,
                    );
                    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                };
                let adapter = crate::messaging::telegram::TelegramAdapter::new(&token, telegram_perms);
                if let Err(error) = manager.register_and_start(adapter).await {
                    tracing::error!(%error, "failed to hot-start telegram adapter");
                }
            }
        }
    }

    Ok(Json(CreateBindingResponse {
        success: true,
        restart_required: false,
        message: "Binding created and active.".to_string(),
    }))
}

#[derive(Deserialize)]
struct DeleteBindingRequest {
    agent_id: String,
    channel: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
}

#[derive(Serialize)]
struct DeleteBindingResponse {
    success: bool,
    message: String,
}

/// Update an existing binding.
#[derive(Deserialize)]
struct UpdateBindingRequest {
    // Original identifier (to find the binding)
    original_agent_id: String,
    original_channel: String,
    #[serde(default)]
    original_guild_id: Option<String>,
    #[serde(default)]
    original_workspace_id: Option<String>,
    #[serde(default)]
    original_chat_id: Option<String>,

    // New values
    agent_id: String,
    channel: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    channel_ids: Vec<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
}

#[derive(Serialize)]
struct UpdateBindingResponse {
    success: bool,
    message: String,
}

async fn update_binding(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<UpdateBindingRequest>,
) -> Result<Json<UpdateBindingResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let bindings_array = doc
        .get_mut("bindings")
        .and_then(|b| b.as_array_of_tables_mut())
        .ok_or(StatusCode::NOT_FOUND)?;

    // Find the matching binding by original identifiers
    let mut match_idx: Option<usize> = None;
    for (i, table) in bindings_array.iter().enumerate() {
        let matches_agent = table
            .get("agent_id")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == request.original_agent_id);
        let matches_channel = table
            .get("channel")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == request.original_channel);
        let matches_guild = match &request.original_guild_id {
            Some(gid) => table
                .get("guild_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == gid),
            None => table.get("guild_id").is_none(),
        };
        let matches_workspace = match &request.original_workspace_id {
            Some(wid) => table
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == wid),
            None => table.get("workspace_id").is_none(),
        };
        let matches_chat = match &request.original_chat_id {
            Some(cid) => table
                .get("chat_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == cid),
            None => table.get("chat_id").is_none(),
        };
        if matches_agent && matches_channel && matches_guild && matches_workspace && matches_chat {
            match_idx = Some(i);
            break;
        }
    }

    let Some(idx) = match_idx else {
        return Ok(Json(UpdateBindingResponse {
            success: false,
            message: "No matching binding found.".to_string(),
        }));
    };

    // Update the binding in place
    let binding = bindings_array
        .get_mut(idx)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    binding["agent_id"] = toml_edit::value(&request.agent_id);
    binding["channel"] = toml_edit::value(&request.channel);

    // Clear and set optional fields
    binding.remove("guild_id");
    binding.remove("workspace_id");
    binding.remove("chat_id");

    if let Some(ref guild_id) = request.guild_id {
        if !guild_id.is_empty() {
            binding["guild_id"] = toml_edit::value(guild_id);
        }
    }
    if let Some(ref workspace_id) = request.workspace_id {
        if !workspace_id.is_empty() {
            binding["workspace_id"] = toml_edit::value(workspace_id);
        }
    }
    if let Some(ref chat_id) = request.chat_id {
        if !chat_id.is_empty() {
            binding["chat_id"] = toml_edit::value(chat_id);
        }
    }

    // Update arrays
    if !request.channel_ids.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.channel_ids {
            arr.push(id.as_str());
        }
        binding["channel_ids"] = toml_edit::value(arr);
    } else {
        binding.remove("channel_ids");
    }

    if !request.dm_allowed_users.is_empty() {
        let mut arr = toml_edit::Array::new();
        for id in &request.dm_allowed_users {
            arr.push(id.as_str());
        }
        binding["dm_allowed_users"] = toml_edit::value(arr);
    } else {
        binding.remove("dm_allowed_users");
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding updated via API"
    );

    // Hot-reload bindings and permissions
    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
        drop(bindings_guard);

        if let Some(discord_config) = &new_config.messaging.discord {
            let new_perms = crate::config::DiscordPermissions::from_config(
                discord_config,
                &new_config.bindings,
            );
            let perms = state.discord_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        if let Some(slack_config) = &new_config.messaging.slack {
            let new_perms =
                crate::config::SlackPermissions::from_config(slack_config, &new_config.bindings);
            let perms = state.slack_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }
    }

    Ok(Json(UpdateBindingResponse {
        success: true,
        message: "Binding updated.".to_string(),
    }))
}

/// Delete a binding by matching agent_id + channel + platform-specific identifiers.
async fn delete_binding(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<DeleteBindingRequest>,
) -> Result<Json<DeleteBindingResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to read config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let bindings_array = doc
        .get_mut("bindings")
        .and_then(|b| b.as_array_of_tables_mut())
        .ok_or(StatusCode::NOT_FOUND)?;

    // Find the matching binding index by iterating
    let mut match_idx: Option<usize> = None;
    for (i, table) in bindings_array.iter().enumerate() {
        let matches_agent = table
            .get("agent_id")
            .and_then(|v: &toml_edit::Item| v.as_str())
            .is_some_and(|v| v == request.agent_id);
        let matches_channel = table
            .get("channel")
            .and_then(|v: &toml_edit::Item| v.as_str())
            .is_some_and(|v| v == request.channel);
        let matches_guild = match &request.guild_id {
            Some(gid) => table
                .get("guild_id")
                .and_then(|v: &toml_edit::Item| v.as_str())
                .is_some_and(|v| v == gid),
            None => table.get("guild_id").is_none(),
        };
        let matches_workspace = match &request.workspace_id {
            Some(wid) => table
                .get("workspace_id")
                .and_then(|v: &toml_edit::Item| v.as_str())
                .is_some_and(|v| v == wid),
            None => table.get("workspace_id").is_none(),
        };
        let matches_chat = match &request.chat_id {
            Some(cid) => table
                .get("chat_id")
                .and_then(|v: &toml_edit::Item| v.as_str())
                .is_some_and(|v| v == cid),
            None => table.get("chat_id").is_none(),
        };
        if matches_agent && matches_channel && matches_guild && matches_workspace && matches_chat {
            match_idx = Some(i);
            break;
        }
    }

    let Some(idx) = match_idx else {
        return Ok(Json(DeleteBindingResponse {
            success: false,
            message: "No matching binding found.".to_string(),
        }));
    };

    bindings_array.remove(idx);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(
        agent_id = %request.agent_id,
        channel = %request.channel,
        "binding deleted via API"
    );

    // Hot-reload bindings and permissions
    if let Ok(new_config) = crate::config::Config::load_from_path(&config_path) {
        let bindings_guard = state.bindings.read().await;
        if let Some(bindings_swap) = bindings_guard.as_ref() {
            bindings_swap.store(std::sync::Arc::new(new_config.bindings.clone()));
        }
        drop(bindings_guard);

        if let Some(discord_config) = &new_config.messaging.discord {
            let new_perms = crate::config::DiscordPermissions::from_config(
                discord_config,
                &new_config.bindings,
            );
            let perms = state.discord_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }

        if let Some(slack_config) = &new_config.messaging.slack {
            let new_perms =
                crate::config::SlackPermissions::from_config(slack_config, &new_config.bindings);
            let perms = state.slack_permissions.read().await;
            if let Some(arc_swap) = perms.as_ref() {
                arc_swap.store(std::sync::Arc::new(new_perms));
            }
        }
    }

    Ok(Json(DeleteBindingResponse {
        success: true,
        message: "Binding deleted.".to_string(),
    }))
}

// -- Global Settings handlers --

#[derive(Serialize)]
struct GlobalSettingsResponse {
    brave_search_key: Option<String>,
    api_enabled: bool,
    api_port: u16,
    api_bind: String,
    worker_log_mode: String,
    opencode: OpenCodeSettingsResponse,
}

#[derive(Serialize)]
struct OpenCodeSettingsResponse {
    enabled: bool,
    path: String,
    max_servers: usize,
    server_startup_timeout_secs: u64,
    max_restart_retries: u32,
    permissions: OpenCodePermissionsResponse,
}

#[derive(Serialize)]
struct OpenCodePermissionsResponse {
    edit: String,
    bash: String,
    webfetch: String,
}

#[derive(Deserialize)]
struct GlobalSettingsUpdate {
    brave_search_key: Option<String>,
    api_enabled: Option<bool>,
    api_port: Option<u16>,
    api_bind: Option<String>,
    worker_log_mode: Option<String>,
    opencode: Option<OpenCodeSettingsUpdate>,
}

#[derive(Deserialize)]
struct OpenCodeSettingsUpdate {
    enabled: Option<bool>,
    path: Option<String>,
    max_servers: Option<usize>,
    server_startup_timeout_secs: Option<u64>,
    max_restart_retries: Option<u32>,
    permissions: Option<OpenCodePermissionsUpdate>,
}

#[derive(Deserialize)]
struct OpenCodePermissionsUpdate {
    edit: Option<String>,
    bash: Option<String>,
    webfetch: Option<String>,
}

#[derive(Serialize)]
struct GlobalSettingsUpdateResponse {
    success: bool,
    message: String,
    requires_restart: bool,
}

async fn get_global_settings(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<GlobalSettingsResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let (brave_search_key, api_enabled, api_port, api_bind, worker_log_mode, opencode) =
        if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let doc: toml_edit::DocumentMut = content
                .parse()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let brave_search = doc
                .get("defaults")
                .and_then(|d| d.get("brave_search_key"))
                .and_then(|v| v.as_str())
                .map(|s| {
                    if let Some(var) = s.strip_prefix("env:") {
                        std::env::var(var).ok()
                    } else {
                        Some(s.to_string())
                    }
                })
                .flatten();

            let api_enabled = doc
                .get("api")
                .and_then(|a| a.get("enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let api_port = doc
                .get("api")
                .and_then(|a| a.get("port"))
                .and_then(|v| v.as_integer())
                .and_then(|i| u16::try_from(i).ok())
                .unwrap_or(19898);

            let api_bind = doc
                .get("api")
                .and_then(|a| a.get("bind"))
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();

            let worker_log_mode = doc
                .get("defaults")
                .and_then(|d| d.get("worker_log_mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("errors_only")
                .to_string();

            let opencode_table = doc.get("defaults").and_then(|d| d.get("opencode"));
            let opencode_perms = opencode_table.and_then(|o| o.get("permissions"));
            let opencode = OpenCodeSettingsResponse {
                enabled: opencode_table
                    .and_then(|o| o.get("enabled"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                path: opencode_table
                    .and_then(|o| o.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("opencode")
                    .to_string(),
                max_servers: opencode_table
                    .and_then(|o| o.get("max_servers"))
                    .and_then(|v| v.as_integer())
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or(5),
                server_startup_timeout_secs: opencode_table
                    .and_then(|o| o.get("server_startup_timeout_secs"))
                    .and_then(|v| v.as_integer())
                    .and_then(|i| u64::try_from(i).ok())
                    .unwrap_or(30),
                max_restart_retries: opencode_table
                    .and_then(|o| o.get("max_restart_retries"))
                    .and_then(|v| v.as_integer())
                    .and_then(|i| u32::try_from(i).ok())
                    .unwrap_or(5),
                permissions: OpenCodePermissionsResponse {
                    edit: opencode_perms
                        .and_then(|p| p.get("edit"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                    bash: opencode_perms
                        .and_then(|p| p.get("bash"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                    webfetch: opencode_perms
                        .and_then(|p| p.get("webfetch"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow")
                        .to_string(),
                },
            };

            (
                brave_search,
                api_enabled,
                api_port,
                api_bind,
                worker_log_mode,
                opencode,
            )
        } else {
            (
                None,
                true,
                19898,
                "127.0.0.1".to_string(),
                "errors_only".to_string(),
                OpenCodeSettingsResponse {
                    enabled: false,
                    path: "opencode".to_string(),
                    max_servers: 5,
                    server_startup_timeout_secs: 30,
                    max_restart_retries: 5,
                    permissions: OpenCodePermissionsResponse {
                        edit: "allow".to_string(),
                        bash: "allow".to_string(),
                        webfetch: "allow".to_string(),
                    },
                },
            )
        };

    Ok(Json(GlobalSettingsResponse {
        brave_search_key: brave_search_key,
        api_enabled,
        api_port,
        api_bind,
        worker_log_mode,
        opencode,
    }))
}

async fn update_global_settings(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GlobalSettingsUpdate>,
) -> Result<Json<GlobalSettingsUpdateResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut requires_restart = false;

    // Update brave_search_key
    if let Some(key) = request.brave_search_key {
        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if key.is_empty() {
            if let Some(table) = doc["defaults"].as_table_mut() {
                table.remove("brave_search_key");
            }
        } else {
            doc["defaults"]["brave_search_key"] = toml_edit::value(key);
        }
    }

    // Update API settings (requires restart)
    if request.api_enabled.is_some() || request.api_port.is_some() || request.api_bind.is_some() {
        requires_restart = true;

        if doc.get("api").is_none() {
            doc["api"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(enabled) = request.api_enabled {
            doc["api"]["enabled"] = toml_edit::value(enabled);
        }
        if let Some(port) = request.api_port {
            doc["api"]["port"] = toml_edit::value(i64::from(port));
        }
        if let Some(bind) = request.api_bind {
            doc["api"]["bind"] = toml_edit::value(bind);
        }
    }

    // Update worker_log_mode
    if let Some(mode) = request.worker_log_mode {
        // Validate the mode
        if !["errors_only", "all_separate", "all_combined"].contains(&mode.as_str()) {
            return Ok(Json(GlobalSettingsUpdateResponse {
                success: false,
                message: format!("Invalid worker log mode: {}", mode),
                requires_restart: false,
            }));
        }

        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["defaults"]["worker_log_mode"] = toml_edit::value(mode);
    }

    // Update OpenCode settings
    if let Some(opencode) = request.opencode {
        if doc.get("defaults").is_none() {
            doc["defaults"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if doc["defaults"].get("opencode").is_none() {
            doc["defaults"]["opencode"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        if let Some(enabled) = opencode.enabled {
            doc["defaults"]["opencode"]["enabled"] = toml_edit::value(enabled);
        }
        if let Some(path) = opencode.path {
            doc["defaults"]["opencode"]["path"] = toml_edit::value(path);
        }
        if let Some(max_servers) = opencode.max_servers {
            doc["defaults"]["opencode"]["max_servers"] = toml_edit::value(max_servers as i64);
        }
        if let Some(timeout) = opencode.server_startup_timeout_secs {
            doc["defaults"]["opencode"]["server_startup_timeout_secs"] =
                toml_edit::value(timeout as i64);
        }
        if let Some(retries) = opencode.max_restart_retries {
            doc["defaults"]["opencode"]["max_restart_retries"] = toml_edit::value(retries as i64);
        }
        if let Some(permissions) = opencode.permissions {
            if doc["defaults"]["opencode"].get("permissions").is_none() {
                doc["defaults"]["opencode"]["permissions"] =
                    toml_edit::Item::Table(toml_edit::Table::new());
            }
            if let Some(edit) = permissions.edit {
                doc["defaults"]["opencode"]["permissions"]["edit"] = toml_edit::value(edit);
            }
            if let Some(bash) = permissions.bash {
                doc["defaults"]["opencode"]["permissions"]["bash"] = toml_edit::value(bash);
            }
            if let Some(webfetch) = permissions.webfetch {
                doc["defaults"]["opencode"]["permissions"]["webfetch"] = toml_edit::value(webfetch);
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let message = if requires_restart {
        "Settings updated. API server changes require a restart to take effect.".to_string()
    } else {
        "Settings updated successfully.".to_string()
    };

    Ok(Json(GlobalSettingsUpdateResponse {
        success: true,
        message,
        requires_restart,
    }))
}

// -- Update handlers --

/// Return the current update status (from background check).
async fn update_check(State(state): State<Arc<ApiState>>) -> Json<crate::update::UpdateStatus> {
    let status = state.update_status.load();
    Json((**status).clone())
}

/// Force an immediate update check against GitHub.
async fn update_check_now(State(state): State<Arc<ApiState>>) -> Json<crate::update::UpdateStatus> {
    crate::update::check_for_update(&state.update_status).await;
    let status = state.update_status.load();
    Json((**status).clone())
}

/// Pull the new Docker image and recreate this container.
async fn update_apply(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match crate::update::apply_docker_update(&state.update_status).await {
        Ok(()) => Ok(Json(serde_json::json!({ "status": "updating" }))),
        Err(error) => {
            tracing::error!(%error, "update apply failed");
            Ok(Json(serde_json::json!({
                "status": "error",
                "error": error.to_string(),
            })))
        }
    }
}

// -- Raw config endpoints --

#[derive(Serialize)]
struct RawConfigResponse {
    content: String,
}

#[derive(Deserialize)]
struct RawConfigUpdateRequest {
    content: String,
}

#[derive(Serialize)]
struct RawConfigUpdateResponse {
    success: bool,
    message: String,
}

async fn get_raw_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<RawConfigResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to read config.toml");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    } else {
        String::new()
    };

    Ok(Json(RawConfigResponse { content }))
}

async fn update_raw_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RawConfigUpdateRequest>,
) -> Result<Json<RawConfigUpdateResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Validate TOML syntax and config structure
    if let Err(error) = crate::config::Config::validate_toml(&request.content) {
        return Ok(Json(RawConfigUpdateResponse {
            success: false,
            message: format!("Validation error: {error}"),
        }));
    }

    // Write to disk
    tokio::fs::write(&config_path, &request.content)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!("config.toml updated via raw editor");

    // Reload RuntimeConfig for all agents
    match crate::config::Config::load_from_path(&config_path) {
        Ok(new_config) => {
            let runtime_configs = state.runtime_configs.load();
            for (agent_id, rc) in runtime_configs.iter() {
                rc.reload_config(&new_config, agent_id);
            }
        }
        Err(error) => {
            tracing::warn!(%error, "config.toml written but failed to reload immediately");
        }
    }

    Ok(Json(RawConfigUpdateResponse {
        success: true,
        message: "Config saved and reloaded.".to_string(),
    }))
}

// -- Static file serving --

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(content) = InterfaceAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            content.data,
        )
            .into_response();
    }

    // SPA fallback
    if let Some(content) = InterfaceAssets::get("index.html") {
        return Html(std::str::from_utf8(&content.data).unwrap_or("").to_string()).into_response();
    }

    (StatusCode::NOT_FOUND, "not found").into_response()
}
