# Plan: Rig-Spacebot Alignment And Leverage

```yaml
plan_contract:
  plan_id: rig-spacebot-alignment-2026-03-05
  generated_at: 2026-03-05
  owner:
    primary: spacebot-core
    supporting:
      - llm-runtime
      - agent-orchestration
      - messaging-adapters
      - observability
      - qa-docs
  handoff_state: implemented-and-verified
  risk_level: high
  assumptions:
    - Spacebot must keep its custom CompletionModel and LlmManager-based provider stack.
    - Spacebot must not adopt Rig built-in provider clients, RAG/vector-store integrations, Agent-as-Tool, or Pipeline system.
    - Existing per-process architecture (channel, branch, worker, compactor, cortex) remains the core execution model.
    - New behavior must be rollout-gated and backward compatible by default.
    - No historical migration files may be edited; any new persisted schema change requires a new migration.
  open_questions: []
```

## Goals

- Fully align Spacebot's custom Rig integration with the parts of Rig 0.31 that materially improve correctness, latency, observability, and maintainability.
- Implement faithful support for Rig request semantics currently dropped by `SpacebotModel`, specifically `tool_choice` and `output_schema`.
- Introduce native Rig streaming into user-facing paths where it provides real time-to-first-token improvement without violating current safety guarantees.
- Add controlled, opt-in request-level tool concurrency for clearly read-only worker tool sets.
- Add parity and drift-detection tests so future Rig upgrades do not silently break Spacebot's custom integration layer.
- Preserve Spacebot's architectural intent: delegation-first multi-process orchestration, custom routing, custom provider stack, and process-scoped tool isolation.

## Non-goals

- Do not replace `SpacebotModel` with Rig built-in provider clients.
- Do not adopt Rig Agent-as-Tool composition for core channel/branch/worker orchestration.
- Do not adopt Rig vector store or dynamic tool retrieval as the primary memory/tool architecture.
- Do not replace current memory system, task system, or channel history model.
- Do not enable broad concurrent tool execution for side-effecting or secret-handling tools.
- Do not require database migrations unless a later implementation decision introduces new persisted state.

## Executive Summary

Spacebot already uses Rig deeply, but primarily as a low-level substrate: `CompletionModel`, `AgentBuilder`, `PromptHook`, `ToolServer`, Rig message/history types, and Rig's multi-turn tool loop. The main gaps are not architectural absence; they are semantic mismatch and underused Rig capabilities:

1. `SpacebotModel` does not currently forward Rig `tool_choice` and `output_schema` request semantics.
2. App-level process loops use non-streaming `prompt(...)` paths even though the model implements Rig `stream()`.
3. Request-level `with_tool_concurrency(...)` is unused, even where worker tools are independent and read-only.
4. Drift detection is weak: Spacebot's custom Rig bridge can silently diverge from Rig's expected request surface on future upgrades.

The recommended implementation sequence is:

1. Request semantic parity
2. Streaming adoption in channel and cortex-chat
3. Safe read-only tool concurrency
4. Drift detection, instrumentation, and documentation hardening

This sequence maximizes correctness first, then user-visible latency wins, then performance wins, while preserving Spacebot's repo-level architecture constraints.

## Existing Architecture Snapshot

### Relevant Existing Components

- `src/llm/model.rs`
  - `SpacebotModel`
  - custom request serialization for OpenAI-compatible, Responses API, Gemini-compatible, and Anthropic
  - custom streaming parsing and aggregation
- `src/llm/manager.rs`
  - provider config resolution, OAuth refresh, HTTP client ownership, cooldown tracking
- `src/llm/routing.rs`
  - process-type routing and fallback selection
- `src/hooks/spacebot.rs`
  - `PromptHook` implementation
  - leak detection
  - tool nudge behavior
  - status event emission
- `src/agent/channel.rs`
  - main user-facing prompt loop
  - per-turn tool add/remove
  - history reconciliation
- `src/agent/worker.rs`
  - segmented multi-turn worker loop
  - tool nudge retry
  - compaction recovery
- `src/agent/cortex_chat.rs`
  - interactive admin/cortex chat path
- `src/tools.rs`
  - branch/worker/cortex tool server construction
- `src/messaging/*.rs`
  - adapter-specific streaming edit/render behavior already exists at outbound transport layer

### Architectural Boundaries

- Rig boundary:
  - Rig owns agent loop primitives, request builders, hook interfaces, tool server protocol, message model, and streaming interfaces.
- Spacebot boundary:
  - Spacebot owns provider transport, model routing, secrets handling, message persistence, task orchestration, memory system, and adapter delivery semantics.
