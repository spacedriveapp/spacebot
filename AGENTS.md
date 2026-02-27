# AGENTS.md

Implementation guide for coding agents working on Spacebot. Read `RUST_STYLE_GUIDE.md` before writing any code.

## What Spacebot Is

A Rust agentic system where every LLM process has a dedicated role and delegation is the only way work gets done. It replaces the monolithic session model (one LLM thread doing conversation + thinking + tool execution + memory retrieval + compaction) with specialized processes that only do one thing.

Single binary. No server dependencies. Runs on tokio. All data lives in embedded databases in a local data directory.

**Stack:** Rust (edition 2024), tokio, Rig (v0.30.0, agentic loop framework), SQLite (sqlx), LanceDB (embedded vector + FTS), redb (embedded key-value).

## Migration Safety

- **NEVER edit an existing migration file in place** once it has been committed or applied in any environment.
- Treat migration files as immutable; modifying historical migrations causes checksum mismatches and can block startup.
- For schema changes, always create a new migration with a new timestamp/version.

## Delivery Gates (Mandatory)

Run these checks in this order for code changes before pushing or updating a PR:

1. `just preflight` — validate git/remote/auth state and avoid push-loop churn.
2. `just gate-pr` — enforce formatting, compile checks, migration safety, lib tests, and integration test compile.

If `just` is unavailable, run the equivalent scripts directly in the same order: `./scripts/preflight.sh` then `./scripts/gate-pr.sh`.

Additional rules:

- If the same command fails twice in one session, stop rerunning it blindly. Capture root cause and switch strategy.
- For every external review finding marked P1/P2, add a targeted verification command in the final handoff.
- For changes in async/stateful paths (worker lifecycle, cancellation, retrigger, recall cache behavior), include explicit race/terminal-state reasoning in the PR summary and run targeted tests in addition to `just gate-pr`.
- Do not push if any gate is red.

## Architecture Overview

Five process types. Every LLM process is a Rig `Agent<SpacebotModel, SpacebotHook>`. They differ in system prompt, tools, history, and hooks.

### Channels

The user-facing LLM process. One per conversation (Telegram DM, Discord thread, etc). Has soul, identity, personality. Talks to the user. Delegates everything else.

A channel does NOT: execute tasks directly, search memories itself, do heavy tool work.

The channel is always responsive — never blocked by work, never frozen by compaction. When it needs to think, it branches. When it needs work done, it spawns a worker. When context gets full, the compactor has already handled it.

**Tools:** reply, branch, spawn_worker, route, cancel, skip, react  
**Context:** Conversation history + compaction summaries + status block  
**History:** Persistent `Vec<Message>`, passed via `agent.prompt().with_history(&mut history)`

### Branches

A fork of the channel's context that goes off to think. Has the channel's full conversation history — same context, same memories, same understanding. Operates independently. The channel never sees the working, only the conclusion.

Creating a branch is `let branch_history = channel_history.clone()`.

The branch result is injected into the channel's history as a distinct message type. Then the branch is deleted. Multiple branches can run concurrently per channel (configurable limit). First done, first incorporated.

**Tools:** memory_recall, memory_save, channel_recall, spawn_worker  
**Context:** Clone of channel history at fork time  
**Lifecycle:** Short-lived. Returns a conclusion, then deleted.

### Workers

Independent process that does a job. Gets a specific task, a focused system prompt, and task-appropriate tools. No channel context, no soul, no personality.

Two kinds:
- **Fire-and-forget:** Does a job and returns a result. Memory recall, summarization, one-shot tasks.
- **Interactive:** Long-running, accepts follow-up input from the channel. Coding sessions, complex multi-step tasks.

Workers are pluggable. A worker can be:
- A Rig agent with shell/file/exec tools
- An OpenCode subprocess
- Any external process that accepts a task and reports status

**Tools:** shell, file, exec, set_status (varies by worker type)  
**Context:** Fresh prompt + task description. No channel history.  
**Lifecycle:** Fire-and-forget or long-running. Reports status via `set_status` tool.

