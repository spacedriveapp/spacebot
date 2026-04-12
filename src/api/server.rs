//! HTTP server setup: router, static file serving, and API route wiring.

use super::state::ApiState;
use super::{
    activity, agents, attachments, bindings, channels, config, cortex, cron, factory, ingest,
    integrations, links, mcp, memories, messaging, models, notifications, opencode_proxy, portal,
    projects, providers, secrets, settings, skills, ssh, system, tasks, tools, usage, wiki,
    workers,
};

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::any;
use rust_embed::Embed;
use serde_json::json;
use tower_http::cors::CorsLayer;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use std::net::SocketAddr;
use std::sync::Arc;

/// Embedded frontend assets from the Vite build output.
#[derive(Embed)]
#[folder = "interface/dist/"]
#[allow(unused)]
struct InterfaceAssets;

/// API documentation structure for utoipa.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Spacebot API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Spacebot agent system API"
    ),
    servers(
        (url = "/api")
    )
)]
struct ApiDoc;

/// Build the OpenApiRouter for all API routes.
/// This is separated so it can be used both by the server and the OpenAPI spec generator.
pub fn api_router() -> OpenApiRouter<Arc<ApiState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        // System routes
        .routes(routes!(system::health))
        .routes(routes!(system::idle))
        .routes(routes!(system::status))
        .routes(routes!(system::storage_status))
        .routes(routes!(system::backup_export))
        .routes(routes!(system::backup_restore))
        .routes(routes!(system::events_sse))
        // Agent routes
        .routes(routes!(agents::instance_overview))
        .routes(routes!(agents::list_agents))
        .routes(routes!(agents::create_agent))
        .routes(routes!(agents::update_agent))
        .routes(routes!(agents::delete_agent))
        .routes(routes!(agents::list_agent_mcp))
        .routes(routes!(agents::reconnect_agent_mcp))
        .routes(routes!(agents::get_warmup_status))
        .routes(routes!(agents::trigger_warmup))
        .routes(routes!(agents::agent_overview))
        .routes(routes!(agents::get_agent_profile))
        .routes(routes!(
            agents::get_avatar,
            agents::upload_avatar,
            agents::delete_avatar
        ))
        .routes(routes!(agents::get_identity, agents::update_identity))
        // MCP routes
        .routes(routes!(
            mcp::list_mcp_servers,
            mcp::create_mcp_server,
            mcp::update_mcp_server
        ))
        .routes(routes!(mcp::delete_mcp_server))
        .routes(routes!(mcp::reconnect_mcp_server))
        .routes(routes!(mcp::mcp_status))
        // Channel routes
        .routes(routes!(channels::list_channels, channels::delete_channel))
        .routes(routes!(channels::set_channel_archive))
        .routes(routes!(channels::channel_messages))
        .routes(routes!(channels::channel_status))
        .routes(routes!(channels::inspect_prompt))
        .routes(routes!(channels::set_prompt_capture))
        .routes(routes!(channels::list_prompt_snapshots))
        .routes(routes!(channels::get_prompt_snapshot))
        .routes(routes!(channels::cancel_process))
        .routes(routes!(
            channels::get_channel_settings,
            channels::update_channel_settings
        ))
        // Worker routes
        .routes(routes!(workers::list_workers))
        .routes(routes!(workers::worker_detail))
        // Memory routes
        .routes(routes!(memories::list_memories))
        .routes(routes!(memories::search_memories))
        .routes(routes!(memories::memory_graph))
        .routes(routes!(memories::memory_graph_neighbors))
        // Cortex routes
        .routes(routes!(cortex::cortex_events))
        .routes(routes!(cortex::cortex_chat_messages))
        .routes(routes!(cortex::cortex_chat_threads))
        .routes(routes!(cortex::cortex_chat_delete_thread))
        .routes(routes!(cortex::cortex_chat_send))
        // Config routes
        .routes(routes!(
            config::get_agent_config,
            config::update_agent_config
        ))
        // Cron routes
        .routes(routes!(
            cron::list_cron_jobs,
            cron::create_or_update_cron,
            cron::delete_cron
        ))
        .routes(routes!(cron::cron_executions))
        .routes(routes!(cron::trigger_cron))
        .routes(routes!(cron::toggle_cron))
        // Notification routes
        .routes(routes!(notifications::list_notifications))
        .routes(routes!(notifications::unread_count))
        .routes(routes!(notifications::mark_read))
        .routes(routes!(notifications::dismiss_notification))
        .routes(routes!(notifications::mark_all_read))
        .routes(routes!(notifications::dismiss_read))
        // Task routes
        .routes(routes!(tasks::list_tasks, tasks::create_task))
        .routes(routes!(
            tasks::get_task,
            tasks::update_task,
            tasks::delete_task
        ))
        .routes(routes!(tasks::approve_task))
        .routes(routes!(tasks::execute_task))
        .routes(routes!(tasks::assign_task))
        // Wiki routes
        .routes(routes!(wiki::list_pages, wiki::create_page))
        .routes(routes!(wiki::search_pages))
        .routes(routes!(wiki::get_page, wiki::archive_page))
        .routes(routes!(wiki::edit_page))
        .routes(routes!(wiki::get_history))
        .routes(routes!(wiki::restore_version))
        // Project routes
        .routes(routes!(projects::list_projects, projects::create_project))
        .routes(routes!(projects::reorder_projects))
        .routes(routes!(
            projects::get_project,
            projects::update_project,
            projects::delete_project
        ))
        .routes(routes!(projects::scan_project))
        .routes(routes!(projects::serve_logo))
        .routes(routes!(projects::disk_usage))
        .routes(routes!(projects::create_repo))
        .routes(routes!(projects::delete_repo))
        .routes(routes!(projects::create_worktree))
        .routes(routes!(projects::delete_worktree))
        // Ingest routes
        .routes(routes!(
            ingest::list_ingest_files,
            ingest::delete_ingest_file
        ))
        .routes(routes!(ingest::upload_ingest_file))
        // Skill routes
        .routes(routes!(skills::list_skills))
        .routes(routes!(skills::get_skill_content))
        .routes(routes!(skills::install_skill))
        .routes(routes!(skills::upload_skill))
        .routes(routes!(skills::remove_skill))
        .routes(routes!(skills::registry_browse))
        .routes(routes!(skills::registry_search))
        .routes(routes!(skills::registry_skill_content))
        // Tool routes
        .routes(routes!(tools::list_tools))
        // Secret routes
        .routes(routes!(secrets::secrets_status))
        .routes(routes!(secrets::list_secrets))
        .routes(routes!(secrets::put_secret, secrets::delete_secret))
        .routes(routes!(secrets::secret_info))
        .routes(routes!(secrets::migrate_secrets))
        .routes(routes!(secrets::enable_encryption))
        .routes(routes!(secrets::unlock_secrets))
        .routes(routes!(secrets::lock_secrets))
        .routes(routes!(secrets::rotate_key))
        .routes(routes!(secrets::export_secrets))
        .routes(routes!(secrets::import_secrets))
        // Provider routes
        .routes(routes!(
            providers::get_providers,
            providers::update_provider
        ))
        .routes(routes!(providers::start_openai_browser_oauth))
        .routes(routes!(providers::openai_browser_oauth_status))
        .routes(routes!(providers::test_provider_model))
        .routes(routes!(providers::delete_provider))
        .routes(routes!(providers::get_provider_config))
        // Model routes
        .routes(routes!(models::get_models))
        .routes(routes!(models::refresh_models))
        // Messaging routes
        .routes(routes!(messaging::messaging_status))
        .routes(routes!(messaging::disconnect_platform))
        .routes(routes!(messaging::toggle_platform))
        .routes(routes!(
            messaging::create_messaging_instance,
            messaging::delete_messaging_instance
        ))
        // Binding routes
        .routes(routes!(
            bindings::list_bindings,
            bindings::create_binding,
            bindings::update_binding,
            bindings::delete_binding
        ))
        // Settings routes
        .routes(routes!(
            settings::get_global_settings,
            settings::update_global_settings
        ))
        .routes(routes!(
            settings::get_raw_config,
            settings::update_raw_config
        ))
        .routes(routes!(settings::update_check, settings::update_check_now))
        .routes(routes!(settings::update_apply))
        .routes(routes!(settings::changelog))
        // Integration routes
        .routes(routes!(integrations::get_integrations))
        .routes(routes!(integrations::update_integration))
        // SSH routes
        .routes(routes!(ssh::set_authorized_key))
        .routes(routes!(ssh::ssh_status))
        // Portal routes
        .routes(routes!(portal::portal_send))
        .routes(routes!(portal::portal_history))
        .routes(routes!(portal::list_portal_conversations))
        .routes(routes!(portal::create_portal_conversation))
        .routes(routes!(portal::update_portal_conversation))
        .routes(routes!(portal::delete_portal_conversation))
        .routes(routes!(portal::conversation_defaults))
        // Attachment routes
        .routes(routes!(attachments::upload_attachment))
        .routes(routes!(attachments::serve_attachment))
        .routes(routes!(attachments::list_attachments))
        // Link routes
        .routes(routes!(links::list_links, links::create_link))
        .routes(routes!(links::update_link, links::delete_link))
        .routes(routes!(links::agent_links))
        .routes(routes!(links::topology))
        .routes(routes!(links::list_groups, links::create_group))
        .routes(routes!(links::update_group, links::delete_group))
        .routes(routes!(links::list_humans, links::create_human))
        .routes(routes!(links::update_human, links::delete_human))
        // Usage routes
        .routes(routes!(usage::get_usage))
        .routes(routes!(usage::get_conversation_usage))
        // Activity routes
        .routes(routes!(activity::get_activity))
        // Factory routes
        .routes(routes!(factory::list_presets))
        .routes(routes!(factory::get_preset))
}

