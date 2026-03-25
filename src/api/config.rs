use super::state::ApiState;
use crate::config::ClosePolicy;

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
    voice: String,
    voice_language: Option<String>,
    voice_translate: bool,
    stt_provider: Option<String>,
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
    maintenance_interval_secs: u64,
    worker_timeout_secs: u64,
    branch_timeout_secs: u64,
    detached_worker_timeout_retry_limit: u8,
    supervisor_kill_budget_per_tick: usize,
    circuit_breaker_threshold: u8,
    bulletin_interval_secs: u64,
    bulletin_max_words: usize,
    bulletin_max_turns: usize,
    maintenance_decay_rate: f32,
    maintenance_prune_threshold: f32,
    maintenance_min_age_days: i64,
    maintenance_merge_similarity_threshold: f32,
}

#[derive(Serialize, Debug)]
pub(super) struct WarmupSection {
    enabled: bool,
    eager_embedding_load: bool,
    refresh_secs: u64,
    startup_delay_secs: u64,
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
    persist_session: bool,
    close_policy: String,
}

#[derive(Serialize, Debug)]
pub(super) struct ChannelSection {
    listen_only_mode: bool,
}

#[derive(Serialize, Debug)]
pub(super) struct SandboxSection {
    mode: String,
    writable_paths: Vec<String>,
    passthrough_env: Vec<String>,
}

#[derive(Serialize, Debug)]
pub(super) struct ProjectsSection {
    use_worktrees: bool,
    worktree_name_template: String,
    auto_create_worktrees: bool,
    auto_discover_repos: bool,
    auto_discover_worktrees: bool,
    disk_usage_warning_threshold: u64,
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
    warmup: WarmupSection,
    coalesce: CoalesceSection,
    memory_persistence: MemoryPersistenceSection,
    browser: BrowserSection,
    channel: ChannelSection,
    sandbox: SandboxSection,
    projects: ProjectsSection,
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
    warmup: Option<WarmupUpdate>,
    #[serde(default)]
    coalesce: Option<CoalesceUpdate>,
    #[serde(default)]
    memory_persistence: Option<MemoryPersistenceUpdate>,
    #[serde(default)]
    browser: Option<BrowserUpdate>,
    #[serde(default)]
    channel: Option<ChannelUpdate>,
    #[serde(default)]
    sandbox: Option<SandboxUpdate>,
    #[serde(default)]
    projects: Option<ProjectsUpdate>,
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
    voice: Option<String>,
    voice_language: Option<String>,
    voice_translate: Option<bool>,
    stt_provider: Option<String>,
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
    maintenance_interval_secs: Option<u64>,
    worker_timeout_secs: Option<u64>,
    branch_timeout_secs: Option<u64>,
    detached_worker_timeout_retry_limit: Option<u8>,
    supervisor_kill_budget_per_tick: Option<usize>,
    circuit_breaker_threshold: Option<u8>,
    bulletin_interval_secs: Option<u64>,
    bulletin_max_words: Option<usize>,
    bulletin_max_turns: Option<usize>,
    maintenance_decay_rate: Option<f32>,
    maintenance_prune_threshold: Option<f32>,
    maintenance_min_age_days: Option<i64>,
    maintenance_merge_similarity_threshold: Option<f32>,
}

