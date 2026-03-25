//! API handlers for agent links and topology.

use crate::api::state::ApiState;
use crate::links::{AgentLink, LinkDirection, LinkKind};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// List all links in the instance.
#[utoipa::path(
    get,
    path = "/links",
    responses(
        (status = 200, body = serde_json::Value),
    ),
    tag = "links",
)]
pub async fn list_links(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let links = state.agent_links.load();
    Json(serde_json::json!({ "links": &**links }))
}

/// Get links for a specific agent.
#[utoipa::path(
    get,
    path = "/links/agent/{agent_id}",
    params(
        ("agent_id" = String, Path, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = serde_json::Value),
    ),
    tag = "links",
)]
pub async fn agent_links(
    State(state): State<Arc<ApiState>>,
    Path(agent_id): Path<String>,
) -> impl IntoResponse {
    let all_links = state.agent_links.load();
    let links: Vec<_> = crate::links::links_for_agent(&all_links, &agent_id);
    Json(serde_json::json!({ "links": links }))
}

/// Topology response for graph rendering.
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct TopologyResponse {
    agents: Vec<TopologyAgent>,
    humans: Vec<TopologyHuman>,
    links: Vec<TopologyLink>,
    groups: Vec<TopologyGroup>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct TopologyHuman {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    discord_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telegram_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slack_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct TopologyAgent {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct TopologyLink {
    from: String,
    to: String,
    direction: String,
    kind: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
struct TopologyGroup {
    name: String,
    agent_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
}

/// Get the full agent topology for graph rendering.
#[utoipa::path(
    get,
    path = "/topology",
    responses(
        (status = 200, body = TopologyResponse),
    ),
    tag = "links",
)]
pub async fn topology(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let agent_configs = state.agent_configs.load();
    let agents: Vec<TopologyAgent> = agent_configs
        .iter()
        .map(|config| TopologyAgent {
            id: config.id.clone(),
            name: config
                .display_name
                .clone()
                .unwrap_or_else(|| config.id.clone()),
            display_name: config.display_name.clone(),
            role: config.role.clone(),
        })
        .collect();

    let all_links = state.agent_links.load();
    let links: Vec<TopologyLink> = all_links
        .iter()
        .map(|link| TopologyLink {
            from: link.from_agent_id.clone(),
            to: link.to_agent_id.clone(),
            direction: link.direction.as_str().to_string(),
            kind: link.kind.as_str().to_string(),
        })
        .collect();

    let all_groups = state.agent_groups.load();
    let groups: Vec<TopologyGroup> = all_groups
        .iter()
        .map(|group| TopologyGroup {
            name: group.name.clone(),
            agent_ids: group.agent_ids.clone(),
            color: group.color.clone(),
        })
        .collect();

    let all_humans = state.agent_humans.load();
    let humans: Vec<TopologyHuman> = all_humans
        .iter()
        .map(|h| TopologyHuman {
            id: h.id.clone(),
            display_name: h.display_name.clone(),
            role: h.role.clone(),
            bio: h.bio.clone(),
            description: h.description.clone(),
            discord_id: h.discord_id.clone(),
            telegram_id: h.telegram_id.clone(),
            slack_id: h.slack_id.clone(),
            email: h.email.clone(),
        })
        .collect();

    Json(TopologyResponse {
        agents,
        humans,
        links,
        groups,
    })
}

// -- Write endpoints --

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateLinkRequest {
    pub from: String,
    pub to: String,
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_direction() -> String {
    "two_way".into()
}

fn default_kind() -> String {
    "peer".into()
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateLinkRequest {
    pub direction: Option<String>,
    pub kind: Option<String>,
}

/// Create a new link between two agents. Persists to config.toml and updates in-memory state.
#[utoipa::path(
    post,
    path = "/links",
    request_body = CreateLinkRequest,
    responses(
        (status = 201, body = serde_json::Value),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Agent or human not found"),
        (status = 409, description = "Link already exists"),
    ),
    tag = "links",
)]
pub async fn create_link(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateLinkRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // Validate direction and relationship parse correctly
    let direction: LinkDirection = request
        .direction
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let kind: LinkKind = request.kind.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    // Validate node IDs exist (agents or humans)
    let agent_configs = state.agent_configs.load();
    let human_configs = state.agent_humans.load();
    let from_is_agent = agent_configs.iter().any(|a| a.id == request.from);
    let from_is_human = human_configs.iter().any(|h| h.id == request.from);
    let to_is_agent = agent_configs.iter().any(|a| a.id == request.to);
    let to_is_human = human_configs.iter().any(|h| h.id == request.to);

    if (!from_is_agent && !from_is_human) || (!to_is_agent && !to_is_human) {
        return Err(StatusCode::NOT_FOUND);
    }

    // Reject human↔human links
    if from_is_human && to_is_human {
        return Err(StatusCode::BAD_REQUEST);
    }

    if request.from == request.to {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Check for duplicate
    let existing = state.agent_links.load();
    let duplicate = existing.iter().any(|link| {
        (link.from_agent_id == request.from && link.to_agent_id == request.to)
            || (link.from_agent_id == request.to && link.to_agent_id == request.from)
    });
    if duplicate {
        return Err(StatusCode::CONFLICT);
    }

    // Write to config.toml
    let config_path = state.config_path.read().await.clone();
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

    // Get or create the [[links]] array
    if doc.get("links").is_none() {
        doc["links"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let links_array = doc["links"]
        .as_array_of_tables_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut link_table = toml_edit::Table::new();
    link_table["from"] = toml_edit::value(&request.from);
    link_table["to"] = toml_edit::value(&request.to);
    link_table["direction"] = toml_edit::value(direction.as_str());
    link_table["kind"] = toml_edit::value(kind.as_str());
    links_array.push(link_table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Update in-memory state
    let new_link = AgentLink {
        from_agent_id: request.from.clone(),
        to_agent_id: request.to.clone(),
        direction,
        kind,
    };
    let mut links = (**existing).clone();
    links.push(new_link.clone());
    state.set_agent_links(links);

    tracing::info!(
        from = %request.from,
        to = %request.to,
        direction = %direction,
        kind = %kind,
        "link created via API"
    );

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "link": new_link,
        })),
    ))
}

/// Update a link's properties. Identifies the link by from/to agent IDs in the path.
#[utoipa::path(
    put,
    path = "/links/{from}/{to}",
    params(
        ("from" = String, Path, description = "Source agent"),
        ("to" = String, Path, description = "Target agent"),
    ),
    request_body = UpdateLinkRequest,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Link not found"),
    ),
    tag = "links",
)]
pub async fn update_link(
    State(state): State<Arc<ApiState>>,
    Path((from, to)): Path<(String, String)>,
    Json(request): Json<UpdateLinkRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = state.agent_links.load();
    let link_index = existing
        .iter()
        .position(|link| link.from_agent_id == from && link.to_agent_id == to)
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut updated = existing[link_index].clone();
    if let Some(dir) = &request.direction {
        updated.direction = dir.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    if let Some(k) = &request.kind {
        updated.kind = k.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    }

    // Write to config.toml
    let config_path = state.config_path.read().await.clone();
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

    // Find and update the matching [[links]] entry
    if let Some(links_array) = doc
        .get_mut("links")
        .and_then(|l| l.as_array_of_tables_mut())
    {
        for table in links_array.iter_mut() {
            let table_from = table.get("from").and_then(|v| v.as_str());
            let table_to = table.get("to").and_then(|v| v.as_str());
            if table_from == Some(&from) && table_to == Some(&to) {
                if request.direction.is_some() {
                    table["direction"] = toml_edit::value(updated.direction.as_str());
                }
                if request.kind.is_some() {
                    table["kind"] = toml_edit::value(updated.kind.as_str());
                    table.remove("relationship"); // clean up old field
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

    // Update in-memory state
    let mut links = (**existing).clone();
    links[link_index] = updated.clone();
    state.set_agent_links(links);

    tracing::info!(from = %from, to = %to, "agent link updated via API");

    Ok(Json(serde_json::json!({ "link": updated })))
}

/// Delete a link between two agents.
#[utoipa::path(
    delete,
    path = "/links/{from}/{to}",
    params(
        ("from" = String, Path, description = "Source agent"),
        ("to" = String, Path, description = "Target agent"),
    ),
    responses(
        (status = 204),
        (status = 404, description = "Link not found"),
    ),
    tag = "links",
)]
pub async fn delete_link(
    State(state): State<Arc<ApiState>>,
    Path((from, to)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = state.agent_links.load();
    let had_link = existing
        .iter()
        .any(|link| link.from_agent_id == from && link.to_agent_id == to);
    if !had_link {
        return Err(StatusCode::NOT_FOUND);
    }

    // Write to config.toml
    let config_path = state.config_path.read().await.clone();
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

    // Remove the matching [[links]] entry
    if let Some(links_array) = doc
        .get_mut("links")
        .and_then(|l| l.as_array_of_tables_mut())
    {
        let mut remove_index = None;
        for (idx, table) in links_array.iter().enumerate() {
            let table_from = table.get("from").and_then(|v| v.as_str());
            let table_to = table.get("to").and_then(|v| v.as_str());
            if table_from == Some(&from) && table_to == Some(&to) {
                remove_index = Some(idx);
                break;
            }
        }
        if let Some(idx) = remove_index {
            links_array.remove(idx);
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Update in-memory state
    let links: Vec<_> = existing
        .iter()
        .filter(|link| !(link.from_agent_id == from && link.to_agent_id == to))
        .cloned()
        .collect();
    state.set_agent_links(links);

    tracing::info!(from = %from, to = %to, "agent link deleted via API");

    Ok(StatusCode::NO_CONTENT)
}

// -- Group CRUD --

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateGroupRequest {
    pub name: String,
    #[serde(default)]
    pub agent_ids: Vec<String>,
    pub color: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateGroupRequest {
    pub name: Option<String>,
    pub agent_ids: Option<Vec<String>>,
    pub color: Option<String>,
}

/// List all groups.
#[utoipa::path(
    get,
    path = "/links/groups",
    responses(
        (status = 200, body = serde_json::Value),
    ),
    tag = "links",
)]
pub async fn list_groups(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let groups = state.agent_groups.load();
    Json(serde_json::json!({ "groups": &**groups }))
}

/// Create a visual agent group.
#[utoipa::path(
    post,
    path = "/links/groups",
    request_body = CreateGroupRequest,
    responses(
        (status = 201, body = serde_json::Value),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "Group already exists"),
    ),
    tag = "links",
)]
pub async fn create_group(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateGroupRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    if request.name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let existing = state.agent_groups.load();
    if existing.iter().any(|g| g.name == request.name) {
        return Err(StatusCode::CONFLICT);
    }

    // Write to config.toml
    let config_path = state.config_path.read().await.clone();
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

    if doc.get("groups").is_none() {
        doc["groups"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let groups_array = doc["groups"]
        .as_array_of_tables_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut group_table = toml_edit::Table::new();
    group_table["name"] = toml_edit::value(&request.name);
    let mut ids = toml_edit::Array::new();
    for id in &request.agent_ids {
        ids.push(id.as_str());
    }
    group_table["agent_ids"] = toml_edit::value(ids);
    if let Some(color) = &request.color {
        group_table["color"] = toml_edit::value(color.as_str());
    }
    groups_array.push(group_table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let new_group = crate::config::GroupDef {
        name: request.name.clone(),
        agent_ids: request.agent_ids.clone(),
        color: request.color.clone(),
    };
    let mut groups = (**existing).clone();
    groups.push(new_group.clone());
    state.set_agent_groups(groups);

    tracing::info!(name = %request.name, "agent group created via API");

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "group": new_group })),
    ))
}

