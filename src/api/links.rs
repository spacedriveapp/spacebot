//! API handlers for agent link CRUD and topology.

use crate::api::state::ApiState;
use crate::links::types::{AgentLink, LinkDirection, LinkRelationship};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// List all links in the instance.
pub async fn list_links(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    match link_store.get_all().await {
        Ok(links) => Json(serde_json::json!({ "links": links })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// Get a specific link by ID.
pub async fn get_link(
    State(state): State<Arc<ApiState>>,
    Path(link_id): Path<String>,
) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    match link_store.get(&link_id).await {
        Ok(Some(link)) => Json(link).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "link not found"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// Request body for creating a link.
#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    pub from_agent_id: String,
    pub to_agent_id: String,
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default = "default_relationship")]
    pub relationship: String,
}

fn default_direction() -> String {
    "two_way".into()
}
fn default_relationship() -> String {
    "peer".into()
}

/// Create a new link between agents.
pub async fn create_link(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<CreateLinkRequest>,
) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    let direction: LinkDirection = match body.direction.parse() {
        Ok(d) => d,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };
    let relationship: LinkRelationship = match body.relationship.parse() {
        Ok(r) => r,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };

    let now = Utc::now();
    let link = AgentLink {
        id: uuid::Uuid::new_v4().to_string(),
        from_agent_id: body.from_agent_id,
        to_agent_id: body.to_agent_id,
        direction,
        relationship,
        enabled: true,
        created_at: now,
        updated_at: now,
    };

    match link_store.create(&link).await {
        Ok(()) => (StatusCode::CREATED, Json(link)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// Request body for updating a link.
#[derive(Debug, Deserialize)]
pub struct UpdateLinkRequest {
    pub direction: Option<String>,
    pub relationship: Option<String>,
    pub enabled: Option<bool>,
}

/// Update a link's properties.
pub async fn update_link(
    State(state): State<Arc<ApiState>>,
    Path(link_id): Path<String>,
    Json(body): Json<UpdateLinkRequest>,
) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    let mut link = match link_store.get(&link_id).await {
        Ok(Some(link)) => link,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "link not found"})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };

    if let Some(direction) = body.direction {
        link.direction = match direction.parse() {
            Ok(d) => d,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": error})),
                )
                    .into_response();
            }
        };
    }
    if let Some(relationship) = body.relationship {
        link.relationship = match relationship.parse() {
            Ok(r) => r,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": error})),
                )
                    .into_response();
            }
        };
    }
    if let Some(enabled) = body.enabled {
        link.enabled = enabled;
    }

    match link_store.update(&link).await {
        Ok(()) => Json(link).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// Delete a link by ID.
pub async fn delete_link(
    State(state): State<Arc<ApiState>>,
    Path(link_id): Path<String>,
) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    match link_store.delete(&link_id).await {
        Ok(()) => Json(serde_json::json!({"deleted": true})).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// Get links for a specific agent.
pub async fn agent_links(
    State(state): State<Arc<ApiState>>,
    Path(agent_id): Path<String>,
) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    match link_store.get_links_for_agent(&agent_id).await {
        Ok(links) => Json(serde_json::json!({ "links": links })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

/// Topology response for graph rendering.
#[derive(Debug, Serialize)]
struct TopologyResponse {
    agents: Vec<TopologyAgent>,
    links: Vec<TopologyLink>,
}

#[derive(Debug, Serialize)]
struct TopologyAgent {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct TopologyLink {
    id: String,
    from: String,
    to: String,
    direction: String,
    relationship: String,
    enabled: bool,
}

/// Get the full agent topology for graph rendering.
pub async fn topology(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let Some(link_store) = get_link_store(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "link store not initialized"})),
        )
            .into_response();
    };

    let agent_configs = state.agent_configs.load();
    let agents: Vec<TopologyAgent> = agent_configs
        .iter()
        .map(|config| TopologyAgent {
            id: config.id.clone(),
            name: config.id.clone(),
        })
        .collect();

    let links = match link_store.get_all().await {
        Ok(links) => links
            .into_iter()
            .map(|link| TopologyLink {
                id: link.id,
                from: link.from_agent_id,
                to: link.to_agent_id,
                direction: link.direction.as_str().to_string(),
                relationship: link.relationship.as_str().to_string(),
                enabled: link.enabled,
            })
            .collect(),
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };

    Json(TopologyResponse { agents, links }).into_response()
}

/// Extract the link store from API state.
fn get_link_store(state: &ApiState) -> Option<Arc<crate::links::LinkStore>> {
    let guard = state.link_store.load();
    guard.as_ref().clone()
}
