//! REST API handlers for the dynamic project registry.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::process::Command as TokioCommand;

use crate::registry::store::RegistryRepo;
use crate::registry::sync::{SyncResult, SyncStatus, sync_registry};

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct AgentQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct RepoListQuery {
    agent_id: String,
    #[serde(default)]
    enabled_only: bool,
}

#[derive(Deserialize)]
pub(super) struct RepoQuery {
    agent_id: String,
    full_name: String,
}

#[derive(Deserialize)]
pub(super) struct UpdateRepoOverridesBody {
    agent_id: String,
    full_name: String,
    /// Set to `Some(Some("model"))` to set, `Some(None)` to clear, `None` to leave unchanged.
    worker_model: Option<Option<String>>,
    enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct RepoListResponse {
    repos: Vec<RegistryRepo>,
    total: usize,
}

#[derive(Serialize)]
pub(super) struct SyncResponse {
    result: SyncResult,
}

#[derive(Serialize)]
pub(super) struct StatusResponse {
    status: SyncStatus,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/registry/repos — list all registry repos for an agent.
pub(super) async fn list_registry_repos(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RepoListQuery>,
) -> Result<Json<RepoListResponse>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repos = store
        .list_repos(&query.agent_id, query.enabled_only)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total = repos.len();
    Ok(Json(RepoListResponse { repos, total }))
}

/// GET /api/registry/repos/detail — get a single repo by full_name.
pub(super) async fn get_registry_repo(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RepoQuery>,
) -> Result<Json<RegistryRepo>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repo = store
        .get_by_full_name(&query.agent_id, &query.full_name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(repo))
}

/// PUT /api/registry/repos/overrides — update per-repo overrides.
pub(super) async fn update_repo_overrides(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UpdateRepoOverridesBody>,
) -> Result<Json<RegistryRepo>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&body.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repo = store
        .set_overrides(
            &body.agent_id,
            &body.full_name,
            body.worker_model,
            body.enabled,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(repo))
}

/// POST /api/registry/sync — trigger a manual sync.
pub(super) async fn trigger_sync(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<SyncResponse>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let configs = state.runtime_configs.load();
    let runtime_config = configs.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let registry_config = runtime_config.registry.load();

    if !registry_config.enabled {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Update sync status
    let status_map = state.registry_sync_status.load();
    if let Some(status) = status_map.get(&query.agent_id) {
        status.store(Arc::new(SyncStatus::Syncing));
    }

    let result = sync_registry(store, &query.agent_id, &registry_config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Update sync status
    if let Some(status) = status_map.get(&query.agent_id) {
        status.store(Arc::new(SyncStatus::Completed {
            at: chrono::Utc::now().to_rfc3339(),
            result: result.clone(),
        }));
    }

    Ok(Json(SyncResponse { result }))
}

/// GET /api/registry/status — get current sync status.
pub(super) async fn registry_status(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<StatusResponse>, StatusCode> {
    let status_map = state.registry_sync_status.load();
    let status = status_map
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let current = status.load().as_ref().clone();
    Ok(Json(StatusResponse { status: current }))
}

// ---------------------------------------------------------------------------
// GitHub Issues integration
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct IssuesQuery {
    agent_id: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default = "default_issues_limit")]
    limit: usize,
    #[serde(default = "default_issues_state")]
    state: String,
}

fn default_issues_limit() -> usize { 50 }
fn default_issues_state() -> String { "open".to_string() }

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub repository: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Serialize)]
pub(super) struct IssuesResponse {
    issues: Vec<GitHubIssue>,
    repos: Vec<String>,
}

/// GET /api/registry/issues — fetch GitHub issues from all registry repos.
/// Uses the `gh` CLI to query GitHub.
pub(super) async fn list_registry_issues(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<IssuesQuery>,
) -> Result<Json<IssuesResponse>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repos = store
        .list_repos(&query.agent_id, true)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let repo_names: Vec<String> = repos.iter().map(|r| r.full_name.clone()).collect();

    // Filter to specific repo if requested
    let target_repos: Vec<&str> = if let Some(ref repo_filter) = query.repo {
        repo_names
            .iter()
            .filter(|r| r.as_str() == repo_filter.as_str())
            .map(|r| r.as_str())
            .collect()
    } else {
        repo_names.iter().map(|r| r.as_str()).collect()
    };

    let mut all_issues = Vec::new();

    for repo_name in &target_repos {
        let output = TokioCommand::new("gh")
            .args([
                "issue", "list",
                "--repo", repo_name,
                "--state", &query.state,
                "--limit", &query.limit.to_string(),
                "--json", "number,title,state,url,labels,assignees,createdAt,updatedAt",
            ])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                if let Ok(issues) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) {
                    for issue in issues {
                        all_issues.push(GitHubIssue {
                            number: issue["number"].as_i64().unwrap_or(0),
                            title: issue["title"].as_str().unwrap_or("").to_string(),
                            state: issue["state"].as_str().unwrap_or("OPEN").to_string(),
                            url: issue["url"].as_str().unwrap_or("").to_string(),
                            repository: repo_name.to_string(),
                            labels: issue["labels"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|l| l["name"].as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            assignees: issue["assignees"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|a| a["login"].as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            created_at: issue["createdAt"].as_str().unwrap_or("").to_string(),
                            updated_at: issue["updatedAt"].as_str().unwrap_or("").to_string(),
                        });
                    }
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(%repo_name, %stderr, "gh issue list failed");
            }
            Err(e) => {
                tracing::warn!(%repo_name, %e, "failed to run gh CLI");
            }
        }
    }

    // Sort by updated_at descending
    all_issues.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    Ok(Json(IssuesResponse {
        issues: all_issues,
        repos: repo_names,
    }))
}
