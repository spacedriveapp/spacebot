//! Framework-aware route and API endpoint detection.
//!
//! Scans source files for framework-specific patterns (decorators,
//! function names, file paths) that indicate HTTP route handlers.
//! Creates Route nodes with path/method metadata and HANDLES_ROUTE
//! edges from the handler function to its Route.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::lang;

fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn normalize_path(s: &str) -> String {
    s.replace('\\', "/")
}

/// A detected MCP tool definition.
struct DetectedTool {
    /// Tool name as registered with the MCP server.
    name: String,
    /// Qualified name of the handler function.
    handler_qname: String,
}

/// A detected route endpoint.
struct DetectedRoute {
    /// HTTP method (GET, POST, etc.) or "*" for catch-all.
    method: String,
    /// URL path pattern (e.g. "/api/users/:id").
    path: String,
    /// Qualified name of the handler function.
    handler_qname: String,
    /// Source file where the route was found.
    source_file: String,
}

/// Detect routes and create Route nodes + HANDLES_ROUTE edges.
pub async fn detect_routes(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    let mut routes: Vec<DetectedRoute> = Vec::new();
    let mut tools: Vec<DetectedTool> = Vec::new();

    for file_path in files {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if lang::provider_for_extension(ext).is_none() {
            continue;
        }

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        let relative = normalize_path(
            &file_path
                .strip_prefix(root_path)
                .unwrap_or(file_path)
                .to_string_lossy(),
        );

        detect_nextjs_routes(&relative, &content, &mut routes);
        detect_decorator_routes(&relative, &content, &mut routes);
        detect_tool_definitions(&relative, &content, &mut tools);
    }

    if routes.is_empty() {
        tracing::info!(project_id = %project_id, "no routes detected");
        return Ok(result);
    }

    // Query existing Function/Method nodes so we can validate handler qnames.
    let mut known_symbols: HashSet<String> = HashSet::new();
    for label in &["Function", "Method"] {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN n.qualified_name"
            ))
            .await?;
        for row in &rows {
            if let Some(lbug::Value::String(qn)) = row.first() {
                known_symbols.insert(qn.clone());
            }
        }
    }

    let mut node_stmts: Vec<String> = Vec::new();
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut seen_routes: HashSet<String> = HashSet::new();

    for route in &routes {
        let route_qname = format!(
            "{pid}::route::{}::{}",
            cypher_escape(&route.method),
            cypher_escape(&route.path)
        );
        if !seen_routes.insert(route_qname.clone()) {
            continue;
        }

        let route_qname_escaped = cypher_escape(&route_qname);
        let route_name = format!("{} {}", route.method, route.path);
        let route_name_escaped = cypher_escape(&route_name);
        let sf_escaped = cypher_escape(&route.source_file);
        let method_escaped = cypher_escape(&route.method);
        let path_escaped = cypher_escape(&route.path);

        node_stmts.push(format!(
            "CREATE (:Route {{qualified_name: '{route_qname_escaped}', \
             name: '{route_name_escaped}', project_id: '{pid}', \
             source_file: '{sf_escaped}', line_start: 0, line_end: 0, \
             source: 'pipeline', written_by: 'pipeline', \
             extends_type: '{method_escaped}', import_source: '{path_escaped}', \
             declared_type: ''}})"
        ));

        // HANDLES_ROUTE from handler → Route
        let handler_escaped = cypher_escape(&route.handler_qname);
        if known_symbols.contains(&route.handler_qname) {
            for label in &["Function", "Method"] {
                edge_stmts.push(format!(
                    "MATCH (h:{label}), (r:Route) WHERE h.qualified_name = '{handler_escaped}' \
                     AND h.project_id = '{pid}' AND r.qualified_name = '{route_qname_escaped}' \
                     CREATE (h)-[:CodeRelation {{type: 'HANDLES_ROUTE', confidence: 0.90, reason: 'framework', step: 0}}]->(r)"
                ));
            }
        }
    }

    // HANDLES_TOOL edges: link handler functions to their tool names.
    // Tool definitions don't get their own node type — the tool name
    // is stored in the edge reason field, keeping the schema compact.
    for tool in &tools {
        if !known_symbols.contains(&tool.handler_qname) {
            continue;
        }
        let handler_escaped = cypher_escape(&tool.handler_qname);
        let tool_name_escaped = cypher_escape(&tool.name);
        for label in &["Function", "Method"] {
            edge_stmts.push(format!(
                "MATCH (h:{label}) WHERE h.qualified_name = '{handler_escaped}' \
                 AND h.project_id = '{pid}' \
                 CREATE (h)-[:CodeRelation {{type: 'HANDLES_TOOL', confidence: 0.85, \
                 reason: '{tool_name_escaped}', step: 0}}]->(h)"
            ));
        }
    }

    if !node_stmts.is_empty() {
        let batch = db.execute_batch(node_stmts).await?;
        result.nodes_created += batch.success;
        result.errors += batch.errors;
    }
    if !edge_stmts.is_empty() {
        for chunk in edge_stmts.chunks(100) {
            let batch = db.execute_batch(chunk.to_vec()).await?;
            result.edges_created += batch.success;
            result.errors += batch.errors;
        }
    }

    tracing::info!(
        project_id = %project_id,
        routes = seen_routes.len(),
        tools = tools.len(),
        nodes = result.nodes_created,
        edges = result.edges_created,
        "route and tool detection complete"
    );

    Ok(result)
}

