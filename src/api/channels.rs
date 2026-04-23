use super::state::ApiState;

use crate::conversation::channels::ChannelStore;
use crate::conversation::history::ProcessRunLogger;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ChannelResponse {
    agent_id: String,
    id: String,
    platform: String,
    display_name: Option<String>,
    is_active: bool,
    last_activity_at: String,
    created_at: String,
    response_mode: Option<String>,
    model: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ChannelsResponse {
    channels: Vec<ChannelResponse>,
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
    channel_id: String,
    process_type: String,
    process_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct CancelProcessResponse {
    success: bool,
    message: String,
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
        ("before" = Option<String>, Query, description = "Pagination cursor for fetching older messages"),
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

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct PromptInspectQuery {
    channel_id: String,
}

/// Render the full prompt that the LLM would see on the next turn for a
/// given channel. Returns the rendered system prompt and conversation
/// history — useful for debugging prompt construction, coalescing,
/// status block content, and context window usage.
#[utoipa::path(
    get,
    path = "/channels/prompt/inspect",
    params(
        ("channel_id" = String, Query, description = "Channel ID to inspect"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Channel not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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
    let knowledge_synthesis = rc.knowledge_synthesis.load();
    let skills = rc.skills.load();
    let skills_prompt = skills
        .render_channel_prompt(&prompt_engine)
        .map_err(|error| {
            tracing::warn!(%error, "failed to render skills prompt for inspect");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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
        .map_err(|error| {
            tracing::warn!(%error, "failed to render worker capabilities for inspect");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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
            Some(
                prompt_engine
                    .render_conversation_context(
                        &info.platform,
                        server_name,
                        info.display_name.as_deref(),
                        Some(&info.id),
                    )
                    .map_err(|error| {
                        tracing::warn!(%error, "failed to render conversation context for inspect");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?,
            )
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "failed to fetch channel metadata for inspect");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let sandbox_enabled = channel_state.deps.sandbox.containment_active();
    let adapter = query.channel_id.split(':').next().filter(|a| !a.is_empty());

    // ── Render working memory layers (Layers 2 + 3) ──
    let wm_config = **rc.working_memory.load();
    let wm_timezone = channel_state.deps.working_memory.timezone();
    let working_memory = crate::memory::working::render_working_memory(
        &channel_state.deps.working_memory,
        &query.channel_id,
        &wm_config,
        wm_timezone,
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to render working memory for prompt inspection");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let channel_activity_map = crate::memory::working::render_channel_activity_map(
        &channel_state.deps.sqlite_pool,
        &channel_state.deps.working_memory,
        &query.channel_id,
        &wm_config,
        wm_timezone,
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to render channel activity map for prompt inspection");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let participant_config = **rc.participant_context.load();
    let tracked_participants = {
        let participants = channel_state.active_participants.read().await;
        crate::conversation::renderable_participants(&participants, &participant_config)
    };
    let participant_context = crate::memory::working::render_participant_context(
        &channel_state.deps.working_memory,
        &tracked_participants,
        &query.channel_id,
        &participant_config,
    )
    .await
    .map_err(|error| {
        tracing::warn!(
            %error,
            channel_id = %query.channel_id,
            "failed to render participant context for prompt inspection"
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // ── Available channels ──
    let available_channels = {
        let channels = channel_state
            .channel_store
            .list_active()
            .await
            .map_err(|error| {
                tracing::warn!(%error, "failed to list channels for prompt inspection");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        let entries: Vec<crate::prompts::engine::ChannelEntry> = channels
            .into_iter()
            .filter(|channel| {
                channel.id.as_str() != query.channel_id.as_str()
                    && channel.platform != "cron"
                    && channel.platform != "webhook"
            })
            .map(|channel| crate::prompts::engine::ChannelEntry {
                name: channel.display_name.unwrap_or_else(|| channel.id.clone()),
                platform: channel.platform,
                id: channel.id,
            })
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(prompt_engine.render_available_channels(entries).map_err(|error| {
                tracing::warn!(%error, "failed to render available channels for prompt inspection");
                StatusCode::INTERNAL_SERVER_ERROR
            })?)
        }
    };

    // ── Org context ──
    let org_context = {
        let agent_id = channel_state.deps.agent_id.as_ref();
        let all_links = channel_state.deps.links.load();
        let links = crate::links::links_for_agent(&all_links, agent_id);
        if links.is_empty() {
            None
        } else {
            let all_humans = channel_state.deps.humans.load();
            let humans_by_id: std::collections::HashMap<&str, &crate::config::HumanDef> =
                all_humans.iter().map(|h| (h.id.as_str(), h)).collect();

            let mut superiors = Vec::new();
            let mut subordinates = Vec::new();
            let mut peers = Vec::new();

            for link in &links {
                let is_from = link.from_agent_id == agent_id;
                let other_id = if is_from {
                    &link.to_agent_id
                } else {
                    &link.from_agent_id
                };
                let is_human = humans_by_id.contains_key(other_id.as_str());
                let (name, role, description) =
                    if let Some(human) = humans_by_id.get(other_id.as_str()) {
                        let name = human
                            .display_name
                            .clone()
                            .unwrap_or_else(|| other_id.clone());
                        (name, human.role.clone(), human.description.clone())
                    } else {
                        let name = channel_state
                            .deps
                            .agent_names
                            .get(other_id.as_str())
                            .cloned()
                            .unwrap_or_else(|| other_id.clone());
                        (name, None, None)
                    };
                let info = crate::prompts::engine::LinkedAgent {
                    name,
                    id: other_id.clone(),
                    is_human,
                    role,
                    description,
                };
                match link.kind {
                    crate::links::LinkKind::Hierarchical => {
                        if is_from {
                            subordinates.push(info);
                        } else {
                            superiors.push(info);
                        }
                    }
                    crate::links::LinkKind::Peer => peers.push(info),
                }
            }

            if superiors.is_empty() && subordinates.is_empty() && peers.is_empty() {
                None
            } else {
                Some(
                    prompt_engine
                        .render_org_context(crate::prompts::engine::OrgContext {
                            superiors,
                            subordinates,
                            peers,
                        })
                        .map_err(|error| {
                            tracing::warn!(%error, "failed to render org context for inspect");
                            StatusCode::INTERNAL_SERVER_ERROR
                        })?,
                )
            }
        }
    };

    // ── Adapter prompt ──
    let adapter_prompt =
        adapter.and_then(|adapter| prompt_engine.render_channel_adapter_prompt(adapter));

    // ── Project context ──
    let project_context = {
        use crate::prompts::engine::{ProjectContext, ProjectRepoContext, ProjectWorktreeContext};
        let store = &channel_state.deps.project_store;
        let projects = store
            .list_projects(Some(crate::projects::ProjectStatus::Active))
            .await
            .map_err(|error| {
                tracing::warn!(
                    %error,
                    projects_count = 0usize,
                    "failed to list active projects for prompt inspection"
                );
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        if projects.is_empty() {
            None
        } else {
            let mut contexts = Vec::with_capacity(projects.len());
            for project in &projects {
                let repos = store.list_repos(&project.id).await.map_err(|error| {
                    tracing::warn!(
                        %error,
                        project_id = %project.id,
                        "failed to list project repos for prompt inspection"
                    );
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
                let worktrees =
                    store
                        .list_worktrees_with_repos(&project.id)
                        .await
                        .map_err(|error| {
                            tracing::warn!(
                                %error,
                                project_id = %project.id,
                                "failed to list project worktrees for prompt inspection"
                            );
                            StatusCode::INTERNAL_SERVER_ERROR
                        })?;
                contexts.push(ProjectContext {
                    name: project.name.clone(),
                    root_path: project.root_path.clone(),
                    description: if project.description.is_empty() {
                        None
                    } else {
                        Some(project.description.clone())
                    },
                    tags: project.tags.clone(),
                    repos: repos
                        .into_iter()
                        .map(|repo| ProjectRepoContext {
                            name: repo.name.clone(),
                            path: repo.path.clone(),
                            default_branch: repo.default_branch.clone(),
                            remote_url: if repo.remote_url.is_empty() {
                                None
                            } else {
                                Some(repo.remote_url.clone())
                            },
                        })
                        .collect(),
                    worktrees: worktrees
                        .into_iter()
                        .map(|worktree_with_repo| ProjectWorktreeContext {
                            name: worktree_with_repo.worktree.name.clone(),
                            path: worktree_with_repo.worktree.path.clone(),
                            branch: worktree_with_repo.worktree.branch.clone(),
                            repo_name: worktree_with_repo.repo_name.clone(),
                        })
                        .collect(),
                });
            }
            let projects_count = contexts.len();
            Some(
                prompt_engine
                    .render_projects_context(contexts)
                    .map_err(|error| {
                        tracing::warn!(
                            %error,
                            projects_count,
                            "failed to render projects context for prompt inspection"
                        );
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?,
            )
        }
    };

    // ── Render the full system prompt ──
    let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };
    let system_prompt = prompt_engine
        .render_channel_prompt_with_links(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            empty_to_none(knowledge_synthesis.to_string()),
            empty_to_none(skills_prompt),
            worker_capabilities,
            conversation_context,
            empty_to_none(status_text),
            None, // coalesce_hint — only set during batched message handling
            available_channels,
            sandbox_enabled,
            org_context,
            adapter_prompt,
            project_context,
            None, // backfill_transcript — only set during channel initialization
            empty_to_none(working_memory),
            empty_to_none(channel_activity_map),
            empty_to_none(participant_context),
            false, // direct_mode — resolved at runtime by the channel, not available here
        )
        .map_err(|error| {
            tracing::warn!(%error, "failed to render full channel prompt for inspect");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct PromptCaptureBody {
    channel_id: String,
    enabled: bool,
}

/// Enable or disable prompt capture for a specific channel.
#[utoipa::path(
    post,
    path = "/channels/prompt/capture",
    request_body = PromptCaptureBody,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Agent or settings not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct SnapshotListQuery {
    channel_id: String,
    #[serde(default = "default_snapshot_limit")]
    limit: usize,
}

fn default_snapshot_limit() -> usize {
    50
}

/// List prompt snapshots for a channel (newest first).
#[utoipa::path(
    get,
    path = "/channels/prompt/snapshots",
    params(
        ("channel_id" = String, Query, description = "Channel ID to list snapshots for"),
        ("limit" = usize, Query, description = "Maximum number of snapshots to return (default: 50)"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Snapshot store not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct SnapshotGetQuery {
    channel_id: String,
    timestamp_ms: i64,
}

/// Retrieve a specific prompt snapshot.
#[utoipa::path(
    get,
    path = "/channels/prompt/snapshots/get",
    params(
        ("channel_id" = String, Query, description = "Channel ID the snapshot belongs to"),
        ("timestamp_ms" = i64, Query, description = "Snapshot timestamp in milliseconds"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Snapshot or store not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "channels",
)]
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