### The Compactor

NOT an LLM process. A programmatic monitor per channel. Watches context size and triggers compaction before the channel fills up.

Tiered thresholds:
- **>80%** — background compaction worker (summarize + extract memories)
- **>85%** — aggressive summarization
- **>95%** — emergency truncation (no LLM, just drop oldest turns)

The compaction worker runs alongside the channel without blocking it. Compacted summaries stack at the top of the context window.

### The Cortex

System-level observer. Primary job: generate the **memory bulletin** — a periodically refreshed, LLM-curated summary of the agent's current knowledge. Runs on a configurable interval (default 60 min), uses `memory_recall` to query across multiple dimensions (identity, events, decisions, preferences), synthesizes into a ~500 word briefing cached in `RuntimeConfig::memory_bulletin`. Every channel reads this on every turn via `ArcSwap`.

Also observes system-wide signals for future health monitoring and memory consolidation.

**Tools (bulletin generation):** memory_recall, memory_save  
**Tools (future health monitoring):** memory_consolidate, system_monitor  
**Context:** Fresh per bulletin run. No compaction needed.

### Status Injection

Every turn, the channel gets a live status block injected into its context — active workers, recently completed work, branch states. Workers set their own status via `set_status` tool. Short branches are invisible (only appear if running >3s).

### Cron Jobs

Database-stored scheduled tasks. Each cron job has a prompt, interval, delivery target, and optional active hours. When a timer fires, it gets a fresh short-lived channel with full branching and worker capabilities. Multiple cron jobs run independently and concurrently.

## Key Types

```
SpacebotModel          — custom CompletionModel impl, routes through LlmManager
SpacebotHook           — PromptHook impl for channels/branches/workers (status, usage, cancellation)
CortexHook             — PromptHook impl for cortex (system observation)
ProcessType            — enum: Channel, Branch, Worker
ProcessEvent           — tagged enum for inter-process events
Channel (struct)       — owns history, spawns branches, routes to workers
WorkerState            — state machine: Running, WaitingForInput, Done, Failed
Memory                 — content + type + importance + timestamps + source + associations
MemoryType             — enum: Fact, Preference, Decision, Identity, Event, Observation
ChannelId              — Arc<str> type alias
AgentDeps              — dependency bundle (memory_store, llm_manager, tool_server, event_tx)
LlmManager             — holds provider clients, routes by model name
DecryptedSecret        — secret wrapper, redacts in Debug/Display
CronConfig             — prompt + interval + active_hours + notify
```

## Module Map

