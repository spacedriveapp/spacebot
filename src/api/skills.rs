use super::state::{ApiEvent, ApiState};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct SkillInfo {
    name: String,
    description: String,
    file_path: String,
    base_dir: String,
    source: String,
}

#[derive(Serialize)]
pub(super) struct SkillsListResponse {
    skills: Vec<SkillInfo>,
}

#[derive(Deserialize)]
pub(super) struct InstallSkillRequest {
    agent_id: String,
    spec: String,
    #[serde(default)]
    instance: bool,
}

#[derive(Serialize)]
pub(super) struct InstallSkillResponse {
    installed: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct RemoveSkillRequest {
    agent_id: String,
    name: String,
}

#[derive(Serialize)]
pub(super) struct RemoveSkillResponse {
    success: bool,
    path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct RegistrySkill {
    source: String,
    #[serde(rename = "skillId")]
    skill_id: String,
    name: String,
    installs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Serialize)]
pub(super) struct RegistryBrowseResponse {
    skills: Vec<RegistrySkill>,
    has_more: bool,
}

#[derive(Serialize)]
pub(super) struct RegistrySearchResponse {
    skills: Vec<RegistrySkill>,
    query: String,
    count: usize,
}

#[derive(Deserialize)]
pub(super) struct SkillsQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct RegistryBrowseQuery {
    #[serde(default = "default_registry_view")]
    view: String,
    #[serde(default)]
    page: u32,
}

fn default_registry_view() -> String {
    "all-time".into()
}

#[derive(Deserialize)]
pub(super) struct RegistrySearchQuery {
    q: String,
    #[serde(default = "default_registry_search_limit")]
    limit: u32,
}

fn default_registry_search_limit() -> u32 {
    50
}

/// List installed skills for an agent.
pub(super) async fn list_skills(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SkillsQuery>,
) -> Result<Json<SkillsListResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs.iter().find(|a| a.id == query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let instance_dir = state.instance_dir.load();
    let instance_skills_dir = instance_dir.join("skills");
    let workspace_skills_dir = agent.workspace.join("skills");

    let skills = crate::skills::SkillSet::load(&instance_skills_dir, &workspace_skills_dir).await;

    let skill_infos: Vec<SkillInfo> = skills
        .list()
        .into_iter()
        .map(|s| SkillInfo {
            name: s.name,
            description: s.description,
            file_path: s.file_path.display().to_string(),
            base_dir: s.base_dir.display().to_string(),
            source: match s.source {
                crate::skills::SkillSource::Instance => "instance".to_string(),
                crate::skills::SkillSource::Workspace => "workspace".to_string(),
            },
        })
        .collect();

    Ok(Json(SkillsListResponse { skills: skill_infos }))
}

/// Install a skill from GitHub.
pub(super) async fn install_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Json(req): axum::extract::Json<InstallSkillRequest>,
) -> Result<Json<InstallSkillResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs.iter().find(|a| a.id == req.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let target_dir = if req.instance {
        state.instance_dir.load().as_ref().join("skills")
    } else {
        agent.workspace.join("skills")
    };

    let installed = crate::skills::install_from_github(&req.spec, &target_dir)
        .await
        .map_err(|error| {
            tracing::warn!("failed to install skill: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    state.send_event(ApiEvent::ConfigReloaded);

    Ok(Json(InstallSkillResponse { installed }))
}

/// Remove an installed skill.
pub(super) async fn remove_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Json(req): axum::extract::Json<RemoveSkillRequest>,
) -> Result<Json<RemoveSkillResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs.iter().find(|a| a.id == req.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let instance_dir = state.instance_dir.load();
    let instance_skills_dir = instance_dir.join("skills");
    let workspace_skills_dir = agent.workspace.join("skills");

    let mut skills = crate::skills::SkillSet::load(&instance_skills_dir, &workspace_skills_dir).await;

    let removed_path = skills.remove(&req.name).await.map_err(|error| {
        tracing::warn!(%error, skill = %req.name, "failed to remove skill");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    state.send_event(ApiEvent::ConfigReloaded);

    tracing::info!(
        agent_id = %req.agent_id,
        skill = %req.name,
        "skill removed"
    );

    Ok(Json(RemoveSkillResponse {
        success: removed_path.is_some(),
        path: removed_path.map(|p| p.display().to_string()),
    }))
}

/// Proxy browse requests to skills.sh leaderboard API.
pub(super) async fn registry_browse(
    Query(query): Query<RegistryBrowseQuery>,
) -> Result<Json<RegistryBrowseResponse>, StatusCode> {
    let view = match query.view.as_str() {
        "all-time" | "trending" | "hot" => &query.view,
        _ => "all-time",
    };

    let url = format!(
        "https://skills.sh/api/skills/{}/{}",
        view, query.page
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "skills.sh registry browse request failed");
            StatusCode::BAD_GATEWAY
        })?;

    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "skills.sh returned error");
        return Err(StatusCode::BAD_GATEWAY);
    }

    #[derive(Deserialize)]
    struct UpstreamResponse {
        skills: Vec<RegistrySkill>,
        #[serde(default)]
        #[serde(rename = "hasMore")]
        has_more: bool,
    }

    let body: UpstreamResponse = response.json().await.map_err(|error| {
        tracing::warn!(%error, "failed to parse skills.sh response");
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Json(RegistryBrowseResponse {
        skills: body.skills,
        has_more: body.has_more,
    }))
}

/// Proxy search requests to skills.sh search API.
pub(super) async fn registry_search(
    Query(query): Query<RegistrySearchQuery>,
) -> Result<Json<RegistrySearchResponse>, StatusCode> {
    if query.q.len() < 2 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let client = reqwest::Client::new();
    let response = client
        .get("https://skills.sh/api/search")
        .query(&[("q", &query.q), ("limit", &query.limit.min(100).to_string())])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "skills.sh search request failed");
            StatusCode::BAD_GATEWAY
        })?;

    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "skills.sh search returned error");
        return Err(StatusCode::BAD_GATEWAY);
    }

    #[derive(Deserialize)]
    struct UpstreamSearchResponse {
        skills: Vec<RegistrySkill>,
        count: usize,
        query: String,
    }

    let body: UpstreamSearchResponse = response.json().await.map_err(|error| {
        tracing::warn!(%error, "failed to parse skills.sh search response");
        StatusCode::BAD_GATEWAY
    })?;

    Ok(Json(RegistrySearchResponse {
        skills: body.skills,
        query: body.query,
        count: body.count,
    }))
}
