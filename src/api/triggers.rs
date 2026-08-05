//! Workflow triggers: schedules, and the inbound webhook.
//!
//! Split from `workflows.rs` because the two halves have different threat
//! models and one of them has to be read as a security surface rather than as
//! CRUD. The configuration endpoints here are ordinary, bearer-authenticated
//! plumbing. [`workflow_webhook_delivery`] is not: it is the one route in this
//! process that a stranger on the internet is expected to reach, and everything
//! about it is arranged so that the default answer is no.

use super::state::ApiState;
use crate::workflows::triggers::{
    DeliveryOutcome, WEBHOOK_SECRET_HEADER, WorkflowSchedule, WorkflowTriggerStore,
};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveScheduleRequest {
    /// Omit to create. Supplying an existing id replaces that schedule.
    #[serde(default)]
    id: Option<String>,
    name: String,
    /// 5-field cron expression, read in UTC. Omit to use `interval_secs`.
    #[serde(default)]
    cron_expr: Option<String>,
    #[serde(default = "default_interval_secs")]
    interval_secs: i64,
    /// The launch payload. A literal, because a schedule cannot prompt.
    #[serde(default)]
    inputs: Option<serde_json::Value>,
    /// The agent that owns and, absent a step assignment, executes the run.
    agent_id: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_interval_secs() -> i64 {
    3600
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ScheduleListResponse {
    schedules: Vec<WorkflowSchedule>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ScheduleResponse {
    schedule: WorkflowSchedule,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SaveWebhookRequest {
    /// The shared secret, in plaintext, once. It is hashed before storage and
    /// there is no endpoint that reads it back.
    secret: String,
    /// `{ "<run input key>": "<JSON Pointer into the payload>" }`.
    #[serde(default)]
    input_pointers: serde_json::Map<String, serde_json::Value>,
    /// The agent that owns and executes the run.
    agent_id: String,
    /// Off by default. An inbound trigger that turns itself on when configured
    /// would make "I set this up to test it" and "I want strangers able to run
    /// this pipeline" the same action.
    #[serde(default)]
    enabled: bool,
}

/// A webhook as it is safe to describe.
///
/// There is no `secret` field, and that is structural rather than a matter of
/// remembering: the type the store hands back does not carry the secret either,
/// so there is nothing here that a future edit could accidentally serialise.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct WebhookResponse {
    webhook: crate::workflows::triggers::WorkflowWebhook,
    /// Where deliveries go, so an operator can paste it into a CI config
    /// without reconstructing it from the route table.
    delivery_path: String,
    /// The header the shared secret goes in.
    secret_header: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct DeliveryResponse {
    outcome: String,
    run_id: Option<String>,
    detail: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_trigger_store(state: &ApiState) -> Result<WorkflowTriggerStore, StatusCode> {
    // Built from the task store's pool, exactly as `cortex.rs` builds its
    // `WorkflowStore`: the trigger tables live in the same instance database,
    // and a second `ApiState` field to reach them would be one more thing to
    // register at startup and one more way to have a `None` at runtime.
    let tasks = state
        .task_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(WorkflowTriggerStore::new(tasks.pool().clone()))
}

fn get_workflow_store(
    state: &ApiState,
) -> Result<Arc<crate::workflows::WorkflowStore>, StatusCode> {
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
    tracing::error!(%error, "workflow trigger request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "workflow trigger storage error".to_string(),
    )
}

/// The delivery path for a workflow's webhook.
pub(super) fn webhook_delivery_path(workflow_id: &str) -> String {
    format!("/api/webhooks/workflow/{workflow_id}")
}

/// The single answer every rejected delivery gets.
///
/// One function, called from one place, so there is no route by which the three
/// rejection reasons could come to render differently. They *are* different —
/// "no webhook configured", "configured but off", and "wrong or absent secret"
/// need three different things from an operator — and the difference goes to
/// the log, where the person who can act on it is, rather than to the caller,
/// who by construction is not authenticated and would be handed a working
/// oracle for which workflows exist and which of them are one switch away from
/// accepting deliveries.
fn rejection_response() -> (StatusCode, String) {
    (StatusCode::UNAUTHORIZED, "unauthorized".to_string())
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

/// `GET /workflows/{id}/schedules` — the schedules attached to one template.
#[utoipa::path(
    get,
    path = "/workflows/{id}/schedules",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, body = ScheduleListResponse)),
    tag = "workflows",
)]
pub(super) async fn list_schedules(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ScheduleListResponse>, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let schedules = triggers.list_schedules(&id).await.map_err(internal)?;
    Ok(Json(ScheduleListResponse { schedules }))
}

/// `POST /workflows/{id}/schedules` — create or replace a schedule.
#[utoipa::path(
    post,
    path = "/workflows/{id}/schedules",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = SaveScheduleRequest,
    responses(
        (status = 200, body = ScheduleResponse),
        (status = 404, description = "No such workflow"),
        (status = 422, description = "Unusable schedule"),
    ),
    tag = "workflows",
)]
pub(super) async fn put_schedule(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SaveScheduleRequest>,
) -> Result<Json<ScheduleResponse>, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let workflows = get_workflow_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    // Refused here rather than discovered at 03:00. A schedule pointing at a
    // template that does not exist is a timer that fires into a refusal
    // forever, and the one moment a person is watching is this one.
    if workflows
        .get_workflow(&id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!("workflow {id} does not exist"),
        ));
    }

    // An expression cleared to blank means "use the interval", not "use the
    // empty expression". Folded here so the stored row has one representation
    // of "unset" rather than two that read differently.
    let cron_expr = request.cron_expr.filter(|expr| !expr.trim().is_empty());

    // Same argument for the template check: an unparseable expression produces
    // no cursor, so the sweep would skip it silently forever. Checked against
    // the same parser that will read it.
    if let Some(expr) = cron_expr.as_deref()
        && crate::cron::scheduler::next_cron_fire(expr, chrono::Utc::now(), chrono_tz::UTC)
            .is_none()
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "`{expr}` is not a usable cron expression — it must be 5-field standard cron \
                 syntax with at least one future fire, e.g. `0 3 * * *` for 03:00 UTC daily"
            ),
        ));
    }

    let schedule = WorkflowSchedule {
        id: request
            .id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        workflow_id: id,
        name: request.name,
        cron_expr,
        interval_secs: request.interval_secs.max(1),
        inputs: request.inputs.unwrap_or_else(|| serde_json::json!({})),
        agent_id: request.agent_id,
        enabled: request.enabled,
        next_run_at: None,
        last_fired_at: None,
        last_outcome: None,
        last_detail: None,
        last_run_id: None,
        created_at: String::new(),
    };

    triggers.put_schedule(&schedule).await.map_err(internal)?;

    let saved = triggers
        .get_schedule(&schedule.id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("schedule saved but not found"))?;

    Ok(Json(ScheduleResponse { schedule: saved }))
}

