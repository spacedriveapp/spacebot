//! API handlers for MCP server management.
//!
//! CRUD endpoints for `[[mcp_servers]]` in config.toml, plus per-agent
//! connection status and force-reconnect.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ── Request / Response types ────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct CreateMcpServerRequest {
    pub id: String,
    pub transport: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub scopes: Vec<crate::mcp::McpScope>,
}

#[derive(Serialize)]
pub(super) struct McpServerInfo {
    pub id: String,
    pub transport: String,
    pub state: String,
    pub tool_count: usize,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct McpAgentStatus {
    pub agent_id: String,
    pub servers: Vec<McpServerInfo>,
}

#[derive(Serialize)]
pub(super) struct MutationResponse {
    pub success: bool,
    pub message: String,
}

// ── Handlers ────────────────────────────────────────────────────────────

/// GET /api/mcp/servers — list all configured MCP servers with connection status.
pub(super) async fn list_mcp_servers(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<McpServerInfo>>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(Vec::new()));
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Parse config and collect server definitions in a non-async block
    // (toml_edit::DocumentMut is !Send and can't be held across await points)
    let server_defs: Vec<(String, String)> = {
        let doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let mut defs = Vec::new();
        if let Some(arr) = doc.get("mcp_servers").and_then(|v| v.as_array_of_tables()) {
            for table in arr.iter() {
                let id = table
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let transport = table
                    .get("transport")
                    .and_then(|v| v.as_str())
                    .unwrap_or("stdio")
                    .to_string();
                defs.push((id, transport));
            }
        }
        defs
    };

    // Now look up live status (requires await)
    let mut servers = Vec::with_capacity(server_defs.len());
    for (id, transport) in server_defs {
        let (state_str, tool_count, error) = get_server_status(&state, &id).await;
        servers.push(McpServerInfo {
            id,
            transport,
            state: state_str,
            tool_count,
            error,
        });
    }

    Ok(Json(servers))
}

/// POST /api/mcp/servers — add a new MCP server definition to config.toml.
pub(super) async fn create_mcp_server(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateMcpServerRequest>,
) -> Result<Json<MutationResponse>, StatusCode> {
    if request.id.trim().is_empty() {
        return Ok(Json(MutationResponse {
            success: false,
            message: "Server ID cannot be empty".into(),
        }));
    }

    let config_path = state.config_path.read().await.clone();
    let content = if config_path.exists() {
        tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Check for duplicates
    if let Some(arr) = doc.get("mcp_servers").and_then(|v| v.as_array_of_tables()) {
        for table in arr.iter() {
            if table.get("id").and_then(|v| v.as_str()) == Some(&request.id) {
                return Ok(Json(MutationResponse {
                    success: false,
                    message: format!("MCP server '{}' already exists", request.id),
                }));
            }
        }
    }

    // Build the new table
    let mut new_table = toml_edit::Table::new();
    new_table.set_implicit(true);
    new_table["id"] = toml_edit::value(&request.id);
    new_table["transport"] = toml_edit::value(&request.transport);

    if let Some(cmd) = &request.command {
        new_table["command"] = toml_edit::value(cmd);
    }
    if !request.args.is_empty() {
        let mut arr = toml_edit::Array::new();
        for arg in &request.args {
            arr.push(arg.as_str());
        }
        new_table["args"] = toml_edit::value(arr);
    }
    if let Some(url) = &request.url {
        new_table["url"] = toml_edit::value(url);
    }
    if !request.env.is_empty() {
        let mut env_table = toml_edit::InlineTable::new();
        for (k, v) in &request.env {
            env_table.insert(k, v.as_str().into());
        }
        new_table["env"] = toml_edit::value(env_table);
    }
    if !request.headers.is_empty() {
        let mut headers_table = toml_edit::InlineTable::new();
        for (k, v) in &request.headers {
            headers_table.insert(k, v.as_str().into());
        }
        new_table["headers"] = toml_edit::value(headers_table);
    }

    // Append to [[mcp_servers]] array
    if doc.get("mcp_servers").is_none() {
        doc.insert(
            "mcp_servers",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    if let Some(arr) = doc
        .get_mut("mcp_servers")
        .and_then(|v| v.as_array_of_tables_mut())
    {
        arr.push(new_table);
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MutationResponse {
        success: true,
        message: format!("MCP server '{}' added", request.id),
    }))
}

/// PUT /api/mcp/servers — update an existing MCP server definition.
pub(super) async fn update_mcp_server(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateMcpServerRequest>,
) -> Result<Json<MutationResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(MutationResponse {
            success: false,
            message: "No config file found".into(),
        }));
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(arr) = doc
        .get_mut("mcp_servers")
        .and_then(|v| v.as_array_of_tables_mut())
    else {
        return Ok(Json(MutationResponse {
            success: false,
            message: format!("MCP server '{}' not found", request.id),
        }));
    };

    let mut found = false;
    for table in arr.iter_mut() {
        if table.get("id").and_then(|v| v.as_str()) == Some(&request.id) {
            table["transport"] = toml_edit::value(&request.transport);
            if let Some(cmd) = &request.command {
                table["command"] = toml_edit::value(cmd);
            } else {
                table.remove("command");
            }
            if !request.args.is_empty() {
                let mut args_arr = toml_edit::Array::new();
                for arg in &request.args {
                    args_arr.push(arg.as_str());
                }
                table["args"] = toml_edit::value(args_arr);
            } else {
                table.remove("args");
            }
            if let Some(url) = &request.url {
                table["url"] = toml_edit::value(url);
            } else {
                table.remove("url");
            }
            found = true;
            break;
        }
    }

    if !found {
        return Ok(Json(MutationResponse {
            success: false,
            message: format!("MCP server '{}' not found", request.id),
        }));
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MutationResponse {
        success: true,
        message: format!("MCP server '{}' updated", request.id),
    }))
}

