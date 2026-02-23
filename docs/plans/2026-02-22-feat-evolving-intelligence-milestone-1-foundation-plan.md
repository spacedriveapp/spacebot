---
title: "Evolving Intelligence System — Milestone 1: Foundation & Infrastructure"
type: feat
date: 2026-02-22
status: completed
milestone: 1 of 4
brainstorm: docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md
research: docs/research/spark-parity.md
---

# Evolving Intelligence — Milestone 1: Foundation & Infrastructure

Build the event infrastructure, database, configuration, and module scaffolding that all subsequent milestones depend on. Nothing user-visible yet — this is plumbing.

## Overview

The current Cortex is timer-driven and passive. The learning system requires an event-driven processing path, a separate database, runtime-configurable thresholds, and a module structure to house 20+ files. This milestone wires all of that without implementing any learning logic.

**Deliverable:** A `src/learning/` module with a running async loop that receives `ProcessEvent`s, writes to `learning.db`, and reads config from `RuntimeConfig` — doing nothing useful yet, but proving the infrastructure works end-to-end.

## Problem Statement

Three infrastructure gaps block the learning system:

1. **No event-driven Cortex path.** The Cortex runs on a timer (`tick_interval_secs`). The learning engine needs to react to events as they happen (WorkerStarted, WorkerComplete, ToolCompleted, etc.).
2. **Missing event emissions.** `ProcessEvent::MemorySaved` and `CompactionTriggered` are defined in the enum but never emitted by any code. Additional variants are needed for user messages and reactions.
3. **No learning database.** High-frequency learning writes (8-15 per task) would contend with the main memory graph's latency-sensitive reads.

## Proposed Solution

### Phase 0A: ProcessEvent Enrichment

Extend the `ProcessEvent` enum and wire missing emissions.

**Add structured outcome signal to `WorkerComplete`:**

```rust
// src/lib.rs — ProcessEvent::WorkerComplete (existing fields + new learning fields)
WorkerComplete {
    agent_id: AgentId,
    worker_id: WorkerId,
    channel_id: Option<ChannelId>,
    result: String,
    notify: bool,

    // NEW — structured success/failure signal + wall-clock duration for learning.
    success: bool,
    duration_secs: f64,

    // NEW — links this work back to the inbound user message that triggered it.
    // Generated when `UserMessage` is emitted; propagated into Worker/Branch hooks.
    trace_id: Option<String>,
}
```

**Add tool call correlation IDs to `ToolStarted` / `ToolCompleted`:**

> Spark parity: without a deterministic `call_id`, you cannot reliably pair ToolStarted ↔ ToolCompleted to build step envelopes (Milestone 2) or attribute outcomes.

```rust
// src/lib.rs — extend existing tool events
ToolStarted {
    agent_id: AgentId,
    process_id: ProcessId,
    channel_id: Option<ChannelId>,
    tool_name: String,
    call_id: String,            // NEW — Rig internal call correlation id
    trace_id: Option<String>,   // NEW — set from the inbound UserMessage that triggered the work

    // NEW — sanitized structured args for learning.
    // Never include raw file contents or secrets.
    // Cap size (e.g. <= 4KB) so events stay lightweight.
    args_summary: Option<serde_json::Value>,
}
ToolCompleted {
    agent_id: AgentId,
    process_id: ProcessId,
    channel_id: Option<ChannelId>,
    tool_name: String,
    call_id: String,            // NEW — must match ToolStarted.call_id
    trace_id: Option<String>,   // NEW

    // Duplicate args summary for robustness if ToolStarted was missed due to lag.
    args_summary: Option<serde_json::Value>,

    result: String,
}
```

**Propagate `trace_id` into system events spawned from a user message:**
- Add `trace_id: Option<String>` to `WorkerStarted`, `WorkerStatus`, `BranchStarted`, and `BranchResult`.
- For events not triggered by a user message (cron/timers), keep `trace_id = None`.

**Add new variants:**

