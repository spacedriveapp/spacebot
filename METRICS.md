# Metrics Reference

Comprehensive reference for Spacebot's Prometheus metrics. For quick-start setup, see `docs/metrics.md`. For the published docs, see the metrics page on [docs.spacebot.sh](https://docs.spacebot.sh).

## Feature Gate

All telemetry code is behind the `metrics` cargo feature flag. Without it, every `#[cfg(feature = "metrics")]` block compiles out to nothing — zero runtime cost.

```bash
cargo build --release --features metrics
```

The `[metrics]` config block is always parsed (so config validation works) but has no effect without the feature.

### Docker

The Docker image always includes metrics — `--features metrics` is hardcoded in the Dockerfile. Local development without metrics is still possible via `cargo build --release` (without the feature flag).

## Metric Inventory

All metrics are prefixed with `spacebot_`. The registry uses a private `prometheus::Registry` (not the default global one) to avoid conflicts with other libraries.

### LLM

#### `spacebot_llm_requests_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `model`, `tier`, `worker_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Total LLM completion requests (one per `completion()` call, including retries and fallbacks). |

#### `spacebot_llm_request_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `model`, `tier`, `worker_type` |
| Buckets | 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 15, 30, 60, 120 |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | End-to-end LLM request duration in seconds. Includes retry loops and fallback chain traversal. |

#### `spacebot_llm_tokens_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `model`, `tier`, `direction`, `worker_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Total LLM tokens consumed. `direction` is one of `input`, `output`, or `cached_input`. |

#### `spacebot_llm_estimated_cost_dollars`

| Field | Value |
|-------|-------|
| Type | `CounterVec` (f64) |
| Labels | `agent_id`, `model`, `tier`, `worker_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Estimated LLM cost in USD. Uses a built-in pricing table (`src/llm/pricing.rs`). |

**Note:** Costs are best-effort estimates. The pricing table covers major models (Claude 4/3.5/3, GPT-4o, o-series, Gemini, DeepSeek) with a conservative fallback for unknown models ($3/M input, $15/M output).

### Tools

#### `spacebot_tool_calls_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `tool_name`, `process_type` |
| Instrumented in | `src/hooks/spacebot.rs` — `SpacebotHook::on_tool_result()` |
| Description | Total tool calls executed across all processes. Incremented after each tool call completes (success or failure). |

#### `spacebot_tool_call_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `tool_name`, `process_type` |
| Buckets | 0.01, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30 |
| Instrumented in | `src/hooks/spacebot.rs` — `on_tool_call()` starts timer, `on_tool_result()` observes |
| Description | Tool call execution duration in seconds. |

**Implementation note:** Duration is tracked via a `LazyLock<Mutex<HashMap<String, Instant>>>` static keyed by Rig's internal call ID. If a tool call starts but the agent terminates before `on_tool_result` fires (e.g. leak detection), the timer entry remains — bounded by concurrent tool calls, not a practical concern.

### MCP

#### `spacebot_mcp_connections`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `server_name`, `state` |
| Instrumented in | `src/mcp.rs` |
| Description | Active MCP connections by server and connection state (connecting, connected, disconnected, failed). |

#### `spacebot_mcp_tools_registered`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `server_name` |
| Instrumented in | `src/mcp.rs` |
| Description | Number of tools registered per MCP server. |

#### `spacebot_mcp_connection_attempts_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `server_name`, `result` |
| Instrumented in | `src/mcp.rs` |
| Description | MCP connection attempts. `result` is `success` or `failure`. |

#### `spacebot_mcp_tool_calls_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `server_name`, `tool_name` |
| Instrumented in | `src/mcp.rs` |
| Description | MCP tool calls by server and tool name. |

#### `spacebot_mcp_reconnects_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `server_name` |
| Instrumented in | `src/mcp.rs` |
| Description | MCP reconnection attempts. |

#### `spacebot_mcp_connection_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `server_name` |
| Buckets | 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30 |
| Instrumented in | `src/mcp.rs` |
| Description | MCP connection establishment duration in seconds. |

#### `spacebot_mcp_tool_call_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `server_name`, `tool_name` |
| Buckets | 0.01, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30 |
| Instrumented in | `src/mcp.rs` |
| Description | MCP tool call duration in seconds. |

### Channel / Messaging

#### `spacebot_messages_received_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `channel_type` |
| Instrumented in | `src/agent/channel.rs` |
| Description | Total messages received from external channels. |

#### `spacebot_messages_sent_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `channel_type` |
| Instrumented in | `src/agent/channel.rs` |
| Description | Total messages sent (replies) via channels. |

#### `spacebot_message_handling_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `channel_type` |
| Buckets | 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, 60, 120 |
| Instrumented in | `src/agent/channel.rs` |
| Description | End-to-end message handling duration from receipt to reply. |