/// DELETE /api/mcp/servers/{id} — remove a server definition from config.toml.
pub(super) async fn delete_mcp_server(
    State(state): State<Arc<ApiState>>,
    Path(server_id): Path<String>,
) -> Result<Json<MutationResponse>, StatusCode> {
    let config_path = state.config_path.read().await.clone();
    if !config_path.exists() {
        return Ok(Json(MutationResponse {
            success: false,
            message: "No config file found".into(),
        }));
    }

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(arr) = doc
        .get_mut("mcp_servers")
        .and_then(|v| v.as_array_of_tables_mut())
    else {
        return Ok(Json(MutationResponse {
            success: false,
            message: format!("MCP server '{}' not found", server_id),
        }));
    };

    // Find and remove the matching entry
    let original_len = arr.len();
    let mut idx = 0;
    while idx < arr.len() {
        let matches = arr
            .get(idx)
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            == Some(&server_id);
        if matches {
            arr.remove(idx);
        } else {
            idx += 1;
        }
    }

    if arr.len() == original_len {
        return Ok(Json(MutationResponse {
            success: false,
            message: format!("MCP server '{}' not found", server_id),
        }));
    }

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(MutationResponse {
        success: true,
        message: format!("MCP server '{}' removed", server_id),
    }))
}

/// POST /api/mcp/servers/{id}/reconnect — force-reconnect a specific server.
#[cfg(feature = "mcp")]
pub(super) async fn reconnect_mcp_server(
    State(state): State<Arc<ApiState>>,
    Path(server_id): Path<String>,
) -> Result<Json<MutationResponse>, StatusCode> {
    let managers = state.mcp_managers.load();

    for manager in managers.values() {
        match manager.reconnect_server(&server_id).await {
            Ok(()) => {
                return Ok(Json(MutationResponse {
                    success: true,
                    message: format!("Server '{}' reconnected", server_id),
                }));
            }
            Err(crate::error::McpError::ConnectionFailed { reason, .. })
                if reason == "server not found" =>
            {
                continue;
            }
            Err(error) => {
                return Ok(Json(MutationResponse {
                    success: false,
                    message: format!("Reconnect failed: {error}"),
                }));
            }
        }
    }

    Ok(Json(MutationResponse {
        success: false,
        message: format!("Server '{}' not found in any agent", server_id),
    }))
}

#[cfg(not(feature = "mcp"))]
pub(super) async fn reconnect_mcp_server(
    Path(_server_id): Path<String>,
) -> Result<Json<MutationResponse>, StatusCode> {
    Ok(Json(MutationResponse {
        success: false,
        message: "MCP feature not enabled".into(),
    }))
}

/// GET /api/mcp/status — per-agent MCP connection status.
#[cfg(feature = "mcp")]
pub(super) async fn mcp_status(
    State(state): State<Arc<ApiState>>,
) -> Json<Vec<McpAgentStatus>> {
    let managers = state.mcp_managers.load();
    let mut result = Vec::with_capacity(managers.len());

    for (agent_id, manager) in managers.iter() {
        let statuses = manager.status().await;
        let servers = statuses
            .into_iter()
            .map(|s| {
                let error = if s.state.starts_with("failed") {
                    Some(s.state.clone())
                } else {
                    None
                };
                McpServerInfo {
                    id: s.server_id,
                    transport: String::new(),
                    state: if s.state.starts_with("connected") {
                        "connected".into()
                    } else if s.state.starts_with("failed") {
                        "failed".into()
                    } else {
                        s.state
                    },
                    tool_count: s.tool_count,
                    error,
                }
            })
            .collect();

        result.push(McpAgentStatus {
            agent_id: agent_id.clone(),
            servers,
        });
    }

    Json(result)
}

#[cfg(not(feature = "mcp"))]
pub(super) async fn mcp_status() -> Json<Vec<McpAgentStatus>> {
    Json(Vec::new())
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Look up live connection status for a server ID across all agents.
#[cfg(feature = "mcp")]
async fn get_server_status(state: &ApiState, server_id: &str) -> (String, usize, Option<String>) {
    let managers = state.mcp_managers.load();
    for manager in managers.values() {
        for status in manager.status().await {
            if status.server_id == server_id {
                let error = if status.state.starts_with("failed") {
                    Some(status.state.clone())
                } else {
                    None
                };
                let state_str = if status.state.starts_with("connected") {
                    "connected".into()
                } else if status.state.starts_with("failed") {
                    "failed".into()
                } else {
                    status.state
                };
                return (state_str, status.tool_count, error);
            }
        }
    }
    ("not_connected".into(), 0, None)
}

#[cfg(not(feature = "mcp"))]
async fn get_server_status(
    _state: &ApiState,
    _server_id: &str,
) -> (String, usize, Option<String>) {
    ("not_connected".into(), 0, None)
}