/// `DELETE /workflow-schedules/{schedule_id}` — remove a schedule.
///
/// Not nested under the workflow, matching the run routes: a caller holding a
/// schedule id from a listing should not have to also know which template it
/// belongs to in order to delete it.
#[utoipa::path(
    delete,
    path = "/workflow-schedules/{schedule_id}",
    params(("schedule_id" = String, Path, description = "Workflow schedule id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such schedule"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_schedule(
    State(state): State<Arc<ApiState>>,
    Path(schedule_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if triggers
        .delete_schedule(&schedule_id)
        .await
        .map_err(internal)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("schedule {schedule_id} does not exist"),
        ))
    }
}

// ---------------------------------------------------------------------------
// Webhook configuration
// ---------------------------------------------------------------------------

/// `GET /workflows/{id}/webhook` — the webhook config, without its secret.
#[utoipa::path(
    get,
    path = "/workflows/{id}/webhook",
    params(("id" = String, Path, description = "Workflow id")),
    responses(
        (status = 200, body = WebhookResponse),
        (status = 404, description = "No webhook is configured"),
    ),
    tag = "workflows",
)]
pub(super) async fn get_webhook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<WebhookResponse>, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let webhook = triggers
        .get_webhook(&id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("workflow {id} has no webhook configured"),
            )
        })?;

    Ok(Json(WebhookResponse {
        delivery_path: webhook_delivery_path(&webhook.workflow_id),
        secret_header: WEBHOOK_SECRET_HEADER.to_string(),
        webhook,
    }))
}