```rust
// src/lib.rs — new ProcessEvent variants
UserMessage {
    agent_id: AgentId,
    channel_id: ChannelId,
    conversation_id: String,
    message_id: String,
    content: String,
    sender_id: String,      // platform-specific user ID
    platform: String,       // "discord", "slack", "telegram", "web", "webchat"
    trace_id: String,       // NEW — stable per inbound user message; propagated to work + tool events
},
UserReaction {
    agent_id: AgentId,
    channel_id: ChannelId,
    conversation_id: String,
    reaction: String,       // emoji or text
    target_message_id: Option<String>,
    sender_id: String,
    platform: String,
    trace_id: String,
},
```

**Emit missing events:**

| Event | Where to emit | File | Approx line |
|-------|--------------|------|-------------|
| `MemorySaved` | After `memory_save` tool succeeds | `src/tools/memory_save.rs` | TBD (in save handler) |
| `CompactionTriggered` | When compactor starts | `src/agent/compactor.rs` | TBD (in compact fn) |
| `UserMessage` | On inbound message receipt | `src/agent/channel.rs` | ~642 (`handle_message`) |
| `UserReaction` | On reaction/emoji events from adapters | `src/messaging/*.rs` | Per-adapter |

**Add `success` field to WorkerComplete emission:**

Currently in `src/agent/channel.rs` at `spawn_worker_task` (~line 1637), `WorkerComplete` is sent for both success and error. Add:
- `success: bool` derived from whether the worker returned `Ok` or `Err`
- `duration_secs: f64` measured from `Instant::now()` at spawn time
- `trace_id` propagated from the inbound `UserMessage` that triggered the worker (store on the worker’s hook/context so tool events carry it too)

### Phase 0B: Module Scaffolding

Create the `src/learning/` module following project conventions (no `mod.rs` — use `src/learning.rs` as module root).

```
src/learning.rs           — Module root: mod declarations + pub use re-exports
src/learning/
  engine.rs               — LearningEngine: async event loop coordinator
  store.rs                — LearningStore: learning.db CRUD operations
  types.rs                — All learning data types (enums, structs)
  config.rs               — LearningConfig struct + defaults
```

**Registration:**
- Add `pub mod learning;` to `src/lib.rs` (between `identity` and `llm`, alphabetical)
- Add `Learning(Box<learning::LearningError>)` variant to `src/error.rs`

**Convention reminders:**
- `//!` module doc comment at top of every file
- No `#[async_trait]` — use native RPITIT
- Full words, no abbreviations
- `#[non_exhaustive]` on public enums/structs
- `#[serde(rename_all = "snake_case")]` on enums
- Derive order: `Debug, Clone, Serialize, Deserialize`

### Phase 0C: Learning Database

Create a separate `learning.db` SQLite database per agent at `~/.spacebot/agents/{agent_id}/data/learning.db`.

**Initial schema (foundation tables only — layers add their own tables in later milestones):**

