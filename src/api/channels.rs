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

/// List active channels across all agents.
pub(super) async fn list_channels(State(state): State<Arc<ApiState>>) -> Json<ChannelsResponse> {
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

/// Cancel a running worker or branch via the API.
pub(super) async fn cancel_process(
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

#[derive(Deserialize)]
pub(super) struct DumpQuery {
    /// Max messages per channel (default 50).
    #[serde(default = "default_dump_limit")]
    limit: i64,
    /// Filter to a specific agent_id.
    agent_id: Option<String>,
    /// Filter to a specific platform (e.g. "link", "portal", "discord").
    platform: Option<String>,
}

fn default_dump_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub(super) struct DumpChannel {
    agent_id: String,
    channel_id: String,
    platform: String,
    display_name: Option<String>,
    last_activity_at: String,
    created_at: String,
    messages: Vec<crate::conversation::history::TimelineItem>,
}

#[derive(Serialize)]
pub(super) struct DumpResponse {
    channels: Vec<DumpChannel>,
    total_channels: usize,
    total_messages: usize,
}

/// Dump all channels with their message history in a single call.
/// Useful for inspecting test runs and debugging agent communication.
///
/// Query params:
///   - `limit`: max messages per channel (default 50)
///   - `agent_id`: filter to a specific agent
///   - `platform`: filter to a specific platform (e.g. "link", "portal")
pub(super) async fn dump_channels(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<DumpQuery>,
) -> Json<DumpResponse> {
    let pools = state.agent_pools.load();
    let limit = query.limit.min(500);
    let mut channels = Vec::new();
    let mut total_messages = 0;

    for (agent_id, pool) in pools.iter() {
        if let Some(filter_agent) = &query.agent_id {
            if agent_id != filter_agent {
                continue;
            }
        }

        let store = ChannelStore::new(pool.clone());
        let logger = ProcessRunLogger::new(pool.clone());

        let active_channels = match store.list_active().await {
            Ok(channels) => channels,
            Err(error) => {
                tracing::warn!(%error, agent_id, "dump: failed to list channels");
                continue;
            }
        };

        for channel in active_channels {
            if let Some(filter_platform) = &query.platform {
                if &channel.platform != filter_platform {
                    continue;
                }
            }

            let messages = match logger.load_channel_timeline(&channel.id, limit, None).await {
                Ok(items) => items,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        channel_id = %channel.id,
                        "dump: failed to load timeline"
                    );
                    Vec::new()
                }
            };

            total_messages += messages.len();
            channels.push(DumpChannel {
                agent_id: agent_id.clone(),
                channel_id: channel.id,
                platform: channel.platform,
                display_name: channel.display_name,
                last_activity_at: channel.last_activity_at.to_rfc3339(),
                created_at: channel.created_at.to_rfc3339(),
                messages,
            });
        }
    }

    // Sort by last activity (most recent first)
    channels.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));

    let total_channels = channels.len();
    Json(DumpResponse {
        channels,
        total_channels,
        total_messages,
    })
}
