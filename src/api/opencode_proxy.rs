//! Reverse proxy for OpenCode web UI.
//!
//! Forwards requests from `/api/opencode/{port}/{path}` to the OpenCode
//! server running on `127.0.0.1:{port}`. This keeps the iframe same-origin
//! with the Spacebot interface, avoiding CSP/CORS issues and working on
//! hosted Fly instances where the OpenCode server is on localhost inside
//! the VM.
//!
//! For HTML responses (the SPA shell), we inject a `<base>` tag so asset
//! URLs resolve through the proxy path, and a script that sets the OpenCode
//! SDK's `defaultServerUrl` in localStorage so API calls also route through
//! the proxy rather than hitting `location.origin` directly.

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
///
/// HTML responses are intercepted and rewritten to inject:
/// - A `<base href>` tag so relative asset URLs resolve through the proxy
/// - A script that sets `localStorage` so the OpenCode SDK routes API calls
///   through the proxy path instead of `location.origin`
pub(super) async fn opencode_proxy(request: Request) -> Response {
    let uri = request.uri().clone();
    let path = uri.path();

    // Parse port and remainder from the path.
    // Path format: /api/opencode/{port}/{rest...}
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
    let client = reqwest::Client::builder()
        .no_proxy()
        // No read timeout — SSE connections are long-lived
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut proxy_request = client.request(method, &target_url);

    // Forward headers, skipping hop-by-hop and host
    for (name, value) in request.headers() {
        if name == header::HOST || is_hop_by_hop(name) {
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

    // Check if this is an HTML response that needs rewriting
    let is_html = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/html"));

    // Build the response, streaming the body
    let status = upstream_response.status();
    let mut response_builder = Response::builder().status(status.as_u16());

    // Forward response headers, skipping hop-by-hop (and content-length for HTML
    // since we'll modify the body)
    for (name, value) in upstream_response.headers() {
        if is_hop_by_hop(name) {
            continue;
        }
        if is_html && name == header::CONTENT_LENGTH {
            continue;
        }
        response_builder = response_builder.header(name.clone(), value.clone());
    }

    if is_html {
        // Buffer the HTML body and inject base href + SDK URL override
        let html_bytes = match upstream_response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, "failed to read HTML response body");
                return (StatusCode::BAD_GATEWAY, "failed to read upstream HTML").into_response();
            }
        };

        let html = String::from_utf8_lossy(&html_bytes);
        let proxy_base = format!("/api/opencode/{port}/");

        // Inject <base href> for asset resolution and a script to set the SDK
        // base URL in localStorage. The OpenCode SPA checks localStorage key
        // "opencode.settings.dat:defaultServerUrl" before falling back to
        // location.origin. By setting it to our proxy path, all SDK fetch
        // calls route through /api/opencode/{port}/... which we proxy to
        // the real OpenCode server.
        let injection = format!(
            "<base href=\"{proxy_base}\">\
             <script>\
             (function(){{\
               try{{\
                 var key='opencode.settings.dat:defaultServerUrl';\
                 var url=location.origin+'{proxy_base}';\
                 if(url.endsWith('/'))url=url.slice(0,-1);\
                 localStorage.setItem(key,url);\
               }}catch(e){{}}\
             }})();\
             </script>"
        );

        let rewritten = if let Some(head_pos) = html.find("<head>") {
            let insert_at = head_pos + "<head>".len();
            format!("{}{injection}{}", &html[..insert_at], &html[insert_at..])
        } else if let Some(head_pos) = html.find("<HEAD>") {
            let insert_at = head_pos + "<HEAD>".len();
            format!("{}{injection}{}", &html[..insert_at], &html[insert_at..])
        } else {
            // No <head> tag found — prepend the injection
            format!("{injection}{html}")
        };

        match response_builder.body(Body::from(rewritten)) {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "failed to build rewritten HTML response");
                (StatusCode::INTERNAL_SERVER_ERROR, "proxy response error").into_response()
            }
        }
    } else {
        // Non-HTML: stream the response body as-is (supports SSE)
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
}
