# Worker Briefing

Workers are context-poor by default — they get the task description, filesystem context, and tool definitions. Nothing else. A worker continuing a multi-day refactor has the same starting context as one checking a file.

This doc defines worker briefing: on-demand context enrichment controlled per spawn via `WorkerContextMode`. Workers always have memory tools (`memory_recall`, `memory_save`, `memory_delete`) — briefing is about what gets **injected into the system prompt** before the worker starts.

---

## What Already Exists

`WorkerContextMode` is a struct on `ChannelState` today:

```rust
pub struct WorkerContextMode {
    pub history: WorkerHistoryMode,  // None | Summary | Recent(n) | Full
    pub memory: WorkerMemoryMode,    // None | Ambient | Tools | Full
}
```

Both default to `None` — workers get no context. The memory variants are:
- `Ambient` — injects knowledge synthesis + working memory into the system prompt (read-only)
- `Tools` — ambient + `memory_recall` tool
- `Full` — ambient + full memory tools (recall, save, delete)

The history variants let conversation messages be passed to the worker.

**Two problems with this:**

First, it's a **static channel setting** — read from `ChannelState.worker_context_settings` which is set from conversation settings at channel init. The agent has no per-spawn control. Every worker from a channel gets identical context regardless of what it's doing.

Second, the design is over-engineered for what's actually needed. `WorkerHistoryMode` is the wrong primitive — if a worker needs conversation context, the channel should branch first, reason about it, and write a self-contained task description. Passing raw history to a worker is context dumping with no framework to interpret it. `WorkerMemoryMode::Tools` is confusingly named — sounds like execution tools (shell, file, browser), not memory tools. And the four-variant memory enum collapses to a single question once memory tools are always on: does the worker get ambient context injected or not?

---

## `WorkerContextMode` — Redesigned

A flat enum set by the agent per `spawn_worker` call. Replaces the struct (which is dropped along with `WorkerHistoryMode` and `WorkerMemoryMode`).

```rust
pub enum WorkerContextMode {
    /// No ambient context. Worker sees only the task description.
    /// For self-contained one-shot work: run this command, check this file.
    None,

    /// Knowledge synthesis + working memory injected.
    /// Worker knows what the agent knows and what's been happening.
    /// Good default for most tasks.
    Ambient,

    /// Ambient + targeted recall synthesis scoped to this task.
    /// A curated briefing block is generated before the worker starts.
    /// For complex ongoing work where prior context matters.
    Briefed,
}
```

**Default: `None`.** The agent explicitly opts in. The channel system prompt gets guidance on when to use each level.

**When to use each:**

`None` — self-contained tasks where the task description is fully sufficient:
- "Run `cargo test` and report results"
- "Check the contents of config.toml"
- "Fetch this URL and return the response"

`Ambient` — tasks that benefit from knowing the agent's current state:
- "Fix the failing CI build"
- "Add the new endpoint to the API"
- "Write a summary of the changes in this PR"

`Briefed` — tasks that depend on prior decisions, patterns, or ongoing work:
- "Continue the auth migration — pick up from where the last worker left off"
- "Fix the flaky test in the payments module"
- "Write a changelog entry for the v2 release in Jamie's voice"
- "Review the PR against our established patterns"

---

## Memory Tools

Workers always have `memory_recall`, `memory_save`, and `memory_delete`. No setting controls this. Workers are trusted processes — they already have shell and file access. Memory tools are strictly less dangerous and strictly more useful. A worker that finds something worth remembering while doing a task should just save it.

---

## The Briefing Pipeline

When `WorkerContextMode::Briefed`, before `Worker::new()` is called, `WorkerBriefing::prepare()` runs:

### Step 1: Targeted Memory Recall

Semantic search against the full memory graph using the task description as the query. Same hybrid search (vector + FTS + RRF) that branches use — run programmatically, not through the LLM.

- Query: task description
- Limit: 12 results
- Exclude `Observation` type (too low signal)
- Boost `Decision` and `Preference` types
- Recency bias: memories accessed in last 7 days get a retrieval boost

### Step 2: Recent Relevant Events

Query the working memory event log for events that overlap with the task:
- Pull last 48 hours of events
- Filter by cosine similarity to task description (threshold: 0.6)
- Always include `WorkerCompleted` and `Decision` events within last 7 days
- Cap at 8 events

### Step 3: LLM Synthesis

A small, fast LLM call synthesizes steps 1–2 into a `## Worker Briefing` block (150–300 words):

