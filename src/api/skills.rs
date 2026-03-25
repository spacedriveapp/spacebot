use super::state::{ApiEvent, ApiState};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const REGISTRY_SKILL_DESCRIPTION_CACHE_CAPACITY: u64 = 10_000;
const REGISTRY_SKILL_DESCRIPTION_CACHE_TTL: Duration = Duration::from_secs(60 * 60 * 6);

static REGISTRY_SKILL_DESCRIPTION_CACHE: LazyLock<Cache<String, Option<String>>> =
    LazyLock::new(|| {
        Cache::builder()
            .max_capacity(REGISTRY_SKILL_DESCRIPTION_CACHE_CAPACITY)
            .time_to_live(REGISTRY_SKILL_DESCRIPTION_CACHE_TTL)
            .build()
    });

/// Cache for full SKILL.md content (fetched from GitHub raw content).
static REGISTRY_SKILL_CONTENT_CACHE: LazyLock<Cache<String, Option<String>>> =
    LazyLock::new(|| {
        Cache::builder()
            .max_capacity(REGISTRY_SKILL_DESCRIPTION_CACHE_CAPACITY)
            .time_to_live(REGISTRY_SKILL_DESCRIPTION_CACHE_TTL)
            .build()
    });

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct SkillInfo {
    name: String,
    description: String,
    file_path: String,
    base_dir: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_repo: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct SkillsListResponse {
    skills: Vec<SkillInfo>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct InstallSkillRequest {
    agent_id: String,
    spec: String,
    #[serde(default)]
    instance: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct InstallSkillResponse {
    installed: Vec<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct RemoveSkillRequest {
    agent_id: String,
    name: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RemoveSkillResponse {
    success: bool,
    path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, utoipa::ToSchema)]
pub(super) struct RegistrySkill {
    source: String,
    #[serde(rename = "skillId")]
    skill_id: String,
    name: String,
    installs: u64,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RegistryBrowseResponse {
    skills: Vec<RegistrySkill>,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RegistrySearchResponse {
    skills: Vec<RegistrySkill>,
    query: String,
    count: usize,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct SkillContentQuery {
    agent_id: String,
    name: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct SkillContentResponse {
    name: String,
    description: String,
    content: String,
    file_path: String,
    base_dir: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_repo: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct UploadSkillResponse {
    installed: Vec<String>,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct SkillsQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct RegistryBrowseQuery {
    #[serde(default = "default_registry_view")]
    view: String,
    #[serde(default)]
    page: u32,
}

fn default_registry_view() -> String {
    "all-time".into()
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct RegistrySearchQuery {
    q: String,
    #[serde(default = "default_registry_search_limit")]
    limit: u32,
}

fn default_registry_search_limit() -> u32 {
    50
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct RegistrySkillContentQuery {
    /// GitHub `owner/repo`.
    source: String,
    /// Skill identifier within the repo.
    skill_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RegistrySkillContentResponse {
    source: String,
    skill_id: String,
    content: Option<String>,
}

/// List installed skills for an agent.
#[utoipa::path(
    get,
    path = "/agents/skills",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = SkillsListResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "skills",
)]
pub(super) async fn list_skills(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SkillsQuery>,
) -> Result<Json<SkillsListResponse>, StatusCode> {
    let skills: crate::skills::SkillSet;
    {
        let configs = state.agent_configs.load();
        let agent = configs
            .iter()
            .find(|a| a.id == query.agent_id)
            .ok_or(StatusCode::NOT_FOUND)?;

        let instance_dir = state.instance_dir.load();
        let instance_skills_dir = instance_dir.join("skills");
        let workspace_skills_dir = std::path::PathBuf::from(&agent.workspace).join("skills");

        skills = crate::skills::SkillSet::load(&instance_skills_dir, &workspace_skills_dir).await;
    }

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
            source_repo: s.source_repo,
        })
        .collect();

    Ok(Json(SkillsListResponse {
        skills: skill_infos,
    }))
}

/// Install a skill from GitHub.
#[utoipa::path(
    post,
    path = "/agents/skills/install",
    request_body = InstallSkillRequest,
    responses(
        (status = 200, body = InstallSkillResponse),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "skills",
)]
pub(super) async fn install_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Json(req): axum::extract::Json<InstallSkillRequest>,
) -> Result<Json<InstallSkillResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs
        .iter()
        .find(|a| a.id == req.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let target_dir = if req.instance {
        state.instance_dir.load().as_ref().join("skills")
    } else {
        std::path::PathBuf::from(&agent.workspace).join("skills")
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
#[utoipa::path(
    delete,
    path = "/agents/skills/remove",
    request_body = RemoveSkillRequest,
    responses(
        (status = 200, body = RemoveSkillResponse),
        (status = 403, description = "Cannot remove instance-level skill"),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "skills",
)]
pub(super) async fn remove_skill(
    State(state): State<Arc<ApiState>>,
    axum::extract::Json(req): axum::extract::Json<RemoveSkillRequest>,
) -> Result<Json<RemoveSkillResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs
        .iter()
        .find(|a| a.id == req.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let instance_dir = state.instance_dir.load();
    let instance_skills_dir = instance_dir.join("skills");
    let workspace_skills_dir = std::path::PathBuf::from(&agent.workspace).join("skills");

    let mut skills =
        crate::skills::SkillSet::load(&instance_skills_dir, &workspace_skills_dir).await;

    let removed_path = skills.remove(&req.name).await.map_err(|error| {
        let msg = error.to_string();
        if msg.contains("instance-level skill") {
            tracing::warn!(skill = %req.name, "rejected removal of instance-level skill");
            StatusCode::FORBIDDEN
        } else {
            tracing::warn!(%error, skill = %req.name, "failed to remove skill");
            StatusCode::INTERNAL_SERVER_ERROR
        }
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

/// Get the full content of an installed skill.
#[utoipa::path(
    get,
    path = "/agents/skills/content",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
        ("name" = String, Query, description = "Skill name"),
    ),
    responses(
        (status = 200, body = SkillContentResponse),
        (status = 404, description = "Agent or skill not found"),
    ),
    tag = "skills",
)]
pub(super) async fn get_skill_content(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SkillContentQuery>,
) -> Result<Json<SkillContentResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs
        .iter()
        .find(|a| a.id == query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let instance_dir = state.instance_dir.load();
    let instance_skills_dir = instance_dir.join("skills");
    let workspace_skills_dir = std::path::PathBuf::from(&agent.workspace).join("skills");

    let skills = crate::skills::SkillSet::load(&instance_skills_dir, &workspace_skills_dir).await;

    let skill = skills.get(&query.name).ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(SkillContentResponse {
        name: skill.name.clone(),
        description: skill.description.clone(),
        content: skill.content.clone(),
        file_path: skill.file_path.display().to_string(),
        base_dir: skill.base_dir.display().to_string(),
        source: match skill.source {
            crate::skills::SkillSource::Instance => "instance".to_string(),
            crate::skills::SkillSource::Workspace => "workspace".to_string(),
        },
        source_repo: skill.source_repo.clone(),
    }))
}

/// Upload skill files (zip archives or directories) from the user's computer.
#[utoipa::path(
    post,
    path = "/agents/skills/upload",
    params(
        ("agent_id" = String, Query, description = "Agent ID"),
    ),
    responses(
        (status = 200, body = UploadSkillResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "skills",
)]
pub(super) async fn upload_skill(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<SkillsQuery>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<UploadSkillResponse>, StatusCode> {
    let configs = state.agent_configs.load();
    let agent = configs
        .iter()
        .find(|a| a.id == query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let target_dir = std::path::PathBuf::from(&agent.workspace).join("skills");

    tokio::fs::create_dir_all(&target_dir)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to create skills directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut all_installed = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        tracing::warn!(%error, "failed to read multipart field");
        StatusCode::BAD_REQUEST
    })? {
        let filename = field
            .file_name()
            .map(|n| {
                std::path::Path::new(n)
                    .file_name()
                    .map(|base| base.to_string_lossy().to_string())
                    .unwrap_or_else(|| n.to_string())
            })
            .unwrap_or_else(|| "upload.zip".to_string());

        let data = field.bytes().await.map_err(|error| {
            tracing::warn!(%error, "failed to read upload field");
            StatusCode::BAD_REQUEST
        })?;

        if data.is_empty() {
            continue;
        }

        // Write to a temp file, then install via the existing installer
        let temp_dir = tempfile::tempdir().map_err(|error| {
            tracing::warn!(%error, "failed to create temp dir");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let temp_path = temp_dir.path().join(&filename);
        let mut file = tokio::fs::File::create(&temp_path).await.map_err(|error| {
            tracing::warn!(%error, "failed to create temp file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        file.write_all(&data).await.map_err(|error| {
            tracing::warn!(%error, "failed to write temp file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        file.sync_all().await.map_err(|error| {
            tracing::warn!(%error, "failed to sync temp file");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        drop(file);

        let installed = crate::skills::install_from_file(&temp_path, &target_dir)
            .await
            .map_err(|error| {
                tracing::warn!(%error, filename = %filename, "failed to install uploaded skill");
                StatusCode::BAD_REQUEST
            })?;

        all_installed.extend(installed);
    }

    if !all_installed.is_empty() {
        state.send_event(ApiEvent::ConfigReloaded);
    }

    tracing::info!(
        agent_id = %query.agent_id,
        installed = ?all_installed,
        "skills uploaded"
    );

    Ok(Json(UploadSkillResponse {
        installed: all_installed,
    }))
}

/// Proxy browse requests to skills.sh leaderboard API.
#[utoipa::path(
    get,
    path = "/skills/registry/browse",
    params(
        ("view" = String, Query, description = "View type (all-time, trending, hot)"),
        ("page" = u32, Query, description = "Page number"),
    ),
    responses(
        (status = 200, body = RegistryBrowseResponse),
        (status = 502, description = "Bad gateway"),
    ),
    tag = "skills",
)]
pub(super) async fn registry_browse(
    Query(query): Query<RegistryBrowseQuery>,
) -> Result<Json<RegistryBrowseResponse>, StatusCode> {
    let view = match query.view.as_str() {
        "all-time" | "trending" | "hot" => &query.view,
        _ => "all-time",
    };

    let url = format!("https://skills.sh/api/skills/{}/{}", view, query.page);

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .timeout(Duration::from_secs(10))
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

    #[derive(Deserialize, utoipa::ToSchema)]
    struct UpstreamResponse {
        skills: Vec<RegistrySkill>,
        #[serde(default)]
        #[serde(rename = "hasMore")]
        has_more: bool,
        #[serde(default)]
        total: Option<u64>,
    }

    let body: UpstreamResponse = response.json().await.map_err(|error| {
        tracing::warn!(%error, "failed to parse skills.sh response");
        StatusCode::BAD_GATEWAY
    })?;

    let mut skills = body.skills;
    apply_cached_registry_descriptions(&mut skills);
    spawn_registry_description_enrichment(client, skills.clone());

    Ok(Json(RegistryBrowseResponse {
        skills,
        has_more: body.has_more,
        total: body.total,
    }))
}

/// Proxy search requests to skills.sh search API.
#[utoipa::path(
    get,
    path = "/skills/registry/search",
    params(
        ("q" = String, Query, description = "Search query"),
        ("limit" = u32, Query, description = "Result limit"),
    ),
    responses(
        (status = 200, body = RegistrySearchResponse),
        (status = 400, description = "Invalid request"),
        (status = 502, description = "Bad gateway"),
    ),
    tag = "skills",
)]
pub(super) async fn registry_search(
    Query(query): Query<RegistrySearchQuery>,
) -> Result<Json<RegistrySearchResponse>, StatusCode> {
    if query.q.len() < 2 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let client = reqwest::Client::new();
    let response = client
        .get("https://skills.sh/api/search")
        .query(&[
            ("q", &query.q),
            ("limit", &query.limit.min(100).to_string()),
        ])
        .timeout(Duration::from_secs(10))
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

    #[derive(Deserialize, utoipa::ToSchema)]
    struct UpstreamSearchResponse {
        skills: Vec<RegistrySkill>,
        count: usize,
        query: String,
    }

    let body: UpstreamSearchResponse = response.json().await.map_err(|error| {
        tracing::warn!(%error, "failed to parse skills.sh search response");
        StatusCode::BAD_GATEWAY
    })?;

    let mut skills = body.skills;
    apply_cached_registry_descriptions(&mut skills);
    spawn_registry_description_enrichment(client, skills.clone());

    Ok(Json(RegistrySearchResponse {
        skills,
        query: body.query,
        count: body.count,
    }))
}

/// Fetch the full SKILL.md content for a registry skill from GitHub.
#[utoipa::path(
    get,
    path = "/skills/registry/content",
    params(
        ("source" = String, Query, description = "GitHub owner/repo"),
        ("skill_id" = String, Query, description = "Skill identifier within the repo"),
    ),
    responses(
        (status = 200, body = RegistrySkillContentResponse),
        (status = 400, description = "Invalid request"),
    ),
    tag = "skills",
)]
pub(super) async fn registry_skill_content(
    Query(query): Query<RegistrySkillContentQuery>,
) -> Result<Json<RegistrySkillContentResponse>, StatusCode> {
    if query.source.split('/').count() != 2 || query.skill_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let cache_key = registry_skill_key(&query.source, &query.skill_id);

    // Check cache first
    if let Some(cached) = REGISTRY_SKILL_CONTENT_CACHE.get(&cache_key) {
        return Ok(Json(RegistrySkillContentResponse {
            source: query.source,
            skill_id: query.skill_id,
            content: cached,
        }));
    }

    let client = reqwest::Client::new();
    let content = fetch_registry_skill_content(&client, &query.source, &query.skill_id).await;

    REGISTRY_SKILL_CONTENT_CACHE.insert(cache_key, content.clone());

    Ok(Json(RegistrySkillContentResponse {
        source: query.source,
        skill_id: query.skill_id,
        content,
    }))
}

fn apply_cached_registry_descriptions(skills: &mut [RegistrySkill]) {
    for skill in skills {
        if skill
            .description
            .as_ref()
            .is_some_and(|description| !description.trim().is_empty())
        {
            continue;
        }

        let cache_key = registry_skill_key(&skill.source, &skill.skill_id);
        if let Some(cached_description) = REGISTRY_SKILL_DESCRIPTION_CACHE.get(&cache_key) {
            skill.description = cached_description;
        }
    }
}

fn spawn_registry_description_enrichment(client: reqwest::Client, skills: Vec<RegistrySkill>) {
    let should_enrich = skills.iter().any(|skill| {
        if skill
            .description
            .as_ref()
            .is_some_and(|description| !description.trim().is_empty())
        {
            return false;
        }

        let cache_key = registry_skill_key(&skill.source, &skill.skill_id);
        REGISTRY_SKILL_DESCRIPTION_CACHE.get(&cache_key).is_none()
    });
    if !should_enrich {
        return;
    }

    tokio::spawn(async move {
        let mut enrichment_skills = skills;
        enrich_registry_descriptions(&client, &mut enrichment_skills).await;
    });
}

async fn enrich_registry_descriptions(client: &reqwest::Client, skills: &mut [RegistrySkill]) {
    let mut join_set = tokio::task::JoinSet::new();

    for (index, skill) in skills.iter_mut().enumerate() {
        if skill
            .description
            .as_ref()
            .is_some_and(|description| !description.trim().is_empty())
        {
            continue;
        }

        let source = skill.source.clone();
        let skill_id = skill.skill_id.clone();
        let cache_key = registry_skill_key(&source, &skill_id);

        if let Some(cached_description) = REGISTRY_SKILL_DESCRIPTION_CACHE.get(&cache_key) {
            skill.description = cached_description;
            continue;
        }

        let client = client.clone();
        join_set.spawn(async move {
            let description = fetch_registry_skill_description(&client, &source, &skill_id).await;
            (index, cache_key, description)
        });
    }

    while let Some(result) = join_set.join_next().await {
        let Ok((index, cache_key, description)) = result else {
            continue;
        };
        let description = description.filter(|candidate| !candidate.trim().is_empty());

        if let Some(skill) = skills.get_mut(index) {
            skill.description = description.clone();
        }

        REGISTRY_SKILL_DESCRIPTION_CACHE.insert(cache_key, description);
    }
}

fn registry_skill_key(source: &str, skill_id: &str) -> String {
    format!("{source}/{skill_id}")
}

async fn fetch_registry_skill_description(
    client: &reqwest::Client,
    source: &str,
    skill_id: &str,
) -> Option<String> {
    let repo_name = source.split('/').next_back().unwrap_or_default();

    let mut candidate_paths = if repo_name == skill_id {
        vec![
            "SKILL.md".to_string(),
            format!("{skill_id}/SKILL.md"),
            format!("skills/{skill_id}/SKILL.md"),
            format!(".claude/skills/{skill_id}/SKILL.md"),
        ]
    } else {
        vec![
            format!("{skill_id}/SKILL.md"),
            format!("skills/{skill_id}/SKILL.md"),
            format!(".claude/skills/{skill_id}/SKILL.md"),
            "SKILL.md".to_string(),
        ]
    };

    for path in candidate_paths.drain(..) {
        for branch in ["HEAD", "main", "master"] {
            let url = format!("https://raw.githubusercontent.com/{source}/{branch}/{path}");
            let response = match client
                .get(&url)
                .header(reqwest::header::USER_AGENT, "spacebot-registry-client")
                .timeout(Duration::from_secs(3))
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => continue,
            };

            if !response.status().is_success() {
                continue;
            }

            let markdown = match response.text().await {
                Ok(markdown) => markdown,
                Err(_) => continue,
            };

            if let Some(description) = extract_skill_description(&markdown) {
                return Some(description);
            }
        }
    }

    None
}

/// Fetch the full raw SKILL.md content from GitHub for a registry skill.
///
/// Uses the same candidate-path probing as description enrichment.
async fn fetch_registry_skill_content(
    client: &reqwest::Client,
    source: &str,
    skill_id: &str,
) -> Option<String> {
    let repo_name = source.split('/').next_back().unwrap_or_default();

    let mut candidate_paths = if repo_name == skill_id {
        vec![
            "SKILL.md".to_string(),
            format!("{skill_id}/SKILL.md"),
            format!("skills/{skill_id}/SKILL.md"),
            format!(".claude/skills/{skill_id}/SKILL.md"),
        ]
    } else {
        vec![
            format!("{skill_id}/SKILL.md"),
            format!("skills/{skill_id}/SKILL.md"),
            format!(".claude/skills/{skill_id}/SKILL.md"),
            "SKILL.md".to_string(),
        ]
    };

    for path in candidate_paths.drain(..) {
        for branch in ["HEAD", "main", "master"] {
            let url = format!("https://raw.githubusercontent.com/{source}/{branch}/{path}");
            let response = match client
                .get(&url)
                .header(reqwest::header::USER_AGENT, "spacebot-registry-client")
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => continue,
            };

            if !response.status().is_success() {
                continue;
            }

            match response.text().await {
                Ok(markdown) if !markdown.trim().is_empty() => return Some(markdown),
                _ => continue,
            }
        }
    }

    None
}

fn extract_skill_description(markdown: &str) -> Option<String> {
    let lines = strip_frontmatter(markdown)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();

    for (index, line) in lines.iter().enumerate() {
        let heading = line.trim().to_ascii_lowercase();
        if heading.starts_with('#')
            && heading.contains("description")
            && let Some(description) = extract_paragraph(&lines[(index + 1)..])
        {
            return Some(description);
        }
    }

    extract_paragraph(&lines)
}

fn strip_frontmatter(markdown: &str) -> String {
    let mut lines = markdown.lines();
    let Some(first_line) = lines.next() else {
        return String::new();
    };

    if first_line.trim() != "---" {
        return markdown.to_string();
    }

    for line in lines.by_ref() {
        if line.trim() == "---" {
            break;
        }
    }

    lines.collect::<Vec<_>>().join("\n")
}

fn extract_paragraph(lines: &[String]) -> Option<String> {
    let mut in_code_block = false;
    let mut paragraph_lines = Vec::new();

    for line in lines {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        if trimmed.is_empty() {
            if paragraph_lines.is_empty() {
                continue;
            }
            break;
        }

        if trimmed.starts_with('#') || trimmed.starts_with("| ---") {
            if paragraph_lines.is_empty() {
                continue;
            }
            break;
        }

        let cleaned = cleaned_description_line(trimmed);
        if cleaned.is_empty() {
            continue;
        }
        paragraph_lines.push(cleaned);
    }

    if paragraph_lines.is_empty() {
        return None;
    }

    let mut description = paragraph_lines.join(" ");
    description = description.split_whitespace().collect::<Vec<_>>().join(" ");

    if description.is_empty() {
        return None;
    }

    if description.chars().count() > 220 {
        description = format!("{}...", description.chars().take(217).collect::<String>());
    }

    Some(description)
}

fn cleaned_description_line(line: &str) -> String {
    line.trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim_start_matches("+ ")
        .replace('`', "")
}
