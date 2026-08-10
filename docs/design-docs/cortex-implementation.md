# Cortex Implementation Plan

The cortex is designed to be the system's self-awareness -- supervising processes, maintaining memory coherence, maintaining knowledge synthesis, and running health loops. Phase 1 plumbing, Phase 2 health supervision, and Phase 3 maintenance are now live; consolidation remains.

This doc covers the path from "context synthesizer" to "full system supervisor."

## What Exists Today

**Running:**
- `spawn_cortex_loop()` -- instantiates `Cortex`, subscribes to both control and memory event buses, runs a `tokio::select!` loop for event observation and periodic ticks, and refreshes knowledge/profile context.
- `spawn_bulletin_loop()` — compatibility alias to `spawn_cortex_loop()`.
- `spawn_warmup_loop()` -- asynchronous warmup that keeps knowledge synthesis and embedding readiness fresh.

**Defined and instantiated:**
- `Cortex` struct — observes all `ProcessEvent` variants, builds a rolling signal buffer, and runs on configurable tick cadence.
- `Signal` enum — aligned to current `ProcessEvent` surface (worker/branch/tool/memory/compaction/task/link events).
- `CortexHook` — all methods return `Continue` with trace logging.

**Implemented and wired into the cortex loop:**
- `memory/maintenance.rs` — `apply_decay()`, `prune_memories()`, and `merge_similar_memories()` are implemented and wired into the cortex loop.

**Wired through config:**
- `tick_interval_secs` and the knowledge synthesis compatibility intervals are read by the running cortex loop and hot-reload during runtime.
- Phase 2 knobs are active and hot-reloaded: `worker_timeout_secs`, `branch_timeout_secs`, `detached_worker_timeout_retry_limit`, `supervisor_kill_budget_per_tick`, and `circuit_breaker_threshold`.

**Referenced in prompts but don't exist:**
- `memory_consolidate` tool
- `system_monitor` tool

**Event buses:**
- Two per-agent `broadcast::Sender<ProcessEvent>` streams:
  - `event_tx` for control/lifecycle events (channel, branch, worker, compactor, task/link events)
  - `memory_event_tx` for `MemorySaved` telemetry emitted by `memory_save`
- `CompactionTriggered` is emitted by the compactor on `event_tx` when thresholds are reached.

## Phase 1: The Tick Loop (Implemented)

Get the cortex running as a persistent process that observes the event bus and ticks on an interval. Purely programmatic — no LLM.

### Wire missing event emission (done)

- Emit `MemorySaved` from `memory_save` tool after successful save
- Emit `CompactionTriggered` from the compactor when thresholds are hit

### Instantiate the cortex (done)

- Call `Cortex::new()` from `spawn_cortex_loop()` during agent startup
- Subscribe to both buses via `deps.event_tx.subscribe()` and `deps.memory_event_tx.subscribe()`
- Run a `tokio::select!` loop:
  - Receive control events → feed through `observe()`
  - Receive memory events (`MemorySaved`) → feed through `observe()`
  - Tick on `cortex_config.tick_interval_secs`
- Move context synthesis into the cortex's tick loop (currently a standalone free function)

### Fix `observe()` to extract real values (done)

- Map all 12 `ProcessEvent` variants, not just 3
- Pull real `memory_type`, `importance`, content summaries from events
- Enrich `MemorySaved` event variant with `memory_type` and `importance` fields so the cortex gets useful data without querying the store

### Rework `Signal` enum (done)

- Align variants with what `ProcessEvent` actually provides
- Add `WorkerStarted`, `BranchStarted`, `WorkerStatus`
- Remove variants that have no event source (`ChannelStarted`, `ChannelEnded` — these can be added later when the messaging layer emits them)

**End state:** The cortex is running, consuming all events, building a signal buffer, and ticking. It sees everything but doesn't act on anything yet.

## Phase 2: System Health (Implemented)

The supervisor role is now active and still fully programmatic (no LLM loop for health decisions).