#### `spacebot_channel_errors_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `channel_type`, `error_type` |
| Instrumented in | `src/agent/channel.rs` |
| Description | Channel-level errors by type. |

### Memory

#### `spacebot_memory_reads_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id` |
| Instrumented in | `src/tools/memory_recall.rs` — `MemoryRecallTool::call()` |
| Description | Total successful memory recall (search) operations. |

#### `spacebot_memory_writes_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id` |
| Instrumented in | `src/tools/memory_save.rs` — `MemorySaveTool::call()` |
| Description | Total successful memory save operations. |

#### `spacebot_memory_updates_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `operation` |
| Instrumented in | `src/memory/store.rs` (save/delete), `src/tools/memory_save.rs`, `src/tools/memory_delete.rs` (forget) |
| Description | Memory mutation operations. `operation` is one of `save`, `delete`, or `forget`. |

#### `spacebot_memory_entry_count`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `agent_id` |
| Instrumented in | `src/memory/store.rs` — `save()` (inc) and `delete()` (dec) |
| Description | Approximate memory entry count per agent. Tracks net saves minus deletes — starts at 0 on process start, not the actual database count. |

**Note:** This gauge tracks deltas from process start, not the absolute database count. On restart it resets to 0. For the true count, query the database directly.

#### `spacebot_memory_operation_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `operation` |
| Buckets | 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 5 |
| Instrumented in | `src/memory/store.rs` |
| Description | Memory operation duration in seconds (save, load, search, delete). |

#### `spacebot_memory_search_results`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id` |
| Buckets | 0, 1, 2, 5, 10, 20, 50 |
| Instrumented in | `src/tools/memory_recall.rs` |
| Description | Number of search results returned per recall query. |

#### `spacebot_memory_embedding_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `Histogram` (no labels) |
| Buckets | 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5 |
| Instrumented in | `src/tools/memory_save.rs` |
| Description | Embedding generation duration in seconds. |

### Agent & Worker Lifecycle

#### `spacebot_active_workers`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `agent_id` |
| Instrumented in | `src/agent/channel_dispatch.rs` — `spawn_worker_task()` |
| Description | Currently active workers. Incremented when a worker task is spawned, decremented when it completes. |

#### `spacebot_active_branches`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `agent_id` |
| Instrumented in | `src/agent/channel_dispatch.rs` — branch spawn (inc) and completion (dec) |
| Description | Currently active branches per agent. |

#### `spacebot_branches_spawned_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id` |
| Instrumented in | `src/agent/channel_dispatch.rs` — `spawn_branch()` |
| Description | Total branches spawned (monotonic counter, unlike the gauge). |

#### `spacebot_worker_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `worker_type` |
| Buckets | 1, 5, 10, 30, 60, 120, 300, 600, 1800 |
| Instrumented in | `src/agent/channel_dispatch.rs` — `spawn_worker_task()` |
| Description | Worker lifetime duration in seconds from spawn to completion. `worker_type` is `builtin` or `opencode`. |

