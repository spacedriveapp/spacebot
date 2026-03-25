use super::state::{AgentInfo, ApiState};

use crate::agent::cortex::CortexLogger;
use crate::conversation::channels::ChannelStore;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn hosted_agent_limit() -> Option<usize> {
    let deployment = std::env::var("SPACEBOT_DEPLOYMENT").ok()?;
    if !deployment.eq_ignore_ascii_case("hosted") {
        return None;
    }

    std::env::var("SPACEBOT_MAX_AGENTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AgentsResponse {
    agents: Vec<AgentInfo>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AgentOverviewResponse {
    memory_counts: HashMap<String, i64>,
    memory_total: i64,
    channel_count: usize,
    cron_jobs: Vec<CronJobInfo>,
    last_bulletin_at: Option<String>,
    recent_cortex_events: Vec<crate::agent::cortex::CortexEvent>,
    memory_daily: Vec<DayCount>,
    activity_daily: Vec<ActivityDayCount>,
    activity_heatmap: Vec<HeatmapCell>,
    latest_bulletin: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct DayCount {
    date: String,
    count: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ActivityDayCount {
    date: String,
    branches: i64,
    workers: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
struct HeatmapCell {
    day: i64,
    hour: i64,
    count: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
struct CronJobInfo {
    id: String,
    prompt: String,
    cron_expr: Option<String>,
    interval_secs: u64,
    delivery_target: String,
    enabled: bool,
    run_once: bool,
    active_hours: Option<(u8, u8)>,
    timeout_secs: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct InstanceOverviewResponse {
    version: &'static str,
    uptime_seconds: u64,
    pid: u32,
    agents: Vec<AgentSummary>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct AgentSummary {
    id: String,
    channel_count: usize,
    memory_total: i64,
    cron_job_count: usize,
    activity_sparkline: Vec<i64>,
    last_activity_at: Option<String>,
    last_bulletin_at: Option<String>,
    profile: Option<crate::agent::cortex::AgentProfile>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AgentProfileResponse {
    profile: Option<crate::agent::cortex::AgentProfile>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct IdentityResponse {
    soul: Option<String>,
    identity: Option<String>,
    role: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct IdentityQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct IdentityUpdateRequest {
    agent_id: String,
    soul: Option<String>,
    identity: Option<String>,
    role: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AgentOverviewQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateAgentRequest {
    pub agent_id: String,
    pub display_name: Option<String>,
    pub role: Option<String>,
}

/// Result from internal agent creation logic.
pub struct CreateAgentResult {
    pub success: bool,
    pub agent_id: String,
    pub message: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateAgentRequest {
    agent_id: String,
    display_name: Option<String>,
    role: Option<String>,
    gradient_start: Option<String>,
    gradient_end: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct DeleteAgentQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AgentMcpQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ReconnectMcpRequest {
    agent_id: String,
    server_name: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AgentMcpResponse {
    servers: Vec<crate::mcp::McpServerStatus>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct WarmupQuery {
    agent_id: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct WarmupTriggerRequest {
    agent_id: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WarmupStatusEntry {
    agent_id: String,
    status: crate::config::WarmupStatus,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WarmupStatusResponse {
    statuses: Vec<WarmupStatusEntry>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WarmupTriggerResponse {
    status: &'static str,
    forced: bool,
    accepted_agents: Vec<String>,
}

fn hydrate_warmup_status(
    runtime_config: &crate::config::RuntimeConfig,
) -> crate::config::WarmupStatus {
    let mut status = runtime_config.warmup_status.load().as_ref().clone();
    let now_ms = chrono::Utc::now().timestamp_millis();
    status.bulletin_age_secs = compute_bulletin_age_secs(status.last_refresh_unix_ms, now_ms);
    status
}

fn compute_bulletin_age_secs(last_refresh_unix_ms: Option<i64>, now_unix_ms: i64) -> Option<u64> {
    last_refresh_unix_ms.map(|refresh_ms| {
        if now_unix_ms > refresh_ms {
            ((now_unix_ms - refresh_ms) / 1000) as u64
        } else {
            0
        }
    })
}

fn resolve_warmup_agent_ids(
    requested_agent_id: Option<&str>,
    runtime_config_ids: &HashSet<String>,
    memory_search_ids: &HashSet<String>,
    mcp_manager_ids: &HashSet<String>,
    sqlite_pool_ids: &HashSet<String>,
) -> Result<Vec<String>, StatusCode> {
    let target_agent_ids = if let Some(agent_id) = requested_agent_id {
        vec![agent_id.to_string()]
    } else {
        runtime_config_ids.iter().cloned().collect::<Vec<_>>()
    };
    let single_target = requested_agent_id.is_some();

    let mut accepted_agents = Vec::new();
    for agent_id in target_agent_ids {
        if !runtime_config_ids.contains(&agent_id) {
            if single_target {
                return Err(StatusCode::NOT_FOUND);
            }
            continue;
        }

        if !memory_search_ids.contains(&agent_id) {
            tracing::warn!(agent_id = %agent_id, "missing memory search for warmup trigger");
            if single_target {
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            continue;
        }

        if !mcp_manager_ids.contains(&agent_id) {
            tracing::warn!(agent_id = %agent_id, "missing mcp manager for warmup trigger");
            if single_target {
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            continue;
        }

        if !sqlite_pool_ids.contains(&agent_id) {
            tracing::warn!(agent_id = %agent_id, "missing sqlite pool for warmup trigger");
            if single_target {
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            continue;
        }

        accepted_agents.push(agent_id);
    }

    Ok(accepted_agents)
}

/// List all configured agents with their config summaries.
#[utoipa::path(
    get,
    path = "/agents",
    responses(
        (status = 200, body = AgentsResponse),
    ),
    tag = "agents",
)]
pub(super) async fn list_agents(State(state): State<Arc<ApiState>>) -> Json<AgentsResponse> {
    let agents = state.agent_configs.load();
    Json(AgentsResponse {
        agents: agents.as_ref().clone(),
    })
}

/// List MCP connection status for an agent.
#[utoipa::path(
    get,
    path = "/agents/mcp",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = AgentMcpResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn list_agent_mcp(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentMcpQuery>,
) -> Result<Json<AgentMcpResponse>, StatusCode> {
    let managers = state.mcp_managers.load();
    let manager = managers
        .get(&query.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    let servers = manager.statuses().await;
    Ok(Json(AgentMcpResponse { servers }))
}

/// Force reconnect for a single MCP server on an agent.
#[utoipa::path(
    post,
    path = "/agents/mcp/reconnect",
    request_body = ReconnectMcpRequest,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Agent not found"),
        (status = 400, description = "Failed to reconnect"),
    ),
    tag = "agents",
)]
pub(super) async fn reconnect_agent_mcp(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ReconnectMcpRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let managers = state.mcp_managers.load();
    let manager = managers
        .get(&request.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    manager
        .reconnect(&request.server_name)
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                agent_id = %request.agent_id,
                server_name = %request.server_name,
                "failed to reconnect mcp server"
            );
            StatusCode::BAD_REQUEST
        })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "agent_id": request.agent_id,
        "server_name": request.server_name
    })))
}

/// Get warmup status for one agent or all agents.
#[utoipa::path(
    get,
    path = "/agents/warmup",
    params(
        ("agent_id" = Option<String>, Query, description = "Optional agent ID to get status for a specific agent"),
    ),
    responses(
        (status = 200, body = WarmupStatusResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn get_warmup_status(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WarmupQuery>,
) -> Result<Json<WarmupStatusResponse>, StatusCode> {
    let runtime_configs = state.runtime_configs.load();

    let mut statuses = if let Some(agent_id) = query.agent_id {
        let runtime_config = runtime_configs
            .get(&agent_id)
            .ok_or(StatusCode::NOT_FOUND)?;
        vec![WarmupStatusEntry {
            agent_id,
            status: hydrate_warmup_status(runtime_config),
        }]
    } else {
        runtime_configs
            .iter()
            .map(|(agent_id, runtime_config)| WarmupStatusEntry {
                agent_id: agent_id.clone(),
                status: hydrate_warmup_status(runtime_config),
            })
            .collect::<Vec<_>>()
    };

    statuses.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    Ok(Json(WarmupStatusResponse { statuses }))
}

/// Trigger warmup for one agent or all agents.
#[utoipa::path(
    post,
    path = "/agents/warmup/trigger",
    request_body = WarmupTriggerRequest,
    responses(
        (status = 200, body = WarmupTriggerResponse),
        (status = 503, description = "LLM manager not available"),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn trigger_warmup(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<WarmupTriggerRequest>,
) -> Result<Json<WarmupTriggerResponse>, StatusCode> {
    let llm_manager = {
        let guard = state.llm_manager.read().await;
        guard.as_ref().cloned().ok_or_else(|| {
            tracing::error!("LLM manager not available for warmup trigger");
            StatusCode::SERVICE_UNAVAILABLE
        })?
    };

    let runtime_configs = state.runtime_configs.load();
    let memory_searches = state.memory_searches.load();
    let mcp_managers = state.mcp_managers.load();
    let pools = state.agent_pools.load();
    let sandboxes = state.sandboxes.load();
    let runtime_config_ids = runtime_configs.keys().cloned().collect::<HashSet<_>>();
    let memory_search_ids = memory_searches.keys().cloned().collect::<HashSet<_>>();
    let mcp_manager_ids = mcp_managers.keys().cloned().collect::<HashSet<_>>();
    let sqlite_pool_ids = pools.keys().cloned().collect::<HashSet<_>>();

    let accepted_agents = resolve_warmup_agent_ids(
        request.agent_id.as_deref(),
        &runtime_config_ids,
        &memory_search_ids,
        &mcp_manager_ids,
        &sqlite_pool_ids,
    )?;

    for agent_id in accepted_agents.iter() {
        let Some(runtime_config) = runtime_configs.get(agent_id).cloned() else {
            continue;
        };
        let Some(memory_search) = memory_searches.get(agent_id).cloned() else {
            continue;
        };
        let Some(mcp_manager) = mcp_managers.get(agent_id).cloned() else {
            continue;
        };
        let Some(sqlite_pool) = pools.get(agent_id).cloned() else {
            continue;
        };
        let Some(sandbox) = sandboxes.get(agent_id).cloned() else {
            continue;
        };
        let Some(task_store) = state.task_store.load().as_ref().clone() else {
            tracing::warn!(
                agent_id,
                "global task store not initialized, skipping warmup"
            );
            continue;
        };

        let llm_manager = llm_manager.clone();
        let force = request.force;
        let agent_id = agent_id.clone();
        let injection_tx = state.injection_tx.clone();
        let humans = (**state.agent_humans.load()).clone();
        tokio::spawn(async move {
            let (event_tx, memory_event_tx) = crate::create_process_event_buses();
            let project_store =
                std::sync::Arc::new(crate::projects::ProjectStore::new(sqlite_pool.clone()));
            let working_memory_tz = runtime_config
                .user_timezone
                .load()
                .as_deref()
                .or(runtime_config.cron_timezone.load().as_deref())
                .and_then(|tz| tz.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(chrono_tz::Tz::UTC);
            let working_memory =
                crate::memory::WorkingMemoryStore::new(sqlite_pool.clone(), working_memory_tz);
            let deps = crate::AgentDeps {
                agent_id: Arc::from(agent_id.as_str()),
                memory_search,
                llm_manager,
                mcp_manager,
                cron_tool: None,
                runtime_config,
                event_tx,
                memory_event_tx,
                sqlite_pool: sqlite_pool.clone(),
                messaging_manager: None,
                sandbox,
                task_store,
                project_store,
                links: Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
                agent_names: Arc::new(std::collections::HashMap::new()),
                humans: Arc::new(arc_swap::ArcSwap::from_pointee(humans)),
                process_control_registry: Arc::new(
                    crate::agent::process_control::ProcessControlRegistry::new(),
                ),
                injection_tx,
                working_memory,
            };
            let logger = CortexLogger::new(sqlite_pool);
            crate::agent::cortex::run_warmup_once(&deps, &logger, "api_trigger", force).await;
        });
    }

    Ok(Json(WarmupTriggerResponse {
        status: "warming",
        forced: request.force,
        accepted_agents,
    }))
}

/// Create a new agent and initialize it live (directories, databases, memory, identity, cron, cortex).
#[utoipa::path(
    post,
    path = "/agents",
    request_body = CreateAgentRequest,
    responses(
        (status = 201, body = serde_json::Value, description = "Agent created successfully"),
        (status = 400, description = "Invalid request or agent limit reached"),
        (status = 409, description = "Agent already exists"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "agents",
)]
pub(super) async fn create_agent(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateAgentRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    match create_agent_internal(&state, request).await {
        Ok(result) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "success": result.success,
                "agent_id": result.agent_id,
                "message": result.message
            })),
        ),
        Err(message) => {
            let status = if message.contains("already exists") {
                StatusCode::CONFLICT
            } else if message.contains("cannot be empty") || message.contains("agent limit") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({
                    "success": false,
                    "message": message
                })),
            )
        }
    }
}

/// Internal agent creation logic shared between the API handler and factory tools.
pub async fn create_agent_internal(
    state: &Arc<ApiState>,
    request: CreateAgentRequest,
) -> Result<CreateAgentResult, String> {
    if let Some(limit) = hosted_agent_limit() {
        let existing = state.agent_configs.load();
        if existing.len() >= limit {
            return Err(format!(
                "agent limit reached for this instance: up to {limit} agent{}",
                if limit == 1 { "" } else { "s" }
            ));
        }
    }

    let agent_id = request.agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Err("Agent ID cannot be empty".into());
    }

    {
        let existing = state.agent_configs.load();
        if existing.iter().any(|a| a.id == agent_id) {
            return Err(format!("Agent '{agent_id}' already exists"));
        }
    }

    let config_path = state.config_path.read().await.clone();
    let instance_dir = (**state.instance_dir.load()).clone();

    // Acquire the config write mutex to prevent concurrent read-modify-write races.
    let _config_guard = state.config_write_mutex.lock().await;

    // Fail early if messaging manager is unavailable — before any config write,
    // directory creation, or database init that would leave a half-created agent.
    let messaging_manager = {
        let guard = state.messaging_manager.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                "Messaging manager not initialized. Please ensure messaging adapters are configured before creating agents.".to_string()
            })?
    };

    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to read config.toml");
                format!("failed to read config.toml: {error}")
            })?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
        tracing::warn!(%error, "failed to parse config.toml");
        format!("failed to parse config.toml: {error}")
    })?;

    if doc.get("agents").is_none() {
        doc["agents"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let agents_array = doc["agents"]
        .as_array_of_tables_mut()
        .ok_or_else(|| "agents is not an array of tables in config.toml".to_string())?;

    // Revalidate uniqueness under the lock — another request may have written the
    // same agent_id to config.toml between our first check and mutex acquisition.
    // Check against the parsed TOML document (agents_array) rather than the stale
    // in-memory cache to ensure we catch concurrent writes.
    if agents_array.iter().any(|t| {
        t.get("id")
            .and_then(|v| v.as_str())
            .map(|id| id == agent_id)
            .unwrap_or(false)
    }) {
        return Err(format!("Agent '{agent_id}' already exists"));
    }

    let mut new_table = toml_edit::Table::new();
    new_table["id"] = toml_edit::value(&agent_id);
    if let Some(display_name) = &request.display_name
        && !display_name.is_empty()
    {
        new_table["display_name"] = toml_edit::value(display_name.as_str());
    }
    if let Some(role) = &request.role
        && !role.is_empty()
    {
        new_table["role"] = toml_edit::value(role.as_str());
    }
    agents_array.push(new_table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            format!("failed to write config.toml: {error}")
        })?;

    // Release the config write mutex — remaining work doesn't touch config.toml.
    drop(_config_guard);

    // Read defaults directly from the config we just wrote to disk rather than
    // relying on the cached `defaults_config` which may be stale (e.g. if a
    // provider was configured but the in-memory cache wasn't refreshed yet).
    let disk_defaults = match crate::config::Config::load_from_path(&config_path) {
        Ok(fresh_config) => {
            // Also update the in-memory cache so subsequent operations
            // (e.g. creating another agent) don't hit stale defaults.
            state
                .set_defaults_config(fresh_config.defaults.clone())
                .await;
            Some(fresh_config.defaults)
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "failed to reload config.toml for defaults; falling back to cached defaults"
            );
            None
        }
    };
    let cached_defaults;
    let defaults = if let Some(ref d) = disk_defaults {
        d
    } else {
        cached_defaults = state.defaults_config.read().await;
        cached_defaults.as_ref().ok_or_else(|| {
            tracing::error!("defaults config not available");
            "defaults config not available".to_string()
        })?
    };

    let raw_config = crate::config::AgentConfig {
        id: agent_id.clone(),
        default: false,
        display_name: request.display_name.clone().filter(|s| !s.is_empty()),
        role: request.role.clone().filter(|s| !s.is_empty()),
        gradient_start: None,
        gradient_end: None,
        workspace: None,
        routing: None,
        max_concurrent_branches: None,
        max_concurrent_workers: None,
        max_turns: None,
        branch_max_turns: None,
        context_window: None,
        compaction: None,
        memory_persistence: None,
        coalesce: None,
        ingestion: None,
        cortex: None,
        warmup: None,
        browser: None,
        channel: None,
        mcp: None,
        brave_search_key: None,
        cron_timezone: None,
        user_timezone: None,
        sandbox: None,
        projects: None,
        cron: Vec::new(),
    };
    let agent_config = raw_config.resolve(&instance_dir, defaults);

    for dir in [
        &agent_config.workspace,
        &agent_config.data_dir,
        &agent_config.archives_dir,
        &agent_config.ingest_dir(),
        &agent_config.logs_dir(),
    ] {
        std::fs::create_dir_all(dir).map_err(|error| {
            tracing::error!(%error, dir = %dir.display(), "failed to create agent directory");
            format!("failed to create directory {}: {error}", dir.display())
        })?;
    }

    let db = crate::db::Db::connect(&agent_config.data_dir)
        .await
        .map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to connect agent databases");
            format!("failed to connect databases: {error}")
        })?;

    let settings_path = agent_config.data_dir.join("settings.redb");
    let settings_store = std::sync::Arc::new(
        crate::settings::SettingsStore::new(&settings_path).map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to init settings store");
            format!("failed to init settings store: {error}")
        })?,
    );

    let embedding_model = {
        let guard = state.embedding_model.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("embedding model not available");
                "embedding model not available".to_string()
            })?
            .clone()
    };

    let memory_store = crate::memory::MemoryStore::new(db.sqlite.clone());
    let embedding_table = crate::memory::EmbeddingTable::open_or_create(&db.lance)
        .await
        .map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to init embeddings");
            format!("failed to init embeddings: {error}")
        })?;

    if let Err(error) = embedding_table.ensure_fts_index().await {
        tracing::warn!(%error, agent_id = %agent_id, "failed to create FTS index");
    }

    let memory_search = std::sync::Arc::new(crate::memory::MemorySearch::new(
        memory_store,
        embedding_table,
        embedding_model,
    ));
    let task_store = state
        .task_store
        .load()
        .as_ref()
        .clone()
        .ok_or_else(|| "global task store not initialized".to_string())?;

    let (event_tx, memory_event_tx) = crate::create_process_event_buses();
    let arc_agent_id: crate::AgentId = std::sync::Arc::from(agent_id.as_str());

    crate::identity::scaffold_identity_files(&agent_config.identity_dir)
        .await
        .map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to scaffold identity files");
            format!("failed to scaffold identity files: {error}")
        })?;
    let identity = crate::identity::Identity::load(&agent_config.identity_dir).await;

    let skills =
        crate::skills::SkillSet::load(&instance_dir.join("skills"), &agent_config.skills_dir())
            .await;

    let prompt_engine = {
        let guard = state.prompt_engine.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("prompt engine not available");
                "prompt engine not available".to_string()
            })?
            .clone()
    };

    let defaults_for_runtime = if let Some(d) = disk_defaults {
        d
    } else {
        let guard = state.defaults_config.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("defaults config not available");
                "defaults config not available".to_string()
            })?
            .clone()
    };

    let runtime_config = std::sync::Arc::new(crate::config::RuntimeConfig::new(
        &instance_dir,
        &agent_config,
        &defaults_for_runtime,
        prompt_engine,
        identity,
        skills,
    ));
    let explicit_listen_only = raw_config.channel.map(|channel| channel.listen_only_mode);
    runtime_config.set_settings(settings_store.clone(), explicit_listen_only);

    let llm_manager = {
        let guard = state.llm_manager.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("LLM manager not available");
                "LLM manager not available".to_string()
            })?
            .clone()
    };

    let mcp_manager = std::sync::Arc::new(crate::mcp::McpManager::new(agent_config.mcp.clone()));
    mcp_manager.connect_all().await;

    let sandbox = std::sync::Arc::new(
        crate::sandbox::Sandbox::new(
            runtime_config.sandbox.clone(),
            agent_config.workspace.clone(),
            &instance_dir,
            agent_config.data_dir.clone(),
        )
        .await,
    );

    let project_store = std::sync::Arc::new(crate::projects::ProjectStore::new(db.sqlite.clone()));

    // Inject active project root paths into the sandbox allowlist.
    crate::projects::refresh_sandbox_project_paths(&project_store, &arc_agent_id, &sandbox).await;

    let deps = crate::AgentDeps {
        agent_id: arc_agent_id.clone(),
        memory_search: memory_search.clone(),
        llm_manager,
        mcp_manager: mcp_manager.clone(),
        task_store: task_store.clone(),
        project_store: project_store.clone(),
        cron_tool: None,
        runtime_config: runtime_config.clone(),
        event_tx: event_tx.clone(),
        memory_event_tx: memory_event_tx.clone(),
        sqlite_pool: db.sqlite.clone(),
        messaging_manager: Some(messaging_manager.clone()),
        sandbox: sandbox.clone(),
        links: Arc::new(arc_swap::ArcSwap::from_pointee(
            (**state.agent_links.load()).clone(),
        )),
        process_control_registry: Arc::new(
            crate::agent::process_control::ProcessControlRegistry::new(),
        ),
        injection_tx: state.injection_tx.clone(),
        agent_names: {
            let configs = state.agent_configs.load();
            let mut names: std::collections::HashMap<String, String> = configs
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        c.display_name.clone().unwrap_or_else(|| c.id.clone()),
                    )
                })
                .collect();
            names.entry(agent_id.clone()).or_insert_with(|| {
                request
                    .display_name
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| agent_id.clone())
            });
            Arc::new(names)
        },
        humans: Arc::new(arc_swap::ArcSwap::from_pointee(
            (**state.agent_humans.load()).clone(),
        )),
        working_memory: {
            let tz = agent_config
                .user_timezone
                .as_deref()
                .or(agent_config.cron_timezone.as_deref())
                .and_then(|tz| tz.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(chrono_tz::Tz::UTC);
            crate::memory::WorkingMemoryStore::new(db.sqlite.clone(), tz)
        },
    };

    let event_rx = event_tx.subscribe();
    state.register_agent_events(agent_id.clone(), event_rx);

    let cron_store = std::sync::Arc::new(crate::cron::CronStore::new(db.sqlite.clone()));
    let cron_context = crate::cron::CronContext {
        deps: deps.clone(),
        screenshot_dir: agent_config.screenshot_dir(),
        logs_dir: agent_config.logs_dir(),
        messaging_manager: messaging_manager.clone(),
        store: cron_store.clone(),
    };
    let scheduler = std::sync::Arc::new(crate::cron::Scheduler::new(cron_context));
    runtime_config.set_cron(cron_store.clone(), scheduler.clone());

    let cron_tool =
        crate::tools::CronTool::new(cron_store.clone(), scheduler.clone(), messaging_manager);

    let browser_config = (**runtime_config.browser_config.load()).clone();
    let brave_search_key = (**runtime_config.brave_search_key.load()).clone();
    let conversation_logger =
        crate::conversation::history::ConversationLogger::new(db.sqlite.clone());
    let channel_store = crate::conversation::ChannelStore::new(db.sqlite.clone());
    let run_logger = crate::conversation::ProcessRunLogger::new(db.sqlite.clone());
    let cortex_ctx = crate::agent::cortex_chat::CortexChatSession::create_context();
    let cortex_tool_server = crate::tools::create_cortex_chat_tool_server(
        deps.agent_id.clone(),
        deps.clone(),
        deps.task_store.clone(),
        memory_search.clone(),
        deps.memory_event_tx.clone(),
        conversation_logger,
        channel_store,
        run_logger,
        browser_config,
        agent_config.screenshot_dir(),
        brave_search_key,
        runtime_config.workspace_dir.clone(),
        sandbox.clone(),
        runtime_config.clone(),
        state.clone(),
        Some(cortex_ctx.clone()),
    );
    // Add factory tools to the cortex chat tool server
    if let Err(error) =
        crate::tools::add_factory_tools(&cortex_tool_server, state.clone(), memory_search.clone())
            .await
    {
        tracing::warn!(%error, agent_id = %agent_id, "failed to add factory tools to cortex chat");
    }

    let cortex_store = crate::agent::cortex_chat::CortexChatStore::new(db.sqlite.clone());
    let cortex_session = crate::agent::cortex_chat::CortexChatSession::new(
        deps.clone(),
        cortex_tool_server,
        cortex_store,
        cortex_ctx,
    )
    .with_factory(true);

    let cortex_logger = crate::agent::cortex::CortexLogger::new(db.sqlite.clone());
    let _warmup_loop = crate::agent::cortex::spawn_warmup_loop(deps.clone(), cortex_logger.clone());
    let _cortex_loop = crate::agent::cortex::spawn_cortex_loop(deps.clone(), cortex_logger.clone());
    let _association_loop =
        crate::agent::cortex::spawn_association_loop(deps.clone(), cortex_logger);
    crate::agent::cortex::spawn_ready_task_loop(
        deps.clone(),
        crate::agent::cortex::CortexLogger::new(db.sqlite.clone()),
    );

    let ingestion_config = **runtime_config.ingestion.load();
    if ingestion_config.enabled {
        crate::agent::ingestion::spawn_ingestion_loop(agent_config.ingest_dir(), deps.clone());
    }

    let sqlite_pool = db.sqlite.clone();
    let mut deps_with_cron = deps.clone();
    deps_with_cron.cron_tool = Some(cron_tool);
    let agent = crate::Agent {
        id: arc_agent_id.clone(),
        config: agent_config.clone(),
        db,
        deps: deps_with_cron,
    };
    if let Err(error) = state.agent_tx.send(agent).await {
        tracing::error!(%error, "failed to send new agent to main loop");
    }

    {
        let mut pools = (**state.agent_pools.load()).clone();
        pools.insert(agent_id.clone(), sqlite_pool);
        state.agent_pools.store(std::sync::Arc::new(pools));

        let mut searches = (**state.memory_searches.load()).clone();
        searches.insert(agent_id.clone(), memory_search);
        state.memory_searches.store(std::sync::Arc::new(searches));

        let mut workspaces = (**state.agent_workspaces.load()).clone();
        workspaces.insert(agent_id.clone(), agent_config.workspace.clone());
        state
            .agent_workspaces
            .store(std::sync::Arc::new(workspaces));

        let mut identity_dirs = (**state.agent_identity_dirs.load()).clone();
        identity_dirs.insert(agent_id.clone(), agent_config.identity_dir.clone());
        state
            .agent_identity_dirs
            .store(std::sync::Arc::new(identity_dirs));

        let mut data_dirs = (**state.agent_data_dirs.load()).clone();
        data_dirs.insert(agent_id.clone(), agent_config.data_dir.clone());
        state.agent_data_dirs.store(std::sync::Arc::new(data_dirs));

        let mut configs = (**state.runtime_configs.load()).clone();
        configs.insert(agent_id.clone(), runtime_config);
        state.runtime_configs.store(std::sync::Arc::new(configs));

        let mut mcp_managers = (**state.mcp_managers.load()).clone();
        mcp_managers.insert(agent_id.clone(), mcp_manager);
        state.mcp_managers.store(std::sync::Arc::new(mcp_managers));

        let mut sandboxes = (**state.sandboxes.load()).clone();
        sandboxes.insert(agent_id.clone(), sandbox);
        state.sandboxes.store(std::sync::Arc::new(sandboxes));

        let mut project_stores_map = (**state.project_stores.load()).clone();
        project_stores_map.insert(agent_id.clone(), project_store);
        state
            .project_stores
            .store(std::sync::Arc::new(project_stores_map));

        let mut agent_infos = (**state.agent_configs.load()).clone();
        agent_infos.push(AgentInfo {
            id: agent_config.id.clone(),
            display_name: agent_config.display_name.clone(),
            role: agent_config.role.clone(),
            gradient_start: agent_config.gradient_start.clone(),
            gradient_end: agent_config.gradient_end.clone(),
            workspace: agent_config.workspace.to_string_lossy().to_string(),
            context_window: agent_config.context_window,
            max_turns: agent_config.max_turns,
            max_concurrent_branches: agent_config.max_concurrent_branches,
            max_concurrent_workers: agent_config.max_concurrent_workers,
        });
        state.agent_configs.store(std::sync::Arc::new(agent_infos));

        let mut cron_stores = (**state.cron_stores.load()).clone();
        cron_stores.insert(agent_id.clone(), cron_store);
        state.cron_stores.store(std::sync::Arc::new(cron_stores));

        let mut cron_schedulers = (**state.cron_schedulers.load()).clone();
        cron_schedulers.insert(agent_id.clone(), scheduler);
        state
            .cron_schedulers
            .store(std::sync::Arc::new(cron_schedulers));

        let mut sessions = (**state.cortex_chat_sessions.load()).clone();
        let cortex_session = std::sync::Arc::new(cortex_session);
        cortex_session.start_event_loop();
        sessions.insert(agent_id.clone(), cortex_session);
        state
            .cortex_chat_sessions
            .store(std::sync::Arc::new(sessions));
    }

    tracing::info!(agent_id = %agent_id, "agent created and initialized via API");

    Ok(CreateAgentResult {
        success: true,
        agent_id: agent_id.clone(),
        message: format!("Agent '{agent_id}' created and running"),
    })
}