### Control plane

- `ProcessControlRegistry` provides cancellation routing for:
  - live channel workers/branches through weak `ChannelControlHandle`s
  - detached ready-task workers through registered `DetachedWorkerControl` entries
- Channel cancel convergence now uses reason-aware methods that emit terminal synthetic events:
  - worker cancel emits one `WorkerComplete` payload (`result`, `notify`, `success`)
  - branch cancel removes branch status, logs terminal run, emits one synthetic `BranchResult`

### Detached worker single-winner lifecycle

Detached ready-task workers use a lifecycle state machine:

```text
0 active -> 1 completing -> 3 terminal
0 active -> 2 killing    -> 3 terminal
```

- Completion path must win `CAS(0 -> 1)` before terminal side-effects.
- Supervisor kill path must win `CAS(0 -> 2)` (or observe prior `2`) before timeout side-effects.
- Losing path performs no terminal writes/events.

### Timeout retry and quarantine policy

On detached timeout kill, a single task update writes both status and metadata:

- `supervisor_timeout_count` increments each timeout.
- `supervisor_timeout_exhausted` flips to `true` once the retry limit is exceeded.
- If count is `<= detached_worker_timeout_retry_limit`: task returns to `ready` and clears `worker_id`.
- If count is `> detached_worker_timeout_retry_limit`: task moves to `backlog` and clears `worker_id`.

### Lag-aware health tick

Per tick, cortex maintains runtime maps for workers, branches, branch latency, and breaker state.

- If control receiver lag occurred since the prior tick, kill enforcement is skipped for that tick only.
- `health_check` logs include `kill_skipped_due_to_lag=true` when skipped.
- Lag flag is cleared at tick end so enforcement resumes next interval.

### Kill budget and ordering

- Overdue workers/branches are selected in deterministic oldest-first order.
- Cancellation attempts are capped by `supervisor_kill_budget_per_tick` each tick.
- Remaining overdue items roll to subsequent ticks.

### Observe-only circuit breaker

- Worker failures increment `worker_type:<type>` counters.
- Tool failures increment only on structured JSON results with boolean `success=false` or `ok=false`.
- No text heuristics are used in Phase 2.
- Threshold crossing emits `circuit_breaker_tripped` with `action_taken="observe_only"`.
- Success resets the counter/tripped flag for that key.
- Breaker state is in-memory only in Phase 2 and resets on process restart.

## Phase 3: Memory Maintenance (Implemented)

Memory maintenance is now wired into the cortex loop.

### Scheduling and control

- `maintenance_interval_secs` is part of `CortexConfig` and hot-reloads at runtime
- The cortex tick loop schedules `run_maintenance_with_cancel()` with the current `MaintenanceConfig`
- Maintenance reports log decayed, pruned, and merged counts
- Invalid maintenance config values are rejected at load/update time:
  - `maintenance_interval_secs >= 1`
  - `maintenance_decay_rate`, `maintenance_prune_threshold`, `maintenance_merge_similarity_threshold` in `[0.0, 1.0]`
  - `maintenance_min_age_days >= 0`
- Maintenance runs with timeout, graceful cancellation, forced abort fallback, and a recurring-failure circuit breaker

### Merge behavior

- LanceDB similarity search drives near-duplicate detection above `merge_similarity_threshold` (default 0.95)
- The higher-importance memory stays the survivor
- Survivor content is updated, associations are rewired atomically in SQLite, and an `Updates` edge is preserved
- The merged memory is soft-forgotten and its embedding is removed
- The survivor embedding is recomputed after merge
- Per-pass work is bounded (candidate and merge caps) to avoid unbounded scans on large corpora

**End state:** Memories decay over time, low-importance orphans get pruned, and near-duplicates get merged on autopilot under cortex supervision.

## Phase 4: Memory Consolidation (LLM-Assisted)

This is where the cortex becomes an LLM agent. Cross-channel memory coherence requires reasoning that can't be done programmatically.

