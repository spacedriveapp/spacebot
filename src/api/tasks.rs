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
    /// How many failures this task tolerates before it is parked.
    ///
    /// Absent leaves it alone; explicit `null` returns it to the instance
    /// default. Distinguishing those needs the doubly-nested option — a plain
    /// `Option` cannot express "clear this".
    #[serde(default, deserialize_with = "double_option")]
    max_retries: Option<Option<i64>>,
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
    /// How many consecutive failures a task tolerates when it sets no limit of
    /// its own.
    ///
    /// Published because the dashboard has to show what "default" *means* — a
    /// budget control that says "uses the default" without the number tells a
    /// reader nothing they can act on. The alternative was hard-coding it in
    /// TypeScript, which is the same silent-drift bug this codebase has already
    /// paid for four times over in dead config.
    default_failure_limit: i64,
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

/// Distinguish "field absent" from "field explicitly null".
///
/// `#[serde(default)]` yields `None` when the key is missing; this wraps a
/// present value — including `null` — in `Some`. Without it, clearing a field
/// and not mentioning it are the same request, which is how the binding patch
/// used to null columns nobody asked about.
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
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

    Ok(Json(TaskListResponse {
        tasks,
        edges,
        default_failure_limit: crate::tasks::DEFAULT_FAILURE_LIMIT,
    }))
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
                max_retries: request.max_retries,
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
        // Waiting on branches that have not finished is not a contract problem,
        // and listing it as one would put a red mark on a healthy pipeline.
        Ok(crate::tasks::ContractResolution::Pending { .. }) => (None, Vec::new()),
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
            // Fan-in bindings are written by a workflow launch, which knows the
            // step key. Hand-editing one task's binding cannot name a set.
            fan_in_step_key: None,
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

// ---------------------------------------------------------------------------
// External gates
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateGateRequest {
    /// `http` | `task_output`
    kind: String,
    /// Shape depends on `kind`. See `crate::tasks::gates`.
    config: serde_json::Value,
    /// What the board should call this gate. "waiting for CI on main" beats a
    /// URL.
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    poll_interval_secs: Option<i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskGatesResponse {
    gates: Vec<crate::tasks::TaskGate>,
}

fn gate_store(state: &ApiState) -> Result<crate::tasks::GateStore, StatusCode> {
    Ok(crate::tasks::GateStore::new(
        get_task_store(state)?.pool().clone(),
    ))
}

/// `GET /tasks/{number}/gates` — what this task is waiting on outside the graph.
#[utoipa::path(
    get,
    path = "/tasks/{number}/gates",
    params(("number" = i64, Path, description = "Task number")),
    responses(
        (status = 200, body = TaskGatesResponse),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn list_task_gates(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskGatesResponse>, StatusCode> {
    let gates = gate_store(&state)?
        .list_for_task(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to list task gates");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(TaskGatesResponse { gates }))
}

/// `POST /tasks/{number}/gates` — hold this task until something outside says go.
///
/// The config is validated here rather than at first poll. A malformed gate
/// accepted now would error once a minute forever with nobody reading the log,
/// so the rejection has to land while a person is still looking at the form.
#[utoipa::path(
    post,
    path = "/tasks/{number}/gates",
    params(("number" = i64, Path, description = "Task number")),
    request_body = CreateGateRequest,
    responses(
        (status = 200, body = TaskGatesResponse),
        (status = 404, description = "Task not found"),
        (status = 422, description = "Gate config is not usable"),
    ),
    tag = "tasks",
)]
pub(super) async fn create_task_gate(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<CreateGateRequest>,
) -> Result<Json<TaskGatesResponse>, (StatusCode, String)> {
    let store =
        get_task_store(&state).map_err(|code| (code, "task store unavailable".to_string()))?;
    let gates = gate_store(&state).map_err(|code| (code, "task store unavailable".to_string()))?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to read task for gate");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to read task".to_string(),
            )
        })?
        .ok_or((StatusCode::NOT_FOUND, format!("no task #{number}")))?;

    let kind = crate::tasks::GateKind::parse(&request.kind).ok_or((
        StatusCode::UNPROCESSABLE_ENTITY,
        crate::tasks::GateConfigError::UnknownKind {
            value: request.kind.clone(),
        }
        .to_string(),
    ))?;

    let interval = request
        .poll_interval_secs
        .unwrap_or(crate::tasks::MIN_POLL_INTERVAL_SECS.max(60));

    crate::tasks::validate_config(kind, &request.config, interval)
        .map_err(|error| (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;

    gates
        .create(
            number,
            kind,
            &request.config,
            request.label.as_deref(),
            interval,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to create task gate");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to create gate".to_string(),
            )
        })?;

    // A gate governs *promotion*, so one added to a task already sitting in
    // `ready` would hold nothing — the sweep has finished with it and the next
    // claim would run it regardless. Park it so the same sweep that honours
    // every other gate honours this one too.
    //
    // `Dependency` is the right kind: it rests in the backlog rather than the
    // blocked column a human triages, and it is one of the signals that marks a
    // task as parked *by the scheduler*, so the scheduler may release it once
    // the gate opens. Anything sticky would need a person to undo.
    if task.status == crate::tasks::TaskStatus::Ready {
        let reason = format!(
            "waiting on {}",
            request.label.as_deref().unwrap_or("an external gate")
        );
        if let Err(error) = store
            .block_task(number, crate::tasks::BlockKind::Dependency, &reason)
            .await
        {
            tracing::warn!(%error, task_number = number, "failed to park a task behind its new gate");
        }
    }

    let gates = gates.list_for_task(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list task gates");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to list gates".to_string(),
        )
    })?;
    Ok(Json(TaskGatesResponse { gates }))
}