/// Next.js file-based routing: pages/*.tsx or app/**/page.tsx become
/// routes. The default export is the handler.
fn detect_nextjs_routes(relative: &str, _content: &str, routes: &mut Vec<DetectedRoute>) {
    // Pages router: pages/api/foo.ts → GET /api/foo
    if let Some(rest) = relative.strip_prefix("pages/") {
        let path = rest
            .trim_end_matches(".tsx")
            .trim_end_matches(".ts")
            .trim_end_matches(".jsx")
            .trim_end_matches(".js");
        if path == "_app" || path == "_document" || path == "_error" {
            return;
        }
        let url = if path == "index" {
            "/".to_string()
        } else {
            format!("/{}", path.replace("/index", "").replace('[', ":").replace(']', ""))
        };
        routes.push(DetectedRoute {
            method: "*".to_string(),
            path: url,
            handler_qname: format!("{relative}::default"),
            source_file: relative.to_string(),
        });
        return;
    }

    // App router: app/foo/page.tsx → GET /foo
    if relative.starts_with("app/") || relative.starts_with("src/app/") {
        let stem = relative
            .trim_start_matches("src/")
            .trim_start_matches("app/");
        if stem.ends_with("/page.tsx")
            || stem.ends_with("/page.ts")
            || stem.ends_with("/page.jsx")
            || stem.ends_with("/page.js")
        {
            let dir = stem.rsplit('/').skip(1).collect::<Vec<_>>();
            let path = if dir.is_empty() {
                "/".to_string()
            } else {
                let joined: String = dir.into_iter().rev().collect::<Vec<_>>().join("/");
                format!("/{}", joined.replace('[', ":").replace(']', ""))
            };
            routes.push(DetectedRoute {
                method: "GET".to_string(),
                path,
                handler_qname: format!("{relative}::default"),
                source_file: relative.to_string(),
            });
        }
        // app/foo/route.ts → API route
        if stem.ends_with("/route.tsx")
            || stem.ends_with("/route.ts")
            || stem.ends_with("/route.js")
        {
            let dir = stem.rsplit('/').skip(1).collect::<Vec<_>>();
            let path = if dir.is_empty() {
                "/".to_string()
            } else {
                let joined: String = dir.into_iter().rev().collect::<Vec<_>>().join("/");
                format!("/{}", joined.replace('[', ":").replace(']', ""))
            };
            for method in &["GET", "POST", "PUT", "DELETE", "PATCH"] {
                routes.push(DetectedRoute {
                    method: method.to_string(),
                    path: path.clone(),
                    handler_qname: format!("{relative}::{method}"),
                    source_file: relative.to_string(),
                });
            }
        }
    }
}