/// `PUT /workflows/{id}/webhook` — configure the inbound trigger.
///
/// The one place a secret is accepted, and the only way this endpoint can ever
/// start accepting deliveries. Both halves are deliberate: there is no global
/// enable, no default row, and no way to end up with a live webhook without
/// having chosen a secret and set `enabled` in the same call.
#[utoipa::path(
    put,
    path = "/workflows/{id}/webhook",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = SaveWebhookRequest,
    responses(
        (status = 200, body = WebhookResponse),
        (status = 404, description = "No such workflow"),
        (status = 422, description = "Unusable webhook configuration"),
    ),
    tag = "workflows",
)]
pub(super) async fn put_webhook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<SaveWebhookRequest>,
) -> Result<Json<WebhookResponse>, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let workflows = get_workflow_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    if workflows
        .get_workflow(&id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            format!("workflow {id} does not exist"),
        ));
    }

    // A short secret is a secret an attacker can guess, and this endpoint is
    // reachable without the instance token. Refused rather than warned about,
    // because a warning on a configuration path is a warning nobody sees.
    if request.secret.trim().len() < MIN_WEBHOOK_SECRET_LEN {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "the shared secret must be at least {MIN_WEBHOOK_SECRET_LEN} characters — this \
                 endpoint is reachable without the instance token, so the secret is the only \
                 thing standing in front of it"
            ),
        ));
    }

    for (input_key, pointer) in &request.input_pointers {
        let Some(pointer) = pointer.as_str() else {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("the mapping for `{input_key}` must be a JSON Pointer string"),
            ));
        };
        // RFC 6901: a non-empty pointer starts with `/`. Caught here because a
        // pointer that can never resolve turns every future delivery into an
        // `unmapped` outcome, which reads like the sender's fault.
        if !pointer.is_empty() && !pointer.starts_with('/') {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "`{pointer}` is not a JSON Pointer — it must start with `/`, e.g. \
                     `/head_commit/id`, or be empty for the whole payload"
                ),
            ));
        }
    }

    triggers
        .put_webhook(
            &id,
            &request.secret,
            &request.input_pointers,
            &request.agent_id,
            request.enabled,
        )
        .await
        .map_err(internal)?;

    let saved = triggers
        .get_webhook(&id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("webhook saved but not found"))?;

    tracing::info!(
        workflow_id = %id,
        enabled = saved.enabled,
        "workflow webhook configured"
    );

    Ok(Json(WebhookResponse {
        delivery_path: webhook_delivery_path(&saved.workflow_id),
        secret_header: WEBHOOK_SECRET_HEADER.to_string(),
        webhook: saved,
    }))
}

/// Shortest shared secret this will store.
///
/// Thirty-two, which is what a `uuid` or 24 random bytes of base64 come out at,
/// and comfortably past anything worth trying online.
const MIN_WEBHOOK_SECRET_LEN: usize = 32;

/// `DELETE /workflows/{id}/webhook` — remove the inbound trigger entirely.
#[utoipa::path(
    delete,
    path = "/workflows/{id}/webhook",
    params(("id" = String, Path, description = "Workflow id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No webhook is configured"),
    ),
    tag = "workflows",
)]
pub(super) async fn delete_webhook(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    if triggers.delete_webhook(&id).await.map_err(internal)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("workflow {id} has no webhook configured"),
        ))
    }
}

// ---------------------------------------------------------------------------
// Webhook delivery
// ---------------------------------------------------------------------------