```sql
-- Migration: YYYYMMDDHHMMSS_create_learning_db.sql

-- Episodes (Layer 1 foundation)
CREATE TABLE episodes (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,

    -- Attribution / identity
    trace_id TEXT,                     -- stable per inbound user message; NULL for cron/timers
    channel_id TEXT,                   -- nullable (some workers have no channel)
    process_id TEXT NOT NULL,          -- e.g. "worker:<uuid>" or "branch:<uuid>"
    process_type TEXT NOT NULL,        -- "worker" | "branch"

    task TEXT NOT NULL,
    predicted_outcome TEXT,            -- 'success'/'failure'/'partial'/'unknown'
    predicted_confidence REAL DEFAULT 0.0,
    actual_outcome TEXT,
    actual_confidence REAL,
    surprise_level REAL,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    duration_secs REAL,
    phase TEXT,                        -- detected phase at start
    metadata TEXT                      -- JSON blob for extensibility
);
CREATE INDEX idx_episodes_agent ON episodes(agent_id, started_at);
CREATE INDEX idx_episodes_trace ON episodes(trace_id, started_at);
CREATE INDEX idx_episodes_outcome ON episodes(actual_outcome);

-- Steps (Layer 1 — step envelopes)
CREATE TABLE steps (
    id TEXT PRIMARY KEY,
    episode_id TEXT NOT NULL,

    -- Tool call correlation / attribution
    call_id TEXT NOT NULL,             -- ToolStarted/ToolCompleted.call_id
    trace_id TEXT,                     -- copied from episode for convenience
    tool_name TEXT,
    args_summary TEXT,             -- JSON blob (sanitized) from ToolStarted/ToolCompleted.args_summary

    intent TEXT,
    hypothesis TEXT,
    prediction TEXT,                   -- 'success'/'failure'/'partial'
    confidence_before REAL,
    alternatives TEXT,                 -- JSON array
    assumptions TEXT,                  -- JSON array
    result TEXT,
    evaluation TEXT,
    surprise_level REAL,
    confidence_after REAL,
    lesson TEXT,
    evidence_gathered INTEGER DEFAULT 0,
    progress_made INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,

    FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE
);
CREATE INDEX idx_steps_episode ON steps(episode_id, created_at);
CREATE INDEX idx_steps_call_id ON steps(call_id);

-- Learning events log (audit trail — like cortex_events)
CREATE TABLE learning_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    summary TEXT NOT NULL,
    details TEXT,                     -- JSON blob
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_learning_events_type ON learning_events(event_type, created_at);

-- System metrics (time-series)
CREATE TABLE metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    metric_name TEXT NOT NULL,
    metric_value REAL NOT NULL,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_metrics_name ON metrics(metric_name, recorded_at);

-- Learning engine state (KV for heartbeats/cursors without unbounded time-series growth)
CREATE TABLE learning_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```


**Database connection:**
- New `LearningStore` struct wrapping `SqlitePool`
- WAL mode enabled for concurrent read/write
- Connection created in `main.rs` agent bootstrap, passed to learning engine
- Follow existing pattern: `LearningStore::new(pool) -> Arc<Self>`

**Migration approach:**
- Learning.db migrations are separate from main spacebot.db migrations
- Store in `migrations/learning/` subdirectory
- Run at startup via `sqlx::migrate!("migrations/learning")`

### Phase 0D: LearningConfig + RuntimeConfig Integration

**New config struct:**

```rust
// src/learning/config.rs (or in src/config.rs alongside CortexConfig)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LearningConfig {
    pub enabled: bool,
    pub tick_interval_secs: u64,
    pub batch_interval_secs: u64,
    pub advisory_budget_ms: u64,
    pub owner_user_ids: Vec<String>,  // platform:id pairs for owner-only filter
    pub cold_start_episodes: u64,
    pub stale_episode_timeout_secs: u64,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_interval_secs: 30,
            batch_interval_secs: 60,
            advisory_budget_ms: 4000,
            owner_user_ids: Vec::new(),
            cold_start_episodes: 50,
            stale_episode_timeout_secs: 1800,
        }
    }
}
```

**RuntimeConfig integration:**
- Add `pub learning: ArcSwap<LearningConfig>` to `RuntimeConfig` (~line 2815)
- Initialize with `Default::default()` in `RuntimeConfig::new()` (~line 2868)
- Wire hot-reload in `RuntimeConfig::reload_config()` (~line 2937)

**Config TOML:**

```toml
[defaults.learning]
enabled = true
tick_interval_secs = 30
batch_interval_secs = 60
advisory_budget_ms = 4000
owner_user_ids = ["discord:123456", "slack:U12345"]
cold_start_episodes = 50
stale_episode_timeout_secs = 1800
```

### Phase 0E: LearningEngine Async Loop

The coordinator that ties everything together. Follows the `spawn_bulletin_loop` / `spawn_association_loop` pattern.