```
src/
├── main.rs             — CLI entry, config loading, startup
├── lib.rs              — re-exports, shared types
├── config.rs           — configuration loading/validation
├── error.rs            — top-level Error enum wrapping domain errors
│
├── llm.rs              → llm/
│   ├── manager.rs      — LlmManager: provider routing, model resolution, fallback chains
│   ├── model.rs        — SpacebotModel: CompletionModel impl
│   ├── routing.rs      — RoutingConfig: process-type defaults, task-type overrides, fallbacks
│   └── providers.rs    — provider client init (Anthropic, OpenAI, etc.)
│
├── agent.rs            → agent/
│   ├── channel.rs      — Channel: user-facing conversation
│   ├── branch.rs       — Branch: fork context, think, return result
│   ├── worker.rs       — Worker: fire-and-forget + interactive management
│   ├── compactor.rs    — Compactor: programmatic context monitor
│   ├── cortex.rs       — Cortex: system-level observer
│   └── status.rs       — StatusBlock: live status snapshot
│
├── hooks.rs            → hooks/
│   ├── spacebot.rs     — SpacebotHook: channels/branches/workers
│   └── cortex.rs       — CortexHook: cortex observation
│
├── tools.rs            → tools/
│   ├── reply.rs        — send message to user (channel only)
│   ├── branch_tool.rs  — fork context and think (channel only)
│   ├── spawn_worker.rs — create new worker (channel + branch)
│   ├── route.rs        — send follow-up to active worker (channel only)
│   ├── cancel.rs       — cancel worker or branch (channel only)
│   ├── skip.rs         — opt out of responding (channel only)
│   ├── react.rs        — add emoji reaction (channel only)
│   ├── memory_save.rs  — write memory to store (branch + cortex + compactor)
│   ├── memory_recall.rs— search + curate memories (branch only)
│   ├── channel_recall.rs— retrieve transcript from other channels (branch only)
│   ├── set_status.rs   — update worker status (workers only)
│   ├── shell.rs        — execute shell commands (task workers)
│   ├── file.rs         — read/write/list files (task workers)
│   ├── exec.rs         — run subprocess (task workers)
│   ├── browser.rs      — web browsing (task workers)
│   └── cron.rs         — cron management (channel only)
│
├── memory.rs           → memory/
│   ├── store.rs        — MemoryStore: CRUD + graph ops (SQLite)
│   ├── types.rs        — Memory, Association, MemoryType, RelationType
│   ├── search.rs       — hybrid search (vector + FTS + RRF + graph traversal)
│   ├── lance.rs        — LanceDB table management, embedding storage
│   ├── embedding.rs    — embedding generation via LlmManager
│   └── maintenance.rs  — decay, prune, merge, reindex
│
├── messaging.rs        → messaging/
│   ├── traits.rs       — Messaging trait + MessagingDyn companion
│   ├── manager.rs      — MessagingManager: start all, fan-in, route outbound
│   ├── discord.rs      — Discord adapter
│   ├── telegram.rs     — Telegram adapter
│   └── webhook.rs      — Webhook receiver (programmatic access)
│
├── conversation.rs     → conversation/
│   ├── history.rs      — conversation persistence (SQLite)
│   └── context.rs      — context assembly (prompt + identity + memories + status)
│
├── cron.rs             → cron/
│   ├── scheduler.rs    — timer management
│   └── store.rs        — cron CRUD (SQLite)
│
├── identity.rs         → identity/
│   └── files.rs        — load SOUL.md, IDENTITY.md, USER.md
│
├── secrets.rs          → secrets/
│   └── store.rs        — encrypted credentials (AES-256-GCM, redb)
│
├── settings.rs         → settings/
│   └── store.rs        — key-value settings (redb)
│
└── db.rs               → db/
    └── migrations.rs   — SQLite migrations
```

Module roots (e.g., `src/memory.rs`) contain `mod` declarations and re-exports. Never create `mod.rs` files.

Tools are organized by function, not by consumer. Which processes get which tools is configured via factory functions in `tools.rs`.

## Three Databases

Each doing what it's best at. No server processes.

**SQLite** (via sqlx) — relational data: conversations, memory graph, cron jobs. Queries with joins, ordering, filtering. Migrations in `migrations/`.

**LanceDB** — vector/search data: embeddings (HNSW), full-text search (Tantivy), hybrid search (RRF). Joined to SQLite on memory ID.

**redb** — key-value config: settings, encrypted secrets. Separate from SQLite so config can be backed up independently.

Actual queries live in the modules that use them — `memory/store.rs` has graph queries, `memory/lance.rs` has search, `conversation/history.rs` has conversation queries. The `db/` module is just connection setup and migration running.

## Memory System

Memories are structured objects, not files. Every memory is a row in SQLite with typed metadata and graph connections, paired with a vector embedding in LanceDB.

**Types:** Fact, Preference, Decision, Identity, Event, Observation.

**Graph edges:** RelatedTo, Updates, Contradicts, CausedBy, PartOf. Auto-associated on creation via similarity search. >0.9 similarity marks as `Updates`.

**Three creation paths:**
1. Branch-initiated (during conversation) — branch uses `memory_save` tool
2. Compactor-initiated (during compaction) — extract memories from context being compacted
3. Cortex-initiated (system-level) — consolidation, observations

**Recall flow:** Branch → recall tool → hybrid search (vector + FTS + RRF + graph traversal) → curate → return clean results. The channel never sees raw search results.

