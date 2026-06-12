//! REST API for the instance-level notification inbox.

use super::state::{ApiEvent, ApiState};
use crate::notifications::{Notification, NotificationFilter, NotificationKind, NotificationStore};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(super) struct ListNotificationsQuery {
    /// "unread" returns only unread notifications; anything else returns all.
    #[serde(default)]
    pub filter: Option<String>,
    /// Filter by agent id.
    pub agent_id: Option<String>,
    /// Filter by kind: "task_approval", "worker_failed", "cortex_observation".
    pub kind: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct NotificationsResponse {
    pub notifications: Vec<Notification>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct UnreadCountResponse {
    pub count: i64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_notification_store(state: &ApiState) -> Result<Arc<NotificationStore>, StatusCode> {
    state
        .notification_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn parse_kind(value: Option<&str>) -> Option<NotificationKind> {
    match value? {
        "task_approval" => Some(NotificationKind::TaskApproval),
        "worker_failed" => Some(NotificationKind::WorkerFailed),
        "cortex_observation" => Some(NotificationKind::CortexObservation),
        _ => None,
    }
}

fn broadcast_updated(state: &ApiState, id: &str, read: bool, dismissed: bool) {
    state
        .event_tx
        .send(ApiEvent::NotificationUpdated {
            id: id.to_string(),
            read,
            dismissed,
        })
        .ok();
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /notifications` — list notifications with optional filters.
#[utoipa::path(
    get,
    path = "/notifications",
    params(ListNotificationsQuery),
    responses(
        (status = 200, body = NotificationsResponse),
        (status = 503, description = "Notification store not initialized"),
    ),
    tag = "notifications",
)]
pub(super) async fn list_notifications(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListNotificationsQuery>,
) -> Result<Json<NotificationsResponse>, StatusCode> {
    let store = get_notification_store(&state)?;
    let unread_only = query.filter.as_deref() == Some("unread");
    let kind = parse_kind(query.kind.as_deref());

    let notifications = store
        .list(NotificationFilter {
            unread_only,
            include_dismissed: false,
            agent_id: query.agent_id,
            kind,
            limit: Some(query.limit.clamp(1, 500)),
            offset: Some(query.offset),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to list notifications");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(NotificationsResponse { notifications }))
}

/// `GET /notifications/unread_count` — count unread, undismissed notifications.
#[utoipa::path(
    get,
    path = "/notifications/unread_count",
    responses(
        (status = 200, body = UnreadCountResponse),
        (status = 503, description = "Notification store not initialized"),
    ),
    tag = "notifications",
)]
pub(super) async fn unread_count(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<UnreadCountResponse>, StatusCode> {
    let store = get_notification_store(&state)?;
    let count = store.unread_count().await.map_err(|error| {
        tracing::warn!(%error, "failed to count unread notifications");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(UnreadCountResponse { count }))
}

/// `POST /notifications/{id}/read` — mark a single notification as read.
#[utoipa::path(
    post,
    path = "/notifications/{id}/read",
    params(("id" = String, Path, description = "Notification id")),
    responses(
        (status = 204, description = "Marked as read"),
        (status = 404, description = "Not found or already read"),
        (status = 503, description = "Notification store not initialized"),
    ),
    tag = "notifications",
)]
pub(super) async fn mark_read(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let store = get_notification_store(&state)?;
    let updated = store.mark_read(&id).await.map_err(|error| {
        tracing::warn!(%error, %id, "failed to mark notification read");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    broadcast_updated(&state, &id, true, false);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /notifications/{id}/dismiss` — dismiss a single notification.
#[utoipa::path(
    post,
    path = "/notifications/{id}/dismiss",
    params(("id" = String, Path, description = "Notification id")),
    responses(
        (status = 204, description = "Dismissed"),
        (status = 404, description = "Not found or already dismissed"),
        (status = 503, description = "Notification store not initialized"),
    ),
    tag = "notifications",
)]
pub(super) async fn dismiss_notification(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let store = get_notification_store(&state)?;
    let updated = store.dismiss(&id).await.map_err(|error| {
        tracing::warn!(%error, %id, "failed to dismiss notification");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    broadcast_updated(&state, &id, true, true);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /notifications/read_all` — mark all undismissed notifications as read.
#[utoipa::path(
    post,
    path = "/notifications/read_all",
    responses(
        (status = 204, description = "All marked as read"),
        (status = 503, description = "Notification store not initialized"),
    ),
    tag = "notifications",
)]
pub(super) async fn mark_all_read(
    State(state): State<Arc<ApiState>>,
) -> Result<StatusCode, StatusCode> {
    let store = get_notification_store(&state)?;
    store.mark_all_read().await.map_err(|error| {
        tracing::warn!(%error, "failed to mark all notifications read");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    // Broadcast a generic update so connected clients re-fetch
    state
        .event_tx
        .send(ApiEvent::NotificationUpdated {
            id: String::new(),
            read: true,
            dismissed: false,
        })
        .ok();
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /notifications/dismiss_read` — dismiss all already-read notifications.
#[utoipa::path(
    post,
    path = "/notifications/dismiss_read",
    responses(
        (status = 204, description = "Read notifications dismissed"),
        (status = 503, description = "Notification store not initialized"),
    ),
    tag = "notifications",
)]
pub(super) async fn dismiss_read(
    State(state): State<Arc<ApiState>>,
) -> Result<StatusCode, StatusCode> {
    let store = get_notification_store(&state)?;
    store.dismiss_read().await.map_err(|error| {
        tracing::warn!(%error, "failed to dismiss read notifications");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    state
        .event_tx
        .send(ApiEvent::NotificationUpdated {
            id: String::new(),
            read: true,
            dismissed: true,
        })
        .ok();
    Ok(StatusCode::NO_CONTENT)
}
