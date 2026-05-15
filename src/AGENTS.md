# src/ — Rust Source Overview

175 files, 24 directories, ~98k lines. Module roots (`src/X.rs`) declare submodules for `src/X/`. No `mod.rs` files.

## WHERE TO LOOK

| Task | Location |
|------|----------|
| Add/change a tool | `src/tools/<tool>.rs` + factory fn in `src/tools.rs` |
| Channel message handling | `src/agent/channel.rs` (3149 lines) |
| Channel dispatch logic | `src/agent/channel_dispatch.rs` |
| Branch fork/return | `src/agent/branch.rs` |
| Worker lifecycle/state machine | `src/agent/worker.rs` |
| Cortex bulletin generation | `src/agent/cortex.rs` (4265 lines — largest file) |
| Cortex interactive chat | `src/agent/cortex_chat.rs` |
| Compactor thresholds | `src/agent/compactor.rs` |
| Status block assembly | `src/agent/status.rs` |
| LLM routing/fallback chains | `src/llm/routing.rs` |
| Provider client init | `src/llm/providers.rs` |
| Custom CompletionModel | `src/llm/model.rs` (3324 lines) |
| Memory CRUD + graph ops | `src/memory/store.rs` |
| Hybrid search (RRF) | `src/memory/search.rs` |
| Embedding generation | `src/memory/embedding.rs` |
| LanceDB table management | `src/memory/lance.rs` |
| Memory decay/prune/merge | `src/memory/maintenance.rs` |
| Memory types/relations | `src/memory/types.rs` |
| Conversation persistence | `src/conversation/history.rs` |
| Context assembly | `src/conversation/context.rs` |
| SpacebotHook (status/usage/cancel) | `src/hooks/spacebot.rs` |
| CortexHook | `src/hooks/cortex.rs` |
| Config loading | `src/config/load.rs` (2282 lines) |
| Config types | `src/config/types.rs` (2337 lines) |
| Config hot-reload | `src/config/watcher.rs` |
| Permissions | `src/config/permissions.rs` |
| Startup, CLI, process init | `src/main.rs` (3308 lines) |
| All shared types + re-exports | `src/lib.rs` (820 lines) |
| Error enum | `src/error.rs` (247 lines) |
| REST API routes | `src/api/` (27 submodules) |
| Messaging adapters | `src/messaging/` (discord, slack, telegram, email, twitch, webchat, webhook) |
| Messaging fan-in/routing | `src/messaging/manager.rs` |
| Messaging trait | `src/messaging/traits.rs` |
| Cron scheduler | `src/cron/scheduler.rs` |
| Cron CRUD | `src/cron/store.rs` |
| Encrypted secrets | `src/secrets/store.rs` |
| Secret scrubbing | `src/secrets/scrub.rs` |
| MCP server/client | `src/mcp.rs` (917 lines) |
| Sandbox/isolation | `src/sandbox.rs` (970 lines) |
| OpenCode subprocess | `src/opencode/` (3 files) |
| Daemon mode + IPC | `src/daemon.rs` (494 lines) |
| Self-awareness tools | `src/self_awareness.rs` (698 lines) |
| Skills system | `src/skills.rs` |
| DB connection + migrations | `src/db.rs` |

## Module Sizes at a Glance

| Module root | Submodules |
|-------------|-----------|
| `agent.rs` | 14 (channel, branch, worker, cortex, compactor, status, channel_dispatch, channel_history, channel_prompt, channel_attachments, cortex_chat, ingestion, process_control, invariant_harness) |
| `api.rs` | 27 |
| `tools.rs` | 40 tool files |
| `config.rs` | 8 |
| `llm.rs` | 7 |
| `messaging.rs` | 10 |
| `memory.rs` | 6 |
| `conversation.rs` | 4 |
| `hooks.rs` | 3 |
| `secrets.rs` | 3 |
| `cron.rs` | 2 |

## Key Shared Types (src/lib.rs)

```
AgentDeps       — dependency injection bundle passed to all processes
ProcessEvent    — 30+ variants for inter-process communication
ProcessId       — identifies any running process
ProcessType     — Channel | Branch | Worker | Cortex | Compactor
ChannelId       — Arc<str> alias
WorkerId        — Arc<str> alias
BranchId        — Arc<str> alias
InboundMessage  — normalized message from any adapter
OutboundResponse — normalized reply to any adapter
```

## Error Handling

`src/error.rs` -- 8 domain variants: `ConfigError | DbError | LlmError | MemoryError | AgentError | SecretsError | SettingsError` + misc. All propagate via `?`. Tools return structured error results, not panics.

## Complexity Hotspots

Files over 2000 lines — read carefully before editing, use `lsp_symbols` to navigate:

| File | Lines |
|------|-------|
| `src/agent/cortex.rs` | 4265 |
| `src/llm/model.rs` | 3324 |
| `src/main.rs` | 3308 |
| `src/agent/channel.rs` | 3149 |
| `src/config/types.rs` | 2337 |
| `src/tools/browser.rs` | 2330 |
| `src/config/load.rs` | 2282 |