```
A worker is about to execute the following task:
{task}

Here are potentially relevant memories and recent events:
{recalled_memories}
{recent_events}

Write a concise briefing covering:
- What the agent has already decided or established about this area
- Relevant preferences or patterns that should shape the work
- What prior workers have done on related tasks (if any)
- Any constraints or context the worker should know going in

Be specific and direct. Omit anything not relevant to this task.
If nothing is relevant, output: NO_BRIEFING
```

If `NO_BRIEFING` is returned, the block is omitted and the worker spawns with ambient context only. No noise injection.

Model: fast model (same as cortex synthesis).

### Step 4: Injection

Appended to the worker system prompt after all other context:

```
[... worker system prompt ...]
[... knowledge synthesis ...]
[... working memory ...]

## Worker Briefing

{synthesized briefing text}
```

The worker's first message remains the task description unchanged. Briefing is context, not instruction.

---

## Latency

| Step | Latency |
|---|---|
| Memory recall | ~50ms |
| Event log query + similarity filter | ~30ms |
| LLM synthesis | ~800ms–1.5s |
| Total | ~1–2s |

Acceptable for tasks where the worker will run for tens of seconds to minutes. For task channels (spawned by the cortex for autonomous task execution), `Briefed` runs unconditionally — every autonomous task is by definition complex.

---

## System Prompt Changes

Two prompts need updating.

### `spawn_worker` tool description

Currently: *"Spawn an independent worker process. The worker only sees the task description you provide — no conversation history."*

Needs a `context` parameter added to the tool schema and its description updated to explain when to use each level:

```
Spawn an independent worker process.

**context** — how much agent context the worker receives:
- `"none"` (default) — worker sees only the task. Use for self-contained
  one-shot work: run a command, read a file, fetch a URL.
- `"ambient"` — worker receives the agent's knowledge synthesis and recent
  working memory. Use when the worker needs to know the agent's current state
  but not specific prior history.
- `"briefed"` — ambient plus a targeted briefing synthesized from memory and
  recent events relevant to this specific task. Use for complex or ongoing
  work: continuing a refactor, fixing a recurring issue, producing output in
  the agent's voice, working within established patterns.
```

### Channel system prompt (`channel.md.j2`)

The delegation section currently says workers only know what the task description tells them. Add guidance alongside the existing worker spawning instructions:

```
When spawning a worker, set context based on what the worker needs:
- Most tasks: omit context (defaults to none) — write a self-contained task description
- Worker needs to know the agent's current state: context "ambient"
- Worker is continuing prior work or needs established patterns: context "briefed"

Do not branch just to enrich worker context — set context "briefed" instead.
```

The last line matters: currently agents branch before spawning workers specifically to gather context for the task description. With `briefed`, that branch is unnecessary for context enrichment (though branching for other reasons — memory saves, decisions — is unchanged).

---

## What This Doesn't Do

**It doesn't replace good task descriptions.** The channel should still write specific, contextual task descriptions. Briefing adds what can't go in the description — prior history, established patterns, previous related work.

**It doesn't inject conversation history.** `WorkerHistoryMode` is dropped. If conversation context matters before spawning a worker, branch first, reason about it, write a self-contained task description. Workers don't need raw conversation history.

---

## Implementation

**Location:** `src/agent/worker_briefing.rs` — `WorkerBriefing::prepare()`. Called from `spawn_worker_inner()` in `channel_dispatch.rs` and from the task channel context builder.

**Dependencies:** memory store, working memory store, LLM manager, embedding model.

**Error handling:** Briefing failure degrades silently to `Ambient` — never block worker spawn.

**Observability:** Emit a low-importance `System` working memory event when a briefing runs.

---

## Implementation Phases

**Phase 1 — `WorkerContextMode` enum + ambient**
- Replace existing `WorkerContextMode` struct with flat enum (`None`, `Ambient`, `Briefed`)
- Drop `WorkerHistoryMode` and `WorkerMemoryMode`
- Wire `WorkerContextMode` as a per-spawn arg on `SpawnWorkerArgs` (not a channel setting)
- Implement `Ambient`: inject knowledge synthesis + working memory
- Always give workers memory tools regardless of mode
- Update `spawn_worker` tool description with guidance on when to use each level

**Phase 2 — Targeted recall (no LLM)**
- `WorkerBriefing::prepare()` steps 1–2: memory recall + event query
- Inject as raw blocks: `## Relevant Knowledge`, `## Related Activity`
- Validate recall quality before adding synthesis overhead

**Phase 3 — LLM synthesis**
- Add step 3: fast LLM synthesis call
- Synthesized `## Worker Briefing` block replaces raw blocks
- `NO_BRIEFING` handling
- Working memory event emission