```rust
// src/learning/engine.rs

pub fn spawn_learning_loop(
    deps: AgentDeps,
    learning_store: Arc<LearningStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_learning_loop(&deps, &learning_store).await {
            tracing::error!(%error, "learning loop exited with error");
        }
    })
}

async fn run_learning_loop(
    deps: &AgentDeps,
    store: &Arc<LearningStore>,
) -> anyhow::Result<()> {
    let mut event_rx = deps.event_tx.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));

    loop {
        let config = (**deps.runtime_config.learning.load()).clone();
        if !config.enabled {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        tokio::select! {
            _ = heartbeat.tick() => {
                // Storage-backed liveness signal for pipeline health checks.
                if let Err(error) = store
                    .set_state("learning_heartbeat", chrono::Utc::now().to_rfc3339())
                    .await
                {
                    tracing::warn!(%error, "failed to update learning heartbeat");
                }
            }
            event = event_rx.recv() => {
                match event {
                    Ok(process_event) => {
                        // Fail-open: log and continue on any error
                        if let Err(error) = handle_event(deps, store, &config, &process_event).await {
                            tracing::warn!(%error, "learning engine event handling failed");
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        tracing::warn!(count, "learning engine lagged behind event stream");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("event channel closed, learning loop exiting");
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn handle_event(
    deps: &AgentDeps,
    store: &Arc<LearningStore>,
    config: &LearningConfig,
    event: &ProcessEvent,
) -> anyhow::Result<()> {
    // Milestone 1: just log events to learning_events table
    // Milestone 2+: route to Layer 1, Layer 2, Layer 4, etc.
    store.log_event(event).await?;
    Ok(())
}
```

**Startup registration in `main.rs` (~line 1583):**

```rust
// After cortex loop spawns
let learning_store = spacebot::learning::LearningStore::connect(
    agent.data_dir.join("learning.db")
).await?;
let learning_handle = spacebot::learning::spawn_learning_loop(
    agent.deps.clone(),
    learning_store.clone(),
);
learning_handles.push(learning_handle);
```

### Phase 0F: Owner-Only Filtering

Implement the owner identification mechanism:

```rust
// In handle_event, skip non-owner *traces* (not just the UserMessage itself).
//
// Spark parity: if you only skip UserMessage, you still learn from all the downstream
// Worker/Tool events that message triggered. The filter must key off `trace_id`.
fn should_process_event(
    config: &LearningConfig,
    non_owner_traces: &mut std::collections::HashSet<String>,
    event: &ProcessEvent,
) -> bool {
    match event {
        ProcessEvent::UserMessage {
            sender_id,
            platform,
            trace_id,
            ..
        } => {
            let key = format!("{platform}:{sender_id}");
            let is_owner = config.owner_user_ids.is_empty() || config.owner_user_ids.contains(&key);
            if !is_owner {
                non_owner_traces.insert(trace_id.clone());
            }
            is_owner
        }

        // Any downstream system event that carries a trace_id inherits the filter decision.
        ProcessEvent::WorkerStarted {
            trace_id: Some(trace_id),
            ..
        }
        | ProcessEvent::WorkerStatus {
            trace_id: Some(trace_id),
            ..
        }
        | ProcessEvent::WorkerComplete {
            trace_id: Some(trace_id),
            ..
        }
        | ProcessEvent::BranchStarted {
            trace_id: Some(trace_id),
            ..
        }
        | ProcessEvent::BranchResult {
            trace_id: Some(trace_id),
            ..
        }
        | ProcessEvent::ToolStarted {
            trace_id: Some(trace_id),
            ..
        }
        | ProcessEvent::ToolCompleted {
            trace_id: Some(trace_id),
            ..
        } => !non_owner_traces.contains(trace_id),

        // Events without a trace_id are treated as system/global signals.
        _ => true,
    }
}
```

## Acceptance Criteria

### Functional Requirements

- [x] `ProcessEvent::WorkerComplete` includes `success: bool`, `duration_secs: f64`, and `trace_id: Option<String>`
- [x] `ProcessEvent::ToolStarted` / `ToolCompleted` include `call_id: String`, `trace_id: Option<String>`, and `args_summary` (sanitized + capped)
- [x] `ProcessEvent::UserMessage` and `ProcessEvent::UserReaction` variants exist and carry `trace_id`
- [x] `trace_id` is propagated into Worker/Branch/Tool events spawned from a `UserMessage`
- [x] `MemorySaved` event emitted from `memory_save`
- [x] `CompactionTriggered` event emitted from compactor
- [x] `UserMessage` event emitted from channel on inbound message
- [x] `src/learning/` module exists with `engine.rs`, `store.rs`, `types.rs`, `config.rs`
- [x] `pub mod learning;` registered in `src/lib.rs`
- [x] `LearningError` variant in `src/error.rs`
- [x] `learning.db` created per agent with WAL mode
- [x] Foundation schema tables exist: `episodes`, `steps`, `learning_events`, `metrics`, `learning_state`
- [x] Learning engine writes a heartbeat to `learning_state` (storage-backed liveness)
- [x] `LearningConfig` struct with `Default` impl
- [x] `RuntimeConfig.learning` field with ArcSwap + hot-reload
- [x] `[defaults.learning]` section parseable from config.toml
- [x] `spawn_learning_loop` runs and receives events without blocking hot path
- [x] Events logged to `learning_events` table
- [x] Owner-only filtering works for full traces (non-owner traces + downstream events skipped)
- [x] All learning engine errors logged and swallowed (fail-open)
- [x] `cargo clippy` passes with no new warnings
- [x] `cargo test` passes (1 pre-existing failure in telegram tests)