/// Update an agent's display_name and role in config.toml.
#[utoipa::path(
    put,
    path = "/agents",
    request_body = UpdateAgentRequest,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Agent not found"),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "agents",
)]
pub(super) async fn update_agent(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let agent_id = request.agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": "Agent ID cannot be empty"
        })));
    }

    let existing = state.agent_configs.load();
    let index = existing
        .iter()
        .position(|a| a.id == agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let config_path = state.config_path.read().await.clone();

    // Acquire the config write mutex to prevent concurrent read-modify-write races.
    let _config_guard = state.config_write_mutex.lock().await;

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

    if let Some(agents_array) = doc
        .get_mut("agents")
        .and_then(|a| a.as_array_of_tables_mut())
    {
        for table in agents_array.iter_mut() {
            let table_id = table.get("id").and_then(|v| v.as_str());
            if table_id == Some(&agent_id) {
                if let Some(display_name) = &request.display_name {
                    if display_name.is_empty() {
                        table.remove("display_name");
                    } else {
                        table["display_name"] = toml_edit::value(display_name.as_str());
                    }
                }
                if let Some(role) = &request.role {
                    if role.is_empty() {
                        table.remove("role");
                    } else {
                        table["role"] = toml_edit::value(role.as_str());
                    }
                }
                if let Some(gradient_start) = &request.gradient_start {
                    if gradient_start.is_empty() {
                        table.remove("gradient_start");
                    } else {
                        table["gradient_start"] = toml_edit::value(gradient_start.as_str());
                    }
                }
                if let Some(gradient_end) = &request.gradient_end {
                    if gradient_end.is_empty() {
                        table.remove("gradient_end");
                    } else {
                        table["gradient_end"] = toml_edit::value(gradient_end.as_str());
                    }
                }
                break;
            }
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    drop(_config_guard);

    let mut configs = (**existing).clone();
    let info = &mut configs[index];
    if let Some(display_name) = &request.display_name {
        info.display_name = if display_name.is_empty() {
            None
        } else {
            Some(display_name.clone())
        };
    }
    if let Some(role) = &request.role {
        info.role = if role.is_empty() {
            None
        } else {
            Some(role.clone())
        };
    }
    if let Some(gradient_start) = &request.gradient_start {
        info.gradient_start = if gradient_start.is_empty() {
            None
        } else {
            Some(gradient_start.clone())
        };
    }
    if let Some(gradient_end) = &request.gradient_end {
        info.gradient_end = if gradient_end.is_empty() {
            None
        } else {
            Some(gradient_end.clone())
        };
    }
    state.set_agent_configs(configs);

    tracing::info!(agent_id = %agent_id, "agent updated via API");

    Ok(Json(serde_json::json!({
        "success": true,
        "agent_id": agent_id,
        "message": format!("Agent '{agent_id}' updated")
    })))
}

/// Delete an agent: remove from config.toml, clean up API state, signal main loop.
#[utoipa::path(
    delete,
    path = "/agents",
    params(
        ("agent_id" = String, Query, description = "Agent ID to delete"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 400, description = "Invalid request"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "agents",
)]
pub(super) async fn delete_agent(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeleteAgentQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let agent_id = query.agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": "Agent ID cannot be empty"
        })));
    }

    // Verify the agent exists
    {
        let existing = state.agent_configs.load();
        if !existing.iter().any(|a| a.id == agent_id) {
            return Ok(Json(serde_json::json!({
                "success": false,
                "message": format!("Agent '{agent_id}' not found")
            })));
        }
    }

    // Remove the [[agents]] entry from config.toml
    let config_path = state.config_path.read().await.clone();
    if config_path.exists() {
        // Acquire the config write mutex to prevent concurrent read-modify-write races.
        let _config_guard = state.config_write_mutex.lock().await;

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

        if let Some(agents_array) = doc
            .get_mut("agents")
            .and_then(|v| v.as_array_of_tables_mut())
        {
            let mut index_to_remove = None;
            for (i, table) in agents_array.iter().enumerate() {
                if let Some(id) = table.get("id").and_then(|v| v.as_str())
                    && id == agent_id
                {
                    index_to_remove = Some(i);
                    break;
                }
            }
            if let Some(idx) = index_to_remove {
                agents_array.remove(idx);
            }
        }

        tokio::fs::write(&config_path, doc.to_string())
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write config.toml");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    // Close the SQLite pool before removing state
    {
        let pools = state.agent_pools.load();
        if let Some(pool) = pools.get(&agent_id) {
            pool.close().await;
        }
    }

    // Remove from all API state maps
    {
        let mut mcp_managers = (**state.mcp_managers.load()).clone();
        if let Some(mcp_manager) = mcp_managers.remove(&agent_id) {
            mcp_manager.disconnect_all().await;
        }
        state.mcp_managers.store(std::sync::Arc::new(mcp_managers));

        let mut pools = (**state.agent_pools.load()).clone();
        pools.remove(&agent_id);
        state.agent_pools.store(std::sync::Arc::new(pools));

        let mut searches = (**state.memory_searches.load()).clone();
        searches.remove(&agent_id);
        state.memory_searches.store(std::sync::Arc::new(searches));

        let mut workspaces = (**state.agent_workspaces.load()).clone();
        workspaces.remove(&agent_id);
        state
            .agent_workspaces
            .store(std::sync::Arc::new(workspaces));

        let mut identity_dirs = (**state.agent_identity_dirs.load()).clone();
        identity_dirs.remove(&agent_id);
        state
            .agent_identity_dirs
            .store(std::sync::Arc::new(identity_dirs));

        let mut data_dirs = (**state.agent_data_dirs.load()).clone();
        data_dirs.remove(&agent_id);
        state.agent_data_dirs.store(std::sync::Arc::new(data_dirs));

        let mut configs = (**state.runtime_configs.load()).clone();
        configs.remove(&agent_id);
        state.runtime_configs.store(std::sync::Arc::new(configs));

        let mut sandboxes = (**state.sandboxes.load()).clone();
        sandboxes.remove(&agent_id);
        state.sandboxes.store(std::sync::Arc::new(sandboxes));

        let mut agent_infos = (**state.agent_configs.load()).clone();
        agent_infos.retain(|a| a.id != agent_id);
        state.agent_configs.store(std::sync::Arc::new(agent_infos));

        let mut cron_stores = (**state.cron_stores.load()).clone();
        cron_stores.remove(&agent_id);
        state.cron_stores.store(std::sync::Arc::new(cron_stores));

        let mut cron_schedulers = (**state.cron_schedulers.load()).clone();
        cron_schedulers.remove(&agent_id);
        state
            .cron_schedulers
            .store(std::sync::Arc::new(cron_schedulers));

        let mut sessions = (**state.cortex_chat_sessions.load()).clone();
        sessions.remove(&agent_id);
        state
            .cortex_chat_sessions
            .store(std::sync::Arc::new(sessions));

        let mut project_stores_map = (**state.project_stores.load()).clone();
        project_stores_map.remove(&agent_id);
        state
            .project_stores
            .store(std::sync::Arc::new(project_stores_map));
    }

    // Signal the main event loop to remove the agent
    if let Err(error) = state.agent_remove_tx.send(agent_id.clone()).await {
        tracing::error!(%error, "failed to send agent removal to main loop");
    }

    tracing::info!(agent_id = %agent_id, "agent deleted via API");

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Agent '{agent_id}' deleted")
    })))
}