/// Scan for framework decorator/call patterns that define routes:
/// - Python: @app.route("/path"), @app.get("/path"), @router.post("/path")
/// - Express: app.get("/path", handler), router.post("/path", handler)
/// - Rust: #[get("/path")], #[post("/path")]
fn detect_decorator_routes(relative: &str, content: &str, routes: &mut Vec<DetectedRoute>) {
    // Simple line-by-line scan — not AST-based but catches the common
    // patterns with minimal cost. Covers Flask, FastAPI, Express, Actix,
    // Rocket, Axum, Spring (@GetMapping etc.), Laravel (Route::get), etc.
    let patterns: &[(&str, &str)] = &[
        ("@app.route(", "*"),
        ("@app.get(", "GET"),
        ("@app.post(", "POST"),
        ("@app.put(", "PUT"),
        ("@app.delete(", "DELETE"),
        ("@app.patch(", "PATCH"),
        ("@router.get(", "GET"),
        ("@router.post(", "POST"),
        ("@router.put(", "PUT"),
        ("@router.delete(", "DELETE"),
        ("@router.patch(", "PATCH"),
        ("app.get(", "GET"),
        ("app.post(", "POST"),
        ("app.put(", "PUT"),
        ("app.delete(", "DELETE"),
        ("app.patch(", "PATCH"),
        ("router.get(", "GET"),
        ("router.post(", "POST"),
        ("router.put(", "PUT"),
        ("router.delete(", "DELETE"),
        ("router.patch(", "PATCH"),
        ("#[get(", "GET"),
        ("#[post(", "POST"),
        ("#[put(", "PUT"),
        ("#[delete(", "DELETE"),
        ("#[patch(", "PATCH"),
        ("@GetMapping(", "GET"),
        ("@PostMapping(", "POST"),
        ("@PutMapping(", "PUT"),
        ("@DeleteMapping(", "DELETE"),
        ("@PatchMapping(", "PATCH"),
        ("@RequestMapping(", "*"),
        ("Route::get(", "GET"),
        ("Route::post(", "POST"),
        ("Route::put(", "PUT"),
        ("Route::delete(", "DELETE"),
        ("Route::patch(", "PATCH"),
    ];

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        for (pattern, method) in patterns {
            if let Some(pos) = trimmed.find(pattern) {
                let after = &trimmed[pos + pattern.len()..];
                let path = extract_string_arg(after);
                if let Some(path) = path {
                    // Pending handler — will be patched to the next
                    // function definition when the fn/def line is seen.
                    routes.push(DetectedRoute {
                        method: method.to_string(),
                        path,
                        handler_qname: format!("{relative}::__pending_{line_idx}"),
                        source_file: relative.to_string(),
                    });
                }
                break;
            }
        }

        // Track function definitions to resolve pending handlers
        if (trimmed.starts_with("def ")
            || trimmed.starts_with("async def ")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("function ")
            || trimmed.starts_with("export function ")
            || trimmed.starts_with("export default function ")
            || trimmed.starts_with("export async function "))
            && let Some(name) = extract_fn_name(trimmed)
        {
            let fn_qname = format!("{relative}::{name}");
            // Patch the most recent pending route's handler
            if let Some(last) = routes.last_mut()
                && last.handler_qname.contains("__pending_")
            {
                last.handler_qname = fn_qname.clone();
            }
            let _ = fn_qname;
        }
    }
}

/// Extract the first quoted string argument from a pattern like
/// `"/api/users")` or `'/api/users', ...`.
fn extract_string_arg(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else if let Some(rest) = s.strip_prefix('\'') {
        let end = rest.find('\'')?;
        Some(rest[..end].to_string())
    } else {
        None
    }
}

/// Extract a function name from a def/fn/function line.
fn extract_fn_name(line: &str) -> Option<String> {
    let line = line
        .trim_start_matches("export ")
        .trim_start_matches("default ")
        .trim_start_matches("pub ")
        .trim_start_matches("async ")
        .trim_start_matches("def ")
        .trim_start_matches("fn ")
        .trim_start_matches("function ");
    let name: String = line
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Detect MCP tool definitions by scanning for common SDK patterns:
/// - `server.tool("name", ...)` (TypeScript MCP SDK)
/// - `@server.tool()` / `@mcp.tool()` decorators (Python)
/// - `.tool("name", handler)` method calls
/// - `Tool { name: "...", ... }` struct literals (Rust)
fn detect_tool_definitions(relative: &str, content: &str, tools: &mut Vec<DetectedTool>) {
    let tool_patterns: &[&str] = &[
        "server.tool(",
        ".tool(",
        "Tool::new(",
        "@server.tool",
        "@mcp.tool",
        "add_tool(",
        "register_tool(",
    ];

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        for pattern in tool_patterns {
            if let Some(pos) = trimmed.find(pattern) {
                let after = &trimmed[pos + pattern.len()..];
                if let Some(name) = extract_string_arg(after)
                    && !name.is_empty()
                {
                    tools.push(DetectedTool {
                        name: name.clone(),
                        handler_qname: format!("{relative}::__tool_pending_{line_idx}"),
                    });
                }
                break;
            }
        }

        // Resolve pending tool handlers to the next function definition
        if (trimmed.starts_with("def ")
            || trimmed.starts_with("async def ")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("function ")
            || trimmed.starts_with("export function ")
            || trimmed.starts_with("export async function "))
            && let Some(name) = extract_fn_name(trimmed)
        {
            let fn_qname = format!("{relative}::{name}");
            if let Some(last) = tools.last_mut()
                && last.handler_qname.contains("__tool_pending_")
            {
                last.handler_qname = fn_qname;
            }
        }
    }

    // Drop any tools whose handler was never resolved
    tools.retain(|t| !t.handler_qname.contains("__tool_pending_"));
}
