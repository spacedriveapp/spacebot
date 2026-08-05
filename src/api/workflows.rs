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
    /// Say what the step needs instead of who should do it.
    ///
    /// Set, and the emitted task is unassigned: any agent declaring all of
    /// these claims it. Mutually exclusive with `assigned_agent_id`, and a
    /// requirement no agent in the fleet can satisfy is refused at launch.
    #[serde(default)]
    required_capabilities: Option<Vec<String>>,
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
    /// Set to put this step in a loop body. Every step sharing the name is one
    /// body, and the whole body runs again until it converges or runs out.
    #[serde(default)]
    loop_group: Option<String>,
    /// How many passes the body may run. Omit for 3.
    ///
    /// Only read on the body's **exit step** — the one step with nothing after
    /// it inside the body. Set anywhere else, launch refuses rather than
    /// leaving a number that does nothing.
    #[serde(default)]
    loop_max_iterations: Option<i64>,
    /// The exit predicate, in the same shape a `task_output` gate takes:
    /// `{"pointer": "/tests/passed", "equals": true}`. Required on the exit
    /// step of a loop body.
    #[serde(default)]
    loop_until: Option<serde_json::Value>,
    /// `agent` (default) or `command`.
    ///
    /// A command step runs a process instead of a model. Its outputs are
    /// `{"exit_code", "stdout", "stderr", "duration_ms"}`, which bindings,
    /// gates, `loop_until` and conditions read with the pointers they already
    /// use.
    #[serde(default)]
    kind: Option<String>,
    /// The command line for a command step. Refused on an agent step, where
    /// nothing would run it.
    #[serde(default)]
    command: Option<String>,
    /// Hard timeout for a command step, in seconds. Required on one.
    #[serde(default)]
    command_timeout_secs: Option<i64>,
    /// The exit code that means success, for steps where non-zero really is a
    /// failure. Omit — the usual case — to treat the exit code as data: a
    /// command that ran and reported a problem is a step that succeeded.
    #[serde(default)]
    expect_exit_code: Option<i64>,
    /// `inherit` (default), `per_run`, or `per_branch`.
    ///
    /// `per_branch` requires a fan-out and is refused at launch otherwise.
    #[serde(default)]
    worktree_mode: Option<String>,
    /// What a provisioned worktree forks from — a branch, tag or sha. Omit for
    /// the repo's current HEAD.
    #[serde(default)]
    worktree_base_ref: Option<String>,
    /// The question a decision step asks, as the person answering reads it.
    /// Required on a decision step; refused on every other kind.
    #[serde(default)]
    decision_question: Option<String>,
    /// Who may answer. Omit for anyone.
    ///
    /// **Advisory in v1.** It is recorded on the task and shown alongside the
    /// answerer, so an audit can compare them — but it is not enforced, because
    /// this layer has no authenticated caller identity to enforce it against and
    /// checking a self-declared name would be enforcement in name only.
    #[serde(default)]
    decision_asked_of: Option<Vec<String>>,
    /// `wait` (default), `default`, or `fail`.
    ///
    /// `wait` parks until answered — the run is legitimately blocked and is not
    /// reported as stuck. `default` applies `decision_default_answer` after
    /// `decision_timeout_secs`, recorded *as* a default. `fail` fails the step
    /// and lets the failure path route it.
    #[serde(default)]
    decision_timeout_action: Option<String>,
    /// How long to wait, in seconds, from the moment the decision is asked —
    /// not from launch. Required by `default` and `fail`, refused by `wait`.
    #[serde(default)]
    decision_timeout_secs: Option<i64>,
    /// The answer that applies on a `default` timeout. Validated against this
    /// step's own `output_schema` at launch.
    #[serde(default)]
    decision_default_answer: Option<serde_json::Value>,
    /// `each_pass` (default) or `once`, for a decision inside a loop body.
    ///
    /// `each_pass` re-asks on every pass, because pass 2 exists precisely
    /// because the artefact changed and reusing pass 1's answer would credit a
    /// person with approving work they never saw. `once` carries the first
    /// answer forward, recorded as `carried` with the original answerer and
    /// timestamp, for gates that are a property of the run rather than the pass.
    #[serde(default)]
    decision_ask: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct StepEdgeRequest {
    parent_step_key: String,
    child_step_key: String,
    /// `normal` (default) or `on_exhausted`.
    ///
    /// An `on_exhausted` edge is followed only when the loop ending at the
    /// parent runs out of attempts. Converging and giving up are opposite
    /// results, so they get different edges rather than one edge meaning both.
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveBindingRequest {
    /// `step` | `literal` | `run_input` | `fan_in` | `previous_iteration`
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

/// A condition on a step: the predicate, and what a false answer means.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveStepGateRequest {
    /// `http` | `task_output`
    kind: String,
    /// Required when `kind` is `task_output`: whose output to read, by name.
    /// Becomes a task number at launch.
    #[serde(default)]
    source_step_key: Option<String>,
    /// The predicate — an RFC 6901 `pointer` plus `equals` or `any_of`, in the
    /// same shape a task gate takes. For `task_output`, `task_number` is filled
    /// in by the launch and must not be set here.
    config: serde_json::Value,
    /// What the board should call this. "needs legal review" beats a pointer.
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    poll_interval_secs: Option<i64>,
    /// `wait` | `route`, or omit to derive it.
    ///
    /// `wait` holds the step until the condition becomes true — a gate in the
    /// original sense. `route` says a false answer means the step does not
    /// apply, and settles it as skipped.
    ///
    /// Omitting it is right nearly always: a `task_output` condition whose
    /// source has settled routes, and everything else waits. That is a fact
    /// about whether the answer can still change, not a guess. Set it for what
    /// the derivation cannot see — an http endpoint whose answer really is
    /// final, or a condition that should hold the pipeline rather than skip
    /// past it.
    #[serde(default)]
    disposition: Option<String>,
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
    /// `normal` or `on_exhausted`. An editor that drew both alike would draw a
    /// pipeline that is not the one that runs.
    kind: String,
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
    /// Conditions on steps. Part of the same response for the same reason the
    /// edges are: a canvas that draws a step without its condition draws a
    /// pipeline that is not the one that runs.
    gates: Vec<crate::workflows::StepGate>,
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

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CancelRunRequest {
    /// Who stopped it. Recorded on the run and on every card it settles, so
    /// "why did this stop" is answerable from the row rather than from memory.
    cancelled_by: String,
}

/// A run that was stopped, and what that did to its tasks.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct CancelRunResponse {
    run: crate::workflows::WorkflowRun,
    /// Unstarted tasks settled as `skipped`.
    settled: i64,
    /// Tasks left in flight. They are not killed: whatever they had already
    /// done would be lost, so they finish or are reaped normally.
    left_running: i64,
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
        | LaunchError::FanOutOnLoopBody { .. }
        | LaunchError::FanInNotFanOut { .. }
        | LaunchError::FanInNotWaiting { .. }
        | LaunchError::FanInWithPointer { .. }
        | LaunchError::StepBindingOnFanOut { .. }
        | LaunchError::StepBindingNotWaiting { .. }
        | LaunchError::LoopBodyNotSingleExit { .. }
        | LaunchError::LoopSettingOffExitStep { .. }
        | LaunchError::LoopSettingsWithoutLoop { .. }
        | LaunchError::LoopWithoutExitCondition { .. }
        | LaunchError::LoopExitConditionInvalid { .. }
        | LaunchError::LoopMaxIterationsOutOfRange { .. }
        | LaunchError::LoopStepIsAlsoFanOut { .. }
        | LaunchError::OnExhaustedNotFromLoop { .. }
        | LaunchError::StepAwaitsTwoLoops { .. }
        | LaunchError::PreviousIterationOutsideLoop { .. }
        | LaunchError::PreviousIterationOutsideBody { .. }
        | LaunchError::LoopEntryAmbiguous { .. }
        | LaunchError::UnknownGateStep { .. }
        | LaunchError::UnknownGateSource { .. }
        | LaunchError::GateReadsItsOwnStep { .. }
        | LaunchError::GateOnFanOut { .. }
        | LaunchError::GateOnLoopBody { .. }
        | LaunchError::GateConfigInvalid { .. }
        // A template bigger than one run may hold. `422` with the rest: the
        // template is what has to change, and the message says by how much.
        | LaunchError::RunTaskCeiling { .. }
        // Command and worktree declarations are template facts too, and every
        // one of them names the field to change.
        | LaunchError::CommandStepWithoutCommand { .. }
        | LaunchError::AgentStepWithCommand { .. }
        | LaunchError::CommandStepWithoutTimeout { .. }
        | LaunchError::CommandTimeoutOutOfRange { .. }
        | LaunchError::CommandStepWithoutBinding { .. }
        | LaunchError::PerBranchWithoutFanOut { .. }
        // So are decision declarations. Every one of these names the field to
        // change, including the default answer — checked here rather than when
        // the timeout fires, which would be hours later and unattended.
        | LaunchError::DecisionStepWithoutQuestion { .. }
        | LaunchError::DecisionFieldsOnNonDecision { .. }
        | LaunchError::DecisionStepWithoutSchema { .. }
        | LaunchError::DecisionTimeoutWithoutDeadline { .. }
        | LaunchError::DecisionDeadlineWithoutTimeout { .. }
        | LaunchError::DecisionTimeoutOutOfRange { .. }
        | LaunchError::DecisionDefaultWithoutAnswer { .. }
        | LaunchError::DecisionAnswerWithoutDefault { .. }
        | LaunchError::DecisionDefaultRejected { .. }
        | LaunchError::DecisionStepRequiresCapabilities { .. }
        | LaunchError::DecisionStepWithWorktree { .. }
        // A step that says both who and what, or neither usefully, is a
        // template fact and the message names the field.
        | LaunchError::StepAssignedAndRequires { .. }
        | LaunchError::StepRequiresNothing { .. }
        | LaunchError::PooledStepGrowsTheGraph { .. }
        | LaunchError::WorktreeWithoutRepo { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        // The template may be perfectly good and the *fleet* is what does not
        // match it — an agent was deleted, or has not declared the label yet.
        // `409` with the worktree case for the same reason: nothing about the
        // steps has to change, something outside them does.
        LaunchError::NoAgentDeclaresCapability { .. }
        | LaunchError::NoAgentCoversCapabilities { .. } => StatusCode::CONFLICT,
        // Not the template's fault and not the input's: the repo, its base ref,
        // or the disk would not cooperate. A person has to go and look at the
        // checkout, which is a different job from editing a step.
        LaunchError::WorktreeUnavailable { .. } => StatusCode::CONFLICT,
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
    let edges = store.list_step_edges(&id).await.map_err(internal)?;
    let bindings = store.list_bindings(&id).await.map_err(internal)?;
    let gates = store.list_gates(&id).await.map_err(internal)?;

    Ok(Json(WorkflowDetailResponse {
        workflow,
        steps,
        edges: edges
            .into_iter()
            .map(|edge| WorkflowEdge {
                parent_step_key: edge.parent_step_key,
                child_step_key: edge.child_step_key,
                kind: edge.kind.as_str().to_string(),
            })
            .collect(),
        bindings,
        gates,
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
        (status = 409, description = "A workflow with that name already exists"),
    ),
    tag = "workflows",
)]
pub(super) async fn update_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SaveWorkflowRequest>,
) -> Result<Json<WorkflowResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    // The store types the duplicate, so the rename gets the same answer the
    // create does: a user mistake, not a server fault.
    let workflow = store
        .update_workflow(
            &id,
            &request.name,
            request.description.as_deref(),
            request.input_schema.as_ref(),
        )
        .await
        .map_err(|error| match error {
            crate::workflows::WorkflowSaveError::DuplicateName { .. } => {
                (StatusCode::CONFLICT, error.to_string())
            }
            other => internal(other),
        })?
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

    let kind = match request.kind.as_deref() {
        None => crate::workflows::StepKind::Agent,
        Some(value) => crate::workflows::StepKind::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{value}` is not a step kind — use `agent`, `command` or `decision`"),
        ))?,
    };
    let decision_timeout_action = match request.decision_timeout_action.as_deref() {
        None => crate::tasks::DecisionTimeoutAction::default(),
        Some(value) => crate::tasks::DecisionTimeoutAction::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{value}` is not a decision timeout policy — use `wait`, `default` or `fail`"),
        ))?,
    };
    let decision_ask = match request.decision_ask.as_deref() {
        None => crate::tasks::DecisionAsk::default(),
        Some(value) => crate::tasks::DecisionAsk::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{value}` is not a decision repeat policy — use `each_pass` or `once`"),
        ))?,
    };
    let worktree_mode = match request.worktree_mode.as_deref() {
        None => crate::workflows::WorktreeMode::Inherit,
        Some(value) => crate::workflows::WorktreeMode::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{value}` is not a worktree mode — use `inherit`, `per_run` or `per_branch`"),
        ))?,
    };

    store
        .put_step(&WorkflowStep {
            workflow_id: id.clone(),
            step_key,
            title: request.title,
            description: request.description,
            assigned_agent_id: request.assigned_agent_id,
            required_capabilities: request.required_capabilities,
            priority,
            input_schema: request.input_schema,
            output_schema: request.output_schema,
            system_prompt: request.system_prompt,
            repo_id: request.repo_id,
            position,
            for_each_step_key: request.for_each_step_key,
            for_each_pointer: request.for_each_pointer,
            for_each_key: request.for_each_key,
            loop_group: request.loop_group,
            loop_max_iterations: request.loop_max_iterations,
            loop_until: request.loop_until,
            kind,
            command: request.command,
            command_timeout_secs: request.command_timeout_secs,
            expect_exit_code: request.expect_exit_code,
            worktree_mode,
            worktree_base_ref: request.worktree_base_ref,
            decision_question: request.decision_question,
            decision_asked_of: request.decision_asked_of,
            decision_timeout_action,
            decision_timeout_secs: request.decision_timeout_secs,
            decision_default_answer: request.decision_default_answer,
            decision_ask,
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

    let kind = match request.kind.as_deref() {
        None => crate::workflows::StepEdgeKind::Normal,
        Some(value) => crate::workflows::StepEdgeKind::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("`{value}` is not an edge kind — use normal or on_exhausted"),
        ))?,
    };

    store
        .link_steps_with_kind(&id, &request.parent_step_key, &request.child_step_key, kind)
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
        // Checked here as far as it can be: the step must be in a loop and the
        // step it reads must be in the same body. Which step is the body's exit
        // and where iteration 1 falls back to depend on the edges, so those are
        // launch's job — this is the half the author can see on screen.
        BindingSource::PreviousIteration => {
            let target = request.source_step_key.as_deref().unwrap_or("");
            let Some(owner) = steps.iter().find(|step| step.step_key == step_key) else {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("no step `{step_key}` in this workflow"),
                ));
            };
            let Some(group) = owner.loop_group.as_deref().filter(|name| !name.is_empty()) else {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "step `{step_key}` is not in a loop body, so it has no previous iteration \
                         to read"
                    ),
                ));
            };
            let same_body = steps
                .iter()
                .any(|step| step.step_key == target && step.loop_group.as_deref() == Some(group));
            if !same_body {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("step `{target}` is not in loop `{group}`"),
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

/// `PUT /workflows/{id}/steps/{step_key}/gates/{gate_key}` — declare the
/// condition under which a step runs.
///
/// Idempotent on `gate_key`, so an editor saving the same condition twice edits
/// it. A generated id would leave the step held behind two copies of one gate,
/// which on the board reads as a condition that cannot be satisfied.
#[utoipa::path(
    put,
    path = "/workflows/{id}/steps/{step_key}/gates/{gate_key}",
    params(
        ("id" = String, Path, description = "Workflow id"),
        ("step_key" = String, Path, description = "Step being gated"),
        ("gate_key" = String, Path, description = "Author-chosen name for this condition"),
    ),
    request_body = SaveStepGateRequest,
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such workflow"),
        (status = 422, description = "Unknown kind or disposition, a step that does not exist, or an unusable config"),
    ),
    tag = "workflows",
)]
pub(super) async fn put_step_gate(
    State(state): State<Arc<ApiState>>,
    Path((id, step_key, gate_key)): Path<(String, String, String)>,
    Json(request): Json<SaveStepGateRequest>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    let kind = crate::tasks::GateKind::parse(&request.kind).ok_or((
        StatusCode::UNPROCESSABLE_ENTITY,
        crate::tasks::GateConfigError::UnknownKind {
            value: request.kind.clone(),
        }
        .to_string(),
    ))?;

    let disposition = match request.disposition.as_deref() {
        None => None,
        Some(value) => Some(crate::tasks::GateDisposition::parse(value).ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "`{value}` is not a disposition for the condition on step `{step_key}` — use \
                 wait or route, or omit it to derive"
            ),
        ))?),
    };

    // Every refusal below names the step. The author is looking at one step in
    // an editor, and a message that does not say which one sends them reading
    // rows — the same reason every `LaunchError` variant names one.
    let steps = store.list_steps(&id).await.map_err(internal)?;
    if !steps.iter().any(|step| step.step_key == step_key) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("no step `{step_key}` in this workflow"),
        ));
    }

    if kind == crate::tasks::GateKind::TaskOutput {
        let target = request.source_step_key.as_deref().unwrap_or("");
        if target.is_empty() {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "the condition `{gate_key}` on step `{step_key}` reads a step's output, so it \
                     needs source_step_key"
                ),
            ));
        }
        if !steps.iter().any(|step| step.step_key == target) {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("no step `{target}` in this workflow"),
            ));
        }
        // A step gated on its own output can never run, so it can never produce
        // the output. Caught here rather than at launch, where the author is no
        // longer looking at the step that says it.
        if target == step_key {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("step `{step_key}` cannot be the condition for whether it runs"),
            ));
        }
    }

    let gate = crate::workflows::StepGate {
        workflow_id: id.clone(),
        step_key: step_key.clone(),
        gate_key,
        kind,
        source_step_key: request.source_step_key,
        config: request.config,
        label: request.label,
        // The floor, not a preference: a gate is server-side, unattended, and
        // repeating.
        poll_interval_secs: request
            .poll_interval_secs
            .unwrap_or(crate::tasks::MIN_POLL_INTERVAL_SECS.max(60)),
        disposition,
    };

    // The same validator the task level uses. A condition that will not
    // evaluate would error once a minute forever with nobody reading the log,
    // so it is refused while a person is still looking at the form.
    crate::workflows::validate_step_gate(&gate).map_err(|error| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("the condition on step `{step_key}` is not usable: {error}"),
        )
    })?;

    store.put_gate(&gate).await.map_err(internal)?;

    get_workflow(State(state), Path(id)).await
}

