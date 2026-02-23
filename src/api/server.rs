//! HTTP server setup: router, static file serving, and API route wiring.

use super::state::ApiState;
use super::{
    agents, bindings, channels, config, cortex, cron, ingest, mcp, memories, messaging, models,
    providers, settings, skills, system, webchat,
};

use axum::Json;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use rust_embed::Embed;
use serde_json::json;
use tower_http::cors::CorsLayer;

use std::net::SocketAddr;
use std::sync::Arc;

/// Embedded frontend assets from the Vite build output.
#[derive(Embed)]
#[folder = "interface/dist/"]
#[allow(unused)]
struct InterfaceAssets;

/// Start the HTTP server on the given address.
///
/// The caller provides a pre-built `ApiState` so agent event streams and
/// DB pools can be registered after startup.
pub async fn start_http_server(
    bind: SocketAddr,
    state: Arc<ApiState>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let cors = CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT]);

    let api_routes = Router::new()
        .route("/health", get(system::health))
        .route("/idle", get(system::idle))
        .route("/status", get(system::status))
        .route("/system/storage", get(system::storage_status))
        .route("/system/backup/export", get(system::backup_export))
        .route("/system/backup/restore", post(system::backup_restore))
        .route("/overview", get(agents::instance_overview))
        .route("/events", get(system::events_sse))
        .route(
            "/agents",
            get(agents::list_agents)
                .post(agents::create_agent)
                .delete(agents::delete_agent),
        )
        .route("/agents/mcp", get(agents::list_agent_mcp))
        .route("/agents/mcp/reconnect", post(agents::reconnect_agent_mcp))
        .route(
            "/mcp/servers",
            get(mcp::list_mcp_servers)
                .post(mcp::create_mcp_server)
                .put(mcp::update_mcp_server),
        )
        .route("/mcp/servers/{name}", delete(mcp::delete_mcp_server))
        .route(
            "/mcp/servers/{name}/reconnect",
            post(mcp::reconnect_mcp_server),
        )
        .route("/mcp/status", get(mcp::mcp_status))
        .route("/agents/overview", get(agents::agent_overview))
        .route("/channels", get(channels::list_channels))
        .route("/channels/messages", get(channels::channel_messages))
        .route("/channels/status", get(channels::channel_status))
        .route("/agents/memories", get(memories::list_memories))
        .route("/agents/memories/search", get(memories::search_memories))
        .route("/agents/memories/graph", get(memories::memory_graph))
        .route(
            "/agents/memories/graph/neighbors",
            get(memories::memory_graph_neighbors),
        )
        .route("/cortex/events", get(cortex::cortex_events))
        .route("/cortex-chat/messages", get(cortex::cortex_chat_messages))
        .route("/cortex-chat/send", post(cortex::cortex_chat_send))
        .route("/agents/profile", get(agents::get_agent_profile))
        .route(
            "/agents/identity",
            get(agents::get_identity).put(agents::update_identity),
        )
        .route(
            "/agents/config",
            get(config::get_agent_config).put(config::update_agent_config),
        )
        .route(
            "/agents/cron",
            get(cron::list_cron_jobs)
                .post(cron::create_or_update_cron)
                .delete(cron::delete_cron),
        )
        .route("/agents/cron/executions", get(cron::cron_executions))
        .route("/agents/cron/trigger", post(cron::trigger_cron))
        .route("/agents/cron/toggle", put(cron::toggle_cron))
        .route("/channels/cancel", post(channels::cancel_process))
        .route(
            "/agents/ingest/files",
            get(ingest::list_ingest_files).delete(ingest::delete_ingest_file),
        )
        .route("/agents/ingest/upload", post(ingest::upload_ingest_file))
        .route("/agents/skills", get(skills::list_skills))
        .route("/agents/skills/install", post(skills::install_skill))
        .route("/agents/skills/remove", delete(skills::remove_skill))
        .route("/skills/registry/browse", get(skills::registry_browse))
        .route("/skills/registry/search", get(skills::registry_search))
        .route(
            "/providers",
            get(providers::get_providers).put(providers::update_provider),
        )
        .route("/providers/test", post(providers::test_provider_model))
        .route("/providers/{provider}", delete(providers::delete_provider))
        .route("/models", get(models::get_models))
        .route("/models/refresh", post(models::refresh_models))
        .route("/messaging/status", get(messaging::messaging_status))
        .route(
            "/messaging/disconnect",
            post(messaging::disconnect_platform),
        )
        .route("/messaging/toggle", post(messaging::toggle_platform))
        .route(
            "/bindings",
            get(bindings::list_bindings)
                .post(bindings::create_binding)
                .put(bindings::update_binding)
                .delete(bindings::delete_binding),
        )
        .route(
            "/settings",
            get(settings::get_global_settings).put(settings::update_global_settings),
        )
        .route(
            "/config/raw",
            get(settings::get_raw_config).put(settings::update_raw_config),
        )
        .route(
            "/update/check",
            get(settings::update_check).post(settings::update_check_now),
        )
        .route("/update/apply", post(settings::update_apply))
        .route("/webchat/send", post(webchat::webchat_send))
        .route("/webchat/history", get(webchat::webchat_history))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            api_auth_middleware,
        ));

    let app = Router::new()
        .nest("/api", api_routes)
        .fallback(static_handler)
        .layer(cors)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)) // 10 MiB
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "HTTP server listening");

    let handle = tokio::spawn(async move {
        let mut shutdown = shutdown_rx;
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.wait_for(|v| *v).await;
            })
            .await
        {
            tracing::error!(%error, "HTTP server exited with error");
        }
    });

    Ok(handle)
}

async fn api_auth_middleware(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected_token) = state.auth_token.as_deref() else {
        return next.run(request).await;
    };

    let path = request.uri().path();
    if path == "/api/health" || path == "/health" {
        return next.run(request).await;
    }

    let is_authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| token == expected_token);

    if is_authorized {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response()
    }
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(content) = InterfaceAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            content.data,
        )
            .into_response();
    }

    if let Some(content) = InterfaceAssets::get("index.html") {
        return Html(std::str::from_utf8(&content.data).unwrap_or("").to_string()).into_response();
    }

    (StatusCode::NOT_FOUND, "not found").into_response()
}
