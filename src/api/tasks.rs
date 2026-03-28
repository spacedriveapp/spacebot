use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::process::Command as TokioCommand;

#[derive(Deserialize)]
pub(super) struct TaskListQuery {
    agent_id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default = "default_task_limit")]
    limit: i64,
}

#[derive(Deserialize)]
pub(super) struct TaskGetQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct CreateTaskRequest {
    agent_id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    subtasks: Vec<crate::tasks::TaskSubtask>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    source_memory_id: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct UpdateTaskRequest {
    agent_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    subtasks: Option<Vec<crate::tasks::TaskSubtask>>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    complete_subtask: Option<usize>,
    #[serde(default)]
    worker_id: Option<String>,
    #[serde(default)]
    approved_by: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct DeleteTaskQuery {
    agent_id: String,
}

#[derive(Serialize)]
pub(super) struct TaskListResponse {
    tasks: Vec<crate::tasks::Task>,
}

#[derive(Serialize)]
pub(super) struct TaskResponse {
    task: crate::tasks::Task,
}

#[derive(Serialize)]
pub(super) struct TaskActionResponse {
    success: bool,
    message: String,
}

fn default_task_limit() -> i64 {
    20
}

pub(super) async fn list_tasks(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<TaskListResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let status = match query.status.as_deref() {
        None => None,
        Some(value) => Some(crate::tasks::TaskStatus::parse(value).ok_or(StatusCode::BAD_REQUEST)?),
    };
    let priority = match query.priority.as_deref() {
        None => None,
        Some(value) => {
            Some(crate::tasks::TaskPriority::parse(value).ok_or(StatusCode::BAD_REQUEST)?)
        }
    };

    let tasks = store
        .list(&query.agent_id, status, priority, query.limit.clamp(1, 500))
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, "failed to list tasks");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(TaskListResponse { tasks }))
}

pub(super) async fn get_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Query(query): Query<TaskGetQuery>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let task = store
        .get_by_number(&query.agent_id, number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, task_number = number, "failed to get task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(TaskResponse { task }))
}

pub(super) async fn create_task(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let status = match request.status.as_deref() {
        None => crate::tasks::TaskStatus::Backlog,
        Some(value) => crate::tasks::TaskStatus::parse(value).ok_or(StatusCode::BAD_REQUEST)?,
    };
    let priority = match request.priority.as_deref() {
        None => crate::tasks::TaskPriority::Medium,
        Some(value) => crate::tasks::TaskPriority::parse(value).ok_or(StatusCode::BAD_REQUEST)?,
    };

    let task = store
        .create(crate::tasks::CreateTaskInput {
            agent_id: request.agent_id.clone(),
            title: request.title,
            description: request.description,
            status,
            priority,
            subtasks: request.subtasks,
            metadata: request.metadata.unwrap_or_else(|| serde_json::json!({})),
            source_memory_id: request.source_memory_id,
            created_by: request.created_by.unwrap_or_else(|| "human".to_string()),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, "failed to create task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.agent_id.clone(),
            task_number: task.task_number,
            status: task.status.to_string(),
            action: "created".to_string(),
        })
        .ok();

    Ok(Json(TaskResponse { task }))
}

pub(super) async fn update_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<UpdateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let status = match request.status.as_deref() {
        None => None,
        Some(value) => Some(crate::tasks::TaskStatus::parse(value).ok_or(StatusCode::BAD_REQUEST)?),
    };
    let priority = match request.priority.as_deref() {
        None => None,
        Some(value) => {
            Some(crate::tasks::TaskPriority::parse(value).ok_or(StatusCode::BAD_REQUEST)?)
        }
    };

    let task = store
        .update(
            &request.agent_id,
            number,
            crate::tasks::UpdateTaskInput {
                title: request.title,
                description: request.description,
                status,
                priority,
                subtasks: request.subtasks,
                metadata: request.metadata,
                worker_id: request.worker_id,
                clear_worker_id: false,
                approved_by: request.approved_by,
                complete_subtask: request.complete_subtask,
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, task_number = number, "failed to update task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.agent_id.clone(),
            task_number: task.task_number,
            status: task.status.to_string(),
            action: "updated".to_string(),
        })
        .ok();

    Ok(Json(TaskResponse { task }))
}

pub(super) async fn delete_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Query(query): Query<DeleteTaskQuery>,
) -> Result<Json<TaskActionResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let deleted = store
        .delete(&query.agent_id, number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, task_number = number, "failed to delete task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: query.agent_id.clone(),
            task_number: number,
            status: "deleted".to_string(),
            action: "deleted".to_string(),
        })
        .ok();

    Ok(Json(TaskActionResponse {
        success: true,
        message: format!("Task #{number} deleted"),
    }))
}

pub(super) async fn approve_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<UpdateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let task = store
        .update(
            &request.agent_id,
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                approved_by: request.approved_by,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, task_number = number, "failed to approve task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.agent_id.clone(),
            task_number: task.task_number,
            status: task.status.to_string(),
            action: "updated".to_string(),
        })
        .ok();

    Ok(Json(TaskResponse { task }))
}

/// Move a task to `ready` so the cortex ready-task loop picks it up and spawns
/// a worker. Tasks already in `ready` or `in_progress` are returned as-is.
pub(super) async fn execute_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<UpdateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    // Fetch current task to check if transition is needed.
    let current = store
        .get_by_number(&request.agent_id, number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, task_number = number, "failed to get task for execution");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // If already ready or in-progress, return as-is — cortex will handle it.
    if matches!(
        current.status,
        crate::tasks::TaskStatus::Ready | crate::tasks::TaskStatus::InProgress
    ) {
        return Ok(Json(TaskResponse { task: current }));
    }

    let task = store
        .update(
            &request.agent_id,
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                approved_by: request.approved_by,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %request.agent_id, task_number = number, "failed to execute task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.agent_id.clone(),
            task_number: task.task_number,
            status: task.status.to_string(),
            action: "updated".to_string(),
        })
        .ok();

    Ok(Json(TaskResponse { task }))
}

