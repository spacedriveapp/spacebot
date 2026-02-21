use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Debug)]
pub(super) struct RoutingSection {
    channel: String,
    branch: String,
    worker: String,
    compactor: String,
    cortex: String,
    rate_limit_cooldown_secs: u64,
}

#[derive(Serialize, Debug)]
pub(super) struct TuningSection {
    max_concurrent_branches: usize,
    max_concurrent_workers: usize,
    max_turns: usize,
    branch_max_turns: usize,
    context_window: usize,
    history_backfill_count: usize,
}

#[derive(Serialize, Debug)]
pub(super) struct CompactionSection {
    background_threshold: f32,
    aggressive_threshold: f32,
    emergency_threshold: f32,
}

#[derive(Serialize, Debug)]
pub(super) struct CortexSection {
    tick_interval_secs: u64,
    worker_timeout_secs: u64,
    branch_timeout_secs: u64,
    circuit_breaker_threshold: u8,
    bulletin_interval_secs: u64,
    bulletin_max_words: usize,
    bulletin_max_turns: usize,
}

#[derive(Serialize, Debug)]
pub(super) struct CoalesceSection {
    enabled: bool,
    debounce_ms: u64,
    max_wait_ms: u64,
    min_messages: usize,
    multi_user_only: bool,
}

#[derive(Serialize, Debug)]
pub(super) struct MemoryPersistenceSection {
    enabled: bool,
    message_interval: usize,
}

#[derive(Serialize, Debug)]
pub(super) struct BrowserSection {
    enabled: bool,
    headless: bool,
    evaluate_enabled: bool,
}

#[derive(Serialize, Debug)]
pub(super) struct DiscordSection {
    enabled: bool,
    allow_bot_messages: bool,
}

#[derive(Serialize, Debug)]
pub(super) struct AgentConfigResponse {
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
pub(super) struct AgentConfigQuery {
    agent_id: String,
}

#[derive(Deserialize, Debug, Default)]
pub(super) struct AgentConfigUpdateRequest {
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
pub(super) struct RoutingUpdate {
    channel: Option<String>,
    branch: Option<String>,
    worker: Option<String>,
    compactor: Option<String>,
    cortex: Option<String>,
    rate_limit_cooldown_secs: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub(super) struct TuningUpdate {
    max_concurrent_branches: Option<usize>,
    max_concurrent_workers: Option<usize>,
    max_turns: Option<usize>,
    branch_max_turns: Option<usize>,
    context_window: Option<usize>,
    history_backfill_count: Option<usize>,
}

#[derive(Deserialize, Debug)]
pub(super) struct CompactionUpdate {
    background_threshold: Option<f32>,
    aggressive_threshold: Option<f32>,
    emergency_threshold: Option<f32>,
}

#[derive(Deserialize, Debug)]
pub(super) struct CortexUpdate {
    tick_interval_secs: Option<u64>,
    worker_timeout_secs: Option<u64>,
    branch_timeout_secs: Option<u64>,
    circuit_breaker_threshold: Option<u8>,
    bulletin_interval_secs: Option<u64>,
    bulletin_max_words: Option<usize>,
    bulletin_max_turns: Option<usize>,
}

#[derive(Deserialize, Debug)]
pub(super) struct CoalesceUpdate {
    enabled: Option<bool>,
    debounce_ms: Option<u64>,
    max_wait_ms: Option<u64>,
    min_messages: Option<usize>,
    multi_user_only: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub(super) struct MemoryPersistenceUpdate {
    enabled: Option<bool>,
    message_interval: Option<usize>,
}

#[derive(Deserialize, Debug)]
pub(super) struct BrowserUpdate {
    enabled: Option<bool>,
    headless: Option<bool>,
    evaluate_enabled: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub(super) struct DiscordUpdate {
    allow_bot_messages: Option<bool>,
}

/// Get the resolved configuration for an agent.
/// Reads live values from the agent's RuntimeConfig (hot-reloaded via ArcSwap).
pub(super) async fn get_agent_config(
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
pub(super) async fn update_agent_config(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<AgentConfigUpdateRequest>,
) -> Result<Json<AgentConfigResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if config_path.as_os_str().is_empty() {
        tracing::error!("config_path not set in ApiState");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

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

    let agent_idx = find_or_create_agent_table(&mut doc, &request.agent_id)?;

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

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!(agent_id = %request.agent_id, "config.toml updated via API");

    match crate::config::Config::load_from_path(&config_path) {
        Ok(new_config) => {
            let runtime_configs = state.runtime_configs.load();
            let mcp_managers = state.mcp_managers.load();
            if let (Some(rc), Some(mcp_manager)) = (
                runtime_configs.get(&request.agent_id).cloned(),
                mcp_managers.get(&request.agent_id).cloned(),
            ) {
                rc.reload_config(&new_config, &request.agent_id, &mcp_manager)
                    .await;
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

// -- TOML edit helpers --

/// Find the index of an agent table in the [[agents]] array, or create a new one.
pub(super) fn find_or_create_agent_table(
    doc: &mut toml_edit::DocumentMut,
    agent_id: &str,
) -> Result<usize, StatusCode> {
    if doc.get("agents").is_none() {
        doc["agents"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }

    let agents = doc
        .get_mut("agents")
        .and_then(|a| a.as_array_of_tables_mut())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    for (idx, table) in agents.iter().enumerate() {
        if let Some(id) = table.get("id").and_then(|v| v.as_str()) {
            if id == agent_id {
                return Ok(idx);
            }
        }
    }

    let mut new_agent = toml_edit::Table::new();
    new_agent["id"] = toml_edit::value(agent_id);
    agents.push(new_agent);

    Ok(agents.len() - 1)
}

fn get_agent_table_mut(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
) -> Result<&mut toml_edit::Table, StatusCode> {
    doc.get_mut("agents")
        .and_then(|a| a.as_array_of_tables_mut())
        .and_then(|arr| arr.get_mut(agent_idx))
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
}

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