#[derive(Deserialize, Debug)]
pub(super) struct WarmupUpdate {
    enabled: Option<bool>,
    eager_embedding_load: Option<bool>,
    refresh_secs: Option<u64>,
    startup_delay_secs: Option<u64>,
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
    persist_session: Option<bool>,
    close_policy: Option<ClosePolicy>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ChannelUpdate {
    listen_only_mode: Option<bool>,
}

#[derive(Deserialize, Debug)]
pub(super) struct SandboxUpdate {
    mode: Option<String>,
    writable_paths: Option<Vec<String>>,
    passthrough_env: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ProjectsUpdate {
    use_worktrees: Option<bool>,
    worktree_name_template: Option<String>,
    auto_create_worktrees: Option<bool>,
    auto_discover_repos: Option<bool>,
    auto_discover_worktrees: Option<bool>,
    disk_usage_warning_threshold: Option<u64>,
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
    let warmup = rc.warmup.load();
    let coalesce = rc.coalesce.load();
    let memory_persistence = rc.memory_persistence.load();
    let browser = rc.browser_config.load();
    let channel = rc.channel_config.load();
    let sandbox = rc.sandbox.load();
    let projects = rc.projects.load();

    let response = AgentConfigResponse {
        routing: RoutingSection {
            channel: routing.channel.clone(),
            branch: routing.branch.clone(),
            worker: routing.worker.clone(),
            compactor: routing.compactor.clone(),
            cortex: routing.cortex.clone(),
            voice: routing.voice.clone(),
            voice_language: routing.voice_language.clone(),
            voice_translate: routing.voice_translate,
            stt_provider: routing.stt_provider.clone(),
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
            maintenance_interval_secs: cortex.maintenance_interval_secs,
            worker_timeout_secs: cortex.worker_timeout_secs,
            branch_timeout_secs: cortex.branch_timeout_secs,
            detached_worker_timeout_retry_limit: cortex.detached_worker_timeout_retry_limit,
            supervisor_kill_budget_per_tick: cortex.supervisor_kill_budget_per_tick,
            circuit_breaker_threshold: cortex.circuit_breaker_threshold,
            bulletin_interval_secs: cortex.bulletin_interval_secs,
            bulletin_max_words: cortex.bulletin_max_words,
            bulletin_max_turns: cortex.bulletin_max_turns,
            maintenance_decay_rate: cortex.maintenance_decay_rate,
            maintenance_prune_threshold: cortex.maintenance_prune_threshold,
            maintenance_min_age_days: cortex.maintenance_min_age_days,
            maintenance_merge_similarity_threshold: cortex.maintenance_merge_similarity_threshold,
        },
        warmup: WarmupSection {
            enabled: warmup.enabled,
            eager_embedding_load: warmup.eager_embedding_load,
            refresh_secs: warmup.refresh_secs,
            startup_delay_secs: warmup.startup_delay_secs,
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
            persist_session: browser.persist_session,
            close_policy: browser.close_policy.as_str().to_string(),
        },
        channel: ChannelSection {
            listen_only_mode: channel.listen_only_mode,
        },
        sandbox: SandboxSection {
            mode: match sandbox.mode {
                crate::sandbox::SandboxMode::Enabled => "enabled".to_string(),
                crate::sandbox::SandboxMode::Disabled => "disabled".to_string(),
            },
            writable_paths: sandbox
                .writable_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            passthrough_env: sandbox.passthrough_env.clone(),
        },
        projects: ProjectsSection {
            use_worktrees: projects.use_worktrees,
            worktree_name_template: projects.worktree_name_template.clone(),
            auto_create_worktrees: projects.auto_create_worktrees,
            auto_discover_repos: projects.auto_discover_repos,
            auto_discover_worktrees: projects.auto_discover_worktrees,
            disk_usage_warning_threshold: projects.disk_usage_warning_threshold,
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

    // Acquire the config write mutex to prevent concurrent read-modify-write races
    // with factory tools and other API handlers that also edit config.toml.
    let _config_guard = state.config_write_mutex.lock().await;

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
    if let Some(warmup) = &request.warmup {
        update_warmup_table(&mut doc, agent_idx, warmup)?;
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
    if let Some(channel) = &request.channel {
        update_channel_table(&mut doc, agent_idx, channel)?;
    }
    if let Some(sandbox) = &request.sandbox {
        update_sandbox_table(&mut doc, agent_idx, sandbox)?;
    }
    if let Some(projects) = &request.projects {
        update_projects_table(&mut doc, agent_idx, projects)?;
    }
    if let Some(discord) = &request.discord {
        update_discord_table(&mut doc, discord)?;
    }

    let updated_content = doc.to_string();
    if let Err(error) = crate::config::Config::validate_toml(&updated_content) {
        tracing::warn!(%error, "rejected config API update due to invalid resulting TOML");
        return Err(StatusCode::BAD_REQUEST);
    }

    tokio::fs::write(&config_path, updated_content)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Release the config write mutex — remaining work is read-only state updates.
    drop(_config_guard);

    tracing::info!(agent_id = %request.agent_id, "config.toml updated via API");

    match crate::config::Config::load_from_path(&config_path) {
        Ok(new_config) => {
            // Keep in-memory defaults fresh so newly created agents inherit
            // the latest routing values.
            state.set_defaults_config(new_config.defaults.clone()).await;

            let runtime_configs = state.runtime_configs.load();
            let mcp_managers = state.mcp_managers.load();
            if let (Some(rc), Some(mcp_manager)) = (
                runtime_configs.get(&request.agent_id).cloned(),
                mcp_managers.get(&request.agent_id).cloned(),
            ) {
                rc.reload_config(&new_config, &request.agent_id, &mcp_manager)
                    .await;
            }
            if request.discord.is_some()
                && let Some(discord_config) = &new_config.messaging.discord
            {
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
        if let Some(id) = table.get("id").and_then(|v| v.as_str())
            && id == agent_id
        {
            return Ok(idx);
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
) -> Result<&'a mut toml_edit::Table, StatusCode> {
    if !agent.contains_key(key) {
        agent[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    agent[key]
        .as_table_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn update_routing_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    routing: &RoutingUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "routing")?;
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
    if let Some(ref v) = routing.voice {
        table["voice"] = toml_edit::value(v.as_str());
    }
    if let Some(ref v) = routing.voice_language {
        table["voice_language"] = toml_edit::value(v.as_str());
    }
    if let Some(v) = routing.voice_translate {
        table["voice_translate"] = toml_edit::value(v);
    }
    if let Some(ref v) = routing.stt_provider {
        table["stt_provider"] = toml_edit::value(v.as_str());
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
    let table = get_or_create_subtable(agent, "compaction")?;
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

fn validate_maintenance_unit_interval(name: &str, value: f32) -> Result<(), StatusCode> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        tracing::warn!(
            field = name,
            value,
            "invalid maintenance value in config update"
        );
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn to_i64_from_u64(field: &'static str, value: u64) -> Result<i64, StatusCode> {
    i64::try_from(value).map_err(|_| {
        tracing::warn!(field, value, "config value exceeds i64 range");
        StatusCode::BAD_REQUEST
    })
}

fn to_i64_from_usize(field: &'static str, value: usize) -> Result<i64, StatusCode> {
    i64::try_from(value).map_err(|_| {
        tracing::warn!(field, value, "config value exceeds i64 range");
        StatusCode::BAD_REQUEST
    })
}

fn update_cortex_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    cortex: &CortexUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "cortex")?;
    if let Some(v) = cortex.tick_interval_secs {
        table["tick_interval_secs"] = toml_edit::value(to_i64_from_u64("tick_interval_secs", v)?);
    }
    if let Some(v) = cortex.worker_timeout_secs {
        table["worker_timeout_secs"] = toml_edit::value(to_i64_from_u64("worker_timeout_secs", v)?);
    }
    if let Some(v) = cortex.branch_timeout_secs {
        table["branch_timeout_secs"] = toml_edit::value(to_i64_from_u64("branch_timeout_secs", v)?);
    }
    if let Some(v) = cortex.detached_worker_timeout_retry_limit {
        table["detached_worker_timeout_retry_limit"] = toml_edit::value(i64::from(v));
    }
    if let Some(v) = cortex.supervisor_kill_budget_per_tick {
        table["supervisor_kill_budget_per_tick"] =
            toml_edit::value(to_i64_from_usize("supervisor_kill_budget_per_tick", v)?);
    }
    if let Some(v) = cortex.circuit_breaker_threshold {
        table["circuit_breaker_threshold"] = toml_edit::value(i64::from(v));
    }
    if let Some(v) = cortex.bulletin_interval_secs {
        table["bulletin_interval_secs"] =
            toml_edit::value(to_i64_from_u64("bulletin_interval_secs", v)?);
    }
    if let Some(v) = cortex.bulletin_max_words {
        table["bulletin_max_words"] = toml_edit::value(to_i64_from_usize("bulletin_max_words", v)?);
    }
    if let Some(v) = cortex.bulletin_max_turns {
        table["bulletin_max_turns"] = toml_edit::value(to_i64_from_usize("bulletin_max_turns", v)?);
    }
    if let Some(v) = cortex.maintenance_interval_secs {
        if v == 0 {
            tracing::warn!("maintenance_interval_secs must be >= 1");
            return Err(StatusCode::BAD_REQUEST);
        }
        table["maintenance_interval_secs"] =
            toml_edit::value(to_i64_from_u64("maintenance_interval_secs", v)?);
    }
    if let Some(v) = cortex.maintenance_decay_rate {
        validate_maintenance_unit_interval("maintenance_decay_rate", v)?;
        table["maintenance_decay_rate"] = toml_edit::value(v as f64);
    }
    if let Some(v) = cortex.maintenance_prune_threshold {
        validate_maintenance_unit_interval("maintenance_prune_threshold", v)?;
        table["maintenance_prune_threshold"] = toml_edit::value(v as f64);
    }
    if let Some(v) = cortex.maintenance_min_age_days {
        if v < 0 {
            tracing::warn!(
                maintenance_min_age_days = v,
                "maintenance_min_age_days must be >= 0"
            );
            return Err(StatusCode::BAD_REQUEST);
        }
        table["maintenance_min_age_days"] = toml_edit::value(v);
    }
    if let Some(v) = cortex.maintenance_merge_similarity_threshold {
        validate_maintenance_unit_interval("maintenance_merge_similarity_threshold", v)?;
        table["maintenance_merge_similarity_threshold"] = toml_edit::value(v as f64);
    }
    Ok(())
}

fn update_coalesce_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    coalesce: &CoalesceUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "coalesce")?;
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

fn update_warmup_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    warmup: &WarmupUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "warmup")?;
    if let Some(v) = warmup.enabled {
        table["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = warmup.eager_embedding_load {
        table["eager_embedding_load"] = toml_edit::value(v);
    }
    if let Some(v) = warmup.refresh_secs {
        table["refresh_secs"] =
            toml_edit::value(i64::try_from(v).map_err(|_| StatusCode::BAD_REQUEST)?);
    }
    if let Some(v) = warmup.startup_delay_secs {
        table["startup_delay_secs"] =
            toml_edit::value(i64::try_from(v).map_err(|_| StatusCode::BAD_REQUEST)?);
    }
    Ok(())
}

fn update_memory_persistence_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    memory_persistence: &MemoryPersistenceUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "memory_persistence")?;
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
    let table = get_or_create_subtable(agent, "browser")?;
    if let Some(v) = browser.enabled {
        table["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = browser.headless {
        table["headless"] = toml_edit::value(v);
    }
    if let Some(v) = browser.evaluate_enabled {
        table["evaluate_enabled"] = toml_edit::value(v);
    }
    if let Some(v) = browser.persist_session {
        table["persist_session"] = toml_edit::value(v);
    }
    if let Some(v) = browser.close_policy {
        table["close_policy"] = toml_edit::value(v.as_str());
    }
    Ok(())
}

fn update_channel_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    channel: &ChannelUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "channel")?;
    if let Some(v) = channel.listen_only_mode {
        table["listen_only_mode"] = toml_edit::value(v);
    }
    Ok(())
}

fn update_sandbox_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    sandbox: &SandboxUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "sandbox")?;
    if let Some(ref mode) = sandbox.mode {
        table["mode"] = toml_edit::value(mode.as_str());
    }
    if let Some(ref paths) = sandbox.writable_paths {
        let mut array = toml_edit::Array::new();
        for path in paths {
            array.push(path.as_str());
        }
        table["writable_paths"] = toml_edit::value(array);
    }
    if let Some(ref env_vars) = sandbox.passthrough_env {
        let mut array = toml_edit::Array::new();
        for var_name in env_vars {
            array.push(var_name.as_str());
        }
        table["passthrough_env"] = toml_edit::value(array);
    }
    Ok(())
}

fn update_projects_table(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    projects: &ProjectsUpdate,
) -> Result<(), StatusCode> {
    let agent = get_agent_table_mut(doc, agent_idx)?;
    let table = get_or_create_subtable(agent, "projects")?;
    if let Some(use_worktrees) = projects.use_worktrees {
        table["use_worktrees"] = toml_edit::value(use_worktrees);
    }
    if let Some(ref template) = projects.worktree_name_template {
        table["worktree_name_template"] = toml_edit::value(template.as_str());
    }
    if let Some(auto_create) = projects.auto_create_worktrees {
        table["auto_create_worktrees"] = toml_edit::value(auto_create);
    }
    if let Some(auto_repos) = projects.auto_discover_repos {
        table["auto_discover_repos"] = toml_edit::value(auto_repos);
    }
    if let Some(auto_worktrees) = projects.auto_discover_worktrees {
        table["auto_discover_worktrees"] = toml_edit::value(auto_worktrees);
    }
    if let Some(threshold) = projects.disk_usage_warning_threshold {
        let clamped = i64::try_from(threshold).unwrap_or(i64::MAX);
        table["disk_usage_warning_threshold"] = toml_edit::value(clamped);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_warmup_table_writes_values() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");
        let update = WarmupUpdate {
            enabled: Some(false),
            eager_embedding_load: Some(false),
            refresh_secs: Some(300),
            startup_delay_secs: Some(7),
        };

        update_warmup_table(&mut doc, agent_idx, &update).expect("failed to update warmup");

        let agent = doc
            .get("agents")
            .and_then(|item| item.as_array_of_tables())
            .and_then(|agents| agents.get(agent_idx))
            .expect("missing agent table");
        let warmup = agent
            .get("warmup")
            .and_then(|item| item.as_table())
            .expect("missing warmup table");

        assert_eq!(warmup["enabled"].as_bool(), Some(false));
        assert_eq!(warmup["eager_embedding_load"].as_bool(), Some(false));
        assert_eq!(warmup["refresh_secs"].as_integer(), Some(300));
        assert_eq!(warmup["startup_delay_secs"].as_integer(), Some(7));
    }

    #[test]
    fn test_update_warmup_table_partial_update_only_sets_requested_keys() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");
        let update = WarmupUpdate {
            enabled: Some(true),
            eager_embedding_load: None,
            refresh_secs: None,
            startup_delay_secs: None,
        };

        update_warmup_table(&mut doc, agent_idx, &update).expect("failed to update warmup");

        let agent = doc
            .get("agents")
            .and_then(|item| item.as_array_of_tables())
            .and_then(|agents| agents.get(agent_idx))
            .expect("missing agent table");
        let warmup = agent
            .get("warmup")
            .and_then(|item| item.as_table())
            .expect("missing warmup table");

        assert_eq!(warmup["enabled"].as_bool(), Some(true));
        assert!(warmup.get("eager_embedding_load").is_none());
        assert!(warmup.get("refresh_secs").is_none());
        assert!(warmup.get("startup_delay_secs").is_none());
    }