/// Update a group by name.
#[utoipa::path(
    put,
    path = "/links/groups/{group_name}",
    params(
        ("group_name" = String, Path, description = "Group name"),
    ),
    request_body = UpdateGroupRequest,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Group not found"),
        (status = 409, description = "Group name conflict"),
    ),
    tag = "links",
)]
pub async fn update_group(
    State(state): State<Arc<ApiState>>,
    Path(group_name): Path<String>,
    Json(request): Json<UpdateGroupRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = state.agent_groups.load();
    let index = existing
        .iter()
        .position(|g| g.name == group_name)
        .ok_or(StatusCode::NOT_FOUND)?;

    if request.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut updated = existing[index].clone();
    let new_name = request.name.as_deref().unwrap_or(&group_name);

    // If renaming, check for conflict
    if new_name != group_name && existing.iter().any(|g| g.name == new_name) {
        return Err(StatusCode::CONFLICT);
    }

    if let Some(name) = &request.name {
        updated.name = name.clone();
    }
    if let Some(agent_ids) = &request.agent_ids {
        updated.agent_ids = agent_ids.clone();
    }
    if let Some(color) = &request.color {
        updated.color = Some(color.clone());
    }

    // Write to config.toml
    let config_path = state.config_path.read().await.clone();
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

    if let Some(groups_array) = doc
        .get_mut("groups")
        .and_then(|g| g.as_array_of_tables_mut())
    {
        for table in groups_array.iter_mut() {
            let table_name = table.get("name").and_then(|v| v.as_str());
            if table_name == Some(&group_name) {
                if request.name.is_some() {
                    table["name"] = toml_edit::value(updated.name.as_str());
                }
                if let Some(agent_ids) = &request.agent_ids {
                    let mut arr = toml_edit::Array::new();
                    for id in agent_ids {
                        arr.push(id.as_str());
                    }
                    table["agent_ids"] = toml_edit::value(arr);
                }
                if let Some(color) = &request.color {
                    table["color"] = toml_edit::value(color.as_str());
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

    let mut groups = (**existing).clone();
    groups[index] = updated.clone();
    state.set_agent_groups(groups);

    tracing::info!(name = %group_name, "agent group updated via API");

    Ok(Json(serde_json::json!({ "group": updated })))
}

/// Delete a group by name.
#[utoipa::path(
    delete,
    path = "/links/groups/{group_name}",
    params(
        ("group_name" = String, Path, description = "Group name"),
    ),
    responses(
        (status = 204),
        (status = 404, description = "Group not found"),
    ),
    tag = "links",
)]
pub async fn delete_group(
    State(state): State<Arc<ApiState>>,
    Path(group_name): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = state.agent_groups.load();
    if !existing.iter().any(|g| g.name == group_name) {
        return Err(StatusCode::NOT_FOUND);
    }

    let config_path = state.config_path.read().await.clone();
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

    if let Some(groups_array) = doc
        .get_mut("groups")
        .and_then(|g| g.as_array_of_tables_mut())
    {
        let mut remove_index = None;
        for (idx, table) in groups_array.iter().enumerate() {
            let table_name = table.get("name").and_then(|v| v.as_str());
            if table_name == Some(&group_name) {
                remove_index = Some(idx);
                break;
            }
        }
        if let Some(idx) = remove_index {
            groups_array.remove(idx);
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let groups: Vec<_> = existing
        .iter()
        .filter(|g| g.name != group_name)
        .cloned()
        .collect();
    state.set_agent_groups(groups);

    tracing::info!(name = %group_name, "agent group deleted via API");

    Ok(StatusCode::NO_CONTENT)
}

// -- Human CRUD --

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateHumanRequest {
    pub id: String,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub bio: Option<String>,
    pub description: Option<String>,
    pub discord_id: Option<String>,
    pub telegram_id: Option<String>,
    pub slack_id: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateHumanRequest {
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub bio: Option<String>,
    pub description: Option<String>,
    pub discord_id: Option<String>,
    pub telegram_id: Option<String>,
    pub slack_id: Option<String>,
    pub email: Option<String>,
}

/// List all humans.
#[utoipa::path(
    get,
    path = "/links/humans",
    responses(
        (status = 200, body = serde_json::Value),
    ),
    tag = "links",
)]
pub async fn list_humans(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let humans = state.agent_humans.load();
    Json(serde_json::json!({ "humans": &**humans }))
}

/// Ensure the TOML document has `[[humans]]` entries matching the in-memory state.
///
/// When the auto-created "admin" human (or any human bootstrapped at startup)
/// exists only in memory and not in config.toml, API writes that create a fresh
/// `[[humans]]` array will silently drop those entries. This function
/// materializes the in-memory humans into the document when the section is missing.
fn ensure_humans_in_doc(doc: &mut toml_edit::DocumentMut, humans: &[crate::config::HumanDef]) {
    let has_humans = doc
        .get("humans")
        .and_then(|h| h.as_array_of_tables())
        .is_some_and(|arr| !arr.is_empty());

    if has_humans || humans.is_empty() {
        return;
    }

    let mut humans_array = toml_edit::ArrayOfTables::new();
    for human in humans {
        let mut table = toml_edit::Table::new();
        table["id"] = toml_edit::value(human.id.as_str());
        if let Some(ref display_name) = human.display_name {
            table["display_name"] = toml_edit::value(display_name.as_str());
        }
        if let Some(ref role) = human.role {
            table["role"] = toml_edit::value(role.as_str());
        }
        if let Some(ref bio) = human.bio {
            table["bio"] = toml_edit::value(bio.as_str());
        }
        if let Some(ref discord_id) = human.discord_id {
            table["discord_id"] = toml_edit::value(discord_id.as_str());
        }
        if let Some(ref telegram_id) = human.telegram_id {
            table["telegram_id"] = toml_edit::value(telegram_id.as_str());
        }
        if let Some(ref slack_id) = human.slack_id {
            table["slack_id"] = toml_edit::value(slack_id.as_str());
        }
        if let Some(ref email) = human.email {
            table["email"] = toml_edit::value(email.as_str());
        }
        humans_array.push(table);
    }
    doc["humans"] = toml_edit::Item::ArrayOfTables(humans_array);
}

/// Create an org-level human.
#[utoipa::path(
    post,
    path = "/links/humans",
    request_body = CreateHumanRequest,
    responses(
        (status = 201, body = serde_json::Value),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "Human ID already exists"),
    ),
    tag = "links",
)]
pub async fn create_human(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateHumanRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let id = request.id.trim().to_string();
    if id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let existing = state.agent_humans.load();
    if existing.iter().any(|h| h.id == id) {
        return Err(StatusCode::CONFLICT);
    }

    // Also reject IDs that collide with agent IDs
    let agent_configs = state.agent_configs.load();
    if agent_configs.iter().any(|a| a.id == id) {
        return Err(StatusCode::CONFLICT);
    }

    let config_path = state.config_path.read().await.clone();
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

    ensure_humans_in_doc(&mut doc, &existing);

    if doc.get("humans").is_none() {
        doc["humans"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let humans_array = doc["humans"]
        .as_array_of_tables_mut()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut table = toml_edit::Table::new();
    table["id"] = toml_edit::value(&id);
    if let Some(display_name) = &request.display_name
        && !display_name.is_empty()
    {
        table["display_name"] = toml_edit::value(display_name.as_str());
    }
    if let Some(role) = &request.role
        && !role.is_empty()
    {
        table["role"] = toml_edit::value(role.as_str());
    }
    if let Some(bio) = &request.bio
        && !bio.is_empty()
    {
        table["bio"] = toml_edit::value(bio.as_str());
    }
    if let Some(discord_id) = &request.discord_id
        && !discord_id.is_empty()
    {
        table["discord_id"] = toml_edit::value(discord_id.as_str());
    }
    if let Some(telegram_id) = &request.telegram_id
        && !telegram_id.is_empty()
    {
        table["telegram_id"] = toml_edit::value(telegram_id.as_str());
    }
    if let Some(slack_id) = &request.slack_id
        && !slack_id.is_empty()
    {
        table["slack_id"] = toml_edit::value(slack_id.as_str());
    }
    if let Some(email) = &request.email
        && !email.is_empty()
    {
        table["email"] = toml_edit::value(email.as_str());
    }
    humans_array.push(table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Write HUMAN.md to the human's directory on disk.
    let instance_dir = (**state.instance_dir.load()).clone();
    let human_dir = instance_dir.join("humans").join(&id);
    tokio::fs::create_dir_all(&human_dir)
        .await
        .map_err(|error| {
            tracing::warn!(%error, human_id = %id, "failed to create human directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let description = request.description.clone().filter(|s| !s.is_empty());
    if let Some(ref content) = description {
        let md_path = human_dir.join("HUMAN.md");
        tokio::fs::write(&md_path, content.as_bytes())
            .await
            .map_err(|error| {
                tracing::warn!(%error, human_id = %id, "failed to write HUMAN.md");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    let new_human = crate::config::HumanDef {
        id: id.clone(),
        display_name: request.display_name.clone().filter(|s| !s.is_empty()),
        role: request.role.clone().filter(|s| !s.is_empty()),
        bio: request.bio.clone().filter(|s| !s.is_empty()),
        description,
        discord_id: request.discord_id.clone().filter(|s| !s.is_empty()),
        telegram_id: request.telegram_id.clone().filter(|s| !s.is_empty()),
        slack_id: request.slack_id.clone().filter(|s| !s.is_empty()),
        email: request.email.clone().filter(|s| !s.is_empty()),
    };
    let mut humans = (**existing).clone();
    humans.push(new_human.clone());
    state.set_agent_humans(humans);

    tracing::info!(id = %id, "human created via API");

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "human": new_human })),
    ))
}

/// Update a human by ID.
#[utoipa::path(
    put,
    path = "/links/humans/{human_id}",
    params(
        ("human_id" = String, Path, description = "Human ID"),
    ),
    request_body = UpdateHumanRequest,
    responses(
        (status = 200, body = serde_json::Value),
        (status = 404, description = "Human not found"),
    ),
    tag = "links",
)]
pub async fn update_human(
    State(state): State<Arc<ApiState>>,
    Path(human_id): Path<String>,
    Json(request): Json<UpdateHumanRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = state.agent_humans.load();
    let index = existing
        .iter()
        .position(|h| h.id == human_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut updated = existing[index].clone();
    if let Some(display_name) = &request.display_name {
        updated.display_name = if display_name.is_empty() {
            None
        } else {
            Some(display_name.clone())
        };
    }
    if let Some(role) = &request.role {
        updated.role = if role.is_empty() {
            None
        } else {
            Some(role.clone())
        };
    }
    if let Some(bio) = &request.bio {
        updated.bio = if bio.is_empty() {
            None
        } else {
            Some(bio.clone())
        };
    }
    if let Some(description) = &request.description {
        updated.description = if description.is_empty() {
            None
        } else {
            Some(description.clone())
        };
    }
    if let Some(discord_id) = &request.discord_id {
        updated.discord_id = if discord_id.is_empty() {
            None
        } else {
            Some(discord_id.clone())
        };
    }
    if let Some(telegram_id) = &request.telegram_id {
        updated.telegram_id = if telegram_id.is_empty() {
            None
        } else {
            Some(telegram_id.clone())
        };
    }
    if let Some(slack_id) = &request.slack_id {
        updated.slack_id = if slack_id.is_empty() {
            None
        } else {
            Some(slack_id.clone())
        };
    }
    if let Some(email) = &request.email {
        updated.email = if email.is_empty() {
            None
        } else {
            Some(email.clone())
        };
    }

    let config_path = state.config_path.read().await.clone();
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

    ensure_humans_in_doc(&mut doc, &existing);

    if let Some(humans_array) = doc
        .get_mut("humans")
        .and_then(|h| h.as_array_of_tables_mut())
    {
        for table in humans_array.iter_mut() {
            let table_id = table.get("id").and_then(|v| v.as_str());
            if table_id == Some(&human_id) {
                if let Some(display_name) = &updated.display_name {
                    table["display_name"] = toml_edit::value(display_name.as_str());
                } else if request.display_name.is_some() {
                    table.remove("display_name");
                }
                if let Some(role) = &updated.role {
                    table["role"] = toml_edit::value(role.as_str());
                } else if request.role.is_some() {
                    table.remove("role");
                }
                if let Some(bio) = &updated.bio {
                    table["bio"] = toml_edit::value(bio.as_str());
                } else if request.bio.is_some() {
                    table.remove("bio");
                }
                if let Some(discord_id) = &updated.discord_id {
                    table["discord_id"] = toml_edit::value(discord_id.as_str());
                } else if request.discord_id.is_some() {
                    table.remove("discord_id");
                }
                if let Some(telegram_id) = &updated.telegram_id {
                    table["telegram_id"] = toml_edit::value(telegram_id.as_str());
                } else if request.telegram_id.is_some() {
                    table.remove("telegram_id");
                }
                if let Some(slack_id) = &updated.slack_id {
                    table["slack_id"] = toml_edit::value(slack_id.as_str());
                } else if request.slack_id.is_some() {
                    table.remove("slack_id");
                }
                if let Some(email) = &updated.email {
                    table["email"] = toml_edit::value(email.as_str());
                } else if request.email.is_some() {
                    table.remove("email");
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

    // Write or remove HUMAN.md on disk.
    if request.description.is_some() {
        let instance_dir = (**state.instance_dir.load()).clone();
        let human_dir = instance_dir.join("humans").join(&human_id);
        tokio::fs::create_dir_all(&human_dir)
            .await
            .map_err(|error| {
                tracing::warn!(%error, human_id = %human_id, "failed to create human directory");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        let md_path = human_dir.join("HUMAN.md");
        if let Some(ref content) = updated.description {
            tokio::fs::write(&md_path, content.as_bytes())
                .await
                .map_err(|error| {
                    tracing::warn!(%error, human_id = %human_id, "failed to write HUMAN.md");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
        } else {
            // Empty string clears the file
            let _ = tokio::fs::remove_file(&md_path).await;
        }
    }

    let mut humans = (**existing).clone();
    humans[index] = updated.clone();
    state.set_agent_humans(humans);

    tracing::info!(id = %human_id, "human updated via API");

    Ok(Json(serde_json::json!({ "human": updated })))
}

/// Delete a human by ID. Also removes any links involving this human.
#[utoipa::path(
    delete,
    path = "/links/humans/{human_id}",
    params(
        ("human_id" = String, Path, description = "Human ID"),
    ),
    responses(
        (status = 204),
        (status = 404, description = "Human not found"),
    ),
    tag = "links",
)]
pub async fn delete_human(
    State(state): State<Arc<ApiState>>,
    Path(human_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let existing = state.agent_humans.load();
    if !existing.iter().any(|h| h.id == human_id) {
        return Err(StatusCode::NOT_FOUND);
    }

    let config_path = state.config_path.read().await.clone();
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

    // Materialize in-memory humans if the section is missing from disk.
    ensure_humans_in_doc(&mut doc, &existing);

    // Remove the human entry
    if let Some(humans_array) = doc
        .get_mut("humans")
        .and_then(|h| h.as_array_of_tables_mut())
    {
        let mut remove_index = None;
        for (idx, table) in humans_array.iter().enumerate() {
            if table.get("id").and_then(|v| v.as_str()) == Some(&human_id) {
                remove_index = Some(idx);
                break;
            }
        }
        if let Some(idx) = remove_index {
            humans_array.remove(idx);
        }
    }

    // Remove any links involving this human
    if let Some(links_array) = doc
        .get_mut("links")
        .and_then(|l| l.as_array_of_tables_mut())
    {
        let mut indices_to_remove = Vec::new();
        for (idx, table) in links_array.iter().enumerate() {
            let from = table.get("from").and_then(|v| v.as_str());
            let to = table.get("to").and_then(|v| v.as_str());
            if from == Some(&human_id) || to == Some(&human_id) {
                indices_to_remove.push(idx);
            }
        }
        // Remove in reverse order to preserve indices
        for idx in indices_to_remove.into_iter().rev() {
            links_array.remove(idx);
        }
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to write config.toml");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Update in-memory state
    let humans: Vec<_> = existing
        .iter()
        .filter(|h| h.id != human_id)
        .cloned()
        .collect();
    state.set_agent_humans(humans);

    // Also remove links involving this human
    let existing_links = state.agent_links.load();
    let links: Vec<_> = existing_links
        .iter()
        .filter(|l| l.from_agent_id != human_id && l.to_agent_id != human_id)
        .cloned()
        .collect();
    state.set_agent_links(links);

    // Clean up the human's directory on disk (HUMAN.md etc).
    let instance_dir = (**state.instance_dir.load()).clone();
    let human_dir = instance_dir.join("humans").join(&human_id);
    if human_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&human_dir).await;
    }

    tracing::info!(id = %human_id, "human deleted via API");

    Ok(StatusCode::NO_CONTENT)
}
