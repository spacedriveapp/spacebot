//! Workflow template CRUD and launching.
//!
//! The template endpoints are ordinary storage plumbing. The one that matters
//! is `POST /workflows/{id}/run`, and its whole job is to turn a `LaunchError`
//! into something actionable: every variant already names the offending step
//! and key, so the text goes in the response body rather than being collapsed
//! into a bare status code. A refused launch is nearly always a typo in the
//! template, and "422" alone sends someone reading rows.

use super::state::ApiState;
use crate::workflows::{BindingSource, LaunchError, StepBinding, WorkflowStep};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveWorkflowRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    /// JSON Schema for the input a whole run is launched with.
    #[serde(default)]
    input_schema: Option<serde_json::Value>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveStepRequest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    /// Omit to run the step as whoever launched the run.
    #[serde(default)]
    assigned_agent_id: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    input_schema: Option<serde_json::Value>,
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
    /// Extra instructions appended to the worker prompt when this step runs.
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    /// Display order only — execution order comes from the edges.
    #[serde(default)]
    position: Option<i64>,
    /// Set to make this a fan-out: one task per item that step produced,
    /// instead of one task.
    ///
    /// Each branch receives its own item as the input key **`item`**. That is
    /// the name to declare in this step's `input_schema` and to bind against —
    /// there is no way to rename it, and a step that iterates without knowing
    /// the key would declare a contract it never receives.
    #[serde(default)]
    for_each_step_key: Option<String>,
    /// RFC 6901 pointer into that step's outputs. Must select an array.
    #[serde(default)]
    for_each_pointer: Option<String>,
    /// Pointer *within each item* naming its branch. Omit to key by index.
    #[serde(default)]
    for_each_key: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct StepEdgeRequest {
    parent_step_key: String,
    child_step_key: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveBindingRequest {
    /// `step` | `literal` | `run_input`
    source: String,
    /// Required when `source` is `step`.
    #[serde(default)]
    source_step_key: Option<String>,
    /// RFC 6901 JSON Pointer. Empty selects the whole document.
    #[serde(default)]
    source_pointer: Option<String>,
    /// Required when `source` is `literal`.
    #[serde(default)]
    literal_value: Option<serde_json::Value>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct LaunchRequest {
    /// The single payload the whole pipeline is driven from.
    #[serde(default)]
    inputs: serde_json::Value,
    /// Agent credited with the launch, and the default assignee for any step
    /// that does not name one.
    launched_by: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkflowListResponse {
    workflows: Vec<crate::workflows::Workflow>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkflowEdge {
    parent_step_key: String,
    child_step_key: String,
}

/// A template and everything that references it.
///
/// One response rather than four endpoints because the editor cannot render
/// anything useful without all of it — a step list with no edges is not a
/// pipeline — and four round trips can interleave with a save.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkflowDetailResponse {
    workflow: crate::workflows::Workflow,
    steps: Vec<WorkflowStep>,
    edges: Vec<WorkflowEdge>,
    bindings: Vec<StepBinding>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkflowResponse {
    workflow: crate::workflows::Workflow,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct LaunchResponse {
    run: crate::workflows::WorkflowRun,
    /// Emitted task numbers, keyed by the step they came from.
    task_numbers: std::collections::HashMap<String, i64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RunListResponse {
    runs: Vec<crate::workflows::WorkflowRun>,
}

/// A run and the tasks it produced.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct RunDetailResponse {
    run: crate::workflows::WorkflowRun,
    tasks: Vec<crate::tasks::Task>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WorkflowActionResponse {
    success: bool,
    message: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_store(state: &ApiState) -> Result<Arc<crate::workflows::WorkflowStore>, StatusCode> {
    state
        .workflow_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn get_task_store(state: &ApiState) -> Result<Arc<crate::tasks::TaskStore>, StatusCode> {
    state
        .task_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {
    tracing::error!(%error, "workflow request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "workflow storage error".to_string(),
    )
}

/// Map a refused launch onto a status code, keeping the message.
///
/// The split is between "this template is wrong" and "this input is wrong",
/// because they are fixed by different people in different places. A cycle is
/// `409`: the template contradicts itself and no input would make it launch.
/// Everything else a caller can act on is `422`.
fn launch_status(error: &LaunchError) -> StatusCode {
    match error {
        LaunchError::UnknownWorkflow { .. } => StatusCode::NOT_FOUND,
        LaunchError::Cycle { .. } => StatusCode::CONFLICT,
        LaunchError::NoSteps { .. }
        | LaunchError::UnknownStepReference { .. }
        | LaunchError::UnknownEdgeReference { .. }
        | LaunchError::InvalidInput { .. }
        | LaunchError::UnboundRequiredInput { .. }
        | LaunchError::MissingRunInput { .. }
        | LaunchError::UnknownForEachStep { .. }
        | LaunchError::ForEachNotWaiting { .. }
        | LaunchError::FanInNotFanOut { .. }
        | LaunchError::FanInNotWaiting { .. }
        | LaunchError::StepBindingOnFanOut { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        LaunchError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// ---------------------------------------------------------------------------
// Template handlers
// ---------------------------------------------------------------------------

/// `GET /workflows` — list templates.
#[utoipa::path(
    get,
    path = "/workflows",
    responses(
        (status = 200, body = WorkflowListResponse),
        (status = 503, description = "Workflow store not initialized"),
    ),
    tag = "workflows",
)]
pub(super) async fn list_workflows(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<WorkflowListResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let workflows = store.list_workflows().await.map_err(internal)?;
    Ok(Json(WorkflowListResponse { workflows }))
}

/// `POST /workflows` — create a template.
#[utoipa::path(
    post,
    path = "/workflows",
    request_body = SaveWorkflowRequest,
    responses(
        (status = 200, body = WorkflowResponse),
        (status = 409, description = "A workflow with that name already exists"),
    ),
    tag = "workflows",
)]
pub(super) async fn create_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SaveWorkflowRequest>,
) -> Result<Json<WorkflowResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    // The name is UNIQUE, and a duplicate is a user mistake rather than a
    // server fault. Checked up front because the constraint violation reaches
    // us wrapped in context and its Display is just "failed to create
    // workflow" — the useful text is in the cause chain, which is exactly the
    // sort of string-matching that breaks silently later.
    let taken = store
        .list_workflows()
        .await
        .map_err(internal)?
        .into_iter()
        .any(|existing| existing.name == request.name);
    if taken {
        return Err((
            StatusCode::CONFLICT,
            format!("a workflow named `{}` already exists", request.name),
        ));
    }

    let workflow = store
        .create_workflow(
            &request.name,
            request.description.as_deref(),
            request.input_schema.as_ref(),
        )
        .await
        .map_err(internal)?;
    Ok(Json(WorkflowResponse { workflow }))
}

/// `GET /workflows/{id}` — a template with its steps, edges, and bindings.
#[utoipa::path(
    get,
    path = "/workflows/{id}",
    params(("id" = String, Path, description = "Workflow id")),
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such workflow"),
    ),
    tag = "workflows",
)]
pub(super) async fn get_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let workflow = store
        .get_workflow(&id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("no workflow {id}")))?;

    let steps = store.list_steps(&id).await.map_err(internal)?;
    let edges = store.list_edges(&id).await.map_err(internal)?;
    let bindings = store.list_bindings(&id).await.map_err(internal)?;

    Ok(Json(WorkflowDetailResponse {
        workflow,
        steps,
        edges: edges
            .into_iter()
            .map(|(parent_step_key, child_step_key)| WorkflowEdge {
                parent_step_key,
                child_step_key,
            })
            .collect(),
        bindings,
    }))
}

/// `PUT /workflows/{id}` — rename or re-describe a template.
#[utoipa::path(
    put,
    path = "/workflows/{id}",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = SaveWorkflowRequest,
    responses(
        (status = 200, body = WorkflowResponse),
        (status = 404, description = "No such workflow"),
    ),
    tag = "workflows",
)]
pub(super) async fn update_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SaveWorkflowRequest>,
) -> Result<Json<WorkflowResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let workflow = store
        .update_workflow(
            &id,
            &request.name,
            request.description.as_deref(),
            request.input_schema.as_ref(),
        )
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("no workflow {id}")))?;
    Ok(Json(WorkflowResponse { workflow }))
}