/// `DELETE /workflows/{id}/steps/{step_key}/gates/{gate_key}` — the step runs
/// unconditionally again.
#[utoipa::path(
    delete,
    path = "/workflows/{id}/steps/{step_key}/gates/{gate_key}",
    params(
        ("id" = String, Path, description = "Workflow id"),
        ("step_key" = String, Path, description = "Step being gated"),
        ("gate_key" = String, Path, description = "Name of the condition"),
    ),
    responses(
        (status = 200, body = WorkflowDetailResponse),
        (status = 404, description = "No such condition"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_step_gate(
    State(state): State<Arc<ApiState>>,
    Path((id, step_key, gate_key)): Path<(String, String, String)>,
) -> Result<Json<WorkflowDetailResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if !store
        .delete_gate(&id, &step_key, &gate_key)
        .await
        .map_err(internal)?
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no condition `{gate_key}` on step `{step_key}`"),
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

/// `POST /workflow-runs/{run_id}/cancel` — stop a run.
///
/// Unstarted tasks are settled; anything already running is left to finish,
/// because killing work mid-flight loses whatever it had done. Cancelling a
/// `stuck` or `failed` run is allowed and is how the cards it left parked get
/// cleared; a `succeeded` run has nothing to clear.
#[utoipa::path(
    post,
    path = "/workflow-runs/{run_id}/cancel",
    params(("run_id" = String, Path, description = "Workflow run id")),
    request_body = CancelRunRequest,
    responses(
        (status = 200, body = CancelRunResponse),
        (status = 404, description = "No such run"),
        (status = 409, description = "The run has already finished"),
    ),
    tag = "workflows",
)]
pub(super) async fn cancel_run(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
    Json(request): Json<CancelRunRequest>,
) -> Result<Json<CancelRunResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    let outcome = store
        .cancel_run(&run_id, &request.cancelled_by)
        .await
        .map_err(internal)?;

    let (settled, left_running) = match outcome {
        crate::workflows::CancelOutcome::Cancelled {
            settled,
            left_running,
        } => (settled, left_running),
        crate::workflows::CancelOutcome::AlreadyFinished { status } => {
            return Err((
                StatusCode::CONFLICT,
                format!("run {run_id} has already finished ({status})"),
            ));
        }
        crate::workflows::CancelOutcome::NotFound => {
            return Err((StatusCode::NOT_FOUND, format!("no run {run_id}")));
        }
    };

    let run = store
        .get_run(&run_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("no run {run_id}")))?;

    Ok(Json(CancelRunResponse {
        run,
        settled,
        left_running,
    }))
}