**Importance:** Score 0-1. Influenced by explicit importance, access frequency, recency, graph centrality. Identity memories exempt from decay.

**Identity files** (SOUL.md, IDENTITY.md, USER.md) are loaded from disk into system prompts. They are NOT graph memories.

## Rig Integration

Every LLM process is a Rig `Agent`. Key patterns:

**Agent construction:**
```rust
let agent = AgentBuilder::new(model.clone())
    .preamble(&system_prompt)
    .hook(SpacebotHook::new(process_id, process_type, event_tx.clone()))
    .tool_server_handle(tools.clone())
    .default_max_turns(50)
    .build();
```

**History is external**, passed on each call:
```rust
let response = agent.prompt(&user_message)
    .with_history(&mut history)
    .max_turns(5)
    .await?;
```

**Branching is a clone:**
```rust
let branch_history = channel_history.clone();
```

**Custom CompletionModel** — `SpacebotModel` routes through `LlmManager`. We don't use Rig's built-in provider clients.

**PromptHook** — `SpacebotHook` sends `ProcessEvent`s for status reporting, usage tracking, cancellation. Returns `Continue`, `Terminate`, or `Skip`.

**ToolServer topology:**
- Per-channel `ToolServer` (no memory tools, just channel action tools added per turn)
- Per-branch `ToolServer` with memory tools (memory_save, memory_recall)
- Per-worker `ToolServer` with task-specific tools (shell, file, exec)
- Per-cortex `ToolServer` with memory_save

**Max turns:** Rig defaults to 0 (single call). Always set explicitly.
- Workers: `max_turns(50)` — many iterations
- Branches: `max_turns(10)` — a few iterations
- Channels: `max_turns(5)` — typically 1-3 turns

**Error recovery:** Rig returns chat history in `MaxTurnsError` and `PromptCancelled`. Use this for worker timeout, cancellation, budget enforcement.

**We don't use:** Rig's built-in provider clients, RAG/vector store integrations, Agent-as-Tool, Pipeline system.

## Build Order

Phase 1 — Foundation:
1. `error.rs` — top-level Error enum
2. `config.rs` — configuration loading
3. `db/` — SQLite + LanceDB + redb connection setup, migrations
4. `llm/` — SpacebotModel, LlmManager, provider init
5. `main.rs` — startup, config loading, database init

Phase 2 — Memory:
1. `memory/types.rs` — Memory, Association, MemoryType, RelationType
2. `memory/store.rs` — MemoryStore CRUD + graph operations
3. `memory/lance.rs` — LanceDB table management, embedding storage
4. `memory/embedding.rs` — embedding generation
5. `memory/search.rs` — hybrid search (vector + FTS + RRF + graph traversal)
6. `memory/maintenance.rs` — decay, prune, merge, reindex

Phase 3 — Agent Core:
1. `hooks/spacebot.rs` — SpacebotHook (ProcessEvent sending)
2. `agent/status.rs` — StatusBlock
3. `tools/` — implement tools (start with memory_save, memory_recall, set_status)
4. `agent/worker.rs` — Worker lifecycle (fire-and-forget first, interactive later)
5. `agent/branch.rs` — Branch (fork, think, return result)
6. `agent/channel.rs` — Channel (message handling, branching, worker management, status injection)
7. `agent/compactor.rs` — Compactor (threshold monitor, compaction worker spawning)

Phase 4 — System:
1. `identity/` — load identity files
2. `conversation/` — history persistence, context assembly
3. `prompts/` — system prompt files for each process type
4. `agent/cortex.rs` — Cortex
5. `hooks/cortex.rs` — CortexHook
6. `cron/` — scheduler + store

Phase 5 — Messaging:
1. `messaging/traits.rs` — Messaging trait + MessagingDyn
2. `messaging/manager.rs` — MessagingManager (fan-in, routing)
3. `messaging/webhook.rs` — Webhook receiver (for testing + programmatic access)
4. `messaging/telegram.rs` — Telegram
5. `messaging/discord.rs` — Discord

