use super::state::ApiState;

use crate::conversation::channels::ChannelStore;
use crate::conversation::history::ProcessRunLogger;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChannelResponse {
    pub agent_id: String,
    pub id: String,
    pub platform: String,
    pub display_name: Option<String>,
    pub is_active: bool,
    pub last_activity_at: String,
    pub created_at: String,
    pub response_mode: Option<String>,
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChannelsResponse {
    pub channels: Vec<ChannelResponse>,
}

#[derive(Deserialize, Default, utoipa::ToSchema, utoipa::IntoParams)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct MessagesResponse {
    items: Vec<crate::conversation::history::TimelineItem>,
    has_more: bool,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct MessagesQuery {
    channel_id: String,
    #[serde(default = "default_message_limit")]
    limit: i64,
    before: Option<String>,
}

fn default_message_limit() -> i64 {
    20
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CancelProcessRequest {
    agent_id: String,
    channel_id: String,
    process_type: String,
    process_id: String,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct CancelProcessResponse {
    pub success: bool,
    pub message: String,
}

/// List channels across agents, with optional activity and agent filters.
#[utoipa::path(
    get,
    path = "/channels",
    params(
        ("include_inactive" = bool, Query, description = "Include inactive channels"),
        ("agent_id" = Option<String>, Query, description = "Filter by agent ID"),
        ("is_active" = Option<bool>, Query, description = "Filter by active state"),
    ),
    responses(
        (status = 200, body = ChannelsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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

    // Read settings from running channel states for response_mode/model display.
    let channel_states = state.channel_states.read().await;

    let all_channels = collected_channels
        .into_iter()
        .map(|(agent_id, channel)| {
            let (response_mode, model) = channel_states
                .get(&channel.id)
                .map(|cs| {
                    let settings = &cs.model_overrides;
                    let mode = match settings.response_mode {
                        crate::conversation::ResponseMode::Active => None,
                        crate::conversation::ResponseMode::Observe => Some("observe".to_string()),
                        crate::conversation::ResponseMode::MentionOnly => {
                            Some("mention_only".to_string())
                        }
                    };
                    let model = settings.resolve_model("channel").map(String::from);
                    (mode, model)
                })
                .unwrap_or((None, None));

            ChannelResponse {
                agent_id,
                id: channel.id,
                platform: channel.platform,
                display_name: channel.display_name,
                is_active: channel.is_active,
                last_activity_at: channel.last_activity_at.to_rfc3339(),
                created_at: channel.created_at.to_rfc3339(),
                response_mode,
                model,
            }
        })
        .collect();

    Json(ChannelsResponse {
        channels: all_channels,
    })
}

/// Get the unified timeline for a channel: messages, branch runs, and worker runs
/// interleaved chronologically.
#[utoipa::path(
    get,
    path = "/channels/messages",
    params(
        ("channel_id" = String, Query, description = "Channel ID"),
        ("limit" = i64, Query, description = "Maximum number of messages to return (default: 20, max: 100)"),
        ("before" = Option<String>, Query, description = "Pagination cursor for fetching older messages, as \"<rfc3339>|<item id>\". A bare timestamp is accepted for older clients but can skip same-second items."),
    ),
    responses(
        (status = 200, body = MessagesResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
pub(super) async fn channel_messages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<MessagesQuery>,
) -> Json<MessagesResponse> {
    let pools = state.agent_pools.load();
    let limit = query.limit.min(100);
    let fetch_limit = limit + 1;

    for pool in pools.values() {
        let logger = ProcessRunLogger::new(pool.clone());
        match logger
            .load_channel_timeline(
                &query.channel_id,
                fetch_limit,
                query
                    .before
                    .as_deref()
                    .map(crate::conversation::history::TimelineCursor::parse),
            )
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
#[utoipa::path(
    get,
    path = "/channels/status",
    responses(
        (status = 200, body = serde_json::Value),
    ),
    tag = "channels",
)]
pub(super) async fn channel_status(
    State(state): State<Arc<ApiState>>,
) -> Json<HashMap<String, serde_json::Value>> {
    let snapshot: Vec<_> = {
        let blocks = state.channel_status_blocks.read().await;
        blocks
            .iter()
            .map(|(channel_id, registration)| {
                (
                    channel_id.clone(),
                    registration.agent_id.clone(),
                    registration.status_block.clone(),
                )
            })
            .collect()
    };

    let mut result = HashMap::new();
    let registries = state.process_control_registries.load();
    for (channel_id, agent_id, status_block) in snapshot {
        let mut block = status_block.read().await.clone();
        let workers = live_workers_for_channel(&registries, &agent_id, &channel_id).await;
        block.replace_workers_from_registry(workers);
        if let Ok(value) = serde_json::to_value(&block) {
            result.insert(channel_id, value);
        }
    }

    Json(result)
}

async fn live_workers_for_channel(
    registries: &HashMap<String, Arc<crate::agent::process_control::ProcessControlRegistry>>,
    agent_id: &str,
    channel_id: &str,
) -> Vec<crate::agent::process_control::WorkerSnapshot> {
    let Some(registry) = registries.get(agent_id) else {
        return Vec::new();
    };
    registry
        .list_worker_snapshots()
        .await
        .into_iter()
        .filter(|worker| worker.provenance.origin_channel_id.as_deref() == Some(channel_id))
        .collect()
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct DeleteChannelQuery {
    agent_id: String,
    channel_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetChannelArchiveRequest {
    agent_id: String,
    channel_id: String,
    archived: bool,
}

/// Delete a channel and its message history.
#[utoipa::path(
    delete,
    path = "/channels",
    params(
        ("agent_id" = String, Query, description = "Agent ID that owns the channel"),
        ("channel_id" = String, Query, description = "Channel ID to delete"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Channel or agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
pub(super) async fn delete_channel(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DeleteChannelQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;
    let store = ChannelStore::new(pool.clone());

    let deletion = store.delete(&query.channel_id).await.map_err(|error| {
        tracing::error!(%error, "failed to delete channel");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match deletion {
        crate::conversation::ChannelDeletion::Deleted => {}
        crate::conversation::ChannelDeletion::NotFound => return Err(StatusCode::NOT_FOUND),
        crate::conversation::ChannelDeletion::BlockedByWorkers {
            nonterminal_workers,
        } => {
            tracing::info!(
                agent_id = %query.agent_id,
                channel_id = %query.channel_id,
                nonterminal_workers,
                "channel delete rejected while workers still reference it"
            );
            return Err(StatusCode::CONFLICT);
        }
    }

    tracing::info!(
        agent_id = %query.agent_id,
        channel_id = %query.channel_id,
        "channel deleted via API"
    );

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Archive or unarchive a channel without deleting its history.
#[utoipa::path(
    post,
    path = "/channels/archive",
    request_body = SetChannelArchiveRequest,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Channel or agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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
#[utoipa::path(
    post,
    path = "/channels/cancel-process",
    request_body = CancelProcessRequest,
    responses(
        (status = 200, body = CancelProcessResponse),
        (status = 400, description = "Invalid process type or process ID"),
        (status = 404, description = "Process or channel not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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

            let registries = state.process_control_registries.load();
            let registry = registries
                .get(&request.agent_id)
                .ok_or(StatusCode::NOT_FOUND)?;
            match registry
                .cancel_worker_runtime(
                    worker_id,
                    "cancelled via API",
                    std::time::Duration::from_secs(2),
                )
                .await
            {
                crate::agent::process_control::ControlActionResult::Cancelled
                | crate::agent::process_control::ControlActionResult::AlreadyTerminal => {
                    Ok(Json(CancelProcessResponse {
                        success: true,
                        message: format!("Worker {} cancelled", request.process_id),
                    }))
                }
                crate::agent::process_control::ControlActionResult::NotFound => {
                    let pools = state.agent_pools.load();
                    let pool = pools.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;
                    let logger = ProcessRunLogger::new(pool.clone());
                    match logger
                        .read_worker_lifecycle(worker_id)
                        .await
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                    {
                        Some(lifecycle) if lifecycle.is_terminal() => {
                            Ok(Json(CancelProcessResponse {
                                success: true,
                                message: format!(
                                    "Worker {} is already terminal",
                                    request.process_id
                                ),
                            }))
                        }
                        Some(_) => Err(StatusCode::CONFLICT),
                        None => Err(StatusCode::NOT_FOUND),
                    }
                }
                crate::agent::process_control::ControlActionResult::Conflict => {
                    Err(StatusCode::CONFLICT)
                }
            }
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

// --- Channel Settings Endpoints ---

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct ChannelSettingsQuery {
    agent_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ChannelSettingsResponse {
    conversation_id: String,
    settings: crate::conversation::ConversationSettings,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateChannelSettingsRequest {
    agent_id: String,
    settings: crate::conversation::ConversationSettings,
}

#[utoipa::path(
    get,
    path = "/channels/{channel_id}/settings",
    params(
        ("channel_id" = String, Path, description = "Channel conversation ID"),
        ChannelSettingsQuery,
    ),
    responses(
        (status = 200, body = ChannelSettingsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
pub(super) async fn get_channel_settings(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(channel_id): axum::extract::Path<String>,
    Query(query): Query<ChannelSettingsQuery>,
) -> Result<Json<ChannelSettingsResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Validate channel exists
    let channel_store = ChannelStore::new(pool.clone());
    channel_store
        .get(&channel_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %channel_id, "failed to load channel for settings fetch");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let store = crate::conversation::ChannelSettingsStore::new(pool.clone());
    let settings = store
        .get(&query.agent_id, &channel_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, %channel_id, "failed to get channel settings");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .unwrap_or_default();

    Ok(Json(ChannelSettingsResponse {
        conversation_id: channel_id,
        settings,
    }))
}

#[utoipa::path(
    put,
    path = "/channels/{channel_id}/settings",
    request_body = UpdateChannelSettingsRequest,
    params(
        ("channel_id" = String, Path, description = "Channel conversation ID"),
    ),
    responses(
        (status = 200, body = ChannelSettingsResponse),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
pub(super) async fn update_channel_settings(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(channel_id): axum::extract::Path<String>,
    Json(request): Json<UpdateChannelSettingsRequest>,
) -> Result<Json<ChannelSettingsResponse>, StatusCode> {
    let pools = state.agent_pools.load();
    let pool = pools.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Validate channel exists
    let channel_store = ChannelStore::new(pool.clone());
    channel_store
        .get(&channel_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %channel_id, "failed to load channel for settings update");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let store = crate::conversation::ChannelSettingsStore::new(pool.clone());
    store
        .upsert(&request.agent_id, &channel_id, &request.settings)
        .await
        .map_err(|error| {
            tracing::warn!(%error, %channel_id, "failed to update channel settings");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Notify the running channel to hot-reload its settings.
    {
        let channel_states = state.channel_states.read().await;
        if let Some(channel_state) = channel_states.get(&channel_id)
            && let Err(error) =
                channel_state
                    .deps
                    .event_tx
                    .send(crate::ProcessEvent::SettingsUpdated {
                        agent_id: channel_state.deps.agent_id.clone(),
                        channel_id: channel_state.channel_id.clone(),
                    })
        {
            tracing::warn!(
                %error,
                %channel_id,
                "failed to send SettingsUpdated event to channel"
            );
        }
    }

    Ok(Json(ChannelSettingsResponse {
        conversation_id: channel_id,
        settings: request.settings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn register_status_test_worker(
        registry: &crate::agent::process_control::ProcessControlRegistry,
        worker_id: crate::WorkerId,
        channel_id: &str,
        task: &str,
    ) {
        use crate::agent::process_control::{
            WorkerBackend, WorkerProvenance, WorkerRuntimeControl,
        };

        let provenance = WorkerProvenance {
            origin_channel_id: Some(Arc::from(channel_id)),
            origin_branch_id: None,
            task: task.to_string(),
            task_id: None,
            autonomy_run_id: None,
            spawning_process: crate::ProcessId::Worker(worker_id),
        };
        let reservation = registry
            .reserve_worker(worker_id, &provenance, 4)
            .await
            .unwrap();
        let control = WorkerRuntimeControl::new(
            crate::agent::worker::new_worker_transcript_snapshot(),
            None,
            None,
            None,
            None,
        )
        .0;
        registry
            .register_restored_worker(
                reservation,
                provenance,
                WorkerBackend::Builtin,
                true,
                "idle",
                0,
                control,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn live_channel_workers_use_only_the_registered_agent_registry() {
        let agent_a_registry =
            Arc::new(crate::agent::process_control::ProcessControlRegistry::new());
        let agent_b_registry =
            Arc::new(crate::agent::process_control::ProcessControlRegistry::new());
        let agent_a_worker = uuid::Uuid::new_v4();
        let agent_b_worker = uuid::Uuid::new_v4();
        register_status_test_worker(
            &agent_a_registry,
            agent_a_worker,
            "shared-channel",
            "agent-a task",
        )
        .await;
        register_status_test_worker(
            &agent_b_registry,
            agent_b_worker,
            "shared-channel",
            "agent-b task",
        )
        .await;
        let registries = HashMap::from([
            ("agent-a".to_string(), agent_a_registry),
            ("agent-b".to_string(), agent_b_registry),
        ]);

        let workers = live_workers_for_channel(&registries, "agent-a", "shared-channel").await;

        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].worker_id, agent_a_worker);
    }

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
