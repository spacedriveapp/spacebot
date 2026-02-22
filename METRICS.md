# Metrics Reference

Comprehensive reference for Spacebot's Prometheus metrics. For quick-start setup, see `docs/metrics.md`. For the published docs, see the metrics page on [docs.spacebot.sh](https://docs.spacebot.sh).

## Feature Gate

All telemetry code is behind the `metrics` cargo feature flag. Without it, every `#[cfg(feature = "metrics")]` block compiles out to nothing — zero runtime cost.

```bash
cargo build --release --features metrics
```

The `[metrics]` config block is always parsed (so config validation works) but has no effect without the feature.

## Metric Inventory

All metrics are prefixed with `spacebot_`. The registry uses a private `prometheus::Registry` (not the default global one) to avoid conflicts with other libraries.

### Counters

#### `spacebot_llm_requests_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `model`, `tier` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Total LLM completion requests (one per `completion()` call, including retries and fallbacks). |

**Cardinality:** `agents × models × tiers`. With agent context wired, expect `agents(1–5) × models(5–15) × tiers(5)` = 25–375 series.

#### `spacebot_tool_calls_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `tool_name` |
| Instrumented in | `src/hooks/spacebot.rs` — `SpacebotHook::on_tool_result()` |
| Description | Total tool calls executed across all processes. Incremented after each tool call completes (success or failure). |

**Cardinality:** `agents × tools`. With 1–5 agents and ~20 tool names, expect 20–100 series.

#### `spacebot_memory_reads_total`

| Field | Value |
|-------|-------|
| Type | `IntCounter` (no labels) |
| Instrumented in | `src/tools/memory_recall.rs` — `MemoryRecallTool::call()` |
| Description | Total successful memory recall (search) operations. |

**Cardinality:** 1 series.

#### `spacebot_memory_writes_total`

| Field | Value |
|-------|-------|
| Type | `IntCounter` (no labels) |
| Instrumented in | `src/tools/memory_save.rs` — `MemorySaveTool::call()` |
| Description | Total successful memory save operations. |

**Cardinality:** 1 series.

#### `spacebot_llm_tokens_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `model`, `tier`, `direction` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Total LLM tokens consumed. `direction` is one of `input`, `output`, or `cached_input`. |

**Cardinality:** `agents × models × tiers × 3`. Expect 75–1125 series.

#### `spacebot_llm_estimated_cost_dollars`

| Field | Value |
|-------|-------|
| Type | `CounterVec` (f64) |
| Labels | `agent_id`, `model`, `tier` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | Estimated LLM cost in USD. Uses a built-in pricing table (`src/llm/pricing.rs`). |

**Cardinality:** Same as `spacebot_llm_requests_total`.

**Note:** Costs are best-effort estimates. The pricing table covers major models (Claude 4/3.5/3, GPT-4o, o-series, Gemini, DeepSeek) with a conservative fallback for unknown models ($3/M input, $15/M output).

#### `spacebot_process_errors_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `process_type`, `error_type` |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` error paths |
| Description | Process errors by type. `error_type` classifies the failure (timeout, rate_limit, auth, server, provider, unknown). |

**Cardinality:** `agents × process_types × error_types`. Expect 15–75 series.

#### `spacebot_memory_updates_total`

| Field | Value |
|-------|-------|
| Type | `IntCounterVec` |
| Labels | `agent_id`, `operation` |
| Instrumented in | `src/memory/store.rs` (save/delete), `src/tools/memory_save.rs`, `src/tools/memory_delete.rs` (forget) |
| Description | Memory mutation operations. `operation` is one of `save`, `delete`, or `forget`. |

**Cardinality:** `agents × operations(3)`. Expect 3–15 series.

### Histograms

#### `spacebot_llm_request_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `model`, `tier` |
| Buckets | 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 15, 30, 60, 120 |
| Instrumented in | `src/llm/model.rs` — `SpacebotModel::completion()` |
| Description | End-to-end LLM request duration in seconds. Includes retry loops and fallback chain traversal. |

**Cardinality:** Same as `spacebot_llm_requests_total` (per-bucket overhead is fixed, not per-series).

