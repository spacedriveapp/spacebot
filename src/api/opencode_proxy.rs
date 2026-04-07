//! Reverse proxy for OpenCode API and SSE streams.
//!
//! Forwards requests from `/api/opencode/{port}/{path}` to the OpenCode
//! server running on `127.0.0.1:{port}`. The embedded OpenCode SPA (mounted
//! via Shadow DOM in the Spacebot interface) makes all API and SSE calls
//! through this same-origin proxy, avoiding CORS issues and working on
//! hosted Fly instances where the OpenCode server is on localhost inside
//! the VM.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::TryStreamExt;

/// Check if a header is a hop-by-hop header that must not be forwarded.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        *name,
        header::CONNECTION
            | header::TRANSFER_ENCODING
            | header::UPGRADE
            | header::TE
            | header::TRAILER
    ) || name.as_str() == "keep-alive"
        || name.as_str() == "proxy-authenticate"
        || name.as_str() == "proxy-authorization"
}

/// Port range used by `OpenCodeServerPool` (deterministic hash of directory path).
const PORT_MIN: u16 = 10000;
const PORT_MAX: u16 = 60000;

/// Reverse proxy handler. Matches `/api/opencode/{port}/{*path}`.
///
/// Validates the port is in the OpenCode deterministic range, then forwards
/// the full request (method, headers, body, query string) to the local
/// OpenCode server. Streams the response back, supporting SSE connections.
pub(super) async fn opencode_proxy(request: Request) -> Response {
    let uri = request.uri().clone();
    let path = uri.path();

    // Parse port and remainder from the path.
    // The route is defined directly with the `/api/opencode/` prefix, so we
    // strip that to extract the port and remainder. Expected path format:
    // `/api/opencode/{port}/{*path}`.
    let after_prefix = match path.strip_prefix("/api/opencode/") {
        Some(rest) => rest,
        None => return (StatusCode::BAD_REQUEST, "invalid proxy path").into_response(),
    };

    let (port_str, remainder) = match after_prefix.split_once('/') {
        Some((p, r)) => (p, r),
        None => (after_prefix, ""),
    };

    let port: u16 = match port_str.parse() {
        Ok(p) if (PORT_MIN..=PORT_MAX).contains(&p) => p,
        Ok(_) => return (StatusCode::BAD_REQUEST, "port out of allowed range").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid port").into_response(),
    };

    // Build target URL preserving query string
    let target_url = match uri.query() {
        Some(query) => format!("http://127.0.0.1:{port}/{remainder}?{query}"),
        None => format!("http://127.0.0.1:{port}/{remainder}"),
    };

    // Build the proxied request
    let method = request.method().clone();

    // Use a shared client so the connection pool (and in-flight SSE streams)
    // are not dropped when the handler function returns.  reqwest's Client is
    // Arc-based, but the connection pool shutdown on last-Arc-drop can race
    // with long-lived response streams.
    use std::sync::LazyLock;
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            // Disable pooled connection idle timeout — SSE streams are long-lived
            .pool_idle_timeout(None)
            .build()
            .unwrap_or_default()
    });
    let client = &*CLIENT;

    let mut proxy_request = client.request(method, &target_url);

    // Forward headers, skipping hop-by-hop, host, and accept-encoding.
    // Stripping accept-encoding prevents the upstream from compressing the
    // response, which would break incremental SSE streaming through the proxy.
    for (name, value) in request.headers() {
        if name == header::HOST || name == header::ACCEPT_ENCODING || is_hop_by_hop(name) {
            continue;
        }
        proxy_request = proxy_request.header(name.clone(), value.clone());
    }

    // Forward request body
    let body_bytes = match axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "failed to read proxy request body");
            return (StatusCode::BAD_REQUEST, "failed to read request body").into_response();
        }
    };

    if !body_bytes.is_empty() {
        proxy_request = proxy_request.body(body_bytes);
    }

    // Send the request
    let upstream_response = match proxy_request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(%error, port, "OpenCode proxy: upstream unreachable");
            return (StatusCode::BAD_GATEWAY, "OpenCode server unreachable").into_response();
        }
    };

    // Build the response, streaming the body (supports SSE)
    let status = upstream_response.status();
    let mut response_builder = Response::builder().status(status.as_u16());

    // Forward response headers, skipping hop-by-hop
    for (name, value) in upstream_response.headers() {
        if is_hop_by_hop(name) {
            continue;
        }
        response_builder = response_builder.header(name.clone(), value.clone());
    }

    let body_stream = upstream_response
        .bytes_stream()
        .map_err(std::io::Error::other);

    match response_builder.body(Body::from_stream(body_stream)) {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "failed to build proxy response");
            (StatusCode::INTERNAL_SERVER_ERROR, "proxy response error").into_response()
        }
    }
}