### Implement `system_monitor` tool

Returns structured data about the current system state:
- Active channels, workers, branches (from Phase 2 tracking state)
- Memory store stats: count by type, importance distribution, recent saves
- Recent error rates and patterns (from circuit breaker state)
- Compaction history

### Implement `memory_consolidate` tool

Accepts operations:
- **merge** — combine two memories by ID, keep richer content, union associations
- **associate** — create a typed edge between two memories (RelatedTo, Updates, Contradicts, CausedBy, PartOf)
- **lower_importance** — decay a specific memory's importance score
- **flag_contradiction** — create a `Contradicts` edge between two memories

### Build the consolidation agent

- Use `cortex.md.j2` system prompt (the existing 93-line supervisor prompt, possibly trimmed to focus on consolidation)
- Tool server: `memory_consolidate` + `system_monitor` + `memory_save` (for creating observations)
- Run on a separate interval from the tick loop (consolidation is expensive — default every 6 hours, or triggered by event volume)
- Feed it a summary of recent signal buffer activity as user prompt context

### Cross-channel coherence

The main value of an LLM-powered cortex — connecting dots that individual branches can't see.

- When `MemorySaved` events arrive from different channels with similar content, queue them for the next consolidation run
- The consolidation agent decides: merge, associate, or leave alone
- Duplicate detection across channels (the same fact saved by two different conversations)

### Observation creation

- The cortex is the only process that creates `Observation` type memories
- Pattern detection from the signal buffer: recurring topics, frequent task types, behavioral patterns
- Low importance by default — ambient awareness, not high-priority recall
- Observations feed into the next knowledge synthesis pass, closing the loop

**End state:** The cortex maintains memory coherence across channels, creates cross-channel associations, detects patterns, and generates observations.

## Phase 5: CortexHook

Make the hook do real work instead of trace logging.

- Track LLM call counts and latencies per process type
- Detect anomalies: unusually high tool call rates, repeated errors on the same tool
- Feed anomaly signals into the signal buffer for the tick loop
- Lower priority than the other phases — the tick loop already sees events via the bus. The hook adds per-LLM-call granularity.

## Phase Ordering

```
Phase 1 (tick loop)     — standalone, just plumbing
Phase 2 (health)        — depends on Phase 1
Phase 3 (maintenance)   — depends on Phase 1, independent of Phase 2
Phase 4 (consolidation) — depends on Phase 1, benefits from Phase 3
Phase 5 (hook)          — independent, can be done anytime after Phase 1
```

Phases 2 and 3 can run in parallel. Phase 4 should wait until maintenance is running so the graph is clean before consolidation adds complexity.

## Open Questions

**Worker/branch cancellation from the cortex.** The current `CancelTool` operates through `ChannelState`, which is channel-scoped. The cortex doesn't own a channel. Options:
- Store cancellation tokens in a shared registry (`HashMap<WorkerId, CancellationToken>` in `AgentDeps` or similar)
- Emit a `ProcessEvent::KillWorker` that the owning channel listens for and acts on
- Give the cortex direct access to worker handles

**Consolidation frequency.** Fixed interval (every N hours) vs event-driven (consolidate when >N new memories since last run). Event-driven is more efficient but harder to reason about. Could start with a fixed interval and add event-driven triggering later.

**Circuit breaker actions.** The prompt says "disable the failing component" but the mechanism matters:
- Disable a tool for all workers? (requires the tool server to check a flag)
- Switch to a fallback model in routing config? (requires writing to `RoutingConfig`)
- Just log and surface in `system_monitor` output for the consolidation agent to reason about?
- Starting with logging + surfacing is safest. Active intervention can come later.

**Cortex vs compactor overlap.** The cortex prompt says to flag channels approaching context limits, but the compactor already handles that per-channel. The cortex's role here is probably monitoring/alerting (detect when a compactor is falling behind) rather than direct intervention.
