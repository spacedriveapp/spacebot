# src/api/ — HTTP API Layer

Axum 0.8. 27 files. 80+ endpoints. Single router assembled in `server.rs`.

## Key Files

| File | Lines | Purpose |
|------|-------|---------|
| `server.rs` | — | Router setup, static file serving, graceful shutdown |
| `state.rs` | — | `ApiState`: per-agent resource maps via `ArcSwap` |
| `agents.rs` | 1952 | Agent CRUD, config, lifecycle |
| `messaging.rs` | 1635 | Adapter management (Telegram, Discord, Webhook) |

## Middleware Stack

Request flows through layers in this order:

```
CORS → Auth (bearer token) → Body limit (10 MB) → Metrics (optional)
```

Auth is instance-level. A single bearer token gates all `/api/*` routes. Static assets bypass auth.

## Route Groups

| Prefix | Description |
|--------|-------------|
| `/api/health` | Liveness / readiness |
| `/api/status` | System status snapshot |
| `/api/events` | SSE stream (server-sent events) |
| `/api/agents/*` | Agent CRUD and lifecycle |
| `/api/channels/*` | Channel management |
| `/api/cortex-chat/*` | Cortex interactive chat |
| `/api/agents/memories/*` | Memory CRUD per agent |
| `/api/agents/workers/*` | Worker status and control |
| `/api/agents/tasks/*` | Task board per agent |
| `/api/agents/cron/*` | Cron job management |
| `/api/agents/projects/*` | Project associations |
| `/api/mcp/servers/*` | MCP server registry |
| `/api/messaging/*` | Messaging adapter config |
| `/api/bindings/*` | Channel-to-agent bindings |
| `/api/secrets/*` | Encrypted credential store |
| `/api/settings/*` | Key-value settings |
| `/api/providers/*` | LLM provider config |
| `/api/models/*` | Model listing and routing |
| `/api/skills/*` | Skill registry |
| `/api/webchat/*` | Embedded web chat |
| `/api/opencode/{port}/*` | OpenCode proxy |
| `/api/links/*` | Short-link management |
| `/api/factory/presets/*` | Agent factory presets |
| `/api/ssh/*` | SSH key management |

## Patterns

**State extraction:** `State<Arc<ApiState>>` on every handler. `ApiState` holds per-agent maps wrapped in `ArcSwap` for hot-reloadable config without locks.

**Request/response:** `Json<T>` for both. Errors return structured JSON with a `code` and `message` field.

**Streaming:** `Sse<S>` for `/api/events`. No WebSocket anywhere. SSE only.

**Static files:** Frontend embedded via `rust_embed` at compile time. `server.rs` serves the SPA from `/` and falls back to `index.html` for client-side routing.

**Graceful shutdown:** `server.rs` listens for `SIGTERM`/`SIGINT` and drains in-flight requests before exit.

## Adding a Route

1. Add handler fn to the relevant module file (e.g., `agents.rs`).
2. Register the route in `server.rs` inside the appropriate `Router::new()` block.
3. Extend `ApiState` only if new shared state is needed.
4. No new files unless the domain is genuinely distinct.