/// `DELETE /workflow-runs/{run_id}` — remove a finished run and its tasks.
///
/// The endpoint whose absence left empty run rows behind: cleanup had nothing
/// to call. Refused while the run is still going — `409` with the sentence
/// saying to cancel it first, because a delete is not a stop.
#[utoipa::path(
    delete,
    path = "/workflow-runs/{run_id}",
    params(("run_id" = String, Path, description = "Workflow run id")),
    responses(
        (status = 200, body = WorkflowActionResponse),
        (status = 404, description = "No such run"),
        (status = 409, description = "The run is still going, or a worker is still in it"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_run(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<WorkflowActionResponse>, (StatusCode, String)> {
    let store = get_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let task_store = get_task_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    match store
        .delete_run(&task_store, &run_id)
        .await
        .map_err(internal)?
    {
        crate::workflows::DeleteRunOutcome::Deleted { tasks_removed } => {
            Ok(Json(WorkflowActionResponse {
                success: true,
                message: format!("run deleted, along with {tasks_removed} task(s)"),
            }))
        }
        crate::workflows::DeleteRunOutcome::Refused { reason } => {
            Err((StatusCode::CONFLICT, reason))
        }
        crate::workflows::DeleteRunOutcome::NotFound => {
            Err((StatusCode::NOT_FOUND, format!("no run {run_id}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn test_api_state() -> Arc<ApiState> {
        let (provider_setup_tx, _provider_setup_rx) = tokio::sync::mpsc::channel(1);
        let (agent_tx, _agent_rx) = tokio::sync::mpsc::channel(1);
        let (agent_remove_tx, _agent_remove_rx) = tokio::sync::mpsc::channel(1);
        let (injection_tx, _injection_rx) = tokio::sync::mpsc::channel(1);
        Arc::new(ApiState::new_with_provider_sender(
            provider_setup_tx,
            agent_tx,
            agent_remove_tx,
            injection_tx,
        ))
    }

    /// Both stores share one pool: launching a workflow writes tasks, so the
    /// fixture mirrors how the instance wires them together.
    async fn state_with_stores() -> Arc<ApiState> {
        let state = test_api_state();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::tasks::store::create_task_schema(&pool).await;
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("seed sequence");
        crate::workflows::store::create_workflow_schema(&pool).await;

        state.set_task_store(Arc::new(crate::tasks::TaskStore::new(pool.clone())));
        state.set_workflow_store(Arc::new(crate::workflows::WorkflowStore::new(pool)));
        state
    }

    fn save_request(name: &str) -> SaveWorkflowRequest {
        SaveWorkflowRequest {
            name: name.to_string(),
            description: None,
            input_schema: None,
        }
    }

    fn step_request(title: &str) -> SaveStepRequest {
        SaveStepRequest {
            title: title.to_string(),
            description: None,
            assigned_agent_id: None,
            required_capabilities: None,
            priority: None,
            input_schema: None,
            output_schema: None,
            system_prompt: None,
            repo_id: None,
            position: None,
            for_each_step_key: None,
            for_each_pointer: None,
            for_each_key: None,
            loop_group: None,
            loop_max_iterations: None,
            loop_until: None,
            kind: None,
            command: None,
            command_timeout_secs: None,
            expect_exit_code: None,
            worktree_mode: None,
            worktree_base_ref: None,
            decision_question: None,
            decision_asked_of: None,
            decision_timeout_action: None,
            decision_timeout_secs: None,
            decision_default_answer: None,
            decision_ask: None,
        }
    }

    async fn create_one(state: &Arc<ApiState>, name: &str) -> crate::workflows::Workflow {
        create_workflow(State(state.clone()), Json(save_request(name)))
            .await
            .expect("workflow should be created")
            .0
            .workflow
    }

    async fn put_one_step(state: &Arc<ApiState>, workflow_id: &str, step_key: &str) {
        let _detail = put_step(
            State(state.clone()),
            Path((workflow_id.to_string(), step_key.to_string())),
            Json(step_request(&format!("step {step_key}"))),
        )
        .await
        .expect("step should be saved");
    }

    fn launch_request(inputs: serde_json::Value) -> LaunchRequest {
        LaunchRequest {
            inputs,
            launched_by: "agent-test".to_string(),
        }
    }

    #[tokio::test]
    async fn list_workflows_without_store_is_service_unavailable() {
        let state = test_api_state();
        let result = list_workflows(State(state)).await;
        assert!(matches!(result, Err((StatusCode::SERVICE_UNAVAILABLE, _))));
    }

    #[tokio::test]
    async fn create_get_and_list_round_trip() {
        let state = state_with_stores().await;

        let workflow = create_one(&state, "deploy").await;
        assert_eq!(workflow.name, "deploy");

        let detail = get_workflow(State(state.clone()), Path(workflow.id.clone()))
            .await
            .expect("workflow should be found")
            .0;
        assert_eq!(detail.workflow.id, workflow.id);
        assert!(detail.steps.is_empty());
        assert!(detail.edges.is_empty());

        let listed = list_workflows(State(state))
            .await
            .expect("workflows should list")
            .0;
        assert!(listed.workflows.iter().any(|w| w.id == workflow.id));
    }

    #[tokio::test]
    async fn create_workflow_duplicate_name_is_conflict() {
        let state = state_with_stores().await;
        create_one(&state, "deploy").await;

        let result = create_workflow(State(state), Json(save_request("deploy"))).await;
        assert!(matches!(result, Err((StatusCode::CONFLICT, _))));
    }

    #[tokio::test]
    async fn update_workflow_duplicate_name_is_conflict() {
        let state = state_with_stores().await;
        create_one(&state, "deploy").await;
        let other = create_one(&state, "release").await;

        let result = update_workflow(
            State(state.clone()),
            Path(other.id.clone()),
            Json(save_request("deploy")),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::CONFLICT, _))));

        // A rename to oneself is not a collision.
        let renamed = update_workflow(
            State(state),
            Path(other.id.clone()),
            Json(save_request("release")),
        )
        .await
        .expect("renaming to oneself should succeed")
        .0;
        assert_eq!(renamed.workflow.name, "release");
    }

    #[tokio::test]
    async fn delete_workflow_then_not_found() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "ephemeral").await;

        let deleted = delete_workflow(State(state.clone()), Path(workflow.id.clone()))
            .await
            .expect("delete should succeed")
            .0;
        assert!(deleted.success);

        let again = delete_workflow(State(state.clone()), Path(workflow.id.clone())).await;
        assert!(matches!(again, Err((StatusCode::NOT_FOUND, _))));

        let fetched = get_workflow(State(state), Path(workflow.id)).await;
        assert!(matches!(fetched, Err((StatusCode::NOT_FOUND, _))));
    }

    #[tokio::test]
    async fn put_step_unknown_workflow_is_not_found() {
        let state = state_with_stores().await;
        let result = put_step(
            State(state),
            Path(("missing".to_string(), "one".to_string())),
            Json(step_request("step one")),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::NOT_FOUND, _))));
    }

    #[tokio::test]
    async fn put_step_rejects_unknown_priority() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "priorities").await;

        let mut request = step_request("step one");
        request.priority = Some("whenever".to_string());
        let result = put_step(
            State(state),
            Path((workflow.id, "one".to_string())),
            Json(request),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::UNPROCESSABLE_ENTITY, _))));
    }

    #[tokio::test]
    async fn put_step_rejects_fan_out_over_missing_or_own_step() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "fan-outs").await;
        put_one_step(&state, &workflow.id, "source").await;

        let mut missing = step_request("iterate");
        missing.for_each_step_key = Some("ghost".to_string());
        let result = put_step(
            State(state.clone()),
            Path((workflow.id.clone(), "branch".to_string())),
            Json(missing),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::UNPROCESSABLE_ENTITY, _))));

        let mut own = step_request("iterate over myself");
        own.for_each_step_key = Some("selfish".to_string());
        let result = put_step(
            State(state),
            Path((workflow.id, "selfish".to_string())),
            Json(own),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::UNPROCESSABLE_ENTITY, _))));
    }

    #[tokio::test]
    async fn put_step_gate_rejects_bad_references() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "conditions").await;
        put_one_step(&state, &workflow.id, "one").await;

        // Unknown gate kind.
        let unknown_kind = put_step_gate(
            State(state.clone()),
            Path((workflow.id.clone(), "one".to_string(), "ci".to_string())),
            Json(SaveStepGateRequest {
                kind: "smoke_signal".to_string(),
                source_step_key: None,
                config: serde_json::json!({}),
                label: None,
                poll_interval_secs: None,
                disposition: None,
            }),
        )
        .await;
        assert!(matches!(
            unknown_kind,
            Err((StatusCode::UNPROCESSABLE_ENTITY, _))
        ));

        // A task_output condition must name the step it reads.
        let no_source = put_step_gate(
            State(state),
            Path((workflow.id, "one".to_string(), "ci".to_string())),
            Json(SaveStepGateRequest {
                kind: "task_output".to_string(),
                source_step_key: None,
                config: serde_json::json!({"pointer": "/ok", "equals": true}),
                label: None,
                poll_interval_secs: None,
                disposition: None,
            }),
        )
        .await;
        assert!(matches!(
            no_source,
            Err((StatusCode::UNPROCESSABLE_ENTITY, _))
        ));
    }

    #[tokio::test]
    async fn launch_unknown_workflow_is_not_found() {
        let state = state_with_stores().await;
        let result = launch_workflow(
            State(state),
            Path("missing".to_string()),
            Json(launch_request(serde_json::json!({}))),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::NOT_FOUND, _))));
    }

    #[tokio::test]
    async fn launch_without_steps_is_unprocessable() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "empty").await;

        let result = launch_workflow(
            State(state),
            Path(workflow.id),
            Json(launch_request(serde_json::json!({}))),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::UNPROCESSABLE_ENTITY, _))));
    }

    #[tokio::test]
    async fn launch_with_input_missing_required_field_is_unprocessable() {
        let state = state_with_stores().await;

        let workflow = create_workflow(
            State(state.clone()),
            Json(SaveWorkflowRequest {
                name: "tagged".to_string(),
                description: None,
                input_schema: Some(serde_json::json!({
                    "type": "object",
                    "required": ["tag"],
                    "properties": {"tag": {"type": "string"}},
                })),
            }),
        )
        .await
        .expect("workflow should be created")
        .0
        .workflow;
        put_one_step(&state, &workflow.id, "release").await;

        let result = launch_workflow(
            State(state),
            Path(workflow.id),
            Json(launch_request(serde_json::json!({}))),
        )
        .await;
        assert!(matches!(result, Err((StatusCode::UNPROCESSABLE_ENTITY, _))));
    }

    #[tokio::test]
    async fn edge_closing_a_cycle_is_refused_at_save() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "cyclic").await;
        put_one_step(&state, &workflow.id, "a").await;
        put_one_step(&state, &workflow.id, "b").await;

        let _detail = add_edge(
            State(state.clone()),
            Path(workflow.id.clone()),
            Json(StepEdgeRequest {
                parent_step_key: "a".to_string(),
                child_step_key: "b".to_string(),
                kind: None,
            }),
        )
        .await
        .expect("first edge should be saved");

        // b -> a would close a -> b -> a. Refused here, while the author is
        // still looking at the canvas, rather than at launch.
        let closing = add_edge(
            State(state),
            Path(workflow.id),
            Json(StepEdgeRequest {
                parent_step_key: "b".to_string(),
                child_step_key: "a".to_string(),
                kind: None,
            }),
        )
        .await;
        assert!(matches!(closing, Err((StatusCode::CONFLICT, _))));
    }

    #[tokio::test]
    async fn launch_minimal_pipeline_emits_tasks() {
        let state = state_with_stores().await;
        let workflow = create_one(&state, "pipeline").await;
        put_one_step(&state, &workflow.id, "build").await;
        put_one_step(&state, &workflow.id, "ship").await;
        let _detail = add_edge(
            State(state.clone()),
            Path(workflow.id.clone()),
            Json(StepEdgeRequest {
                parent_step_key: "build".to_string(),
                child_step_key: "ship".to_string(),
                kind: None,
            }),
        )
        .await
        .expect("edge should be saved");

        let launched = launch_workflow(
            State(state),
            Path(workflow.id),
            Json(launch_request(serde_json::json!({}))),
        )
        .await
        .expect("launch should succeed")
        .0;

        assert_eq!(launched.task_numbers.len(), 2);
        assert!(launched.task_numbers.contains_key("build"));
        assert!(launched.task_numbers.contains_key("ship"));
    }
}