#[derive(Serialize)]
pub(super) struct TaskDiffResponse {
    task_number: i64,
    diff: String,
    branch: Option<String>,
    worktree: Option<String>,
}

/// Get the git diff for a task. Reads `worktree` and `branch` from task
/// metadata to determine which diff to show.
pub(super) async fn task_diff(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Query(query): Query<TaskGetQuery>,
) -> Result<Json<TaskDiffResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let task = store
        .get_by_number(&query.agent_id, number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %query.agent_id, task_number = number, "failed to get task for diff");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let metadata = task.metadata.as_ref();
    let worktree_path = metadata
        .and_then(|m| m.get("worktree"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let branch = metadata
        .and_then(|m| m.get("branch"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let diff = if let Some(ref wt) = worktree_path {
        let output = TokioCommand::new("git")
            .args(["diff", "HEAD"])
            .current_dir(wt)
            .output()
            .await
            .map_err(|error| {
                tracing::warn!(%error, worktree = %wt, "failed to run git diff in worktree");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        String::from_utf8_lossy(&output.stdout).to_string()
    } else if let Some(ref br) = branch {
        let output = TokioCommand::new("git")
            .args(["diff", &format!("main...{br}")])
            .output()
            .await
            .map_err(|error| {
                tracing::warn!(%error, branch = %br, "failed to run git diff for branch");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        String::new()
    };

    Ok(Json(TaskDiffResponse {
        task_number: number,
        diff,
        branch,
        worktree: worktree_path,
    }))
}

#[derive(Deserialize)]
pub(super) struct WorktreeRequest {
    agent_id: String,
    #[serde(default)]
    base_branch: Option<String>,
}

#[derive(Serialize)]
pub(super) struct WorktreeResponse {
    task_number: i64,
    branch: String,
    worktree_path: String,
    created: bool,
}

/// Create a git worktree for a task. Creates a new branch `task/{number}` and
/// a worktree directory. Stores the path and branch in task metadata.
pub(super) async fn create_worktree(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<WorktreeRequest>,
) -> Result<Json<WorktreeResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&request.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let task = store
        .get_by_number(&request.agent_id, number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to get task for worktree creation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Check if worktree already exists
    if let Some(existing) = task.metadata.as_ref().and_then(|m| m.get("worktree")) {
        if let Some(path) = existing.as_str() {
            let branch = task
                .metadata
                .as_ref()
                .and_then(|m| m.get("branch"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Ok(Json(WorktreeResponse {
                task_number: number,
                branch,
                worktree_path: path.to_string(),
                created: false,
            }));
        }
    }

    let branch_name = format!("task/{number}");
    let base = request.base_branch.as_deref().unwrap_or("main");

    // Determine worktree directory (next to the repo)
    let repo_root = TokioCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to find git root");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let root = String::from_utf8_lossy(&repo_root.stdout)
        .trim()
        .to_string();
    let worktree_dir = format!("{}/../.spacebot-worktrees/task-{number}", root);

    // Create branch from base
    let branch_result = TokioCommand::new("git")
        .args(["branch", &branch_name, base])
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to create branch");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !branch_result.status.success() {
        let stderr = String::from_utf8_lossy(&branch_result.stderr);
        // Branch may already exist, which is fine
        if !stderr.contains("already exists") {
            tracing::warn!(%stderr, "git branch creation failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Create worktree
    let wt_result = TokioCommand::new("git")
        .args(["worktree", "add", &worktree_dir, &branch_name])
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to create worktree");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !wt_result.status.success() {
        let stderr = String::from_utf8_lossy(&wt_result.stderr);
        tracing::warn!(%stderr, "git worktree add failed");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Update task metadata with worktree info
    let mut metadata = task.metadata.unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("worktree".to_string(), serde_json::json!(worktree_dir));
        obj.insert("branch".to_string(), serde_json::json!(branch_name));
    }

    let updated_task = store
        .update(
            &request.agent_id,
            number,
            crate::tasks::UpdateTaskInput {
                metadata: Some(metadata),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to update task metadata with worktree");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: updated_task.agent_id.clone(),
            task_number: updated_task.task_number,
            status: updated_task.status.to_string(),
            action: "updated".to_string(),
        })
        .ok();

    Ok(Json(WorktreeResponse {
        task_number: number,
        branch: branch_name,
        worktree_path: worktree_dir,
        created: true,
    }))
}

/// Remove a task's git worktree and optionally delete the branch.
pub(super) async fn delete_worktree(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Query(query): Query<TaskGetQuery>,
) -> Result<Json<TaskActionResponse>, StatusCode> {
    let stores = state.task_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let task = store
        .get_by_number(&query.agent_id, number)
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to get task for worktree deletion");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let worktree_path = task
        .metadata
        .as_ref()
        .and_then(|m| m.get("worktree"))
        .and_then(|v| v.as_str())
        .ok_or(StatusCode::NOT_FOUND)?
        .to_string();

    // Remove worktree
    let _ = TokioCommand::new("git")
        .args(["worktree", "remove", &worktree_path, "--force"])
        .output()
        .await;

    // Clean metadata
    let mut metadata = task.metadata.unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = metadata.as_object_mut() {
        obj.remove("worktree");
        // Keep branch reference for history
    }

    store
        .update(
            &query.agent_id,
            number,
            crate::tasks::UpdateTaskInput {
                metadata: Some(metadata),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to update task after worktree removal");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(TaskActionResponse {
        success: true,
        message: format!("Worktree removed for task #{number}"),
    }))
}
