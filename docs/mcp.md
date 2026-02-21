# MCP (Model Context Protocol) Client

MCP client integration for connecting agents to external tool providers. Opt-in via the `mcp` cargo feature flag.

## Building with MCP

```bash
cargo build --release --features mcp
```

Without the feature flag, all MCP client/manager code is compiled out. Config parsing (`[[mcp_servers]]`) still works regardless — unknown servers are ignored at runtime.

## Configuration

MCP servers are defined at instance level, then referenced per-agent:

```toml
# Instance-level server definitions — shared across agents
[[mcp_servers]]
id = "github"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "env:GITHUB_TOKEN" }

[[mcp_servers]]
id = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp/shared"]

# Defaults — all agents connect to these servers
[defaults.mcp]
servers = ["github"]

# Per-agent override
[[agents]]
id = "coder"

[agents.mcp]
servers = ["github", "filesystem"]
```

### `[[mcp_servers]]`

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `id` | string | Yes | Unique server identifier, used in `servers` lists |
| `transport` | string | Yes | `"stdio"` (Phase 1) or `"sse"` (Phase 2) |
| `command` | string | stdio only | Executable to spawn |
| `args` | string[] | No | Arguments passed to the command |
| `env` | map | No | Environment variables (supports `env:VAR_NAME`) |
| `url` | string | SSE only | Server URL (Phase 2) |
| `headers` | map | SSE only | HTTP headers (supports `env:VAR_NAME`) (Phase 2) |

### `[defaults.mcp]` / `[agents.mcp]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `servers` | string[] | `[]` | Server IDs this agent connects to |

Per-agent `servers` completely replaces the defaults list (no merging). An agent with `servers = []` gets no MCP tools.

## Architecture

### Module Layout

```
src/
  mcp.rs               — Module root, re-exports
  mcp/
    types.rs           — McpServerConfig, McpConfig, McpTransport (always compiled)
    client.rs          — McpClient: connection lifecycle, retry (feature-gated)
    manager.rs         — McpManager: per-agent registry (feature-gated)
```

Types in `types.rs` are unconditional because config parsing needs them regardless of the feature flag. `client.rs` and `manager.rs` are behind `#[cfg(feature = "mcp")]`.

### Connection Lifecycle

```
McpClient state machine:

  Disconnected → Connecting → Connected { server_name, tool_count }
                             → Failed { error, attempts }
```

Each `McpClient` manages a single MCP server connection:
1. Spawns a child process (stdio transport) via `TokioChildProcess`
2. Performs the MCP handshake via `rmcp::ServiceExt::serve()`
3. Calls `list_tools()` to discover available tools
4. Caches tool definitions and the `ServerSink` for tool invocation

### Per-Agent Isolation

Each agent gets its own `McpManager` with independent connections. Two agents referencing the same `[[mcp_servers]]` entry get separate child processes and separate connections. This follows the same isolation pattern as messaging adapters.

### Retry Strategy

Failed connections retry with exponential backoff: 5s initial delay, doubling up to 60s cap, max 12 attempts. Same pattern as `MessagingManager::spawn_retry_task`.

### Tool Registration

MCP tools are registered onto existing `ToolServerHandle`s after creation:

```rust
#[cfg(feature = "mcp")]
if let Some(mcp_manager) = &deps.mcp_manager {
    crate::tools::register_mcp_tools(&tool_server_handle, mcp_manager).await;
}
```

The `register_mcp_tools()` helper iterates over all connected tools, creates `rig::tool::rmcp::McpTool` instances, and adds them to the handle. Tool names are prefixed with `{server_id}_` to prevent collisions with built-in tools (e.g., `github_create_issue`).

### Where MCP Tools Live

| Process | Gets MCP tools? | Why |
|---------|-----------------|-----|
| Worker | Yes | Workers handle external execution |
| Cortex Chat | Yes | Interactive admin needs full capabilities |
| Channel | No | Channels orchestrate, don't execute |
| Branch | No | Branches think, don't call external services |
| Cortex (background) | No | Only needs memory_save for consolidation |

## Hot Reload

MCP config is hot-reloadable. On `config.toml` change:

1. New `McpConfig` is parsed and stored in `RuntimeConfig` via `ArcSwap`
2. New server definitions are diffed against running clients
3. Added servers → `McpManager::add_server()` (connect in background)
4. Removed servers → `McpManager::remove_server()` (disconnect + cleanup)

Existing workers keep their current tools for the duration of their task. New workers pick up the latest configuration.

## Error Handling

MCP errors (`McpError`) have three variants:
- `ConnectionFailed` — server unreachable or transport setup failed
- `DiscoveryFailed` — `list_tools` call failed after successful connection
- `ToolCallFailed` — server returned an error for a tool invocation

MCP errors never crash the agent. Failed connections are logged and retried. Unavailable servers are skipped — the agent continues with remaining tools.

## Dependencies

- **rmcp 0.13** — MCP protocol implementation (client, child process transport)
- **rig 0.30** — `rig::tool::rmcp::McpTool` bridges MCP tools into Rig's `ToolDyn` trait

Both are optional, gated behind the `mcp` feature flag.