#### `spacebot_tool_call_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `Histogram` (no labels) |
| Buckets | 0.01, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30 |
| Instrumented in | `src/hooks/spacebot.rs` — `on_tool_call()` starts timer, `on_tool_result()` observes |
| Description | Tool call execution duration in seconds. |

**Cardinality:** 1 series.

**Implementation note:** Duration is tracked via a `LazyLock<Mutex<HashMap<String, Instant>>>` static keyed by Rig's internal call ID. If a tool call starts but the agent terminates before `on_tool_result` fires (e.g. leak detection), the timer entry remains — bounded by concurrent tool calls, not a practical concern.

#### `spacebot_worker_duration_seconds`

| Field | Value |
|-------|-------|
| Type | `HistogramVec` |
| Labels | `agent_id`, `worker_type` |
| Buckets | 1, 5, 10, 30, 60, 120, 300, 600, 1800 |
| Instrumented in | `src/agent/channel.rs` — `spawn_worker_task()` |
| Description | Worker lifetime duration in seconds from spawn to completion. |

**Cardinality:** `agents × worker_types`. Currently `worker_type` is `"builtin"` — expect 1–5 series.

### Gauges

#### `spacebot_active_workers`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `agent_id` |
| Instrumented in | `src/agent/channel.rs` — `spawn_worker_task()` |
| Description | Currently active workers. Incremented when a worker task is spawned, decremented when it completes. |

**Cardinality:** Number of agents (1–5).

#### `spacebot_memory_entry_count`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `agent_id` |
| Instrumented in | `src/memory/store.rs` — `save()` (inc) and `delete()` (dec) |
| Description | Approximate memory entry count per agent. Tracks net saves minus deletes — starts at 0 on process start, not the actual database count. |

**Cardinality:** Number of agents (1–5).

**Note:** This gauge tracks deltas from process start, not the absolute database count. On restart it resets to 0. For the true count, query the database directly.

#### `spacebot_active_branches`

| Field | Value |
|-------|-------|
| Type | `IntGaugeVec` |
| Labels | `agent_id` |
| Instrumented in | `src/agent/channel.rs` — branch spawn (inc) and completion (dec) |
| Description | Currently active branches per agent. |

**Cardinality:** Number of agents (1–5).

## Total Cardinality

| Metric | Series estimate |
|--------|-----------------|
| `llm_requests_total` | ~25–375 |
| `llm_tokens_total` | ~75–1125 |
| `llm_estimated_cost_dollars` | ~25–375 |
| `tool_calls_total` | ~20–100 |
| `memory_reads_total` | 1 |
| `memory_writes_total` | 1 |
| `llm_request_duration_seconds` | ~25–375 |
| `tool_call_duration_seconds` | 1 |
| `worker_duration_seconds` | ~1–5 |
| `active_workers` | ~1–5 |
| `active_branches` | ~1–5 |
| `memory_entry_count` | ~1–5 |
| `process_errors_total` | ~15–75 |
| `memory_updates_total` | ~3–15 |
| **Total** | **~195–2465** |

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
| `src/tools/memory_recall.rs` | `#[cfg(feature = "metrics")] crate::telemetry::Metrics::global()...` |
| `src/tools/memory_delete.rs` | `#[cfg(feature = "metrics")] crate::telemetry::Metrics::global()...` |
| `src/memory/store.rs` | `#[cfg(feature = "metrics")] if _result...` + `#[cfg(feature = "metrics")] { ... }` |
| `src/agent/channel.rs` | `#[cfg(feature = "metrics")]` (×4, branches + workers) |
| `Cargo.toml` | `prometheus = { version = "0.13", optional = true }`, `metrics = ["dep:prometheus"]` |

All consistent. No path references `crate::telemetry` without a `cfg` gate.

## Endpoints

| Path | Response |
|------|----------|
| `/metrics` | Prometheus text exposition format (0.0.4) |
| `/health` | `200 OK` (liveness probe) |

The metrics server binds to a configurable address (default `0.0.0.0:9090`), separate from the main API server (`127.0.0.1:19898`).