/// `DELETE /tasks/{number}/gates/{gate_id}` — stop waiting on it.
///
/// Removing a gate is the escape hatch for one that has failed or cannot be
/// reached: the task becomes promotable again on the next sweep. It is a
/// deliberate act by a person, which is exactly what a `failed` gate is asking
/// for.
#[utoipa::path(
    delete,
    path = "/tasks/{number}/gates/{gate_id}",
    params(
        ("number" = i64, Path, description = "Task number"),
        ("gate_id" = String, Path, description = "Gate id"),
    ),
    responses(
        (status = 200, body = TaskGatesResponse),
        (status = 404, description = "No such gate"),
    ),
    tag = "tasks",
)]
pub(super) async fn delete_task_gate(
    State(state): State<Arc<ApiState>>,
    Path((number, gate_id)): Path<(i64, String)>,
) -> Result<Json<TaskGatesResponse>, StatusCode> {
    let gates = gate_store(&state)?;
    if !gates.delete(&gate_id).await.map_err(|error| {
        tracing::warn!(%error, %gate_id, "failed to delete task gate");
        StatusCode::INTERNAL_SERVER_ERROR
    })? {
        return Err(StatusCode::NOT_FOUND);
    }

    let gates = gates.list_for_task(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to list task gates");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(TaskGatesResponse { gates }))
}

/// `GET /tasks/{number}/graph` — every task connected to this one, and the
/// edges between them.
///
/// Drawn from real dependency edges rather than from a workflow template,
/// which is what makes it answer the question in the three cases that matter:
/// the template has since been deleted, the step fanned out so one step is now
/// many tasks, or there was never a template at all because the graph was built
/// by hand or by a worker filing cards.
#[utoipa::path(
    get,
    path = "/tasks/{number}/graph",
    params(("number" = i64, Path, description = "Task number to centre on")),
    responses(
        (status = 200, body = crate::tasks::TaskGraph),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn get_task_graph(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<crate::tasks::TaskGraph>, StatusCode> {
    let store = get_task_store(&state)?;

    // Checked before the walk so a missing task is a 404 rather than a graph of
    // one task that does not exist.
    if store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to read task for graph");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }

    let graph = store
        .graph_component(number, crate::tasks::MAX_GRAPH_TASKS)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to walk task graph");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(graph))
}
