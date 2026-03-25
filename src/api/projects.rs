//! REST API handlers for project, repo, and worktree management.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::projects::store::{
    CreateProjectInput, CreateRepoInput, CreateWorktreeInput, ProjectStatus, ProjectWithRelations,
    UpdateProjectInput,
};

// ---------------------------------------------------------------------------
// Path sanitization
// ---------------------------------------------------------------------------

/// Reject paths that contain traversal components (`..`) or are absolute.
/// Returns the cleaned relative path on success.
fn sanitize_relative_path(path: &str) -> Result<String, StatusCode> {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Err(StatusCode::BAD_REQUEST);
    }
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => return Err(StatusCode::BAD_REQUEST),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(StatusCode::BAD_REQUEST);
            }
            _ => {}
        }
    }
    Ok(path.to_string())
}

/// Validate that a name is a single normal path segment (no `/`, `\`, `..`).
fn sanitize_segment(name: &str) -> Result<String, StatusCode> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." || name == "." {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct AgentQuery {
    agent_id: String,
}

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct ProjectListQuery {
    agent_id: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateProjectRequest {
    agent_id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    root_path: String,
    #[serde(default)]
    settings: Option<serde_json::Value>,
    /// When true, scan root_path for git repos and register them automatically.
    #[serde(default = "default_true")]
    auto_discover: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateProjectRequest {
    agent_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    settings: Option<serde_json::Value>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateRepoRequest {
    agent_id: String,
    name: String,
    path: String,
    #[serde(default)]
    remote_url: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateWorktreeRequest {
    agent_id: String,
    repo_id: String,
    branch: String,
    #[serde(default)]
    worktree_name: Option<String>,
    #[serde(default)]
    start_point: Option<String>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProjectListResponse {
    projects: Vec<crate::projects::Project>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProjectResponse {
    #[serde(flatten)]
    project: ProjectWithRelations,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RepoResponse {
    repo: crate::projects::ProjectRepo,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorktreeResponse {
    worktree: crate::projects::ProjectWorktree,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ActionResponse {
    success: bool,
    message: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct DiskUsageResponse {
    total_bytes: u64,
    entries: Vec<DiskUsageEntry>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct DiskUsageEntry {
    name: String,
    bytes: u64,
    is_dir: bool,
}

fn default_true() -> bool {
    true
}

/// Refresh the sandbox allowlist with project root paths after a project
/// create, delete, or scan. Best-effort — logs and continues on error.
async fn refresh_sandbox(state: &ApiState, agent_id: &str) {
    let stores = state.project_stores.load();
    let sandboxes = state.sandboxes.load();
    if let (Some(store), Some(sandbox)) = (stores.get(agent_id), sandboxes.get(agent_id)) {
        crate::projects::refresh_sandbox_project_paths(store, agent_id, sandbox).await;
    }
}

/// Discover worktrees for all repos in a project and register any new ones.
async fn discover_and_register_worktrees(
    store: &Arc<crate::projects::ProjectStore>,
    project_id: &str,
    root: &std::path::Path,
) {
    let repos = match store.list_repos(project_id).await {
        Ok(repos) => repos,
        Err(error) => {
            tracing::warn!(%error, "failed to list repos for worktree discovery");
            return;
        }
    };

    for repo in &repos {
        let repo_abs_path = root.join(&repo.path);
        if !repo_abs_path.is_dir() {
            continue;
        }
        let is_root_repo = repo.path == ".";
        match crate::projects::git::list_worktrees(&repo_abs_path).await {
            Ok(discovered) => {
                for worktree in discovered {
                    // For single-repo projects, worktrees live in the parent
                    // directory. Compute the relative path accordingly.
                    let (name, relative_path) = if is_root_repo {
                        let name = worktree
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        // Store as relative to the parent directory (e.g. "../feat-branch").
                        let parent = root.parent();
                        let rel = parent
                            .and_then(|p| worktree.path.strip_prefix(p).ok())
                            .map(|p| format!("../{}", p.to_string_lossy()))
                            .unwrap_or_else(|| worktree.path.to_string_lossy().to_string());
                        (name, rel)
                    } else {
                        let relative_path = worktree
                            .path
                            .strip_prefix(root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| {
                                worktree
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default()
                            });

                        let name = worktree
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        (name, relative_path)
                    };

                    // Skip if already registered.
                    if store
                        .get_worktree_by_path(project_id, &relative_path)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        continue;
                    }

                    if let Err(error) = store
                        .create_worktree(CreateWorktreeInput {
                            project_id: project_id.to_string(),
                            repo_id: repo.id.clone(),
                            name,
                            path: relative_path,
                            branch: worktree.branch,
                            created_by: "scan".into(),
                        })
                        .await
                    {
                        tracing::warn!(%error, "failed to register discovered worktree");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, repo = %repo.name, "failed to discover worktrees for repo");
            }
        }
    }
}

/// Compute and cache disk usage for all repos and worktrees in a project.
///
/// Runs `dir_size` for each repo and worktree directory and writes the result
/// back to the database. Best-effort — skips entries whose directories are
/// missing or unreadable.
async fn compute_and_cache_disk_usage(
    store: &Arc<crate::projects::ProjectStore>,
    project_id: &str,
    root: &std::path::Path,
) {
    let repos = store.list_repos(project_id).await.unwrap_or_default();
    for repo in &repos {
        let abs_path = root.join(&repo.path);
        if abs_path.is_dir() {
            let bytes = dir_size(&abs_path).await;
            if let Err(error) = store.set_repo_disk_usage(&repo.id, bytes as i64).await {
                tracing::warn!(%error, repo = %repo.name, "failed to cache repo disk usage");
            }
        }
    }

    let worktrees = store.list_worktrees(project_id).await.unwrap_or_default();
    for worktree in &worktrees {
        let abs_path = root.join(&worktree.path);
        if abs_path.is_dir() {
            let bytes = dir_size(&abs_path).await;
            if let Err(error) = store
                .set_worktree_disk_usage(&worktree.id, bytes as i64)
                .await
            {
                tracing::warn!(%error, worktree = %worktree.name, "failed to cache worktree disk usage");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /agents/projects — list projects for an agent.
#[utoipa::path(
    get,
    path = "/agents/projects",
    params(
        ProjectListQuery,
    ),
    responses(
        (status = 200, body = ProjectListResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "projects",
)]
pub(super) async fn list_projects(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProjectListQuery>,
) -> Result<Json<ProjectListResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let status = query.status.as_deref().and_then(ProjectStatus::parse);

    let projects = store
        .list_projects(&query.agent_id, status)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to list projects");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(ProjectListResponse { projects }))
}

/// POST /agents/projects — create a new project.
#[utoipa::path(
    post,
    path = "/agents/projects",
    request_body = CreateProjectRequest,
    responses(
        (status = 200, body = ProjectResponse),
        (status = 404, description = "Agent not found"),
    ),
    tag = "projects",
)]
pub(super) async fn create_project(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateProjectRequest>,
) -> Result<Json<ProjectResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let project = store
        .create_project(CreateProjectInput {
            agent_id: request.agent_id.clone(),
            name: request.name,
            description: request.description.unwrap_or_default(),
            icon: request.icon.unwrap_or_default(),
            tags: request.tags,
            root_path: request.root_path.clone(),
            settings: request
                .settings
                .unwrap_or(serde_json::Value::Object(Default::default())),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to create project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Refresh sandbox allowlist with new project path.
    refresh_sandbox(&state, &request.agent_id).await;

    // Auto-discover repos, worktrees, and disk usage in the background so the
    // API responds immediately. The UI will pick up discovered repos on its
    // next query invalidation / refetch.
    if request.auto_discover {
        let root = std::path::PathBuf::from(&request.root_path);
        if root.is_dir() {
            let store = store.clone();
            let project_id = project.id.clone();
            tokio::spawn(async move {
                match crate::projects::git::discover_repos(&root).await {
                    Ok(discovered) => {
                        for repo in discovered {
                            if let Err(error) = store
                                .create_repo(CreateRepoInput {
                                    project_id: project_id.clone(),
                                    name: repo.name,
                                    path: repo.relative_path,
                                    remote_url: repo.remote_url,
                                    default_branch: repo.default_branch,
                                    current_branch: repo.current_branch,
                                    description: String::new(),
                                })
                                .await
                            {
                                tracing::warn!(%error, "failed to register discovered repo");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to discover repos in project root");
                    }
                }

                discover_and_register_worktrees(&store, &project_id, &root).await;
                compute_and_cache_disk_usage(&store, &project_id, &root).await;

                tracing::info!(project_id = %project_id, "background project scan complete");
            });
        }
    }

    // Return the project immediately (repos/worktrees populate asynchronously).
    let full = store
        .get_project_with_relations(&request.agent_id, &project.id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to load project with relations");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ProjectResponse { project: full }))
}

/// GET /agents/projects/{id} — get a project with repos and worktrees.
#[utoipa::path(
    get,
    path = "/agents/projects/{id}",
    params(
        ("id" = String, Path, description = "Project ID"),
        AgentQuery,
    ),
    responses(
        (status = 200, body = ProjectResponse),
        (status = 404, description = "Agent or project not found"),
    ),
    tag = "projects",
)]
pub(super) async fn get_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<ProjectResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let project = store
        .get_project_with_relations(&query.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ProjectResponse { project }))
}

/// PUT /agents/projects/{id} — update a project.
#[utoipa::path(
    put,
    path = "/agents/projects/{id}",
    params(
        ("id" = String, Path, description = "Project ID"),
    ),
    request_body = UpdateProjectRequest,
    responses(
        (status = 200, body = ProjectResponse),
        (status = 404, description = "Agent or project not found"),
    ),
    tag = "projects",
)]
pub(super) async fn update_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Json(request): Json<UpdateProjectRequest>,
) -> Result<Json<ProjectResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let status = request.status.as_deref().and_then(ProjectStatus::parse);

    store
        .update_project(
            &request.agent_id,
            &project_id,
            UpdateProjectInput {
                name: request.name,
                description: request.description,
                icon: request.icon,
                tags: request.tags,
                settings: request.settings,
                status,
            },
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to update project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Reload with relations.
    let full = store
        .get_project_with_relations(&request.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to reload project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ProjectResponse { project: full }))
}

/// DELETE /agents/projects/{id} — delete a project (DB records only).
#[utoipa::path(
    delete,
    path = "/agents/projects/{id}",
    params(
        ("id" = String, Path, description = "Project ID"),
        AgentQuery,
    ),
    responses(
        (status = 200, body = ActionResponse),
        (status = 404, description = "Agent or project not found"),
    ),
    tag = "projects",
)]
pub(super) async fn delete_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let deleted = store
        .delete_project(&query.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to delete project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    // Refresh sandbox allowlist after removing project path.
    refresh_sandbox(&state, &query.agent_id).await;

    Ok(Json(ActionResponse {
        success: true,
        message: "project deleted".into(),
    }))
}

/// POST /agents/projects/{id}/scan — re-scan project root for repos and worktrees.
#[utoipa::path(
    post,
    path = "/agents/projects/{id}/scan",
    params(
        ("id" = String, Path, description = "Project ID"),
        AgentQuery,
    ),
    responses(
        (status = 200, body = ProjectResponse),
        (status = 404, description = "Agent or project not found"),
    ),
    tag = "projects",
)]
pub(super) async fn scan_project(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<ProjectResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let project = store
        .get_project(&query.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get project for scan");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let root = std::path::PathBuf::from(&project.root_path);
    if !root.is_dir() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    // Discover repos — register new ones and refresh current_branch on existing.
    match crate::projects::git::discover_repos(&root).await {
        Ok(discovered) => {
            for repo in discovered {
                if let Some(existing) = store
                    .get_repo_by_path(&project_id, &repo.relative_path)
                    .await
                    .ok()
                    .flatten()
                {
                    // Refresh the current_branch for existing repos.
                    if let Err(error) = store
                        .update_repo_current_branch(&existing.id, repo.current_branch.as_deref())
                        .await
                    {
                        tracing::warn!(%error, repo = %existing.name, "failed to update current_branch");
                    }
                    continue;
                }
                if let Err(error) = store
                    .create_repo(CreateRepoInput {
                        project_id: project_id.clone(),
                        name: repo.name,
                        path: repo.relative_path,
                        remote_url: repo.remote_url,
                        default_branch: repo.default_branch,
                        current_branch: repo.current_branch,
                        description: String::new(),
                    })
                    .await
                {
                    tracing::warn!(%error, "failed to register discovered repo during scan");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to discover repos during scan");
        }
    }

    // Discover worktrees for each known repo.
    discover_and_register_worktrees(store, &project_id, &root).await;

    // Recompute and cache disk usage.
    compute_and_cache_disk_usage(store, &project_id, &root).await;

    // Reload with relations.
    let full = store
        .get_project_with_relations(&query.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to reload project after scan");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Refresh sandbox allowlist (scan may have added new repos/worktrees).
    refresh_sandbox(&state, &query.agent_id).await;

    Ok(Json(ProjectResponse { project: full }))
}

/// POST /agents/projects/{id}/repos — add a repo to a project.
#[utoipa::path(
    post,
    path = "/agents/projects/{id}/repos",
    params(
        ("id" = String, Path, description = "Project ID"),
    ),
    request_body = CreateRepoRequest,
    responses(
        (status = 200, body = RepoResponse),
        (status = 404, description = "Agent or project not found"),
    ),
    tag = "projects",
)]
pub(super) async fn create_repo(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateRepoRequest>,
) -> Result<Json<RepoResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Verify project exists.
    store
        .get_project(&request.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to verify project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Sanitize the path — must be relative, no traversal components.
    let path = sanitize_relative_path(&request.path)?;

    let repo = store
        .create_repo(CreateRepoInput {
            project_id,
            name: request.name,
            path,
            remote_url: request.remote_url.unwrap_or_default(),
            default_branch: request.default_branch.unwrap_or_else(|| "main".into()),
            current_branch: None,
            description: request.description.unwrap_or_default(),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to create repo");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(RepoResponse { repo }))
}

/// DELETE /agents/projects/{project_id}/repos/{repo_id} — remove a repo.
#[utoipa::path(
    delete,
    path = "/agents/projects/{project_id}/repos/{repo_id}",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("repo_id" = String, Path, description = "Repository ID"),
        AgentQuery,
    ),
    responses(
        (status = 200, body = ActionResponse),
        (status = 404, description = "Agent, project, or repo not found"),
    ),
    tag = "projects",
)]
pub(super) async fn delete_repo(
    State(state): State<Arc<ApiState>>,
    Path((project_id, repo_id)): Path<(String, String)>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Verify the repo belongs to this project.
    let repo = store
        .get_repo(&repo_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get repo");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    if repo.project_id != project_id {
        return Err(StatusCode::NOT_FOUND);
    }

    let deleted = store.delete_repo(&repo_id).await.map_err(|error| {
        tracing::error!(%error, "failed to delete repo");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(ActionResponse {
        success: true,
        message: "repo removed".into(),
    }))
}

/// POST /agents/projects/{id}/worktrees — create a worktree.
#[utoipa::path(
    post,
    path = "/agents/projects/{id}/worktrees",
    params(
        ("id" = String, Path, description = "Project ID"),
    ),
    request_body = CreateWorktreeRequest,
    responses(
        (status = 200, body = WorktreeResponse),
        (status = 404, description = "Agent, project, or repo not found"),
    ),
    tag = "projects",
)]
pub(super) async fn create_worktree(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateWorktreeRequest>,
) -> Result<Json<WorktreeResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Look up the project and repo.
    let project = store
        .get_project(&request.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let repo = store
        .get_repo(&request.repo_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get repo");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Verify the repo belongs to this project.
    if repo.project_id != project_id {
        return Err(StatusCode::NOT_FOUND);
    }

    let root = std::path::PathBuf::from(&project.root_path);
    let repo_abs_path = root.join(&repo.path);
    let is_single_repo = repo.path == ".";

    // Determine worktree name and path — sanitize to prevent traversal.
    let worktree_name = request
        .worktree_name
        .unwrap_or_else(|| request.branch.replace('/', "-"));
    let worktree_name = sanitize_segment(&worktree_name)?;

    // For single-repo projects, place worktrees in the parent directory
    // (as siblings of the repo). For multi-repo projects, place them
    // inside the project root.
    let (worktree_abs_path, worktree_db_path) = if is_single_repo {
        let parent = root.parent().ok_or_else(|| {
            tracing::error!("single-repo project root has no parent directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        (parent.join(&worktree_name), format!("../{worktree_name}"))
    } else {
        (root.join(&worktree_name), worktree_name.clone())
    };

    // Create the git worktree.
    crate::projects::git::create_worktree(
        &repo_abs_path,
        &worktree_abs_path,
        &request.branch,
        request.start_point.as_deref(),
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "failed to create git worktree");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Register in the database.
    let worktree = store
        .create_worktree(CreateWorktreeInput {
            project_id,
            repo_id: request.repo_id,
            name: worktree_name.clone(),
            path: worktree_db_path,
            branch: request.branch,
            created_by: "user".into(),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to register worktree");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(WorktreeResponse { worktree }))
}

/// DELETE /agents/projects/{project_id}/worktrees/{worktree_id} — remove a worktree.
#[utoipa::path(
    delete,
    path = "/agents/projects/{project_id}/worktrees/{worktree_id}",
    params(
        ("project_id" = String, Path, description = "Project ID"),
        ("worktree_id" = String, Path, description = "Worktree ID"),
        AgentQuery,
    ),
    responses(
        (status = 200, body = ActionResponse),
        (status = 404, description = "Agent, project, or worktree not found"),
    ),
    tag = "projects",
)]
pub(super) async fn delete_worktree(
    State(state): State<Arc<ApiState>>,
    Path((project_id, worktree_id)): Path<(String, String)>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<ActionResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Look up worktree and project for the git removal.
    let worktree = store
        .get_worktree(&worktree_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get worktree");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Verify the worktree belongs to this project.
    if worktree.project_id != project_id {
        return Err(StatusCode::NOT_FOUND);
    }

    let project = store
        .get_project(&query.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get project");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let repo = store
        .get_repo(&worktree.repo_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get repo");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Run `git worktree remove`.
    let root = std::path::PathBuf::from(&project.root_path);
    let repo_abs_path = root.join(&repo.path);
    // Worktree paths may be relative with `../` for single-repo projects.
    let worktree_abs_path = root.join(&worktree.path);

    // Only delete the DB record if the git removal succeeds (or the directory
    // no longer exists on disk). This prevents ghost worktrees on disk with no
    // corresponding DB entry.
    if worktree_abs_path.exists() {
        crate::projects::git::remove_worktree(&repo_abs_path, &worktree_abs_path)
            .await
            .map_err(|error| {
                tracing::error!(%error, "git worktree remove failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    // Delete from database.
    store.delete_worktree(&worktree_id).await.map_err(|error| {
        tracing::error!(%error, "failed to delete worktree record");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ActionResponse {
        success: true,
        message: "worktree removed".into(),
    }))
}

/// GET /agents/projects/{id}/disk-usage — calculate disk usage for a project.
#[utoipa::path(
    get,
    path = "/agents/projects/{id}/disk-usage",
    params(
        ("id" = String, Path, description = "Project ID"),
        AgentQuery,
    ),
    responses(
        (status = 200, body = DiskUsageResponse),
        (status = 404, description = "Agent or project not found"),
    ),
    tag = "projects",
)]
pub(super) async fn disk_usage(
    State(state): State<Arc<ApiState>>,
    Path(project_id): Path<String>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<DiskUsageResponse>, StatusCode> {
    let stores = state.project_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let project = store
        .get_project(&query.agent_id, &project_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "failed to get project for disk usage");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let root = std::path::PathBuf::from(&project.root_path);
    if !root.is_dir() {
        return Ok(Json(DiskUsageResponse {
            total_bytes: 0,
            entries: Vec::new(),
        }));
    }

    let mut entries = Vec::new();
    let mut total_bytes: u64 = 0;

    let mut dir_entries = tokio::fs::read_dir(&root).await.map_err(|error| {
        tracing::error!(%error, "failed to read project root for disk usage");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    while let Ok(Some(entry)) = dir_entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Skip symlinks entirely — don't follow them to avoid escaping the project root.
        if metadata.is_symlink() {
            continue;
        }
        let is_dir = metadata.is_dir();
        let bytes = if is_dir {
            // For directories, approximate with a quick du.
            dir_size(&entry.path()).await
        } else {
            metadata.len()
        };
        total_bytes += bytes;
        entries.push(DiskUsageEntry {
            name,
            bytes,
            is_dir,
        });
    }

    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    Ok(Json(DiskUsageResponse {
        total_bytes,
        entries,
    }))
}

/// Recursively calculate directory size. Best-effort — skips entries it can't
/// read. Uses `symlink_metadata` to avoid following symlinks (prevents infinite
/// recursion and escaping project root).
async fn dir_size(path: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];

    while let Some(current) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }

    total
}