#### `spacebot_context_overflow_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `process_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` error paths |
| Description | Context overflow events (when a request exceeds the model's context window). |

#### `spacebot_process_errors_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `process_type`, `error_type`, `worker_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` error paths |
| Description | Process errors by type. `error_type` classifies the failure (timeout, rate_limit, context_overflow, provider_error, other). |

### Cost

#### `spacebot_worker_cost_dollars`

| Field | Value |
|-------|-------|
| Type | `CounterVec` (f64) |
| Labels | `agent_id`, `worker_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Worker-specific cost tracking in USD. Only incremented for requests where `tier == "worker"`. |

### API

#### `spacebot_http_requests_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `method`, `path`, `status` |
| Instrumented in | `src/api/server.rs` — middleware layer |
| Description | Total HTTP requests to the API server. |

#### `spacebot_http_request_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `method`, `path` |
| Buckets | 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 5 |
| Instrumented in | `src/api/server.rs` — middleware layer |
| Description | HTTP request duration in seconds. |

### Cron

#### `spacebot_cron_executions_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `task_type`, `result` |
| Instrumented in | `src/agent/cortex.rs` |
| Description | Cron task executions. `result` is `success` or `failure`. |

### Ingestion

#### `spacebot_ingestion_files_processed_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `result` |
| Instrumented in | `src/agent/ingestion.rs` |
| Description | Ingestion files processed. `result` is `success` or `failure`. |

### Warmup / Readiness

#### `spacebot_dispatch_while_cold_count`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `dispatch_type`, `reason` |
| Instrumented in | `src/agent/channel_dispatch.rs` — readiness guard |
| Description | Dispatch attempts while the readiness contract is unsatisfied. |

#### `spacebot_warmup_recovery_latency_ms`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `dispatch_type` |
| Buckets | 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000 |
| Instrumented in | `src/agent/cortex.rs` — `trigger_forced_warmup()` |
| Description | Time-to-recovery for forced warmup passes kicked by dispatch paths, in milliseconds. |

## Total Cardinality

Assumptions: 1–5 agents, 5–15 models, ~20 tools, 5 tiers, 2–3 worker types, 3–5 MCP servers, 5 channel types, ~20 API paths.

| Metric | Series estimate |
|--------|-----------------|
| `llm_requests_total` | ~50–1125 |
| `llm_request_duration_seconds` | ~50–1125 |
| `llm_tokens_total` | ~150–3375 |
| `llm_estimated_cost_dollars` | ~50–1125 |
| `tool_calls_total` | ~100–500 |
| `tool_call_duration_seconds` | ~100–500 |
| `memory_reads_total` | ~1–5 |
| `memory_writes_total` | ~1–5 |
| `memory_updates_total` | ~3–15 |
| `memory_entry_count` | ~1–5 |
| `memory_operation_duration_seconds` | ~4–20 |
| `memory_search_results` | ~1–5 |
| `memory_embedding_duration_seconds` | 1 |
| `process_errors_total` | ~30–375 |
| `worker_duration_seconds` | ~2–15 |
| `worker_cost_dollars` | ~2–15 |
| `active_workers` | ~1–5 |
| `active_branches` | ~1–5 |
| `branches_spawned_total` | ~1–5 |
| `context_overflow_total` | ~5–25 |
| `mcp_connections` | ~6–20 |
| `mcp_tools_registered` | ~3–5 |
| `mcp_connection_attempts_total` | ~6–10 |
| `mcp_tool_calls_total` | ~15–50 |
| `mcp_reconnects_total` | ~3–5 |
| `mcp_connection_duration_seconds` | ~3–5 |
| `mcp_tool_call_duration_seconds` | ~15–50 |
| `messages_received_total` | ~5–25 |
| `messages_sent_total` | ~5–25 |
| `message_handling_duration_seconds` | ~5–25 |
| `channel_errors_total` | ~15–75 |
| `http_requests_total` | ~60–300 |
| `http_request_duration_seconds` | ~40–200 |
| `cron_executions_total` | ~6–30 |
| `ingestion_files_processed_total` | ~2–10 |
| `dispatch_while_cold_count` | ~3–15 |
| `warmup_recovery_latency_ms` | ~2–10 |
| **Total** | **~800–9200** |

Well within safe operating range for any Prometheus deployment.

## Feature Gate Consistency

Every instrumentation call site uses `#[cfg(feature = "metrics")]` at the statement or block level:

| File | Gate type |
|------|-----------|
| `src/lib.rs` | `#[cfg(feature = "metrics")] pub mod telemetry` |
| `src/main.rs` | `#[cfg(feature = "metrics")] let _metrics_handle = ...` |
| `src/llm/model.rs` | `#[cfg(feature = "metrics")] let start` + `#[cfg(feature = "metrics")] { ... }` |
| `src/hooks/spacebot.rs` | `#[cfg(feature = "metrics")] static TOOL_CALL_TIMERS` + 2 blocks |
| `src/tools/memory_save.rs` | `#[cfg(feature = "metrics")] { ... }` |
| `src/tools/memory_recall.rs` | `#[cfg(feature = "metrics")] { ... }` |
| `src/tools/memory_delete.rs` | `#[cfg(feature = "metrics")] crate::telemetry::Metrics::global()...` |
| `src/memory/store.rs` | `#[cfg(feature = "metrics")] if _result...` + `#[cfg(feature = "metrics")] { ... }` |
| `src/agent/channel.rs` | `#[cfg(feature = "metrics")]` (messaging metrics) |
| `src/agent/channel_dispatch.rs` | `#[cfg(feature = "metrics")]` (branches, workers, warmup) |
| `src/agent/cortex.rs` | `#[cfg(feature = "metrics")]` (cron, warmup recovery) |
| `src/agent/ingestion.rs` | `#[cfg(feature = "metrics")]` (ingestion files) |
| `src/mcp.rs` | `#[cfg(feature = "metrics")]` (MCP connections, tools) |
| `src/api/server.rs` | `#[cfg(feature = "metrics")]` (HTTP middleware) |
| `Cargo.toml` | `prometheus = { version = "0.13", optional = true }`, `metrics = ["dep:prometheus"]` |

All consistent. No path references `crate::telemetry` without a `cfg` gate.

## Endpoints

| Path | Response |
|------|----------|
| `/metrics` | Prometheus text exposition format (0.0.4) |
| `/health` | `200 OK` (liveness probe) |

The metrics server binds to a configurable address (default `0.0.0.0:9090`), separate from the main API server (`127.0.0.1:19898`).