/// Get overview stats for an agent: memory breakdown, channels, cron, cortex.
#[utoipa::path(
    get,
    path = "/agents/overview",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = AgentOverviewResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "agents",
)]
pub(super) async fn agent_overview(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentOverviewQuery>,
) -> Result<Json<AgentOverviewResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

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

    let channel_store = ChannelStore::new(pool.clone());
    let channels = channel_store.list_active().await.unwrap_or_default();
    let channel_count = channels.len();

    let cron_rows = sqlx::query(
        "SELECT id, prompt, cron_expr, interval_secs, delivery_target, active_start_hour, active_end_hour, enabled, run_once, timeout_secs FROM cron_jobs ORDER BY created_at ASC",
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
                cron_expr: row.try_get::<Option<String>, _>("cron_expr").ok().flatten(),
                interval_secs: row.get::<i64, _>("interval_secs") as u64,
                delivery_target: row.get("delivery_target"),
                enabled: row.get::<i64, _>("enabled") != 0,
                run_once: row.get::<i64, _>("run_once") != 0,
                active_hours: match (active_start, active_end) {
                    (Some(s), Some(e)) => Some((s as u8, e as u8)),
                    _ => None,
                },
                timeout_secs: row
                    .try_get::<Option<i64>, _>("timeout_secs")
                    .ok()
                    .flatten()
                    .map(|t| t as u64),
            }
        })
        .collect();

    let cortex_logger = CortexLogger::new(pool.clone());
    let bulletin_events = cortex_logger
        .load_events(1, 0, Some("bulletin_generated"))
        .await
        .unwrap_or_default();
    let last_bulletin_at = bulletin_events.first().map(|e| e.created_at.clone());

    let recent_cortex_events = cortex_logger
        .load_events(5, 0, None)
        .await
        .unwrap_or_default();

    let latest_bulletin = bulletin_events.first().and_then(|e| {
        e.details.as_ref().and_then(|d| {
            d.get("bulletin_text")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
    });

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

/// Get instance-wide overview for the main dashboard.
#[utoipa::path(
    get,
    path = "/agents/instance",
    responses(
        (status = 200, body = InstanceOverviewResponse),
    ),
    tag = "agents",
)]
pub(super) async fn instance_overview(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<InstanceOverviewResponse>, StatusCode> {
    let uptime = state.started_at.elapsed();
    let pools = state.agent_pools.load();
    let configs = state.agent_configs.load();

    let mut agents: Vec<AgentSummary> = Vec::new();

    for agent_config in configs.iter() {
        let agent_id = agent_config.id.clone();

        let Some(pool) = pools.get(&agent_id) else {
            agents.push(AgentSummary {
                id: agent_id,
                channel_count: 0,
                memory_total: 0,
                cron_job_count: 0,
                activity_sparkline: vec![0; 14],
                last_activity_at: None,
                last_bulletin_at: None,
                profile: None,
            });
            continue;
        };

        let channel_store = ChannelStore::new(pool.clone());
        let channels = channel_store.list_active().await.unwrap_or_default();
        let channel_count = channels.len();

        let last_activity_at = channels
            .iter()
            .map(|c| &c.last_activity_at)
            .max()
            .map(|dt| dt.to_rfc3339());

        let memory_total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM memories WHERE forgotten = 0")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        let cron_job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cron_jobs")
            .fetch_one(pool)
            .await
            .unwrap_or(0);

        let activity_window = chrono::Utc::now() - chrono::Duration::days(14);
        let activity_rows = sqlx::query(
            "SELECT date(created_at) as date, COUNT(*) as count FROM conversation_messages WHERE created_at > ? GROUP BY date ORDER BY date",
        )
        .bind(activity_window.to_rfc3339())
        .fetch_all(pool)
        .await
        .unwrap_or_default();

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

        let cortex_logger = CortexLogger::new(pool.clone());
        let bulletin_events = cortex_logger
            .load_events(1, 0, Some("bulletin_generated"))
            .await
            .unwrap_or_default();
        let last_bulletin_at = bulletin_events.first().map(|e| e.created_at.clone());

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

/// Get the cortex-generated profile for an agent.
#[utoipa::path(
    get,
    path = "/agents/profile",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = AgentProfileResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn get_agent_profile(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentOverviewQuery>,
) -> Result<Json<AgentProfileResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let profile = crate::agent::cortex::load_profile(pool, &query.agent_id).await;

    Ok(Json(AgentProfileResponse { profile }))
}

/// Get identity files (SOUL.md, IDENTITY.md, ROLE.md) for an agent.
#[utoipa::path(
    get,
    path = "/agents/identity",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = IdentityResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn get_identity(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IdentityQuery>,
) -> Result<Json<IdentityResponse>, StatusCode> {
    let identity_dirs = state.agent_identity_dirs.load();
    let identity_dir = identity_dirs
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let identity = crate::identity::Identity::load(identity_dir).await;

    Ok(Json(IdentityResponse {
        soul: identity.soul,
        identity: identity.identity,
        role: identity.role,
    }))
}

/// Update identity files for an agent. Only writes files for fields that are present.
/// The file watcher will pick up changes and hot-reload identity into RuntimeConfig.
#[utoipa::path(
    put,
    path = "/agents/identity",
    request_body = IdentityUpdateRequest,
    responses(
        (status = 200, body = IdentityResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "agents",
)]
pub(super) async fn update_identity(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<IdentityUpdateRequest>,
) -> Result<Json<IdentityResponse>, StatusCode> {
    let identity_dirs = state.agent_identity_dirs.load();
    let identity_dir = identity_dirs
        .get(&request.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(soul) = &request.soul {
        tokio::fs::write(identity_dir.join("SOUL.md"), soul)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write SOUL.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    if let Some(identity) = &request.identity {
        tokio::fs::write(identity_dir.join("IDENTITY.md"), identity)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write IDENTITY.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    if let Some(role) = &request.role {
        tokio::fs::write(identity_dir.join("ROLE.md"), role)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to write ROLE.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    let updated = crate::identity::Identity::load(identity_dir).await;

    Ok(Json(IdentityResponse {
        soul: updated.soul,
        identity: updated.identity,
        role: updated.role,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        ApiState, WarmupQuery, WarmupTriggerRequest, compute_bulletin_age_secs, get_warmup_status,
        resolve_warmup_agent_ids, trigger_warmup,
    };
    use crate::config::{Config, RuntimeConfig, WarmupState, WarmupStatus};
    use crate::identity::Identity;
    use crate::prompts::PromptEngine;
    use crate::skills::SkillSet;
    use axum::Json;
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn test_api_state() -> Arc<ApiState> {
        let (provider_setup_tx, _provider_setup_rx) = tokio::sync::mpsc::channel(1);
        let (agent_tx, _agent_rx) = tokio::sync::mpsc::channel(1);
        let (agent_remove_tx, _agent_remove_rx) = tokio::sync::mpsc::channel(1);

        let (injection_tx, _injection_rx) = tokio::sync::mpsc::channel(1);
        Arc::new(ApiState::new_with_provider_sender(
            provider_setup_tx,
            agent_tx,
            agent_remove_tx,
            injection_tx,
        ))
    }

    fn test_runtime_config(instance_dir: &std::path::Path) -> Arc<RuntimeConfig> {
        let config = Config::load_from_env(instance_dir).expect("failed to build config");
        let resolved = config
            .resolve_agents()
            .into_iter()
            .next()
            .expect("missing resolved agent config");
        let prompts = PromptEngine::new("en").expect("failed to build prompt engine");

        Arc::new(RuntimeConfig::new(
            instance_dir,
            &resolved,
            &config.defaults,
            prompts,
            Identity::default(),
            SkillSet::default(),
        ))
    }

    #[test]
    fn test_compute_bulletin_age_secs_none_stays_none() {
        assert_eq!(compute_bulletin_age_secs(None, 1_000), None);
    }

    #[test]
    fn test_compute_bulletin_age_secs_rounds_down_to_seconds() {
        let age = compute_bulletin_age_secs(Some(1_000), 4_999);
        assert_eq!(age, Some(3));
    }

    #[test]
    fn test_compute_bulletin_age_secs_future_timestamp_clamps_to_zero() {
        let age = compute_bulletin_age_secs(Some(10_000), 9_000);
        assert_eq!(age, Some(0));
    }

    #[tokio::test]
    async fn test_get_warmup_status_sorts_agent_ids_and_hydrates_age() {
        let state = test_api_state();
        let tempdir = tempfile::tempdir().expect("failed to create tempdir");
        let refresh_ms = chrono::Utc::now().timestamp_millis() - 3_500;

        let alpha_config = test_runtime_config(tempdir.path());
        alpha_config.warmup_status.store(Arc::new(WarmupStatus {
            state: WarmupState::Warm,
            embedding_ready: true,
            last_refresh_unix_ms: Some(refresh_ms),
            bulletin_age_secs: None,
            last_error: None,
        }));
        let zeta_config = test_runtime_config(tempdir.path());

        let mut runtime_configs = HashMap::new();
        runtime_configs.insert("zeta".to_string(), zeta_config);
        runtime_configs.insert("alpha".to_string(), alpha_config);
        state.runtime_configs.store(Arc::new(runtime_configs));

        let response = get_warmup_status(State(state), Query(WarmupQuery { agent_id: None }))
            .await
            .expect("warmup status request failed")
            .0;

        assert_eq!(response.statuses.len(), 2);
        assert_eq!(response.statuses[0].agent_id, "alpha");
        assert_eq!(response.statuses[1].agent_id, "zeta");
        assert_eq!(response.statuses[0].status.state, WarmupState::Warm);
        assert!(
            response.statuses[0]
                .status
                .bulletin_age_secs
                .expect("missing bulletin age")
                >= 3
        );
    }

    #[tokio::test]
    async fn test_get_warmup_status_single_agent_not_found() {
        let state = test_api_state();

        let result = get_warmup_status(
            State(state),
            Query(WarmupQuery {
                agent_id: Some("missing".to_string()),
            }),
        )
        .await;

        assert!(matches!(result, Err(StatusCode::NOT_FOUND)));
    }

    #[tokio::test]
    async fn test_trigger_warmup_requires_llm_manager() {
        let state = test_api_state();

        let result = trigger_warmup(
            State(state),
            Json(WarmupTriggerRequest {
                agent_id: None,
                force: false,
            }),
        )
        .await;

        assert!(matches!(result, Err(StatusCode::SERVICE_UNAVAILABLE)));
    }

    #[tokio::test]
    async fn test_trigger_warmup_single_agent_not_found_when_llm_manager_is_ready() {
        let state = test_api_state();
        let tempdir = tempfile::tempdir().expect("failed to create tempdir");
        let config = Config::load_from_env(tempdir.path()).expect("failed to build config");
        let llm_manager = Arc::new(
            crate::llm::LlmManager::new(config.llm.clone())
                .await
                .expect("failed to build llm manager"),
        );
        *state.llm_manager.write().await = Some(llm_manager);

        let result = trigger_warmup(
            State(state),
            Json(WarmupTriggerRequest {
                agent_id: Some("missing".to_string()),
                force: false,
            }),
        )
        .await;

        assert!(matches!(result, Err(StatusCode::NOT_FOUND)));
    }

    #[test]
    fn test_resolve_warmup_agent_ids_accepts_single_agent_when_deps_present() {
        let runtime_config_ids = HashSet::from([String::from("main")]);
        let memory_search_ids = HashSet::from([String::from("main")]);
        let mcp_manager_ids = HashSet::from([String::from("main")]);
        let sqlite_pool_ids = HashSet::from([String::from("main")]);

        let accepted = resolve_warmup_agent_ids(
            Some("main"),
            &runtime_config_ids,
            &memory_search_ids,
            &mcp_manager_ids,
            &sqlite_pool_ids,
        )
        .expect("expected warmup target acceptance");

        assert_eq!(accepted, vec![String::from("main")]);
    }
}

// -- Avatar upload / serve / delete --

#[derive(Deserialize)]
pub(super) struct AvatarQuery {
    agent_id: String,
}

/// Serve the agent's avatar image.
#[utoipa::path(
    get,
    path = "/agents/avatar",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, description = "Avatar image"),
        (status = 404, description = "Avatar or agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn get_avatar(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AvatarQuery>,
) -> Result<axum::response::Response, StatusCode> {
    let data_dir = state
        .agent_data_dirs
        .load()
        .get(&query.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    let avatar_path = find_avatar(&data_dir).await.ok_or(StatusCode::NOT_FOUND)?;
    let bytes = tokio::fs::read(&avatar_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mime = mime_guess::from_path(&avatar_path)
        .first_or_octet_stream()
        .to_string();

    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=3600".into(),
            ),
        ],
        bytes,
    )
        .into_response())
}

use axum::response::IntoResponse;

/// Upload (or replace) the agent's avatar image.
#[utoipa::path(
    post,
    path = "/agents/avatar",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = serde_json::Value, description = "Avatar uploaded successfully"),
        (status = 400, description = "Unsupported image type"),
        (status = 404, description = "Agent not found"),
        (status = 413, description = "Payload too large"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "agents",
)]
pub(super) async fn upload_avatar(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AvatarQuery>,
    request: axum::extract::Request,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let data_dir = state
        .agent_data_dirs
        .load()
        .get(&query.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let ext = match content_type.as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => {
            return Ok(Json(serde_json::json!({
                "success": false,
                "message": "Unsupported image type. Use PNG, JPEG, GIF, WebP, or SVG."
            })));
        }
    };

    let body_bytes = axum::body::to_bytes(request.into_body(), 5 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;

    // Remove any existing avatar files.
    remove_existing_avatars(&data_dir).await;

    let avatar_path = data_dir.join(format!("avatar.{ext}"));
    tokio::fs::write(&avatar_path, &body_bytes)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write avatar");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(agent_id = %query.agent_id, path = %avatar_path.display(), "avatar uploaded");

    Ok(Json(serde_json::json!({
        "success": true,
        "path": avatar_path.display().to_string()
    })))
}

/// Delete the agent's avatar image.
#[utoipa::path(
    delete,
    path = "/agents/avatar",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Agent not found"),
    ),
    tag = "agents",
)]
pub(super) async fn delete_avatar(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AvatarQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let data_dir = state
        .agent_data_dirs
        .load()
        .get(&query.agent_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;

    remove_existing_avatars(&data_dir).await;

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Avatar removed"
    })))
}

/// Find the first avatar.* file in the data dir.
async fn find_avatar(data_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for ext in &["png", "jpg", "jpeg", "gif", "webp", "svg"] {
        let path = data_dir.join(format!("avatar.{ext}"));
        if tokio::fs::metadata(&path).await.is_ok() {
            return Some(path);
        }
    }
    None
}

/// Remove all avatar.* files from the data dir.
async fn remove_existing_avatars(data_dir: &std::path::Path) {
    for ext in &["png", "jpg", "jpeg", "gif", "webp", "svg"] {
        let path = data_dir.join(format!("avatar.{ext}"));
        let _ = tokio::fs::remove_file(&path).await;
    }
}
