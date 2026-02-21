# MCP Client Integration

Connect Spacebot to external MCP (Model Context Protocol) servers so workers get access to arbitrary tools — databases, APIs, SaaS products, custom integrations — without building native Rust implementations for each one.

## Context

MCP is a JSON-RPC 2.0 protocol that standardizes how AI applications connect to external tool servers. There's already a large ecosystem of MCP servers (Postgres, GitHub, Sentry, Notion, filesystem, etc.) that we'd get access to immediately. The official Rust SDK (`rmcp` v0.16, 3.8M downloads) is mature enough for production use.

## Decisions

- **Client only.** Spacebot connects TO MCP servers. Exposing Spacebot AS an MCP server is a separate feature.
- **Workers only.** MCP tools are task-execution tools, workers are where tasks run. Channels delegate, they don't execute.
- **Per-agent config.** Each agent configures its own MCP servers, consistent with existing per-agent isolation.
- **Both transports.** stdio (subprocess) for local tools, streamable HTTP for remote servers.
- **Tools only.** MCP resources and prompts are out of scope for now.

## Config Shape

```toml
[defaults]
# MCP servers inherited by all agents

[[defaults.mcp]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]

[[defaults.mcp]]
name = "sentry"
transport = "http"
url = "https://mcp.sentry.io"
headers = { "Authorization" = "Bearer ${SENTRY_TOKEN}" }

# Per-agent override
[[agents]]
id = "main"

[[agents.mcp]]
name = "postgres"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://..."]
```

Environment variable interpolation (`${VAR}`) in string values so secrets don't live in config files.

## Architecture

```
Config (McpServerConfig)
  -> McpManager (per-agent, lives in AgentDeps)
    -> McpConnection (one per configured server)
      -> rmcp client session (stdio or streamable HTTP)
        -> tool listing -> McpToolAdapter (Rig Tool impl)
          -> registered on worker ToolServer via handle.add_tool()
```

The bridge piece is `McpToolAdapter` — implements Rig's `Tool` trait by proxying `call()` to the MCP server's `tools/call` JSON-RPC method. Each MCP tool from each server becomes a separate adapter instance on the worker's `ToolServer`.

## What we get for free

- **Leak detection** — `SpacebotHook` scans all tool args/results, MCP tools included
- **Event broadcasting** — `ProcessEvent::ToolStarted/ToolCompleted` fires for MCP tools
- **Output truncation** — `MAX_TOOL_OUTPUT_BYTES` limit applies
- **Cancellation** — worker cancellation kills the agent loop, MCP calls with it
- **Status visibility** — MCP tool calls show in the status block like native tools

## Phase 1: Config + Types

Add MCP server configuration to the existing config system.

### `McpServerConfig` struct

```rust
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
    pub enabled: bool,  // default true
}

pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
}
```

### Config integration

- Add `mcp: Vec<McpServerConfig>` to `DefaultsConfig`
- Add `mcp: Option<Vec<McpServerConfig>>` to `AgentConfig`
- Add `mcp: Vec<McpServerConfig>` to `ResolvedAgentConfig`
- Add `mcp: ArcSwap<Vec<McpServerConfig>>` to `RuntimeConfig`
- TOML deserialization via `TomlMcpConfig` following existing patterns
- Resolution: agent overrides append to (not replace) defaults. Duplicate names from agent config override the default entry.

### Environment variable interpolation

String values in `command`, `args`, `url`, `headers`, and `env` fields support `${VAR_NAME}` syntax. Resolved at connection time, not parse time, so env changes take effect on reconnect.

## Phase 2: MCP Module

New module at `src/mcp.rs` handling connection lifecycle.

### `McpManager`

```rust
pub struct McpManager {
    connections: RwLock<HashMap<String, McpConnection>>,
    configs: Vec<McpServerConfig>,
}
```

- `connect_all()` — connect to every configured server. Failures log a warning and skip — one broken server doesn't block agent startup.
- `disconnect_all()` — clean shutdown of all connections and child processes.
- `get_tools()` — returns `Vec<McpToolAdapter>` across all connected servers.
- `reconnect(name)` — tear down and reconnect a single server.

### `McpConnection`

Wraps an `rmcp` client session.

- stdio: spawns child process via `TokioChildProcess`, runs `initialize` handshake
- HTTP: creates `StreamableHttpClientTransport`, runs `initialize` handshake
- Caches tool list after `initialize` (refreshed on `notifications/tools/list_changed`)
- Tracks connection state: `Connecting`, `Connected`, `Failed(String)`, `Disconnected`

### Exponential backoff retry

`McpConnection::connect_with_retry()` retries failed connections with exponential backoff:

- **Initial delay:** 5 seconds
- **Backoff multiplier:** 2x (5s, 10s, 20s, 40s, 60s, 60s, ...)
- **Maximum delay cap:** 60 seconds
- **Maximum attempts:** 12

Follows the same retry pattern as `MessagingManager::spawn_retry_task`. `connect_all()` and `reconcile()` spawn background retry tasks on failure so a single broken server never blocks agent startup or config reload.

### Lifecycle rules

