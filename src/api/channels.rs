use super::state::ApiState;

use crate::conversation::channels::ChannelStore;
use crate::conversation::history::ProcessRunLogger;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct ChannelResponse {
    agent_id: String,
    id: String,
    platform: String,
    display_name: Option<String>,
    is_active: bool,
    last_activity_at: String,
    created_at: String,
}

#[derive(Serialize)]
pub(super) struct ChannelsResponse {
    channels: Vec<ChannelResponse>,
}

#[derive(Deserialize, Default)]
pub(super) struct ListChannelsQuery {
    #[serde(default)]
    include_inactive: bool,
    agent_id: Option<String>,
    is_active: Option<bool>,
}

type AgentChannel = (String, crate::conversation::channels::ChannelInfo);

fn resolve_is_active_filter(query: &ListChannelsQuery) -> Option<bool> {
    query.is_active.or(if query.include_inactive {
        None
    } else {
        Some(true)
    })
}

fn sort_channels_newest_first(channels: &mut [AgentChannel]) {
    channels.sort_by(
        |(left_agent_id, left_channel), (right_agent_id, right_channel)| {
            right_channel
                .last_activity_at
                .cmp(&left_channel.last_activity_at)
                .then_with(|| right_channel.created_at.cmp(&left_channel.created_at))
                .then_with(|| left_agent_id.cmp(right_agent_id))
                .then_with(|| left_channel.id.cmp(&right_channel.id))
        },
    );
}

#[derive(Serialize)]
pub(super) struct MessagesResponse {
    items: Vec<crate::conversation::history::TimelineItem>,
    has_more: bool,
}

#[derive(Deserialize)]
pub(super) struct MessagesQuery {
    channel_id: String,
    #[serde(default = "default_message_limit")]
    limit: i64,
    before: Option<String>,
}

fn default_message_limit() -> i64 {
    20
}

#[derive(Deserialize)]
pub(super) struct CancelProcessRequest {
    channel_id: String,
    process_type: String,
    process_id: String,
}

#[derive(Serialize)]
pub(super) struct CancelProcessResponse {
    success: bool,
    message: String,
}

/// List channels across agents, with optional activity and agent filters.
pub(super) async fn list_channels(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListChannelsQuery>,
) -> Json<ChannelsResponse> {
    let pools = state.agent_pools.load();
    let mut collected_channels: Vec<AgentChannel> = Vec::new();
    let is_active_filter = resolve_is_active_filter(&query);

    for (agent_id, pool) in pools.iter() {
        if query.agent_id.as_deref().is_some_and(|id| id != agent_id) {
            continue;
        }
        let store = ChannelStore::new(pool.clone());
        match store.list(is_active_filter).await {
            Ok(channels) => {
                for channel in channels {
                    collected_channels.push((agent_id.clone(), channel));
                }
            }
            Err(error) => {
                tracing::warn!(%error, agent_id, "failed to list channels");
            }
        }
    }

    sort_channels_newest_first(&mut collected_channels);

    let all_channels = collected_channels
        .into_iter()
        .map(|(agent_id, channel)| ChannelResponse {
            agent_id,
            id: channel.id,
            platform: channel.platform,
            display_name: channel.display_name,
            is_active: channel.is_active,
            last_activity_at: channel.last_activity_at.to_rfc3339(),
            created_at: channel.created_at.to_rfc3339(),
        })
        .collect();

    Json(ChannelsResponse {
        channels: all_channels,
    })
}

