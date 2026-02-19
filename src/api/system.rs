use super::state::{ApiEvent, ApiState};

use axum::extract::State;
use axum::response::Sse;
use axum::Json;
use futures::stream::Stream;
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
pub(super) struct IdleResponse {
    idle: bool,
    active_workers: usize,
    active_branches: usize,
}

#[derive(Serialize)]
pub(super) struct StatusResponse {
    status: &'static str,
    version: &'static str,
    pid: u32,
    uptime_seconds: u64,
}

pub(super) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// Reports whether the instance is idle (no active workers or branches).
/// Used by the platform to gate rolling updates.
pub(super) async fn idle(State(state): State<Arc<ApiState>>) -> Json<IdleResponse> {
    let blocks = state.channel_status_blocks.read().await;
    let mut total_workers = 0;
    let mut total_branches = 0;

    for status_block in blocks.values() {
        let block = status_block.read().await;
        total_workers += block.active_workers.len();
        total_branches += block.active_branches.len();
    }

    Json(IdleResponse {
        idle: total_workers == 0 && total_branches == 0,
        active_workers: total_workers,
        active_branches: total_branches,
    })
}

pub(super) async fn status(State(state): State<Arc<ApiState>>) -> Json<StatusResponse> {
    let uptime = state.started_at.elapsed();
    Json(StatusResponse {
        status: "running",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        uptime_seconds: uptime.as_secs(),
    })
}

/// SSE endpoint streaming all agent events to connected clients.
pub(super) async fn events_sse(
    State(state): State<Arc<ApiState>>,
) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, Infallible>>> {
    let mut rx = state.event_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let event_type = match &event {
                            ApiEvent::InboundMessage { .. } => "inbound_message",
                            ApiEvent::OutboundMessage { .. } => "outbound_message",
                            ApiEvent::TypingState { .. } => "typing_state",
                            ApiEvent::WorkerStarted { .. } => "worker_started",
                            ApiEvent::WorkerStatusUpdate { .. } => "worker_status",
                            ApiEvent::WorkerCompleted { .. } => "worker_completed",
                            ApiEvent::BranchStarted { .. } => "branch_started",
                            ApiEvent::BranchCompleted { .. } => "branch_completed",
                            ApiEvent::ToolStarted { .. } => "tool_started",
                            ApiEvent::ToolCompleted { .. } => "tool_completed",
                            ApiEvent::ConfigReloaded => "config_reloaded",
                        };
                        yield Ok(axum::response::sse::Event::default()
                            .event(event_type)
                            .data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    tracing::debug!(count, "SSE client lagged");
                    yield Ok(axum::response::sse::Event::default()
                        .event("lagged")
                        .data(format!("{{\"skipped\":{count}}}")));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}