/// `POST /webhooks/workflow/{id}` — an inbound trigger firing.
///
/// **The one route here that is not behind the instance bearer token.** It has
/// to be: a webhook exists so that CI — which cannot be handed a token that
/// grants the whole API — can start a pipeline, and that is the loop the gate
/// machinery was built for and has never been able to close.
///
/// So the authentication is the per-workflow shared secret and nothing else,
/// and every part of this is arranged to fail closed:
///
/// - No row for the workflow means no. That is the default state of every
///   workflow that has ever existed, and it is not a check that can be
///   forgotten — it is the absence of the thing that would allow it.
/// - A configured webhook is off unless somebody set `enabled`.
/// - The secret is compared as a digest, in constant time, before the payload
///   is looked at, before the workflow is loaded, and before anything is
///   written. A delivery that does not authenticate causes no work.
/// - All three refusals render identically. See [`rejection_response`].
#[utoipa::path(
    post,
    path = "/webhooks/workflow/{id}",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = serde_json::Value,
    responses(
        (status = 200, body = DeliveryResponse),
        (status = 401, description = "No usable webhook, or no valid shared secret"),
        (status = 422, description = "Authenticated, and the payload or the template was unusable"),
    ),
    tag = "workflows",
)]
pub(super) async fn workflow_webhook_delivery(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<DeliveryResponse>, (StatusCode, String)> {
    let triggers = get_trigger_store(&state).map_err(|_| rejection_response())?;

    let presented = headers
        .get(WEBHOOK_SECRET_HEADER)
        .and_then(|value| value.to_str().ok());

    let authenticated = triggers
        .authenticate_webhook(&id, presented)
        .await
        .map_err(|error| {
            // Our storage failing is not the caller's business either. It gets
            // the same refusal as everything else that did not get in.
            tracing::error!(%error, workflow_id = %id, "webhook authentication failed to read");
            rejection_response()
        })?;

    let webhook = match authenticated {
        Ok(webhook) => webhook,
        Err(rejection) => {
            // The distinction lands here, where somebody can act on it, and
            // nowhere else.
            tracing::warn!(
                workflow_id = %id,
                reason = ?rejection,
                detail = rejection.operator_detail(),
                "refused a workflow webhook delivery"
            );
            return Err(rejection_response());
        }
    };

    let workflows = get_workflow_store(&state).map_err(|code| (code, "unavailable".to_string()))?;
    let tasks = get_task_store(&state).map_err(|code| (code, "unavailable".to_string()))?;

    let delivered = triggers
        .deliver(&workflows, &tasks, &webhook, &payload)
        .await
        .map_err(internal)?;

    match delivered {
        Ok(launched) => {
            for number in launched.task_numbers.values() {
                state
                    .event_tx
                    .send(super::state::ApiEvent::TaskUpdated {
                        agent_id: webhook.agent_id.clone(),
                        task_number: *number,
                        status: crate::tasks::TaskStatus::Backlog.as_str().to_string(),
                        action: "created".to_string(),
                    })
                    .ok();
            }
            tracing::info!(
                workflow_id = %id,
                run_id = %launched.run.id,
                "workflow webhook launched a run"
            );
            Ok(Json(DeliveryResponse {
                outcome: DeliveryOutcome::Launched.as_str().to_string(),
                detail: format!("launched run {}", launched.run.id),
                run_id: Some(launched.run.id),
            }))
        }
        // Past the door, so the caller is a trusted integration and the detail
        // is the whole value of the response: it is what tells whoever wired the
        // pointers up which one does not match what they are sending.
        Err((outcome, detail)) => Err((
            match outcome {
                DeliveryOutcome::Errored => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            },
            detail,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::triggers::WebhookRejection;

    /// The refusal must be one answer, whatever the reason.
    ///
    /// If these ever diverge — a 404 for an unconfigured workflow, a distinct
    /// message for a disabled one — an unauthenticated stranger gains a probe
    /// that enumerates which workflow ids exist and which of those are one
    /// operator switch away from executing a pipeline for them. The reason is
    /// not secret from the operator; it is secret from the caller.
    #[test]
    fn every_webhook_rejection_renders_the_same_answer_to_the_caller() {
        let rendered = [
            WebhookRejection::NotConfigured,
            WebhookRejection::Disabled,
            WebhookRejection::BadSecret,
        ]
        .map(|_| rejection_response());

        assert_eq!(rendered[0], rendered[1]);
        assert_eq!(rendered[1], rendered[2]);
        assert_eq!(rendered[0].0, StatusCode::UNAUTHORIZED);

        // And the operator-side text stays distinct, or the log is as useless
        // as the response deliberately is.
        assert_ne!(
            WebhookRejection::NotConfigured.operator_detail(),
            WebhookRejection::Disabled.operator_detail()
        );
        assert_ne!(
            WebhookRejection::Disabled.operator_detail(),
            WebhookRejection::BadSecret.operator_detail()
        );
    }
}