- Provider boundary:
  - Provider-specific serialization must happen in `SpacebotModel` and `src/llm/anthropic/*`, not in higher-level agents.
- Messaging boundary:
  - End-user streaming behavior must remain adapter-aware and respect platform-specific edit rate limits.

### Architectural Invariants

- `SpacebotModel` remains the single translation layer from Rig `CompletionRequest` to provider payloads.
- Channel, branch, worker, compactor, and cortex remain separate process types with separate prompts and tool surfaces.
- No end-user visible text may bypass leak detection or reply-tool safety checks.
- Channel history is not committed until a terminal condition is reached and history reconciliation completes.
- Worker tool concurrency may only apply to explicitly allowlisted read-only tools.
- Unsupported provider features must never fail silently when the caller requested strict semantics.

## Target Architecture

### New Capability Surface

The implementation introduces four logical capabilities:

1. Request Semantic Parity Layer
2. Streaming Execution Layer
3. Read-Only Tool Concurrency Layer
4. Drift Detection And Observability Layer

These are capabilities, not a new top-level subsystem. They fit into the current module boundaries.

### Components

#### 1. Request Semantic Parity Layer

Primary files:

- `src/llm/model.rs`
- `src/llm/anthropic/params.rs`
- `src/agent/cortex.rs`
- `src/config/*`

Responsibilities:

- Carry Rig `CompletionRequest.tool_choice` into provider payloads when supported.
- Carry Rig `CompletionRequest.output_schema` into provider payloads when supported.
- Explicitly classify unsupported combinations.
- Enforce rollout-mode behavior (`off`, `shadow`, `enforced`).

#### 2. Streaming Execution Layer

Primary files:

- `src/agent/channel.rs`
- `src/agent/cortex_chat.rs`
- `src/hooks/spacebot.rs`
- `src/messaging/slack.rs`
- `src/messaging/discord.rs`
- `src/messaging/telegram.rs`
- `src/api/state.rs`

Responsibilities:

- Switch selected paths from `prompt(...)` to Rig streaming request flow.
- Preserve existing hook semantics for deltas, tool calls, terminal responses, and cancellation.
- Reconcile final streamed history with existing persistence and reply semantics.
- Preserve adapter-specific rate limiting and edit chunking.

#### 3. Read-Only Tool Concurrency Layer

Primary files:

- `src/agent/worker.rs`
- `src/tools.rs`
- selected `src/tools/*.rs`
- `src/config/*`

Responsibilities:

- Define a concurrency-safe tool allowlist.
- Apply `with_tool_concurrency(...)` only for eligible worker prompts.
- Preserve ordering and side-effect guarantees for non-allowlisted tools.

#### 4. Drift Detection And Observability Layer

Primary files:

- `src/llm/model.rs`
- `src/hooks/spacebot.rs`
- `src/agent/channel.rs`
- `METRICS.md`
- `README.md`
- `docs/content/docs/(configuration)/config.mdx`
- `tests/*`

Responsibilities:

- Detect when Rig request fields are being dropped by Spacebot's custom bridge.
- Emit metrics for semantic application, streaming latency, concurrency usage, and unsupported-provider decisions.
- Provide tests that fail when Rig request surface and Spacebot behavior diverge.

## Data Flow

### Flow A: Request Semantic Parity

1. Process builds Rig agent with `AgentBuilder`.
2. Caller invokes `prompt(...)` or `prompt_typed(...)`.
3. Rig builds `CompletionRequest`.
4. `SpacebotModel::completion(...)` or `SpacebotModel::stream(...)` receives request.
5. New semantic-parity helper inspects:
   - `tool_choice`
   - `output_schema`
   - `process_type`
   - rollout mode
   - provider capability
6. Provider serializer applies, shadows, or rejects unsupported fields.
7. Metrics/logs record applied vs unsupported vs shadowed decisions.
8. Response returns through existing hook and agent loop.

### Flow B: Channel Streaming

1. Channel builds per-turn tool server.
2. Channel creates Rig agent.
3. Under feature flag, channel uses Rig streaming prompt request instead of one-shot prompt.
4. Streaming hook emits text deltas and tool-call deltas.
5. Adapter updates in-flight message according to adapter edit cadence.
6. Terminal condition occurs:
   - final text response
   - `reply` tool succeeds
   - cancellation
   - max turns
7. Final response is reconciled into channel history using existing history-commit rules.
8. Optional fallback text behavior remains preserved for no-reply cases.

### Flow C: Worker Read-Only Tool Concurrency

1. Worker selects task prompt.
2. Worker computes eligible concurrency profile from tool surface plus config.
3. If eligible:
   - issue request with `with_tool_concurrency(n)`