Phase 6 — Hardening:
1. `secrets/` — encrypted credential storage
2. `settings/` — key-value settings
3. Leak detection (scan tool output via SpacebotHook)
4. Workspace path guards (reject writes to identity/memory paths)
5. Circuit breaker for cron jobs and background tasks

## Anti-Patterns

**Don't block the channel.** The channel never waits on branches, workers, or compaction. If you're writing code where the channel awaits a branch result before responding, the design is wrong.

**Don't dump raw search results into channel context.** Memory recall goes through a branch, which curates. The channel gets clean conclusions, not 50 raw database rows.

**Don't give workers channel context.** Workers get a fresh prompt and a task. If a worker needs conversation context, that's a branch, not a worker.

**Don't make the compactor an LLM process.** The compactor is programmatic — it watches a number (context token count) and spawns workers. The LLM work happens in the compaction worker it spawns.

**Don't store prompts as string constants in Rust.** System prompts live in `prompts/` as markdown files. Load at startup or on demand.

**Don't create `mod.rs` files.** Use `src/memory.rs` as the module root, not `src/memory/mod.rs`.

**Don't silently discard errors.** No `let _ =` on Results. Handle them, log them, or propagate them. The only exception is `.ok()` on channel sends where the receiver may be dropped.

**Don't use `#[async_trait]`.** Use native RPITIT for async traits. Only add a companion `Dyn` trait when you actually need `dyn Trait`.

**Don't create many small files.** Implement functionality in existing files unless it's a new logical component.

**Don't abbreviate variable names.** `queue` not `q`, `message` not `msg`, `channel` not `ch`. Common abbreviations like `config` are fine.

**Don't add new features without updating existing docs.** When a feature change affects user-facing configuration, behaviour, or architecture, update the relevant existing documentation (`README.md`, `docs/`) in the same commit or PR. Don't create new doc files for this — update what's already there.

## Patterns to Implement

These are validated patterns from research (see `docs/research/pattern-analysis.md`). Implement them when building the relevant module.

**Tool nudging:** When an LLM responds with text instead of tool calls in the first 2 iterations, inject "Please proceed and use the available tools." Implement in `SpacebotHook.on_completion_response()`. Workers benefit most.

**Fire-and-forget DB writes:** `tokio::spawn` for conversation history saves, memory writes, worker log persistence. User gets their response immediately.

**Tiered compaction:** >80% background, >85% aggressive, >95% emergency truncation. The compactor uses these thresholds.

**Hybrid search with RRF:** Vector similarity + full-text search, merged via Reciprocal Rank Fusion (`score = sum(1/(60 + rank))`). RRF works on ranks, not raw scores.

**Leak detection:** Regex patterns for API keys, tokens, PEM keys. Scan in `SpacebotHook.on_tool_result()` (after execution) and before outbound HTTP (block exfiltration).

**Workspace path guard:** File tools reject writes to identity/memory paths with an error directing the LLM to the correct tool.

**Circuit breaker:** Auto-disable recurring tasks after 3 consecutive failures. Apply to cron jobs, maintenance workers, cortex routines.

**Config resolution:** `env > DB > default` with per-subsystem `resolve()` methods.

**Error-as-result for tools:** Tool errors are returned as structured results, not panics. The LLM sees the error and can recover.

**Worker state machine:** Validate transitions with `can_transition_to()` using `matches!`. Illegal transitions are runtime errors, not silent bugs.

## Reference Docs

- `README.md` — full architecture design
- `RUST_STYLE_GUIDE.md` — coding conventions (read this first)
- `docs/memory.md` — memory system design
- `docs/research/rig-integration.md` — how Spacebot maps onto Rig
- `docs/research/repo-structure.md` — module layout rationale
- `docs/research/pattern-analysis.md` — patterns to adopt/adapt/skip
- `docs/messaging.md` — messaging system design (Discord, Telegram, webhook)
- `docs/routing.md` — model routing design (process-type defaults, task-type overrides, fallbacks)
- `docs/daemon.md` — daemon mode, IPC, CLI subcommands