### Non-Functional Requirements

- [x] Learning event processing adds < 1ms latency to the event broadcast path
- [x] Tool event payloads are bounded: `ToolCompleted.result` is truncated and `args_summary` is sanitized + capped (no raw file contents)
- [x] learning.db writes are async and non-blocking
- [x] Hot-reload of `LearningConfig` works without restart

## Technical Considerations

### Broadcast channel sizing

The existing `broadcast::Sender<ProcessEvent>` has a bounded buffer. Adding a learning engine subscriber increases consumption. Verify buffer size is adequate — if the learning engine lags, it receives `RecvError::Lagged` (handled above) rather than blocking senders.

### SQLite connection pool for learning.db

Use a separate `SqlitePool` for learning.db (not the main pool). Configure with:
- `max_connections: 2` (one writer, one reader)
- `journal_mode: WAL`
- `busy_timeout: 5000` (5s, generous for personal use)

### Migration separation

Learning.db migrations must be separate from main DB migrations. Two approaches:
1. Separate `migrations/learning/` directory with `sqlx::migrate!("migrations/learning")`
2. Embedded migrations in `LearningStore::connect()` via raw SQL

Option 2 is simpler for a separate database file. The main DB uses sqlx migrations already.

### SpecFlow gap resolution (from analysis)

| Gap | Resolution in this milestone |
|-----|------------------------------|
| Gap 1.1: No success/failure discriminator | Add `success: bool` to `WorkerComplete` |
| Gap 1.2: No user reaction events | Add `UserReaction` variant |
| Gap 1.4: No user message events | Add `UserMessage` variant |
| Gap 8.5: Owner-only has no user ID mapping | `owner_user_ids` config field with `platform:id` format |
| Gap 3.2: learning.db corruption | WAL mode + separate pool. Recreation with empty tables on corruption. |

## Dependencies & Risks

- **ProcessEvent changes are breaking.** All match arms across the codebase need updating for new variants and changed fields. Use `#[non_exhaustive]` and add `_ => {}` catch-all arms in existing handlers.
- **Tool args capture risk.** Never broadcast or persist raw tool args. Always emit a sanitized, capped `args_summary` (drop `file(write).content`, redact secrets) to avoid leaking sensitive data into logs/events.
- **Broadcast channel backpressure.** If learning engine is slow, it lags. This is acceptable (we log and skip) but could cause gaps in learning data during high-activity bursts.
- **Config TOML parsing.** New `[defaults.learning]` section must be backward-compatible with existing configs that don't have it (handled by `Default::default()`).

## References

### Internal References
- ProcessEvent enum: `src/lib.rs:95-173`
- Cortex spawn pattern: `src/main.rs:1572-1583`
- CortexConfig example: `src/config.rs:413-450`
- RuntimeConfig: `src/config.rs:2796-2831`
- MemoryStore pattern: `src/memory/store.rs`
- SpacebotHook event emission: `src/hooks/spacebot.rs:200`
- Channel handle_message: `src/agent/channel.rs:642`
- Worker spawn: `src/agent/channel.rs:1637`

### Project Conventions
- AGENTS.md: No `mod.rs` files, no many small files
- RUST_STYLE_GUIDE.md: Import ordering, struct field ordering, derive ordering, error handling