- Connections are established during agent startup after config resolution.
- If a server fails to connect, log it and spawn a background retry task. The agent works without it.
- On config hot-reload: diff old vs new config, connect new servers, disconnect removed ones, reconnect changed ones. Failed connections during reconciliation also get background retry tasks.
- Child processes (stdio) are killed on disconnect via `rmcp`'s drop semantics.
- Connection health is exposed via API for the dashboard.

## Phase 3: Tool Bridge

New file at `src/tools/mcp.rs` bridging MCP tools into Rig's tool system.

### `McpToolAdapter`

Implements `rig::tool::Tool` for a single MCP tool from a single server.

```rust
pub struct McpToolAdapter {
    server_name: String,
    tool_name: String,
    description: String,
    input_schema: serde_json::Value,
    client: Arc<McpClient>,  // rmcp session handle
}
```

**Tool trait impl:**
- `NAME` — `"{server_name}_{tool_name}"` (namespaced to avoid collisions across servers)
- `Args` — `serde_json::Value` (pass-through, schema validated by the MCP server)
- `Output` — `String` (MCP results serialized to text)
- `definition()` — returns the MCP tool's name, description, and JSON Schema params directly
- `call()` — sends `tools/call` to the MCP server, collects content blocks, joins text content into a single string, applies `truncate_output()`
- Errors from the MCP server are returned as tool error results (not panics), so the LLM can see them and recover.

### Tool naming

MCP servers can expose tools with generic names (`query`, `search`, `run`). Namespacing with the server name (`postgres_query`, `sentry_search`) prevents collisions and makes it clear to the LLM which server a tool belongs to.

## Phase 4: Worker ToolServer Integration

Wire MCP tools into the existing worker tool creation flow.

### Changes to `create_worker_tool_server()`

Add `mcp_tools: Vec<McpToolAdapter>` parameter. After registering native tools (shell, file, exec, etc.), iterate and register each MCP tool adapter:

```rust
for mcp_tool in mcp_tools {
    tool_server = tool_server.tool(mcp_tool);
}
```

Same conditional pattern as `BrowserTool` and `WebSearchTool` — only registered if MCP is configured and connected.

### Changes to worker `run()`

Before creating the tool server, fetch MCP tools from the manager:

```rust
let mcp_tools = self.deps.mcp_manager.get_tools().await;
let worker_tool_server = create_worker_tool_server(
    // ... existing params ...
    mcp_tools,
);
```

Tools are fetched once at worker start. If a server reconnects mid-worker, the worker keeps its original tool set. New workers get the updated tools.

## Phase 5: Wiring

### `AgentDeps`

Add `mcp_manager: Arc<McpManager>` field. Initialized during agent startup alongside memory, LLM, and other per-agent resources.

### Agent startup

After config resolution, before spawning channels:

```rust
let mcp_manager = McpManager::new(resolved_config.mcp.clone());
mcp_manager.connect_all().await;
```

### Hot-reload

In `RuntimeConfig::reload_config()`:

```rust
let old_mcp = self.mcp.load();
let new_mcp = resolved.mcp.clone();
self.mcp.store(Arc::new(new_mcp.clone()));

// Diff and reconcile connections
mcp_manager.reconcile(&old_mcp, &new_mcp).await;
```

### API endpoints

Per-agent visibility endpoints:

- `GET /api/agents/mcp` — list configured MCP servers and their connection status
- `POST /api/agents/mcp/reconnect` — force reconnect a specific server by name

CRUD endpoints for managing `[[mcp_servers]]` in config.toml:

- `GET /api/mcp/servers` — list all configured servers with live connection state
- `POST /api/mcp/servers` — add a new server definition to config.toml
- `PUT /api/mcp/servers` — update an existing server definition
- `DELETE /api/mcp/servers/{name}` — remove a server definition
- `POST /api/mcp/servers/{name}/reconnect` — force-reconnect a specific server across all agents
- `GET /api/mcp/status` — per-agent connection status

### Shutdown

`McpManager::disconnect_all()` called during agent shutdown, before database cleanup. Kills child processes, closes HTTP sessions.

## Dependency

```toml
rmcp = { version = "0.16", features = [
    "client",
    "transport-child-process",
    "transport-streamable-http-client",
    "transport-streamable-http-client-reqwest",
] }
```

`schemars = "0.8"`, `serde = "1.0"`, and `tokio = "1.44"` already match `rmcp`'s requirements.

## File Changes

| File | Change |
|------|--------|
| `Cargo.toml` | add `rmcp` dependency |
| `src/config.rs` | `McpServerConfig`, `McpTransport`, config fields, TOML parsing, hot-reload |
| `src/mcp.rs` | new module: `McpManager`, `McpConnection`, lifecycle |
| `src/tools/mcp.rs` | new file: `McpToolAdapter` (Rig Tool bridge) |
| `src/tools.rs` | update `create_worker_tool_server()`, add `pub mod mcp` |
| `src/lib.rs` | add `pub mod mcp`, add `mcp_manager` to `AgentDeps` |
| `src/agent/worker.rs` | pass MCP tools to tool server creation |
| `src/api/` | MCP status + reconnect endpoints |

## Out of Scope

- MCP server mode (Spacebot as an MCP server)
- MCP resources and prompts (only tools)
- MCP sampling and elicitation
- Per-tool enable/disable within a server
- Dashboard UI for MCP management (API is enough for v1)
- Backward compatibility with pre-MCP configs (new field, defaults to empty)
