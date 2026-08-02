use super::state::ApiState;
use crate::notifications::{NewNotification, NotificationKind, NotificationSeverity};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct TaskListQuery {
    /// Convenience filter: matches tasks where owner OR assigned equals this value.
    #[serde(default)]
    agent_id: Option<String>,
    /// Filter by owner agent. Optional.
    #[serde(default)]
    owner_agent_id: Option<String>,
    /// Filter by assigned agent. Optional.
    #[serde(default)]
    assigned_agent_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default = "default_task_limit")]
    limit: i64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateTaskRequest {
    /// Agent that owns (created) this task.
    owner_agent_id: String,
    /// Agent assigned to execute. Defaults to `owner_agent_id`.
    #[serde(default)]
    assigned_agent_id: Option<String>,
    title: String,
    #[serde(default)]
    description: Option<String>,
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
    /// Project this task acts on.
    #[serde(default)]
    project_id: Option<String>,
    /// Repo within the project. A project holds many repos.
    #[serde(default)]
    repo_id: Option<String>,
    /// Worktree to execute in.
    #[serde(default)]
    worktree_id: Option<String>,
    /// Task numbers that must finish before this one may run.
    #[serde(default)]
    depends_on: Vec<i64>,
    /// Status to create the task in. Defaults to `pending_approval`.
    ///
    /// The dashboard has always sent `backlog` here; the field simply did not
    /// exist, so serde dropped it and every task created from the UI came back
    /// awaiting an approval the creator had just given by clicking "create".
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateTaskRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    assigned_agent_id: Option<String>,
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
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    worktree_id: Option<String>,
    /// Unbind the task from its project/repo/worktree entirely.
    #[serde(default)]
    clear_binding: bool,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ApproveRequest {
    #[serde(default)]
    approved_by: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AssignRequest {
    assigned_agent_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskListResponse {
    tasks: Vec<crate::tasks::Task>,
    /// Edge counts for every task that has any. Tasks with no dependencies are
    /// absent rather than listed with zeroes.
    edges: Vec<crate::tasks::TaskEdgeSummary>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskResponse {
    task: crate::tasks::Task,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskActionResponse {
    success: bool,
    message: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskRunsResponse {
    runs: Vec<crate::tasks::TaskRun>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskTransition {
    from: crate::tasks::TaskStatus,
    to: crate::tasks::TaskStatus,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskTransitionsResponse {
    transitions: Vec<TaskTransition>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskDependenciesResponse {
    /// Tasks this one waits on.
    parents: Vec<i64>,
    /// Tasks waiting on this one.
    children: Vec<i64>,
    /// The subset of `parents` that has not finished yet — what the board
    /// should name when explaining why a task is not moving.
    blocked_by: Vec<i64>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AddDependencyRequest {
    parent_task_number: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskContractResponse {
    input_schema: Option<serde_json::Value>,
    output_schema: Option<serde_json::Value>,
    /// Inputs as they were resolved at the last claim.
    inputs: Option<serde_json::Value>,
    outputs: Option<serde_json::Value>,
    /// What the bindings resolve to right now, which may differ from `inputs`
    /// if the graph changed since the last attempt.
    resolved_inputs: Option<serde_json::Value>,
    bindings: Vec<crate::tasks::TaskInputBinding>,
    /// Why resolution fails, if it does. Empty when the contract is satisfied.
    problems: Vec<crate::tasks::ContractProblem>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskProvenanceResponse {
    /// The task that filed this one, when a worker did.
    filed_by_task_number: Option<i64>,
    /// Cards this task filed.
    filed: Vec<crate::tasks::Task>,
    /// How many more this task may still file before hitting the cap.
    remaining_fan_out: i64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetContractRequest {
    #[serde(default)]
    input_schema: Option<serde_json::Value>,
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetBindingRequest {
    /// Upstream task to read from. Omit for a literal.
    #[serde(default)]
    source_task_number: Option<i64>,
    /// RFC 6901 JSON Pointer into that task's outputs.
    #[serde(default)]
    source_pointer: Option<String>,
    /// Literal JSON value, used when no source task is given.
    #[serde(default)]
    literal_value: Option<serde_json::Value>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct BlockTaskRequest {
    /// dependency | needs_input | capability | transient
    kind: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_task_limit() -> i64 {
    100
}

/// Extract the global task store, returning 503 if not yet initialized.
fn get_task_store(state: &ApiState) -> Result<Arc<crate::tasks::TaskStore>, StatusCode> {
    state
        .task_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn parse_status(value: Option<&str>) -> Result<Option<crate::tasks::TaskStatus>, StatusCode> {
    match value {
        None => Ok(None),
        Some(value) => Ok(Some(
            crate::tasks::TaskStatus::parse(value).ok_or(StatusCode::BAD_REQUEST)?,
        )),
    }
}

fn parse_priority(value: Option<&str>) -> Result<Option<crate::tasks::TaskPriority>, StatusCode> {
    match value {
        None => Ok(None),
        Some(value) => Ok(Some(
            crate::tasks::TaskPriority::parse(value).ok_or(StatusCode::BAD_REQUEST)?,
        )),
    }
}

fn emit_task_event(state: &ApiState, task: &crate::tasks::Task, action: &str) {
    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.assigned_agent_id.clone(),
            task_number: task.task_number,
            status: task.status.to_string(),
            action: action.to_string(),
        })
        .ok();
}

/// Emit a task_approval notification when a task enters the pending_approval state.
fn maybe_emit_approval_notification(state: &ApiState, task: &crate::tasks::Task) {
    if task.status != crate::tasks::TaskStatus::PendingApproval {
        return;
    }
    state.emit_notification(NewNotification {
        kind: NotificationKind::TaskApproval,
        severity: NotificationSeverity::Info,
        title: task.title.clone(),
        body: task.description.clone(),
        agent_id: Some(task.assigned_agent_id.clone()),
        related_entity_type: Some("task".to_string()),
        related_entity_id: Some(task.task_number.to_string()),
        action_url: Some(format!("/tasks/{}", task.task_number)),
        metadata: None,
    });
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /tasks` — list tasks with optional filters.
#[utoipa::path(
    get,
    path = "/tasks",
    params(TaskListQuery),
    responses(
        (status = 200, body = TaskListResponse),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn list_tasks(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<TaskListResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let status = parse_status(query.status.as_deref())?;
    let priority = parse_priority(query.priority.as_deref())?;

    let tasks = store
        .list(crate::tasks::TaskListFilter {
            agent_id: query.agent_id,
            owner_agent_id: query.owner_agent_id,
            assigned_agent_id: query.assigned_agent_id,
            status,
            priority,
            created_by: query.created_by,
            limit: Some(query.limit.clamp(1, 500)),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to list tasks");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Edge counts ride along with the list rather than being fetched per card.
    // The board draws a badge on every row; a request per row would defeat the
    // point of a list endpoint. A failure here degrades the badges, not the
    // board, so it is logged rather than propagated.
    let edges = store.dependency_summaries().await.unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to summarize task dependencies");
        Vec::new()
    });

    Ok(Json(TaskListResponse { tasks, edges }))
}

/// `GET /tasks/transitions` — every legal status move.
///
/// The dashboard reads this instead of hand-maintaining a second transition
/// table in TypeScript, so a board can never offer a move the API rejects.
#[utoipa::path(
    get,
    path = "/tasks/transitions",
    responses((status = 200, body = TaskTransitionsResponse)),
    tag = "tasks",
)]
pub(super) async fn list_task_transitions() -> Json<TaskTransitionsResponse> {
    Json(TaskTransitionsResponse {
        transitions: crate::tasks::legal_transitions()
            .into_iter()
            .map(|(from, to)| TaskTransition { from, to })
            .collect(),
    })
}

/// `GET /tasks/{number}` — get a task by globally unique number.
#[utoipa::path(
    get,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn get_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks` — create a task.
#[utoipa::path(
    post,
    path = "/tasks",
    request_body = CreateTaskRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 400, description = "Invalid request"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn create_task(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let status = parse_status(request.status.as_deref())?
        .unwrap_or(crate::tasks::TaskStatus::PendingApproval);
    let priority =
        parse_priority(request.priority.as_deref())?.unwrap_or(crate::tasks::TaskPriority::Medium);

    let assigned = request
        .assigned_agent_id
        .unwrap_or_else(|| request.owner_agent_id.clone());

    let task = store
        .create(crate::tasks::CreateTaskInput {
            owner_agent_id: request.owner_agent_id,
            assigned_agent_id: assigned,
            title: request.title,
            description: request.description,
            status,
            priority,
            subtasks: request.subtasks,
            metadata: request.metadata.unwrap_or_else(|| serde_json::json!({})),
            source_memory_id: request.source_memory_id,
            created_by: request.created_by.unwrap_or_else(|| "human".to_string()),
            binding: crate::tasks::TaskProjectBinding {
                project_id: request.project_id,
                repo_id: request.repo_id,
                worktree_id: request.worktree_id,
            },
            depends_on: request.depends_on,
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to create task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    emit_task_event(&state, &task, "created");
    maybe_emit_approval_notification(&state, &task);
    Ok(Json(TaskResponse { task }))
}

/// `PUT /tasks/{number}` — update a task.
#[utoipa::path(
    put,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = UpdateTaskRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn update_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<UpdateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let status = parse_status(request.status.as_deref())?;
    let priority = parse_priority(request.priority.as_deref())?;

    // Each binding column is patched independently: naming only `repo_id` must
    // rebind the repo and leave the project and worktree exactly as they were.
    // Use `clear_binding` to unbind entirely.
    let binding = crate::tasks::TaskBindingPatch {
        project_id: request.project_id.map(Some),
        repo_id: request.repo_id.map(Some),
        worktree_id: request.worktree_id.map(Some),
    };

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                title: request.title,
                description: request.description,
                status,
                priority,
                assigned_agent_id: request.assigned_agent_id,
                subtasks: request.subtasks,
                metadata: request.metadata,
                worker_id: request.worker_id,
                approved_by: request.approved_by,
                complete_subtask: request.complete_subtask,
                binding,
                clear_binding: request.clear_binding,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to update task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    maybe_emit_approval_notification(&state, &task);
    Ok(Json(TaskResponse { task }))
}

/// `DELETE /tasks/{number}` — delete a task.
#[utoipa::path(
    delete,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, body = TaskActionResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn delete_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskActionResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    // Fetch before delete so we can emit an event with the correct agent_id.
    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task for deletion");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let deleted = store.delete(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to delete task");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.assigned_agent_id,
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

/// `POST /tasks/{number}/approve` — approve a task (move to ready).
#[utoipa::path(
    post,
    path = "/tasks/{number}/approve",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = ApproveRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn approve_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<ApproveRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                approved_by: request.approved_by,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to approve task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    // Auto-dismiss any pending task_approval notification for this task.
    if let Some(store) = state.notification_store.load().as_ref().clone()
        && let Err(error) = store
            .dismiss_by_entity("task_approval", "task", &number.to_string())
            .await
    {
        tracing::warn!(%error, task_number = number, "failed to auto-dismiss approval notification");
    }
    Ok(Json(TaskResponse { task }))
}

/// `GET /tasks/{number}/runs` — the per-attempt execution log for a task.
#[utoipa::path(
    get,
    path = "/tasks/{number}/runs",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, body = TaskRunsResponse),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn list_task_runs(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskRunsResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let runs = store.list_runs(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list task runs");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(TaskRunsResponse { runs }))
}

/// `POST /tasks/{number}/retry` — clear the failure budget and requeue.
///
/// A human looked at the task, so the budget starts over rather than
/// immediately re-parking it on the next failure.
#[utoipa::path(
    post,
    path = "/tasks/{number}/retry",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn retry_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    store.clear_failures(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to clear task failure budget");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                clear_worker_id: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to requeue task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks/{number}/execute` — move a task to ready for execution.
/// Tasks already in `ready` or `in_progress` are returned as-is.
#[utoipa::path(
    post,
    path = "/tasks/{number}/execute",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = ApproveRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 409, description = "Task pending approval"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn execute_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<ApproveRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let current = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task for execution");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    if matches!(
        current.status,
        crate::tasks::TaskStatus::Ready | crate::tasks::TaskStatus::InProgress
    ) {
        return Ok(Json(TaskResponse { task: current }));
    }

    // Reject pending_approval tasks — they must be approved first.
    if current.status == crate::tasks::TaskStatus::PendingApproval {
        return Err(StatusCode::CONFLICT);
    }

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                approved_by: request.approved_by,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to execute task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks/{number}/assign` — reassign a task to a different agent.
#[utoipa::path(
    post,
    path = "/tasks/{number}/assign",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = AssignRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn assign_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<AssignRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                assigned_agent_id: Some(request.assigned_agent_id),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to assign task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

/// `GET /tasks/{number}/dependencies` — the edges around a task.
#[utoipa::path(
    get,
    path = "/tasks/{number}/dependencies",
    params(("number" = i64, Path, description = "Task number")),
    responses(
        (status = 200, body = TaskDependenciesResponse),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn list_task_dependencies(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskDependenciesResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let parents = store.list_parents(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list task parents");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let children = store.list_children(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list task children");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let blocked_by = store.unfinished_parents(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list unfinished parents");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(TaskDependenciesResponse {
        parents,
        children,
        blocked_by,
    }))
}

/// `POST /tasks/{number}/dependencies` — make this task wait on another.
///
/// Rejects self-loops, unknown tasks, and any edge that would close a cycle.
/// The cycle response names the path so the caller can see which existing edge
/// conflicts, rather than being told only that something is wrong.
#[utoipa::path(
    post,
    path = "/tasks/{number}/dependencies",
    params(("number" = i64, Path, description = "Child task number")),
    request_body = AddDependencyRequest,
    responses(
        (status = 200, body = TaskDependenciesResponse),
        (status = 404, description = "Task not found"),
        (status = 409, description = "Edge would create a cycle"),
        (status = 422, description = "A task cannot depend on itself"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn add_task_dependency(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<AddDependencyRequest>,
) -> Result<Json<TaskDependenciesResponse>, (StatusCode, String)> {
    let store = get_task_store(&state)
        .map_err(|status| (status, "task store not initialized".to_string()))?;

    store
        .link_tasks(request.parent_task_number, number)
        .await
        .map_err(|error| {
            let status = match &error {
                crate::tasks::DependencyError::SelfLoop { .. } => StatusCode::UNPROCESSABLE_ENTITY,
                crate::tasks::DependencyError::UnknownTask { .. } => StatusCode::NOT_FOUND,
                crate::tasks::DependencyError::WouldCycle { .. } => StatusCode::CONFLICT,
                crate::tasks::DependencyError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })?;

    // A task that just gained an unfinished parent must not stay claimable.
    if let Ok(unfinished) = store.unfinished_parents(number).await
        && !unfinished.is_empty()
        && let Err(error) = store
            .block_task(
                number,
                crate::tasks::BlockKind::Dependency,
                &format!(
                    "waiting on {}",
                    unfinished
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .await
    {
        tracing::warn!(%error, task_number = number, "failed to park newly dependent task");
    }

    list_task_dependencies(State(state), Path(number))
        .await
        .map_err(|status| (status, "failed to read dependencies".to_string()))
}

/// `DELETE /tasks/{number}/dependencies/{parent}` — drop an edge.
#[utoipa::path(
    delete,
    path = "/tasks/{number}/dependencies/{parent}",
    params(
        ("number" = i64, Path, description = "Child task number"),
        ("parent" = i64, Path, description = "Parent task number"),
    ),
    responses(
        (status = 200, body = TaskDependenciesResponse),
        (status = 404, description = "Edge not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn remove_task_dependency(
    State(state): State<Arc<ApiState>>,
    Path((number, parent)): Path<(i64, i64)>,
) -> Result<Json<TaskDependenciesResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let removed = store.unlink_tasks(parent, number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to unlink tasks");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if !removed {
        return Err(StatusCode::NOT_FOUND);
    }

    list_task_dependencies(State(state), Path(number)).await
}

/// `POST /tasks/{number}/block` — park a task with a typed reason.
#[utoipa::path(
    post,
    path = "/tasks/{number}/block",
    params(("number" = i64, Path, description = "Task number")),
    request_body = BlockTaskRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 422, description = "Unknown block kind"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn block_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<BlockTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let kind =
        crate::tasks::BlockKind::parse(&request.kind).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;

    store
        .block_task(number, kind, &request.reason)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to block task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to read blocked task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks/{number}/unblock` — release a parked task.
///
/// Lands in `ready` when nothing upstream is outstanding, `backlog` otherwise.
#[utoipa::path(
    post,
    path = "/tasks/{number}/unblock",
    params(("number" = i64, Path, description = "Task number")),
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found or not blocked"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn unblock_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .unblock_task(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to unblock task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

/// `GET /tasks/{number}/contract` — the declared contract, its bindings, and
/// what those bindings currently resolve to.
///
/// Resolution runs live rather than being read back from the last claim, so the
/// page shows what the task *would* get if it ran now. A graph that has drifted
/// since the last attempt is exactly the case worth seeing.
#[utoipa::path(
    get,
    path = "/tasks/{number}/contract",
    params(("number" = i64, Path, description = "Task number")),
    responses(
        (status = 200, body = TaskContractResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn get_task_contract(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskContractResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to read task for contract");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let bindings = store.list_input_bindings(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list input bindings");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (resolved_inputs, problems) = match store.resolve_inputs(number).await {
        Ok(crate::tasks::ContractResolution::Resolved { inputs }) => (Some(inputs), Vec::new()),
        Ok(crate::tasks::ContractResolution::Unresolved { problems }) => (None, problems),
        Ok(crate::tasks::ContractResolution::NotRequired) => (None, Vec::new()),
        Err(error) => {
            tracing::warn!(%error, task_number = number, "failed to resolve inputs for contract view");
            (None, Vec::new())
        }
    };

    Ok(Json(TaskContractResponse {
        input_schema: task.input_schema,
        output_schema: task.output_schema,
        inputs: task.inputs,
        outputs: task.outputs,
        resolved_inputs,
        bindings,
        problems,
    }))
}

/// `PUT /tasks/{number}/contract` — declare what a task needs and produces.
#[utoipa::path(
    put,
    path = "/tasks/{number}/contract",
    params(("number" = i64, Path, description = "Task number")),
    request_body = SetContractRequest,
    responses(
        (status = 200, body = TaskContractResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn set_task_contract(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<SetContractRequest>,
) -> Result<Json<TaskContractResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    store
        .set_contract(
            number,
            request.input_schema.as_ref(),
            request.output_schema.as_ref(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to set task contract");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    get_task_contract(State(state), Path(number)).await
}

/// `PUT /tasks/{number}/bindings/{key}` — point one input at its source.
#[utoipa::path(
    put,
    path = "/tasks/{number}/bindings/{key}",
    params(
        ("number" = i64, Path, description = "Task number"),
        ("key" = String, Path, description = "Input key"),
    ),
    request_body = SetBindingRequest,
    responses(
        (status = 200, body = TaskContractResponse),
        (status = 422, description = "A binding must name either a source task or a literal"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn set_task_binding(
    State(state): State<Arc<ApiState>>,
    Path((number, key)): Path<(i64, String)>,
    Json(request): Json<SetBindingRequest>,
) -> Result<Json<TaskContractResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    // A binding that names neither a source nor a literal resolves to nothing
    // and would fail silently at claim time — reject it while somebody is
    // looking at it.
    if request.source_task_number.is_none() && request.literal_value.is_none() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    store
        .set_input_binding(&crate::tasks::TaskInputBinding {
            child_task_number: number,
            input_key: key,
            source_task_number: request.source_task_number,
            source_pointer: request.source_pointer,
            literal_value: request.literal_value,
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to set input binding");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    get_task_contract(State(state), Path(number)).await
}

/// `DELETE /tasks/{number}/bindings/{key}` — unbind one input.
#[utoipa::path(
    delete,
    path = "/tasks/{number}/bindings/{key}",
    params(
        ("number" = i64, Path, description = "Task number"),
        ("key" = String, Path, description = "Input key"),
    ),
    responses(
        (status = 200, body = TaskContractResponse),
        (status = 404, description = "Binding not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn remove_task_binding(
    State(state): State<Arc<ApiState>>,
    Path((number, key)): Path<(i64, String)>,
) -> Result<Json<TaskContractResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let removed = store
        .remove_input_binding(number, &key)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to remove input binding");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if !removed {
        return Err(StatusCode::NOT_FOUND);
    }

    get_task_contract(State(state), Path(number)).await
}

/// `GET /tasks/{number}/provenance` — where this card came from and what it
/// spawned.
///
/// A worker-filed card is otherwise indistinguishable from one a human wrote,
/// which makes a surprising board impossible to explain.
#[utoipa::path(
    get,
    path = "/tasks/{number}/provenance",
    params(("number" = i64, Path, description = "Task number")),
    responses(
        (status = 200, body = TaskProvenanceResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn get_task_provenance(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskProvenanceResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to read task for provenance");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let filer = crate::tasks::filer_id(number);
    let filed = store.list_tasks_filed_by(&filer).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list filed tasks");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let remaining_fan_out = (crate::tasks::MAX_TASKS_FILED_PER_TASK - filed.len() as i64).max(0);

    Ok(Json(TaskProvenanceResponse {
        filed_by_task_number: crate::tasks::parse_filer_task_number(&task.created_by),
        filed,
        remaining_fan_out,
    }))
}