/// Start the HTTP server on the given address.
///
/// The caller provides a pre-built `ApiState` so agent event streams and
/// DB pools can be registered after startup.
pub async fn start_http_server(
    bind: SocketAddr,
    state: Arc<ApiState>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    // Note: credentials are intentionally disabled. The API uses Bearer
    // token auth (Authorization header), not cookies. Enabling credentials
    // with mirror_request origin would allow any site to make credentialed
    // cross-origin requests if cookie-based auth were ever added.
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

    // Build the OpenAPI router and split into routes and spec
    let (api_routes, api) = api_router().split_for_parts();

    // Create protected routes (require auth)
    let protected_routes = Router::new()
        // API routes under /api
        .nest("/api", api_routes)
        // Swagger UI and OpenAPI spec (protected)
        .merge(utoipa_swagger_ui::SwaggerUi::new("/api/docs").url("/api/openapi.json", api))
        // Opencode proxy routes (protected)
        .route(
            "/api/opencode/{port}/{*path}",
            any(opencode_proxy::opencode_proxy),
        )
        .route("/api/opencode/{port}", any(opencode_proxy::opencode_proxy))
        .route("/api/opencode/{port}/", any(opencode_proxy::opencode_proxy))
        // Apply auth middleware to all protected routes
        .layer(middleware::from_fn_with_state(
            state.clone(),
            api_auth_middleware,
        ));

    #[cfg(feature = "metrics")]
    let protected_routes = protected_routes.layer(middleware::from_fn(metrics_middleware));

    // Build the main application router
    let app = Router::new()
        // Mount all protected routes
        .merge(protected_routes)
        // Static file handler for frontend (unprotected)
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

#[cfg(feature = "metrics")]
async fn metrics_middleware(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let path = normalize_api_path(request.uri().path());

    let start = std::time::Instant::now();
    let response = next.run(request).await;
    let duration = start.elapsed().as_secs_f64();

    let status = response.status().as_u16().to_string();
    let metrics = crate::telemetry::Metrics::global();
    metrics
        .http_requests_total
        .with_label_values(&[&method, &path, &status])
        .inc();
    metrics
        .http_request_duration_seconds
        .with_label_values(&[&method, &path])
        .observe(duration);

    response
}

/// Normalize API path to prevent label cardinality explosion.
///
/// Replaces dynamic segments (UUIDs, numeric IDs, names in known positions)
/// with placeholder tokens.
#[cfg(feature = "metrics")]
fn normalize_api_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let mut normalized = Vec::with_capacity(parts.len());

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            normalized.push(*part);
            continue;
        }
        // UUID pattern (8-4-4-4-12 hex)
        if part.len() == 36 && part.chars().filter(|c| *c == '-').count() == 4 {
            normalized.push("{id}");
        // Purely numeric segment
        } else if part.chars().all(|c| c.is_ascii_digit()) {
            normalized.push("{number}");
        // Dynamic name segments after known resource paths
        } else if i >= 2 {
            let parent = parts.get(i - 1).copied().unwrap_or("");
            match parent {
                "secrets" | "groups" | "humans" | "links" => normalized.push("{name}"),
                "servers" | "providers" => normalized.push("{name}"),
                "opencode" => normalized.push("{port}"),
                "agents"
                    if !matches!(
                        *part,
                        "mcp"
                            | "warmup"
                            | "overview"
                            | "workers"
                            | "memories"
                            | "profile"
                            | "identity"
                            | "config"
                            | "cron"
                            | "tasks"
                            | "ingest"
                            | "skills"
                            | "tools"
                            | "links"
                    ) =>
                {
                    normalized.push("{id}")
                }
                _ => {
                    // Collapse any remaining segments after an opencode
                    // port placeholder to avoid high-cardinality proxy
                    // paths like /api/opencode/{port}/v1/chat/completions.
                    if normalized.contains(&"{port}") {
                        normalized.push("{proxy_path}");
                        break;
                    }
                    normalized.push(part);
                }
            }
        } else {
            normalized.push(part);
        }
    }

    normalized.join("/")
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