4. Rig executes independent tool calls concurrently.
5. Hook still receives each tool start/result event.
6. Existing status/leak/error handling remains authoritative.
7. Worker loop continues or stops under existing segment/max-turn logic.

## Proposed Data Model And Schemas

### Config Schema

Add a new runtime-config group for Rig alignment behavior.

```toml
[defaults.rig_alignment]
request_semantics_mode = "shadow"   # off | shadow | enforced
channel_streaming = false
cortex_chat_streaming = false
worker_read_only_tool_concurrency = 1
worker_max_tool_concurrency = 4
worker_read_only_tool_allowlist = [
  "read_skill",
  "spacebot_docs",
  "memory_recall",
  "channel_recall",
  "web_search",
  "worker_inspect",
  "email_search",
  "config_inspect"
]
output_schema_provider_allowlist = [
  "openai",
  "openai-chatgpt",
  "anthropic"
]
tool_choice_provider_allowlist = [
  "openai",
  "openai-chatgpt",
  "anthropic"
]
```

### Rust Config Types

Add to `src/config/types.rs`, `src/config/toml_schema.rs`, `src/config/load.rs`, and `src/config/runtime.rs`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RigSemanticsMode {
    Off,
    Shadow,
    Enforced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigAlignmentConfig {
    pub request_semantics_mode: RigSemanticsMode,
    pub channel_streaming: bool,
    pub cortex_chat_streaming: bool,
    pub worker_read_only_tool_concurrency: usize,
    pub worker_max_tool_concurrency: usize,
    pub worker_read_only_tool_allowlist: Vec<String>,
    pub output_schema_provider_allowlist: Vec<String>,
    pub tool_choice_provider_allowlist: Vec<String>,
}
```

### Semantic Decision Model

Add an internal non-persisted decision struct in `src/llm/model.rs`:

```rust
struct RequestSemanticDecision {
    apply_tool_choice: bool,
    apply_output_schema: bool,
    rejected_reason: Option<String>,
    shadow_only: bool,
}
```

### Metrics Schema

Add or extend metrics with the following label sets:

- `spacebot_rig_request_semantics_total{process,provider,field,decision}`
  - `field`: `tool_choice` | `output_schema`
  - `decision`: `applied` | `shadowed` | `rejected` | `unsupported`
- `spacebot_rig_stream_sessions_total{process,provider,mode}`
  - `mode`: `native` | `synthetic`
- `spacebot_rig_time_to_first_delta_ms_bucket{process,provider}`
- `spacebot_rig_tool_concurrency_total{worker_type,concurrency}`
- `spacebot_rig_drift_detected_total{surface}`

## Security And Privacy Model

### Threats

#### Threat 1: Secret leakage through streamed deltas

Risk:

- Streaming emits partial text earlier than final reply-tool gating.
- Secrets may appear in deltas before final reconciliation.

Mitigations:

- Route all user-visible delta emission through the same leak scrub path as final output.
- Do not stream raw tool args or tool results to end users.
- Preserve `reply`-tool blocking behavior and reply-result leak checks in `SpacebotHook`.
- Cap delta payload lengths before event fan-out.

#### Threat 2: Unsafe concurrent tool execution

Risk:

- Concurrent side effects can reorder writes, statuses, or task updates.

Mitigations:

- Concurrency applies only to explicit read-only allowlist.
- `set_status`, `file`, `exec`, `shell`, `secret_set`, `task_update`, `browser`, and all write-capable tools remain sequential.
- Add a validator test that fails if a non-allowlisted tool is marked concurrency-safe without an explicit review.

#### Threat 3: Structured output contract silently not enforced

Risk:

- `prompt_typed(...)` appears safe but `SpacebotModel` drops `output_schema`.

Mitigations:

- In `enforced` mode, reject `output_schema` requests for unsupported providers.
- Add serializer tests that assert the schema is present in outgoing requests.
- Emit parity metrics for every structured-output request.

#### Threat 4: Provider-specific tool choice mismatch

Risk:

- `ToolChoice::Required` or `Specific` may be requested but ignored.

Mitigations:

- Capability gating per provider/API type.
- Fail closed in `enforced` mode for unsupported mappings.
- Shadow logs during rollout before enabling strict mode globally.

### Secrets Handling

- Continue to source provider keys only through `LlmManager` and the existing secret store.
- Do not add any new secret propagation path.
- Do not log raw output schema contents if they may encode user data; log schema name/hash instead.
- Continue scrubbing tool outputs before SSE/dashboard emission.
- Preserve current worker/branch secret-scrub behavior in `SpacebotHook`.

## Performance Targets

### Request Semantic Parity Targets

- Added serialization overhead for `tool_choice` / `output_schema` handling:
  - p50 <= 1 ms per request
  - p95 <= 5 ms per request
- No measurable increase in provider HTTP request size above:
  - +3 KB p50
  - +15 KB p95 for structured output requests

### Streaming Targets

- Channel time-to-first-visible-delta on native-streaming providers:
  - p50 <= 1.5 s
  - p95 <= 3.5 s
- Synthetic-stream providers must not regress full-response latency by >5%.
- Adapter edit cadence:
  - Slack/Discord: no more than 2 updates/second/message
  - Telegram: no more than 1 update/second/message

### Tool Concurrency Targets

- For a worker turn with 3 eligible read-only tools, wall-clock completion time should improve by >=30% versus sequential baseline in test harness.
- Maximum request-level tool concurrency:
  - default: 1
  - rollout cap: 4
- No increase in tool error rate >2 percentage points during canary.

## Instrumentation Plan

### Logs

Emit structured logs for:

- request semantic decisions
- streaming session start/stop
- time-to-first-delta
- native vs synthetic stream mode
- concurrency-enabled worker turns
- rejected unsupported semantics
- history reconciliation result after streamed completion

Required log keys:

- `agent_id`
- `process_type`
- `provider`
- `model`
- `request_semantics_mode`
- `tool_choice_requested`
- `output_schema_requested`
- `streaming_enabled`
- `stream_mode`
- `tool_concurrency`
- `terminal_reason`

### Metrics

Use the metrics listed in the metrics schema section.

### Tracing

- Continue using Rig span model for prompt/tool execution.
- Add span fields:
  - `spacebot.rig.tool_choice_applied`
  - `spacebot.rig.output_schema_applied`
  - `spacebot.rig.stream_mode`
  - `spacebot.rig.tool_concurrency`

### Baseline Measurement Before Rollout

Collect a 7-day baseline in current production or staging for:

- channel response latency
- time to first outbound message edit
- worker tool error rate
- provider distribution
- MCP tool count distribution per worker

## Error Handling, Retries, And No-Silent-Failure Policy

### Policy

- No newly introduced request semantic may be dropped silently.
- Every unsupported semantic must produce one of:
  - explicit error
  - warning + metric in shadow mode
  - structured status event
- Every rollout-gated behavior must be observable in logs and metrics.

### Retry Rules

- Preserve existing model retry and fallback logic in `SpacebotModel`.
- Preserve worker overflow retry and segment retry behavior.
- Preserve tool-nudge retry behavior.
- Streaming adapter message-edit failures:
  - retry up to 2 times per update burst
  - then downgrade to final-response-only delivery for that message
  - log adapter-specific warning with message key

### Unsupported Capability Behavior

#### `output_schema`

- `off`: ignore request and log nothing extra
- `shadow`: do not apply request, emit warning + metric
- `enforced`: return explicit provider compatibility error

#### `tool_choice`

- `off`: ignore request and log nothing extra
- `shadow`: do not apply request, emit warning + metric
- `enforced`: return explicit provider compatibility error

### No Silent Failure Rules

- No `Result` may be dropped in new code paths unless the existing repo policy explicitly allows best-effort event sends.
- Every new fallback path must:
  - increment a metric
  - emit a structured log
  - preserve root-cause context

## Orchestration And State Semantics

### Execution States

Apply the following logical execution state model to channel streaming and worker concurrency flows.

| State | Description | Entry Condition | Exit Condition |
|---|---|---|---|
| `Idle` | No prompt running | process available | prompt starts |
| `BuildingRequest` | Rig request assembly in progress | prompt accepted | request built or rejected |
| `ValidatingSemantics` | capability gating for `tool_choice` / `output_schema` | request built | semantic decision made |
| `AwaitingProvider` | provider request dispatched | request sent | first response chunk or full response |
| `StreamingText` | text delta stream active | first delta received | tool call, terminal text, cancel, or error |
| `ExecutingTools` | one or more tool calls executing | tool call received | all tool results returned or failure |
| `AwaitingContinuation` | multi-turn waiting for next model step | tool results committed | next provider call or terminal state |
| `ReconcilingHistory` | final history normalization | terminal response received | history persisted |
| `Completed` | successful terminal condition | final text or reply delivered | none |
| `Cancelled` | prompt cancelled by hook/user/system | cancel reason received | none |
| `Failed` | unrecoverable error | error returned after retries | none |

### State Transitions

- `Idle -> BuildingRequest`
- `BuildingRequest -> ValidatingSemantics`
- `ValidatingSemantics -> Failed` if enforced unsupported semantics
- `ValidatingSemantics -> AwaitingProvider`
- `AwaitingProvider -> StreamingText` if streaming deltas start
- `AwaitingProvider -> ExecutingTools` if tool call arrives before text
- `AwaitingProvider -> ReconcilingHistory` if full final response returns
- `StreamingText -> ExecutingTools` if tool call arrives mid-stream
- `StreamingText -> ReconcilingHistory` on final terminal text
- `ExecutingTools -> AwaitingContinuation` when all tool results return
- `AwaitingContinuation -> AwaitingProvider` on next turn
- `ReconcilingHistory -> Completed|Cancelled|Failed`

### Stop Conditions

- Channel:
  - successful `reply` tool result
  - explicit `skip`
  - user/system cancellation
  - unrecoverable provider error
  - max-turn exhaustion
- Worker:
  - successful final response after outcome signal
  - max segments reached
  - unrecoverable provider error
  - cancellation

### Continue Conditions

- Continue only when:
  - tool results are fully captured
  - current prompt has not exceeded max-turn or max-segment bounds
  - no enforced semantic compatibility failure occurred
  - no leak guard terminated the loop

### Retry And Backoff Rules

- Provider retries:
  - keep existing exponential backoff and fallback chain logic
- Streaming message-edit retries:
  - 2 retries max per burst
  - no blind infinite retry
- Concurrency downgrade:
  - if a concurrency-enabled worker turn hits allowlist violation or ordering assertion, rerun that prompt sequentially once and emit metric

### Reconciliation Loop For Drift

Implement a CI and runtime drift loop:

1. CI drift tests:
   - verify outgoing payload includes or rejects all expected Rig request fields
   - fail when serializer behavior diverges
2. Runtime drift metrics:
   - count requested vs applied semantics
   - count synthetic-stream fallbacks
3. Upgrade reconciliation:
   - when Rig version changes, run targeted parity suite
   - if new `CompletionRequest` fields appear in Rig, add explicit decision handling before upgrade is considered complete

## Implementation Phases

## Phase 0: Baseline And Feature Flag Scaffolding

### Deliverable

- New config surface exists but defaults preserve current behavior.
- Baseline latency/error metrics collection is available.

### Scope

- `src/config/types.rs`
- `src/config/toml_schema.rs`
- `src/config/load.rs`
- `src/config/runtime.rs`
- `METRICS.md`

## Phase 1: Request Semantic Parity

### Deliverable

- `tool_choice` and `output_schema` are explicitly handled in `SpacebotModel`.
- Unsupported combinations produce visible behavior according to mode.

### Scope

- `src/llm/model.rs`
- `src/llm/anthropic/params.rs`
- tests in `src/llm/model.rs` and `tests/`

## Phase 2: Streaming Channel And Cortex Chat

### Deliverable

- Channel and cortex-chat can use Rig streaming under feature flags.
- Final history reconciliation is correct and adapter-safe.

### Scope

- `src/agent/channel.rs`
- `src/agent/cortex_chat.rs`
- `src/hooks/spacebot.rs`
- `src/messaging/slack.rs`
- `src/messaging/discord.rs`
- `src/messaging/telegram.rs`
- `src/api/state.rs`

## Phase 3: Worker Read-Only Tool Concurrency

### Deliverable

- Eligible worker prompts can run read-only tools concurrently with bounded safety.

### Scope

- `src/agent/worker.rs`
- `src/tools.rs`
- relevant tool modules

## Phase 4: Drift Detection, Docs, And Rollout Hardening

### Deliverable

- Validation matrix passes.
- Docs and operational runbooks updated.

### Scope

- `README.md`
- `METRICS.md`
- `docs/content/docs/(configuration)/config.mdx`
- `tests/*`

## Dependency-Aware Task Graph

| Task ID | Task | Depends On |
|---|---|---|
| T1 | Add rollout config types, defaults, load/runtime plumbing | none |
| T2 | Add parity decision helpers and provider capability gating | T1 |
| T3 | Implement `tool_choice` forwarding for OpenAI-compatible, Responses, and Anthropic | T2 |
| T4 | Implement `output_schema` forwarding for OpenAI-compatible, Responses, and Anthropic | T2 |
| T5 | Add parity metrics, logs, and serializer unit tests | T3, T4 |
| T6 | Add channel streaming execution path behind feature flag | T1, T5 |
| T7 | Add cortex-chat streaming execution path behind feature flag | T1, T5 |
| T8 | Harden adapter streaming behavior and terminal reconciliation | T6, T7 |
| T9 | Define read-only tool allowlist and concurrency config validation | T1 |
| T10 | Implement worker request-level concurrency gating | T9 |
| T11 | Add worker concurrency tests and downgrade-on-violation behavior | T10 |
| T12 | Add drift-reconciliation suite and upgrade checklist | T5, T8, T11 |
| T13 | Update docs, metrics docs, and rollout runbook | T12 |
| T14 | Run staged rollout validation and final gates | T13 |

## Task Ownership And File Scope

| Task ID | Owner | File Scope | Merge Conflict Notes |
|---|---|---|---|
| T1 | llm-runtime | `src/config/types.rs`, `src/config/toml_schema.rs`, `src/config/load.rs`, `src/config/runtime.rs` | Keep config-only changes isolated from agent logic |
| T2 | llm-runtime | `src/llm/model.rs`, `src/llm/anthropic/params.rs` | Avoid mixing serializer changes with streaming changes |
| T3 | llm-runtime | `src/llm/model.rs`, `src/llm/anthropic/params.rs` | Same write set as T2/T4; execute serially |
| T4 | llm-runtime | `src/llm/model.rs`, `src/llm/anthropic/params.rs` | Same write set as T2/T3; execute serially |
| T5 | observability + qa | `src/llm/model.rs`, `tests/*`, `METRICS.md` | Keep tests in separate commits if possible |
| T6 | agent-orchestration | `src/agent/channel.rs`, `src/hooks/spacebot.rs` | Hot spot with T8; branch carefully |
| T7 | agent-orchestration | `src/agent/cortex_chat.rs`, `src/hooks/spacebot.rs` | Coordinate with T6 on hook changes |
| T8 | messaging-adapters | `src/messaging/slack.rs`, `src/messaging/discord.rs`, `src/messaging/telegram.rs`, `src/api/state.rs` | Separate by adapter if parallelizing |
| T9 | tools + security | `src/tools.rs`, selected `src/tools/*.rs` | Do not broaden allowlist without review |
| T10 | agent-orchestration | `src/agent/worker.rs` | Isolate from channel changes |
| T11 | qa | `src/agent/worker.rs`, `tests/*` | Can stack on T10 |
| T12 | qa + llm-runtime | `tests/*`, `README.md` | Minimal overlap if docs staged later |
| T13 | qa-docs | `README.md`, `METRICS.md`, `docs/content/docs/(configuration)/config.mdx` | Low conflict if delayed to end |
| T14 | release/qa | no code changes required | Verification-only |

## Detailed Tasks

### T1: Add Rollout Config Plumbing

- Add `RigAlignmentConfig` and `RigSemanticsMode`.
- Expose runtime accessors via `RuntimeConfig`.
- Default all new behavior to existing-safe values:
  - `request_semantics_mode = shadow`
  - streaming disabled
  - concurrency = 1

Acceptance criteria:

- Config parses with and without new block.
- Old configs continue to load unchanged.
- Runtime snapshot surfaces the new config values.

### T2: Add Request Capability Decision Helpers

- Add helper functions that evaluate:
  - provider/API type
  - `tool_choice` present
  - `output_schema` present
  - rollout mode
- Centralize decision logic so every provider path uses the same rules.

Acceptance criteria:

- No serializer path decides independently without using shared helper logic.
- Unsupported combinations produce deterministic decision outcomes.

### T3: Implement `tool_choice` Forwarding

- OpenAI chat-completions payloads:
  - map Rig `ToolChoice` to provider JSON
- OpenAI Responses payloads:
  - map Rig `ToolChoice` to provider JSON
- Anthropic payloads:
  - map Rig `ToolChoice` to provider JSON when supported

Acceptance criteria:

- `ToolChoice::Required` and `ToolChoice::Specific` are observable in serialized requests where supported.
- Unsupported providers hit shadow or enforced behavior explicitly.

### T4: Implement `output_schema` Forwarding

- OpenAI chat-completions:
  - map to `response_format` equivalent where supported
- OpenAI Responses:
  - map to structured output format
- Anthropic:
  - map to `output_config` JSON-schema shape where supported

Acceptance criteria:

- `prompt_typed(...)` is contractually meaningful on supported providers.
- Unsupported providers fail closed in enforced mode.

### T5: Add Parity Metrics And Tests

- Add serializer tests covering:
  - tool choice present
  - output schema present
  - unsupported provider behavior
- Emit semantic parity metrics and logs.

Acceptance criteria:

- Every decision path has a unit test.
- Metrics fire for both applied and rejected cases.

### T6: Add Channel Streaming Path

- Add feature-flagged streaming branch in channel execution.
- Reuse existing hook delta events and history reconciliation.
- Preserve retrigger behavior and malformed tool syntax recovery semantics.

Acceptance criteria:

- Channel streaming can be toggled off without code-path drift.
- Reply-tool terminal behavior remains authoritative.

### T7: Add Cortex Chat Streaming Path

- Replace one-shot prompt path with streaming request under feature flag.
- Continue emitting SSE-compatible events through existing event channel.

Acceptance criteria:

- Cortex chat streams partial text and still persists final assistant response.
- Timeout behavior remains explicit and logged.

### T8: Adapter Streaming Hardening

- Validate per-adapter edit throttling.
- Ensure terminal final response replaces or finalizes in-flight stream cleanly.
- Ensure failure to edit stream degrades gracefully to final-response delivery.

Acceptance criteria:

- No adapter exceeds platform edit cadence.
- Stream failure does not lose final response.

### T9: Read-Only Tool Allowlist And Validation

- Define allowlist in config defaults.
- Add validator/helper that rejects concurrency for any non-allowlisted tool.
- Add comments documenting why each allowlisted tool is safe.

Acceptance criteria:

- No write-capable tool is concurrency-enabled.
- Test suite fails on allowlist regression.

### T10: Worker Request-Level Concurrency

- Apply `with_tool_concurrency(...)` only when:
  - worker config permits it
  - all exposed tools for the request are allowlisted or the specific request class is concurrency-safe

Acceptance criteria:

- Default worker behavior remains sequential.
- Read-only workloads can opt into bounded concurrency.

### T11: Worker Concurrency Tests And Safety Downgrade

- Add tests for:
  - concurrent read-only tools
  - sequential fallback on violation
  - metrics/logging on downgrade

Acceptance criteria:

- Violation path is deterministic and observable.
- Sequential fallback occurs at most once per request.

### T12: Drift-Reconciliation Suite

- Add focused regression suite guarding:
  - request semantics parity
  - stream history reconciliation
  - adapter finalization
  - concurrency allowlist
- Add upgrade checklist entry for future Rig version bumps.

Acceptance criteria:

- Rig upgrade PRs have a targeted parity suite to run.
- Drift is detected by CI before merge.

### T13: Documentation And Runbook

- Update user-facing config docs.
- Update metrics documentation.
- Update README section describing how Spacebot uses Rig and which features are intentionally not adopted.

Acceptance criteria:

- Docs match config keys and rollout modes.
- No new behavior is undocumented.

### T14: Rollout Validation

- Run staged rollout:
  1. semantics shadow mode
  2. semantics enforced in staging
  3. channel streaming canary
  4. cortex-chat streaming canary
  5. worker concurrency canary

Acceptance criteria:

- Each stage has explicit rollback trigger and evidence commands.

## Test Strategy

### Unit Tests

Targets:

- serializer mapping functions
- capability gating helpers
- allowlist classification
- history reconciliation helpers
- streaming terminal-state helpers

Logging expectations:

- each negative-path test asserts at least one structured warning or metric increment

### Integration Tests

Targets:

- channel streaming with tool call then final reply
- cortex-chat streaming SSE events
- worker concurrency with mock read-only tools
- unsupported provider semantics in `shadow` and `enforced` modes

Logging expectations:

- `agent_id`, `process_type`, `provider`, and `terminal_reason` appear in emitted traces/logs

### End-To-End Tests

Targets:

- Slack/Discord/Telegram end-to-end streaming edit path with synthetic inbound message fixture
- final response persistence after stream completion
- concurrency-safe worker scenario with multiple read-only tool calls

Logging expectations:

- no end-user visible secret leakage in logs
- adapter degrade-to-final path is logged when edit retries fail

## Validation Matrix

| Req ID | Requirement | PASS Evidence Command | FAIL Signal |
|---|---|---|---|
| R1 | Config is backward compatible | `cargo test config::load -- --nocapture` | any old-config fixture fails to load |
| R2 | `tool_choice` is forwarded on supported providers | `cargo test forwards_tool_choice -- --nocapture` | serialized payload missing provider-specific tool choice |
| R3 | `output_schema` is forwarded on supported providers | `cargo test forwards_output_schema -- --nocapture` | serialized payload missing schema format field |
| R4 | Unsupported semantics are explicit in enforced mode | `cargo test rejects_unsupported_semantics_enforced -- --nocapture` | request succeeds silently |
| R5 | Channel streaming preserves terminal reply semantics | `cargo test channel_streaming_reply_terminal -- --nocapture` | reply delivered but loop continues or history corrupts |
| R6 | Cortex chat streaming emits partials and persists final response | `cargo test cortex_chat_streaming_persists_final -- --nocapture` | SSE partials absent or final message not stored |
| R7 | Adapter streaming obeys edit cadence | `cargo test adapter_streaming_throttle -- --nocapture` | edit timestamps violate throttle assertions |
| R8 | Worker concurrency only applies to allowlisted tools | `cargo test worker_concurrency_allowlist -- --nocapture` | non-allowlisted tool runs concurrently |
| R9 | Worker concurrency improves eligible workloads | `cargo test worker_concurrency_reduces_wall_clock -- --nocapture` | no measurable improvement or regression beyond threshold |
| R10 | Drift suite guards custom Rig bridge | `cargo test rig_parity -- --nocapture` | any parity regression test fails |
| R11 | Docs and gates are clean | `just preflight && just gate-pr` | any gate red |

## Rollout Plan

### Feature Flags

- `request_semantics_mode`
- `channel_streaming`
- `cortex_chat_streaming`
- `worker_read_only_tool_concurrency`

### Rollout Sequence

1. Deploy config plumbing only, all runtime behavior effectively unchanged.
2. Enable `request_semantics_mode = shadow` in staging.
3. Review metrics for unsupported combinations and payload changes.
4. Enable `request_semantics_mode = enforced` in staging.
5. Canary `channel_streaming` for one adapter and one provider family with native streaming.
6. Expand channel streaming to remaining adapters if metrics hold.
7. Enable `cortex_chat_streaming`.
8. Enable worker read-only concurrency for one worker type or task class.

### Rollback

- Rollback mechanism is config-first:
  - set `request_semantics_mode = off`
  - set streaming flags to `false`
  - set concurrency back to `1`
- No database rollback expected if implementation remains config-only.
- If persisted schema is later introduced, rollback must use a new forward migration or a runtime compatibility gate, never edit historical migration files.

### Backwards Compatibility

- Old config files remain valid.
- Old providers without structured output support remain operable in `shadow` or `off`.
- Existing non-streaming behavior remains default until flags are enabled.

## Operational Risks And Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Provider rejects new `tool_choice` or schema field | request failures | shadow mode first, provider allowlist, serializer tests |
| Streaming exposes secret-like partial text | privacy leak | scrub deltas before user-visible delivery |
| History reconciliation diverges between streaming and non-streaming | broken transcript state | shared terminal reconciliation helper and parity tests |
| Concurrency reorders side effects | task corruption | strict allowlist and downgrade-to-sequential |
| Synthetic streams do not improve UX | wasted complexity | measure native vs synthetic separately and keep synthetic rollout optional |
| Rig upgrade changes request shape | silent drift | explicit parity suite and upgrade checklist |

## Commands For Implementation And Verification

### Targeted Verification Commands

```bash
cargo test forwards_tool_choice -- --nocapture
cargo test forwards_output_schema -- --nocapture
cargo test rejects_unsupported_semantics_enforced -- --nocapture
cargo test channel_streaming_reply_terminal -- --nocapture
cargo test cortex_chat_streaming_persists_final -- --nocapture
cargo test adapter_streaming_throttle -- --nocapture
cargo test worker_concurrency_allowlist -- --nocapture
cargo test worker_concurrency_reduces_wall_clock -- --nocapture
cargo test rig_parity -- --nocapture
just preflight
just gate-pr
```

### Logging Review Commands

```bash
rg "request_semantics_mode|output_schema_requested|tool_choice_requested|stream_mode|tool_concurrency" target debug.log logs -g '*.log'
```

## Definition Of Done

- [x] `RigAlignmentConfig` exists, parses, hot-reloads, and defaults to backward-compatible behavior.
- [x] `SpacebotModel` explicitly handles `tool_choice` for supported providers and explicitly rejects or shadows unsupported cases.
- [x] `SpacebotModel` explicitly handles `output_schema` for supported providers and explicitly rejects or shadows unsupported cases.
- [x] `prompt_typed(...)` is contractually meaningful on supported providers and covered by tests.
- [x] Channel streaming is feature-gated, adapter-safe, and preserves terminal reply semantics.
- [x] Cortex-chat streaming is feature-gated and persists final assistant output after streamed partials.
- [x] Worker request-level tool concurrency is bounded, allowlisted, and disabled by default.
- [x] No new path silently drops a semantic request, retry exhaustion, or adapter streaming failure.
- [x] Metrics and logs exist for semantic parity, streaming mode, time to first delta, and concurrency usage.
- [x] Drift-reconciliation tests fail when custom Rig bridge behavior diverges from expected request semantics.
- [x] User-facing config and metrics docs are updated in existing documentation files.
- [x] `just preflight` passes.
- [x] `just gate-pr` passes.
- [x] All targeted validation commands in this document pass with non-zero failure on regression.
