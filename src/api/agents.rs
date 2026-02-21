use super::state::{AgentInfo, ApiState};

use crate::agent::cortex::CortexLogger;
use crate::conversation::channels::ChannelStore;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::collections::HashMap;
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

#[derive(Serialize)]
pub(super) struct AgentsResponse {
    agents: Vec<AgentInfo>,
}

#[derive(Serialize)]
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
    run_once: bool,
    active_hours: Option<(u8, u8)>,
    timeout_secs: Option<u64>,
}

#[derive(Serialize)]
pub(super) struct InstanceOverviewResponse {
    version: &'static str,
    uptime_seconds: u64,
    pid: u32,
    agents: Vec<AgentSummary>,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
pub(super) struct AgentProfileResponse {
    profile: Option<crate::agent::cortex::AgentProfile>,
}

#[derive(Serialize)]
pub(super) struct IdentityResponse {
    soul: Option<String>,
    identity: Option<String>,
    user: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct IdentityQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct IdentityUpdateRequest {
    agent_id: String,
    soul: Option<String>,
    identity: Option<String>,
    user: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AgentOverviewQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct CreateAgentRequest {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct DeleteAgentQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct AgentMcpQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct ReconnectMcpRequest {
    agent_id: String,
    server_name: String,
}

#[derive(Serialize)]
pub(super) struct AgentMcpResponse {
    servers: Vec<crate::mcp::McpServerStatus>,
}

/// List all configured agents with their config summaries.
pub(super) async fn list_agents(State(state): State<Arc<ApiState>>) -> Json<AgentsResponse> {
    let agents = state.agent_configs.load();
    Json(AgentsResponse {
        agents: agents.as_ref().clone(),
    })
}

/// List MCP connection status for an agent.
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

/// Create a new agent and initialize it live (directories, databases, memory, identity, cron, cortex).
pub(super) async fn create_agent(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if let Some(limit) = hosted_agent_limit() {
        let existing = state.agent_configs.load();
        if existing.len() >= limit {
            return Ok(Json(serde_json::json!({
                "success": false,
                "message": format!("agent limit reached for this instance: up to {limit} agent{}", if limit == 1 { "" } else { "s" })
            })));
        }
    }

    let agent_id = request.agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": "Agent ID cannot be empty"
        })));
    }

    {
        let existing = state.agent_configs.load();
        if existing.iter().any(|a| a.id == agent_id) {
            return Ok(Json(serde_json::json!({
                "success": false,
                "message": format!("Agent '{agent_id}' already exists")
            })));
        }
    }

    let config_path = state.config_path.read().await.clone();
    let instance_dir = (**state.instance_dir.load()).clone();

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

    if doc.get("agents").is_none() {
        doc["agents"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let agents_array = doc["agents"]
        .as_array_of_tables_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut new_table = toml_edit::Table::new();
    new_table["id"] = toml_edit::value(&agent_id);
    agents_array.push(new_table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let defaults = state.defaults_config.read().await;
    let defaults = defaults.as_ref().ok_or_else(|| {
        tracing::error!("defaults config not available");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let raw_config = crate::config::AgentConfig {
        id: agent_id.clone(),
        default: false,
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
        browser: None,
        mcp: None,
        brave_search_key: None,
        cron: Vec::new(),
    };
    let agent_config = raw_config.resolve(&instance_dir, defaults);
    drop(defaults);

    for dir in [
        &agent_config.workspace,
        &agent_config.data_dir,
        &agent_config.archives_dir,
        &agent_config.ingest_dir(),
        &agent_config.logs_dir(),
    ] {
        std::fs::create_dir_all(dir).map_err(|error| {
            tracing::error!(%error, dir = %dir.display(), "failed to create agent directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    let db = crate::db::Db::connect(&agent_config.data_dir)
        .await
        .map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to connect agent databases");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let settings_path = agent_config.data_dir.join("settings.redb");
    let settings_store = std::sync::Arc::new(
        crate::settings::SettingsStore::new(&settings_path).map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to init settings store");
            StatusCode::INTERNAL_SERVER_ERROR
        })?,
    );

    let embedding_model = {
        let guard = state.embedding_model.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("embedding model not available");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .clone()
    };

    let memory_store = crate::memory::MemoryStore::new(db.sqlite.clone());
    let embedding_table = crate::memory::EmbeddingTable::open_or_create(&db.lance)
        .await
        .map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to init embeddings");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if let Err(error) = embedding_table.ensure_fts_index().await {
        tracing::warn!(%error, agent_id = %agent_id, "failed to create FTS index");
    }

    let memory_search = std::sync::Arc::new(crate::memory::MemorySearch::new(
        memory_store,
        embedding_table,
        embedding_model,
    ));

    let (event_tx, _) = tokio::sync::broadcast::channel(256);
    let arc_agent_id: crate::AgentId = std::sync::Arc::from(agent_id.as_str());

    crate::identity::scaffold_identity_files(&agent_config.workspace)
        .await
        .map_err(|error| {
            tracing::error!(%error, agent_id = %agent_id, "failed to scaffold identity files");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let identity = crate::identity::Identity::load(&agent_config.workspace).await;

    let skills =
        crate::skills::SkillSet::load(&instance_dir.join("skills"), &agent_config.skills_dir())
            .await;

    let prompt_engine = {
        let guard = state.prompt_engine.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("prompt engine not available");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .clone()
    };

    let defaults_for_runtime = {
        let guard = state.defaults_config.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("defaults config not available");
                StatusCode::INTERNAL_SERVER_ERROR
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
    runtime_config.set_settings(settings_store.clone());

    let llm_manager = {
        let guard = state.llm_manager.read().await;
        guard
            .as_ref()
            .ok_or_else(|| {
                tracing::error!("LLM manager not available");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .clone()
    };

    let mcp_manager = std::sync::Arc::new(crate::mcp::McpManager::new(agent_config.mcp.clone()));
    mcp_manager.connect_all().await;

    let deps = crate::AgentDeps {
        agent_id: arc_agent_id.clone(),
        memory_search: memory_search.clone(),
        llm_manager,
        mcp_manager: mcp_manager.clone(),
        cron_tool: None,
        runtime_config: runtime_config.clone(),
        event_tx: event_tx.clone(),
        sqlite_pool: db.sqlite.clone(),
        messaging_manager: {
            let guard = state.messaging_manager.read().await;
            guard.as_ref().cloned()
        },
    };

    let event_rx = event_tx.subscribe();
    state.register_agent_events(agent_id.clone(), event_rx);

    let cron_store = std::sync::Arc::new(crate::cron::CronStore::new(db.sqlite.clone()));
    let cron_context = crate::cron::CronContext {
        deps: deps.clone(),
        screenshot_dir: agent_config.screenshot_dir(),
        logs_dir: agent_config.logs_dir(),
        messaging_manager: {
            let guard = state.messaging_manager.read().await;
            guard
                .as_ref()
                .cloned()
                .unwrap_or_else(|| std::sync::Arc::new(crate::messaging::MessagingManager::new()))
        },
        store: cron_store.clone(),
    };
    let scheduler = std::sync::Arc::new(crate::cron::Scheduler::new(cron_context));
    runtime_config.set_cron(cron_store.clone(), scheduler.clone());

    let cron_tool = crate::tools::CronTool::new(cron_store.clone(), scheduler.clone());

    let browser_config = (**runtime_config.browser_config.load()).clone();
    let brave_search_key = (**runtime_config.brave_search_key.load()).clone();
    let conversation_logger =
        crate::conversation::history::ConversationLogger::new(db.sqlite.clone());
    let channel_store = crate::conversation::ChannelStore::new(db.sqlite.clone());
    let cortex_tool_server = crate::tools::create_cortex_chat_tool_server(
        memory_search.clone(),
        conversation_logger,
        channel_store,
        browser_config,
        agent_config.screenshot_dir(),
        brave_search_key,
        runtime_config.workspace_dir.clone(),
        runtime_config.instance_dir.clone(),
    );
    let cortex_store = crate::agent::cortex_chat::CortexChatStore::new(db.sqlite.clone());
    let cortex_session = crate::agent::cortex_chat::CortexChatSession::new(
        deps.clone(),
        cortex_tool_server,
        cortex_store,
    );

    let cortex_logger = crate::agent::cortex::CortexLogger::new(db.sqlite.clone());
    let _bulletin_loop =
        crate::agent::cortex::spawn_bulletin_loop(deps.clone(), cortex_logger.clone());
    let _association_loop =
        crate::agent::cortex::spawn_association_loop(deps.clone(), cortex_logger);

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

        let mut configs = (**state.runtime_configs.load()).clone();
        configs.insert(agent_id.clone(), runtime_config);
        state.runtime_configs.store(std::sync::Arc::new(configs));

        let mut mcp_managers = (**state.mcp_managers.load()).clone();
        mcp_managers.insert(agent_id.clone(), mcp_manager);
        state.mcp_managers.store(std::sync::Arc::new(mcp_managers));

        let mut agent_infos = (**state.agent_configs.load()).clone();
        agent_infos.push(AgentInfo {
            id: agent_config.id.clone(),
            workspace: agent_config.workspace.clone(),
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
        sessions.insert(agent_id.clone(), std::sync::Arc::new(cortex_session));
        state
            .cortex_chat_sessions
            .store(std::sync::Arc::new(sessions));
    }

    tracing::info!(agent_id = %agent_id, "agent created and initialized via API");

    Ok(Json(serde_json::json!({
        "success": true,
        "agent_id": agent_id,
        "message": format!("Agent '{agent_id}' created and running")
    })))
}

/// Delete an agent: remove from config.toml, clean up API state, signal main loop.
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
                if let Some(id) = table.get("id").and_then(|v| v.as_str()) {
                    if id == agent_id {
                        index_to_remove = Some(i);
                        break;
                    }
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

        let mut configs = (**state.runtime_configs.load()).clone();
        configs.remove(&agent_id);
        state.runtime_configs.store(std::sync::Arc::new(configs));

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
        "SELECT id, prompt, interval_secs, delivery_target, active_start_hour, active_end_hour, enabled, run_once, timeout_secs FROM cron_jobs ORDER BY created_at ASC",
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
pub(super) async fn get_agent_profile(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentOverviewQuery>,
) -> Result<Json<AgentProfileResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let profile = crate::agent::cortex::load_profile(pool, &query.agent_id).await;

    Ok(Json(AgentProfileResponse { profile }))
}

/// Get identity files (SOUL.md, IDENTITY.md, USER.md) for an agent.
pub(super) async fn get_identity(
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
pub(super) async fn update_identity(
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

    let updated = crate::identity::Identity::load(workspace).await;

    Ok(Json(IdentityResponse {
        soul: updated.soul,
        identity: updated.identity,
        user: updated.user,
    }))
}