    #[test]
    fn test_update_warmup_table_rejects_large_u64_values() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");
        let update = WarmupUpdate {
            enabled: None,
            eager_embedding_load: None,
            refresh_secs: Some(u64::MAX),
            startup_delay_secs: None,
        };

        let result = update_warmup_table(&mut doc, agent_idx, &update);
        assert_eq!(result, Err(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn test_update_channel_table_writes_listen_only_mode() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");

        let enable_update = ChannelUpdate {
            listen_only_mode: Some(true),
        };
        update_channel_table(&mut doc, agent_idx, &enable_update)
            .expect("failed to update channel table with true");

        let agent = doc
            .get("agents")
            .and_then(|item| item.as_array_of_tables())
            .and_then(|agents| agents.get(agent_idx))
            .expect("missing agent table");
        let channel = agent
            .get("channel")
            .and_then(|item| item.as_table())
            .expect("missing channel table");
        assert_eq!(channel["listen_only_mode"].as_bool(), Some(true));

        let disable_update = ChannelUpdate {
            listen_only_mode: Some(false),
        };
        update_channel_table(&mut doc, agent_idx, &disable_update)
            .expect("failed to update channel table with false");

        let agent = doc
            .get("agents")
            .and_then(|item| item.as_array_of_tables())
            .and_then(|agents| agents.get(agent_idx))
            .expect("missing agent table");
        let channel = agent
            .get("channel")
            .and_then(|item| item.as_table())
            .expect("missing channel table");
        assert_eq!(channel["listen_only_mode"].as_bool(), Some(false));
    }

    #[test]
    fn test_update_cortex_table_rejects_large_numeric_values() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");
        let update = CortexUpdate {
            tick_interval_secs: None,
            maintenance_interval_secs: None,
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: Some(usize::MAX),
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: None,
            maintenance_prune_threshold: None,
            maintenance_min_age_days: None,
            maintenance_merge_similarity_threshold: None,
        };

        let result = update_cortex_table(&mut doc, agent_idx, &update);
        if (usize::MAX as u128) > (i64::MAX as u128) {
            assert_eq!(result, Err(StatusCode::BAD_REQUEST));
        } else {
            assert!(result.is_ok());
        }

        let overflow_u64_update = CortexUpdate {
            tick_interval_secs: Some(u64::MAX),
            maintenance_interval_secs: None,
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: None,
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: None,
            maintenance_prune_threshold: None,
            maintenance_min_age_days: None,
            maintenance_merge_similarity_threshold: None,
        };
        assert_eq!(
            update_cortex_table(&mut doc, agent_idx, &overflow_u64_update),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn test_update_cortex_table_rejects_invalid_maintenance_values() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");

        let invalid_decay = CortexUpdate {
            tick_interval_secs: None,
            maintenance_interval_secs: None,
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: None,
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: Some(1.1),
            maintenance_prune_threshold: None,
            maintenance_min_age_days: None,
            maintenance_merge_similarity_threshold: None,
        };
        assert_eq!(
            update_cortex_table(&mut doc, agent_idx, &invalid_decay),
            Err(StatusCode::BAD_REQUEST)
        );

        let invalid_min_age = CortexUpdate {
            tick_interval_secs: None,
            maintenance_interval_secs: None,
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: None,
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: None,
            maintenance_prune_threshold: None,
            maintenance_min_age_days: Some(-1),
            maintenance_merge_similarity_threshold: None,
        };
        assert_eq!(
            update_cortex_table(&mut doc, agent_idx, &invalid_min_age),
            Err(StatusCode::BAD_REQUEST)
        );

        let invalid_interval = CortexUpdate {
            tick_interval_secs: None,
            maintenance_interval_secs: Some(0),
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: None,
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: None,
            maintenance_prune_threshold: None,
            maintenance_min_age_days: None,
            maintenance_merge_similarity_threshold: None,
        };
        assert_eq!(
            update_cortex_table(&mut doc, agent_idx, &invalid_interval),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn test_update_cortex_table_writes_values() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");
        let update = CortexUpdate {
            tick_interval_secs: Some(45),
            maintenance_interval_secs: Some(3_600),
            worker_timeout_secs: Some(321),
            branch_timeout_secs: Some(12),
            detached_worker_timeout_retry_limit: Some(3),
            supervisor_kill_budget_per_tick: Some(12),
            circuit_breaker_threshold: Some(6),
            bulletin_interval_secs: Some(120),
            bulletin_max_words: Some(4000),
            bulletin_max_turns: Some(5),
            maintenance_decay_rate: Some(0.16),
            maintenance_prune_threshold: Some(0.17),
            maintenance_min_age_days: Some(15),
            maintenance_merge_similarity_threshold: Some(0.98),
        };

        update_cortex_table(&mut doc, agent_idx, &update).expect("failed to update cortex");

        let agent = doc
            .get("agents")
            .and_then(|item| item.as_array_of_tables())
            .and_then(|agents| agents.get(agent_idx))
            .expect("missing agent table");
        let cortex = agent
            .get("cortex")
            .and_then(|item| item.as_table())
            .expect("missing cortex table");

        assert_eq!(cortex["tick_interval_secs"].as_integer(), Some(45));
        assert_eq!(
            cortex["maintenance_interval_secs"].as_integer(),
            Some(3_600)
        );
        assert_eq!(cortex["worker_timeout_secs"].as_integer(), Some(321));
        assert_eq!(cortex["branch_timeout_secs"].as_integer(), Some(12));
        assert_eq!(
            cortex["detached_worker_timeout_retry_limit"].as_integer(),
            Some(3)
        );
        assert_eq!(
            cortex["supervisor_kill_budget_per_tick"].as_integer(),
            Some(12)
        );
        assert_eq!(cortex["bulletin_interval_secs"].as_integer(), Some(120));
        assert_eq!(cortex["bulletin_max_words"].as_integer(), Some(4000));
        assert_eq!(cortex["bulletin_max_turns"].as_integer(), Some(5));
        assert!((cortex["maintenance_decay_rate"].as_float().unwrap_or(0.0) - 0.16).abs() < 1e-6);
        assert!(
            (cortex["maintenance_prune_threshold"]
                .as_float()
                .unwrap_or(0.0)
                - 0.17)
                .abs()
                < 1e-6
        );
        assert_eq!(cortex["maintenance_min_age_days"].as_integer(), Some(15));
        assert!(
            (cortex["maintenance_merge_similarity_threshold"]
                .as_float()
                .unwrap_or(0.0)
                - 0.98)
                .abs()
                < 1e-6
        );
        assert_eq!(cortex["circuit_breaker_threshold"].as_integer(), Some(6));
    }

    #[test]
    fn test_update_cortex_table_partial_update_only_sets_requested_keys() {
        let mut doc: toml_edit::DocumentMut = r#"
[[agents]]
id = "main"
"#
        .parse()
        .expect("failed to parse test TOML");

        let agent_idx =
            find_or_create_agent_table(&mut doc, "main").expect("failed to find/create agent");
        let initial = CortexUpdate {
            tick_interval_secs: Some(45),
            maintenance_interval_secs: None,
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: None,
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: None,
            maintenance_prune_threshold: None,
            maintenance_min_age_days: None,
            maintenance_merge_similarity_threshold: None,
        };

        update_cortex_table(&mut doc, agent_idx, &initial).expect("failed to apply initial update");

        let second = CortexUpdate {
            tick_interval_secs: Some(60),
            maintenance_interval_secs: Some(4_800),
            worker_timeout_secs: None,
            branch_timeout_secs: None,
            detached_worker_timeout_retry_limit: None,
            supervisor_kill_budget_per_tick: None,
            circuit_breaker_threshold: None,
            bulletin_interval_secs: None,
            bulletin_max_words: None,
            bulletin_max_turns: None,
            maintenance_decay_rate: Some(0.2),
            maintenance_prune_threshold: None,
            maintenance_min_age_days: None,
            maintenance_merge_similarity_threshold: Some(0.85),
        };

        update_cortex_table(&mut doc, agent_idx, &second).expect("failed to apply partial update");

        let agent = doc
            .get("agents")
            .and_then(|item| item.as_array_of_tables())
            .and_then(|agents| agents.get(agent_idx))
            .expect("missing agent table");
        let cortex = agent
            .get("cortex")
            .and_then(|item| item.as_table())
            .expect("missing cortex table");

        assert_eq!(cortex["tick_interval_secs"].as_integer(), Some(60));
        assert_eq!(
            cortex["maintenance_interval_secs"].as_integer(),
            Some(4_800)
        );
        assert!((cortex["maintenance_decay_rate"].as_float().unwrap_or(0.0) - 0.2).abs() < 1e-6);
        assert!(
            (cortex["maintenance_merge_similarity_threshold"]
                .as_float()
                .unwrap_or(0.0)
                - 0.85)
                .abs()
                < 1e-6
        );
        assert!(cortex.get("worker_timeout_secs").is_none());
        assert!(cortex.get("maintenance_prune_threshold").is_none());
    }
}