/// `DELETE /workflows/{id}` — delete a template.
///
/// Runs already launched from it keep running: tasks carry the run id as plain
/// text, not a foreign key, so deleting the recipe never deletes the history of
/// work that was done from it.
#[utoipa::path(
    delete,
    path = "/workflows/{id}",
    params(("id" = String, Path, description = "Workflow id")),
    responses(
        (status = 200, body = WorkflowActionResponse),
        (status = 404, description = "No such workflow"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<WorkflowActionResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if !store.delete_workflow(&id).await.map_err(internal)? {
        return Err((StatusCode::NOT_FOUND, format!("no workflow {id}")));
    }
    Ok(Json(WorkflowActionResponse {
        success: true,
        message: "workflow deleted; tasks from past runs are untouched".to_string(),
    }))
}

// ---------------------------------------------------------------------------
// Step handlers
// ---------------------------------------------------------------------------

/// `PUT /workflows/{id}/steps/{step_key}` — add or replace a step.
#[utoipa::path(
    put,
    path = "/workflows/{id}/steps/{step_key}",
    params(
        ("id" = String, Path, description = "Workflow id"),
        ("step_key" = String, Path, description = "Stable step name"),
    ),
    request_body = SaveStepRequest,
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such workflow"),
        (status = 422, description = "Unknown priority"),
    ),
    tag = "workflows",
)]
pub(super) async fn put_step(
    State(state): State<Arc<ApiState>>,
    Path((id, step_key)): Path<(String, String)>,
    Json(request): Json<SaveStepRequest>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if store.get_workflow(&id).await.map_err(internal)?.is_none() {
        return Err((StatusCode::NOT_FOUND, format!("no workflow {id}")));
    }

    // Position is display order, and a caller that omits it means "wherever".
    // Defaulting to zero made every such step collide at the top, which reads
    // as the list ignoring the order it was built in. An edit keeps whatever
    // the step already had; only a genuinely new step takes the next slot.
    let existing = store.list_steps(&id).await.map_err(internal)?;
    let position = match request.position {
        Some(explicit) => explicit,
        None => match existing.iter().find(|step| step.step_key == step_key) {
            Some(current) => current.position,
            None => store.next_step_position(&id).await.map_err(internal)?,
        },
    };

    let priority = match request.priority.as_deref() {
        None => crate::tasks::TaskPriority::Medium,
        Some(value) => crate::tasks::TaskPriority::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{value}` is not a priority"),
        ))?,
    };

    // Checked here rather than left to launch, on the same argument as edges: a
    // step key that does not exist is a typo the author can still see on screen.
    if let Some(source) = request.for_each_step_key.as_deref() {
        if source == step_key {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("step `{step_key}` cannot iterate over its own output"),
            ));
        }
        if !existing.iter().any(|step| step.step_key == source) {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("no step `{source}` in this workflow"),
            ));
        }
    }

    store
        .put_step(&WorkflowStep {
            workflow_id: id.clone(),
            step_key,
            title: request.title,
            description: request.description,
            assigned_agent_id: request.assigned_agent_id,
            priority,
            input_schema: request.input_schema,
            output_schema: request.output_schema,
            system_prompt: request.system_prompt,
            repo_id: request.repo_id,
            position,
            for_each_step_key: request.for_each_step_key,
            for_each_pointer: request.for_each_pointer,
            for_each_key: request.for_each_key,
        })
        .await
        .map_err(internal)?;

    get_workflow(State(state), Path(id)).await
}

/// `DELETE /workflows/{id}/steps/{step_key}` — remove a step and its references.
#[utoipa::path(
    delete,
    path = "/workflows/{id}/steps/{step_key}",
    params(
        ("id" = String, Path, description = "Workflow id"),
        ("step_key" = String, Path, description = "Stable step name"),
    ),
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such step"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_step(
    State(state): State<Arc<ApiState>>,
    Path((id, step_key)): Path<(String, String)>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if !store.delete_step(&id, &step_key).await.map_err(internal)? {
        return Err((StatusCode::NOT_FOUND, format!("no step `{step_key}`")));
    }
    get_workflow(State(state), Path(id)).await
}

/// `POST /workflows/{id}/edges` — make one step wait for another.
#[utoipa::path(
    post,
    path = "/workflows/{id}/edges",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = StepEdgeRequest,
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 422, description = "Self-loop, or a step that does not exist"),
    ),
    tag = "workflows",
)]
pub(super) async fn add_edge(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<StepEdgeRequest>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    if request.parent_step_key == request.child_step_key {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("step `{}` cannot wait for itself", request.parent_step_key),
        ));
    }

    // Checked here rather than left to launch. An edge naming a step that does
    // not exist is a typo the author can still see on screen; discovering it at
    // launch means the pipeline was saved, looked fine, and refused to start.
    let steps = store.list_steps(&id).await.map_err(internal)?;
    for key in [&request.parent_step_key, &request.child_step_key] {
        if !steps.iter().any(|step| &step.step_key == key) {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("no step `{key}` in this workflow"),
            ));
        }
    }

    // Refused here, not only at launch. A template that saves a cycle and
    // refuses to run is a trap: the author gets a success and finds out much
    // later, possibly from somebody else.
    if let Some(cycle) = store
        .cycle_from_edge(&id, &request.parent_step_key, &request.child_step_key)
        .await
        .map_err(internal)?
    {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "that edge would close a loop: {} -> {}",
                cycle.join(" -> "),
                request.child_step_key
            ),
        ));
    }

    store
        .link_steps(&id, &request.parent_step_key, &request.child_step_key)
        .await
        .map_err(internal)?;

    get_workflow(State(state), Path(id)).await
}

/// `DELETE /workflows/{id}/edges` — drop a wait.
#[utoipa::path(
    delete,
    path = "/workflows/{id}/edges",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = StepEdgeRequest,
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such edge"),
    ),
    tag = "workflows",
)]
pub(super) async fn remove_edge(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<StepEdgeRequest>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if !store
        .unlink_steps(&id, &request.parent_step_key, &request.child_step_key)
        .await
        .map_err(internal)?
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!(
                "no edge {} -> {}",
                request.parent_step_key, request.child_step_key
            ),
        ));
    }
    get_workflow(State(state), Path(id)).await
}

/// `PUT /workflows/{id}/steps/{step_key}/bindings/{input_key}` — declare where
/// one of a step's inputs comes from.
#[utoipa::path(
    put,
    path = "/workflows/{id}/steps/{step_key}/bindings/{input_key}",
    params(
        ("id" = String, Path, description = "Workflow id"),
        ("step_key" = String, Path, description = "Step being bound"),
        ("input_key" = String, Path, description = "Name of the input"),
    ),
    request_body = SaveBindingRequest,
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 422, description = "Unknown source, or a source that does not match its kind"),
    ),
    tag = "workflows",
)]
pub(super) async fn put_binding(
    State(state): State<Arc<ApiState>>,
    Path((id, step_key, input_key)): Path<(String, String, String)>,
    Json(request): Json<SaveBindingRequest>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    let source = BindingSource::parse(&request.source).ok_or((
        StatusCode::UNPROCESSABLE_ENTITY,
        format!(
            "`{}` is not a binding source — use step, literal, run_input, or fan_in",
            request.source
        ),
    ))?;

    let steps = store.list_steps(&id).await.map_err(internal)?;
    if !steps.iter().any(|step| step.step_key == step_key) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("no step `{step_key}` in this workflow"),
        ));
    }

    // A binding whose kind and payload disagree resolves to nothing at launch
    // and reads as a mystery. Rejecting it here names the field to fix.
    match source {
        BindingSource::Step => {
            let target = request.source_step_key.as_deref().unwrap_or("");
            if target.is_empty() {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "a step binding needs source_step_key".to_string(),
                ));
            }
            if !steps.iter().any(|step| step.step_key == target) {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("no step `{target}` in this workflow"),
                ));
            }
            if target == step_key {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("step `{step_key}` cannot read its own output"),
                ));
            }
        }
        BindingSource::FanIn => {
            let target = request.source_step_key.as_deref().unwrap_or("");
            let Some(fan_out) = steps.iter().find(|step| step.step_key == target) else {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("no step `{target}` in this workflow"),
                ));
            };
            // A fan-in over an ordinary step collects exactly one thing, keyed
            // by nothing anybody chose. Refused rather than allowed to resolve
            // to plausible-looking nonsense.
            if fan_out.for_each_step_key.is_none() {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("step `{target}` is not a fan-out, so it has no branches to collect"),
                ));
            }
        }
        BindingSource::Literal => {
            if request.literal_value.is_none() {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "a literal binding needs literal_value".to_string(),
                ));
            }
        }
        BindingSource::RunInput => {}
    }

    store
        .put_binding(&StepBinding {
            workflow_id: id.clone(),
            step_key,
            input_key,
            source,
            source_step_key: request.source_step_key,
            source_pointer: request.source_pointer,
            literal_value: request.literal_value,
        })
        .await
        .map_err(internal)?;

    get_workflow(State(state), Path(id)).await
}

/// `DELETE /workflows/{id}/steps/{step_key}/bindings/{input_key}`
#[utoipa::path(
    delete,
    path = "/workflows/{id}/steps/{step_key}/bindings/{input_key}",
    params(
        ("id" = String, Path, description = "Workflow id"),
        ("step_key" = String, Path, description = "Step being bound"),
        ("input_key" = String, Path, description = "Name of the input"),
    ),
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such binding"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_binding(
    State(state): State<Arc<ApiState>>,
    Path((id, step_key, input_key)): Path<(String, String, String)>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if !store
        .delete_binding(&id, &step_key, &input_key)
        .await
        .map_err(internal)?
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no binding `{input_key}` on step `{step_key}`"),
        ));
    }
    get_workflow(State(state), Path(id)).await
}

// ---------------------------------------------------------------------------
// Run handlers
// ---------------------------------------------------------------------------

/// `POST /workflows/{id}/run` — launch a pipeline from one input.
#[utoipa::path(
    post,
    path = "/workflows/{id}/run",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = LaunchRequest,
    responses(
        (status = 200, body = LaunchResponse),
        (status = 404, description = "No such workflow"),
        (status = 409, description = "The steps form a cycle"),
        (status = 422, description = "Bad input, or a reference that does not resolve"),
    ),
    tag = "workflows",
)]
pub(super) async fn launch_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<LaunchRequest>,
) -> Result<Json<LaunchResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let tasks = get_task_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    let launched = store
        .launch(&tasks, &id, &request.inputs, &request.launched_by)
        .await
        .map_err(|error| (launch_status(&error), error.to_string()))?;

    // The emitted tasks all start in backlog, so nothing runs until the sweep
    // looks at them. Running it now is what makes "launch" feel like a launch
    // rather than a filing — without it the entry step waits for the next tick
    // for no reason a person could see.
    match tasks.recompute_ready(&request.launched_by).await {
        Ok(sweep) => tracing::info!(
            run_id = %launched.run.id,
            promoted = ?sweep.promoted,
            "launched workflow"
        ),
        Err(error) => tracing::warn!(
            %error,
            run_id = %launched.run.id,
            "workflow launched but the ready sweep failed; the next tick will pick it up"
        ),
    }

    for number in launched.task_numbers.values() {
        state
            .event_tx
            .send(super::state::ApiEvent::TaskUpdated {
                agent_id: request.launched_by.clone(),
                task_number: *number,
                status: crate::tasks::TaskStatus::Backlog.as_str().to_string(),
                action: "created".to_string(),
            })
            .ok();
    }

    Ok(Json(LaunchResponse {
        run: launched.run,
        task_numbers: launched.task_numbers,
    }))
}

/// `GET /workflows/{id}/runs` — launches of one template, newest first.
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, body = RunListResponse)),
    tag = "workflows",
)]
pub(super) async fn list_runs(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<RunListResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let runs = store.list_runs(&id).await.map_err(internal)?;
    Ok(Json(RunListResponse { runs }))
}

/// `GET /workflow-runs/{run_id}` — one run and the tasks it produced.
///
/// Not nested under the workflow: a run outlives the template it came from, so
/// requiring the template's id to look one up would make deleted templates take
/// their history with them.
#[utoipa::path(
    get,
    path = "/workflow-runs/{run_id}",
    params(("run_id" = String, Path, description = "Workflow run id")),
    responses(
        (status = 200, body = RunDetailResponse),
        (status = 404, description = "No such run"),
    ),
    tag = "workflows",
)]
pub(super) async fn get_run(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<RunDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let task_store = get_task_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    let run = store
        .get_run(&run_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("no run {run_id}")))?;
    let tasks = task_store
        .list_by_workflow_run(&run_id)
        .await
        .map_err(internal)?;

    Ok(Json(RunDetailResponse { run, tasks }))
}