/// Get the unified timeline for a channel: messages, branch runs, and worker runs
/// interleaved chronologically.
pub(super) async fn channel_messages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MessagesQuery>,
) -> Json<MessagesResponse> {
    let pools = state.agent_pools.load();
    let limit = query.limit.min(100);
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
pub(super) async fn channel_status(
    State(state): State<Arc<ApiState>>,
) -> Json<HashMap<String, serde_json::Value>> {
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
pub(super) struct DeleteChannelQuery {
    agent_id: String,
    channel_id: String,
}

#[derive(Deserialize)]
pub(super) struct SetChannelArchiveRequest {
    agent_id: String,
    channel_id: String,
    archived: bool,
}

/// Delete a channel and its message history.
pub(super) async fn delete_channel(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeleteChannelQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = ChannelStore::new(pool.clone());

    let deleted = store.delete(&query.channel_id).await.map_err(|error| {
        tracing::error!(%error, "failed to delete channel");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    tracing::info!(
        agent_id = %query.agent_id,
        channel_id = %query.channel_id,
        "channel deleted via API"
    );

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Archive or unarchive a channel without deleting its history.
pub(super) async fn set_channel_archive(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetChannelArchiveRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = ChannelStore::new(pool.clone());

    let is_active = !request.archived;
    let updated = store
        .set_active(&request.channel_id, is_active)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to update channel active state");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }

    tracing::info!(
        agent_id = %request.agent_id,
        channel_id = %request.channel_id,
        archived = request.archived,
        "channel archive state updated via API"
    );

    Ok(Json(archive_update_response_payload(request.archived)))
}

fn archive_update_response_payload(archived: bool) -> serde_json::Value {
    serde_json::json!({
        "success": true,
        "archived": archived,
        "is_active": !archived,
    })
}

/// Cancel a running worker or branch via the API.
pub(super) async fn cancel_process(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CancelProcessRequest>,
) -> Result<Json<CancelProcessResponse>, StatusCode> {
    match request.process_type.as_str() {
        "worker" => {
            let worker_id: crate::WorkerId = request
                .process_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?;

            let channel_state = {
                let states = state.channel_states.read().await;
                states.get(&request.channel_id).cloned()
            };

            if let Some(channel_state) = channel_state {
                match channel_state
                    .cancel_worker_with_reason(worker_id, "cancelled via API")
                    .await
                {
                    Ok(()) => {
                        return Ok(Json(CancelProcessResponse {
                            success: true,
                            message: format!("Worker {} cancelled", request.process_id),
                        }));
                    }
                    Err(error) => {
                        let not_found = error.to_ascii_lowercase().contains("not found");
                        if not_found {
                            tracing::debug!(
                                channel_id = %request.channel_id,
                                worker_id = %worker_id,
                                %error,
                                "worker not found in active channel state; attempting detached fallback"
                            );
                        } else {
                            tracing::warn!(
                                channel_id = %request.channel_id,
                                worker_id = %worker_id,
                                %error,
                                "failed to cancel worker in channel state"
                            );
                            return Err(StatusCode::INTERNAL_SERVER_ERROR);
                        }
                    }
                }
            }

            // Fallback for detached workers (for example after restart): no live
            // channel state exists, but the DB row is still marked running.
            let pools = state.agent_pools.load();
            for (_agent_id, pool) in pools.iter() {
                let logger = ProcessRunLogger::new(pool.clone());
                match logger.cancel_running_detached_worker(worker_id).await {
                    Ok(true) => {
                        return Ok(Json(CancelProcessResponse {
                            success: true,
                            message: format!(
                                "Worker {} cancelled (detached run reconciled)",
                                request.process_id
                            ),
                        }));
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            channel_id = %request.channel_id,
                            process_id = %request.process_id,
                            "failed to cancel detached worker run"
                        );
                        return Err(StatusCode::INTERNAL_SERVER_ERROR);
                    }
                }
            }

            Err(StatusCode::NOT_FOUND)
        }
        "branch" => {
            let channel_state = {
                let states = state.channel_states.read().await;
                states.get(&request.channel_id).cloned()
            }
            .ok_or(StatusCode::NOT_FOUND)?;

            let branch_id: crate::BranchId = request
                .process_id
                .parse()
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            channel_state
                .cancel_branch_with_reason(branch_id, "cancelled via API")
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

// ── Prompt Inspect ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct PromptInspectQuery {
    channel_id: String,
}

/// Render the full prompt that the LLM would see on the next turn for a
/// given channel. Returns the rendered system prompt and conversation
/// history — useful for debugging prompt construction, coalescing,
/// status block content, and context window usage.
pub(super) async fn inspect_prompt(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<PromptInspectQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let channel_state = {
        let states = state.channel_states.read().await;
        states.get(&query.channel_id).cloned()
    };

    let channel_state = match channel_state {
        Some(cs) => cs,
        None => {
            return Ok(Json(serde_json::json!({
                "error": "channel_not_active",
                "message": "Channel is not currently active in memory. Send a new message to activate this channel.",
            })));
        }
    };
    let rc = &channel_state.deps.runtime_config;
    let prompt_engine = rc.prompts.load();

    // ── Gather all dynamic sections ──
    let identity_context = rc.identity.load().render();
    let memory_bulletin = rc.memory_bulletin.load();
    let skills = rc.skills.load();
    let skills_prompt = skills
        .render_channel_prompt(&prompt_engine)
        .unwrap_or_default();

    let browser_enabled = rc.browser_config.load().enabled;
    let web_search_enabled = rc.brave_search_key.load().is_some();
    let opencode_enabled = rc.opencode.load().enabled;
    let mcp_tool_names = channel_state.deps.mcp_manager.get_tool_names().await;
    let worker_capabilities = prompt_engine
        .render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
            &mcp_tool_names,
        )
        .unwrap_or_default();

    let system_info = crate::agent::status::SystemInfo::from_runtime_config(
        rc.as_ref(),
        &channel_state.deps.sandbox,
    );
    let temporal_context = crate::agent::channel_prompt::TemporalContext::from_runtime(rc.as_ref());
    let current_time_line = temporal_context.current_time_line();
    let status_text = {
        let status = channel_state.status_block.read().await;
        status.render_full(&current_time_line, &system_info)
    };

    let conversation_context = match channel_state.channel_store.get(&query.channel_id).await {
        Ok(Some(info)) => {
            let server_name = info
                .platform_meta
                .as_ref()
                .and_then(|meta| {
                    meta.get("discord_guild_name")
                        .or_else(|| meta.get("slack_workspace_id"))
                })
                .and_then(|v| v.as_str());
            prompt_engine
                .render_conversation_context(
                    &info.platform,
                    server_name,
                    info.display_name.as_deref(),
                )
                .ok()
        }
        _ => None,
    };

    let sandbox_enabled = channel_state.deps.sandbox.containment_active();

    // ── Render the full system prompt ──
    // This is a best-effort reconstruction from the API layer. It lacks
    // available_channels, org_context, adapter_prompt, and project_context
    // (those require Channel methods not available from ChannelState).
    // Captured snapshots store the exact prompt the model received.
    let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };
    let system_prompt = prompt_engine
        .render_channel_prompt_with_links(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            empty_to_none(skills_prompt),
            worker_capabilities,
            conversation_context,
            empty_to_none(status_text),
            None, // coalesce_hint
            None, // available_channels — not available from API layer
            sandbox_enabled,
            None, // org_context — not available from API layer
            None, // adapter_prompt — not available from API layer
            None, // project_context — not available from API layer
            None, // backfill_transcript — not available from API layer
        )
        .unwrap_or_default();

    let total_chars = system_prompt.chars().count();

    // ── History ──
    let history = channel_state.history.read().await;
    let history_json = serde_json::to_value(&*history).map_err(|error| {
        tracing::warn!(%error, "failed to serialize channel history for inspect");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // ── Capture toggle state ──
    let capture_enabled = rc
        .settings
        .load()
        .as_ref()
        .as_ref()
        .map(|s| s.prompt_capture_enabled(&query.channel_id))
        .unwrap_or(false);

    // ── Build response ──
    let response = serde_json::json!({
        "channel_id": query.channel_id,
        "system_prompt": system_prompt,
        "total_chars": total_chars,
        "history_length": history.len(),
        "history": history_json,
        "capture_enabled": capture_enabled,
    });

    Ok(Json(response))
}

// ── Prompt Capture Toggle ──────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct PromptCaptureBody {
    channel_id: String,
    enabled: bool,
}

/// Enable or disable prompt capture for a specific channel.
pub(super) async fn set_prompt_capture(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<PromptCaptureBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Find the agent's runtime config that owns this channel.
    let runtime_config = {
        let configs = state.runtime_configs.load();
        let channel_state = state.channel_states.read().await;
        channel_state
            .get(&body.channel_id)
            .map(|cs| cs.deps.runtime_config.clone())
            .or_else(|| {
                // Fall back to first agent config if channel not active
                configs.values().next().cloned()
            })
    };

    let rc = runtime_config.ok_or(StatusCode::NOT_FOUND)?;
    let settings = rc.settings.load();
    let settings = settings.as_ref().as_ref().ok_or_else(|| {
        tracing::warn!("no settings store available for prompt capture toggle");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    settings
        .set_prompt_capture(&body.channel_id, body.enabled)
        .map_err(|error| {
            tracing::warn!(%error, "failed to set prompt capture");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(serde_json::json!({
        "channel_id": body.channel_id,
        "capture_enabled": body.enabled,
    })))
}

// ── Prompt Snapshot History ────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct SnapshotListQuery {
    channel_id: String,
    #[serde(default = "default_snapshot_limit")]
    limit: usize,
}

fn default_snapshot_limit() -> usize {
    50
}

/// List prompt snapshots for a channel (newest first).
pub(super) async fn list_prompt_snapshots(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SnapshotListQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let snapshot_store = find_snapshot_store(&state, &query.channel_id).await?;

    let summaries = snapshot_store
        .list(&query.channel_id, query.limit)
        .map_err(|error| {
            tracing::warn!(%error, "failed to list prompt snapshots");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(serde_json::json!({
        "channel_id": query.channel_id,
        "snapshots": summaries,
    })))
}

#[derive(Deserialize)]
pub(super) struct SnapshotGetQuery {
    channel_id: String,
    timestamp_ms: i64,
}

/// Retrieve a specific prompt snapshot.
pub(super) async fn get_prompt_snapshot(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SnapshotGetQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let snapshot_store = find_snapshot_store(&state, &query.channel_id).await?;

    let snapshot = snapshot_store
        .get(&query.channel_id, query.timestamp_ms)
        .map_err(|error| {
            tracing::warn!(%error, "failed to get prompt snapshot");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match snapshot {
        Some(snapshot) => Ok(Json(serde_json::to_value(&snapshot).unwrap_or_default())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Find the prompt snapshot store for a channel.
async fn find_snapshot_store(
    state: &ApiState,
    channel_id: &str,
) -> Result<Arc<crate::agent::prompt_snapshot::PromptSnapshotStore>, StatusCode> {
    // Try to find via active channel state first.
    let channel_state = {
        let states = state.channel_states.read().await;
        states.get(channel_id).cloned()
    };

    if let Some(cs) = channel_state
        && let Some(store) = cs.prompt_snapshot_store.as_ref()
    {
        return Ok(store.clone());
    }

    // Fall back to runtime configs.
    let configs = state.runtime_configs.load();
    for rc in configs.values() {
        let store = rc.prompt_snapshots.load();
        if let Some(store) = store.as_ref().as_ref() {
            return Ok(store.clone());
        }
    }

    Err(StatusCode::NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_is_active_filter_defaults_to_active_only() {
        let query = ListChannelsQuery {
            include_inactive: false,
            agent_id: None,
            is_active: None,
        };

        assert_eq!(resolve_is_active_filter(&query), Some(true));
    }

    #[test]
    fn resolve_is_active_filter_allows_explicit_include_inactive() {
        let query = ListChannelsQuery {
            include_inactive: true,
            agent_id: None,
            is_active: None,
        };

        assert_eq!(resolve_is_active_filter(&query), None);
    }

    #[test]
    fn resolve_is_active_filter_prefers_explicit_state_filter() {
        let query = ListChannelsQuery {
            include_inactive: true,
            agent_id: None,
            is_active: Some(false),
        };

        assert_eq!(resolve_is_active_filter(&query), Some(false));
    }

    #[test]
    fn archive_update_response_payload_contains_archived_and_is_active() {
        let payload = archive_update_response_payload(true);

        assert_eq!(payload["success"], serde_json::Value::Bool(true));
        assert_eq!(payload["archived"], serde_json::Value::Bool(true));
        assert_eq!(payload["is_active"], serde_json::Value::Bool(false));
    }

    #[test]
    fn sort_channels_newest_first_by_last_activity_then_created_at() {
        fn make_channel(
            id: &str,
            last_activity_at: &str,
            created_at: &str,
        ) -> crate::conversation::channels::ChannelInfo {
            let last_activity_at = chrono::DateTime::parse_from_rfc3339(last_activity_at)
                .expect("timestamp should parse")
                .with_timezone(&chrono::Utc);
            let created_at = chrono::DateTime::parse_from_rfc3339(created_at)
                .expect("timestamp should parse")
                .with_timezone(&chrono::Utc);

            crate::conversation::channels::ChannelInfo {
                id: id.to_string(),
                platform: "portal".to_string(),
                display_name: None,
                platform_meta: None,
                is_active: true,
                created_at,
                last_activity_at,
            }
        }

        let mut channels = vec![
            (
                "agent-a".to_string(),
                make_channel("a", "2026-03-02T10:00:00Z", "2026-03-02T08:00:00Z"),
            ),
            (
                "agent-b".to_string(),
                make_channel("b", "2026-03-02T12:00:00Z", "2026-03-02T07:00:00Z"),
            ),
            (
                "agent-c".to_string(),
                make_channel("c", "2026-03-02T10:00:00Z", "2026-03-02T09:00:00Z"),
            ),
        ];

        sort_channels_newest_first(&mut channels);

        let ids: Vec<_> = channels
            .into_iter()
            .map(|(agent_id, channel)| format!("{agent_id}:{}", channel.id))
            .collect();

        assert_eq!(ids, vec!["agent-b:b", "agent-c:c", "agent-a:a"]);
    }
}
