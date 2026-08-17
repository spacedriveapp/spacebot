# Autonomy

Spacebot's autonomy channel is a resident process for self-directed work. Durable wakes and interval heartbeats bring it out of idle. Tasks, goals, workers, memories, and run history provide the state it needs to decide what deserves attention.

This doc defines the autonomy system: how Spacebot wakes up, what it sees, what it can do, and how state is tracked — without a heartbeat.json or heartbeat.md.

---

## Philosophy

Most agent harnesses implement "heartbeat" as: run a prompt on a timer, let the LLM figure out what to do, persist state to a markdown or JSON file for next time.

This has a well-known failure mode: the agent writes malformed JSON, overwrites fields it shouldn't, or drifts from the schema over time. The Spacebot approach inverts this: **state lives in structured storage, not in files the LLM writes.** The database is the source of truth. State is tracked through tasks and the working memory event log. The LLM reads from structured queries and writes through typed tools. There is no heartbeat.md and no heartbeat.json.

---

## The Autonomy Channel

The autonomy channel starts with the agent and remains alive until shutdown. It is not per-task and not per-heartbeat. It waits while idle and processes one event at a time. Interactive worker controls live in the agent registry.

The interval is the default trigger. Schedules, webhooks, task approvals, user comments, worker results, and idle conditions can wake it sooner. The channel is the single consumer of these sources. See [`wakes.md`](wakes.md) for queue semantics and authority rules.

The resident channel and an autonomy run have different lifetimes. The agent registry owns live worker controls. A run is a durable decision epoch with a run id, claimed wake events, operation-scoped child attribution, and a terminal summary. `autonomy_complete` closes the epoch and returns the channel to idle.

The autonomy channel is the only process that:
- Enriches and researches `pending_approval` tasks without a user present
- Executes `ready` tasks (user-approved) without a user present
- Creates new tasks as part of its run
- Maintains a run history via `autonomy_complete`

It does **not** branch. Branches exist to keep memory tool calls out of user-facing channel context. The autonomy channel has no user context to protect — it uses tools directly.

---

## Context on Wake

The autonomy supervisor assembles a fresh heartbeat briefing. It includes:

- **Identity** — SOUL.md, IDENTITY.md, ROLE.md. The agent knows who it is.
- **Memory bulletin** — the cortex's current knowledge synthesis.
- **Working memory** — recent system events. What's been happening across all channels.
- **Wake events** — what pulled this run forward, if anything: the wake's name, instructions, and payload for each pending event since the last run. Surfaced first, because they are usually why the run exists.
- **Task state** — active tasks grouped by status, with descriptions, execution plans, dependencies, ownership, and prior attempt summaries.
- **Goals** — all active goals with descriptions and notes. Background context and direction, not a work queue. See [`goals.md`](goals.md).
- **Workers** — registry-backed liveness and routability, plus durable nonterminal rows that require reconciliation.
- **Recent epoch summaries** - compact continuity from `autonomy_complete`.

The run store is the continuity index and provenance record. Live history belongs to the current epoch and is cleared before the next one. Retained worker controls are agent runtime state, not transcript state.

### Heartbeats are control messages

The channel has a dedicated autonomy system contract. Each heartbeat arrives as an ephemeral system message tagged with the current run generation. It is never persisted as user conversation data.

Generation tags fence late messages from older epochs. A bounded doorbell coalesces repeated wake notifications while a turn is busy. Durable wake rows carry the payload, so coalescing the in-memory signal cannot lose work.

Worker results are also control messages. They continue the intent that spawned or routed the worker. They do not grant permission to start unrelated work.

Changing the effective level to `off` suppresses further heartbeat and wake admission immediately. An active epoch is not cancelled: its current turn and owned children can settle, but the supervisor does not feed it additional work. Once that epoch completes, the resident channel remains idle until autonomy is enabled again.

The transcript holds the current epoch's work. System scaffolding and previous epoch detail do not accumulate.

## What It Does

All tasks require human approval before execution. `pending_approval` tasks are waiting for the user to review them. `ready` tasks have been approved and are waiting to be executed. The autonomy channel's primary activity — especially overnight or during long idle windows — is **enrichment**: researching, investigating, and preparing `pending_approval` tasks so they are fully reasoned when the user comes to review them.

The autonomy channel reasons about which tasks to prioritise given goal context and current system state — it is not a FIFO queue.

During a run it can:
- **Enrich pending tasks** — spawn investigation workers, reason about their findings, and add comments to tasks with synthesised results. A task that arrived as a title becomes a fully researched brief before the user ever approves it.
- **Execute ready tasks** — tasks the user has approved. Uses execution tools directly (shell, file, browser) with no forced delegation. Workers available for genuine parallelism.
- **Create new tasks** — identifies follow-on work and adds it to `pending_approval`. The agent proposes; the user decides.
- **Update task metadata** — priority, blockers, progress notes.
- **Record what it notices** — the channel holds `memory_save` and `memory_recall` directly. Durable findings are written as they are found rather than manufactured as a completion quota.

Recording is licensed, not quota'd. A run that genuinely learned nothing records nothing, and the briefing says so in as many words. An agent told to always produce an observation will produce one — restating what it was already given, or narrating its own activity as a discovery — and manufactured memories are worse than none, because they degrade every future recall that has to sift past them.

What it **cannot** do:
- Hold a conversation or message users. There is no reply or outbound messaging tool.
- Execute tasks that are still in `pending_approval`
- Create cron jobs
- Spawn other autonomy channels

---

## The Empty Instance

A fresh instance has no tasks, no goals, and no history. The default outcome is a run that surveys nothing, concludes "nothing new here", and exits — and because nothing changed, the next wake reaches the same conclusion. An agent that idles until someone gives it work is not autonomous; it is a queue consumer with a timer.

The survey already knows when it came back empty, so the briefing branches on it rather than leaving the agent to notice. The template is already conditional on wake events, run history, goals, workers, and level; empty state is one more branch, and it fires deterministically. That matters more than it sounds: routing this through a skill the agent chooses to invoke reintroduces the exact failure being fixed, because the run that fails to reach for the skill is indistinguishable from the run that had nothing to do.

The empty branch is built on one claim: **on an empty instance, learning the user and the system is the highest-value work available, not filler while waiting for real work.**

- **Read what is actually here.** The workspace, registered projects, whatever the user has already done. A cloned repository is a statement of intent.
- **Record what it learns**, under the rules above.
- **Find capability gaps** via `spacebot_docs` — features that fit what the user appears to be doing and that they have not set up.
- **Stop when orientation is exhausted.** A no-action epoch is correct when another pass would only repeat recent summaries.

---

## Task Comments

Comments are the primary output of the enrichment loop. When the autonomy channel or a worker completes investigation on a task, findings are written as a comment — not appended to the task description, not stuffed into metadata. Comments are append-only and chronological. The task description remains the stable statement of what needs to be done; comments are everything that has been learned or decided since.

Both the agent and the user can comment on a task. This makes tasks a shared workspace: the agent enriches overnight, the user wakes up to investigated briefs and can weigh in before approving.

### Schema

```sql
CREATE TABLE task_comments (
    id          TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author_type TEXT NOT NULL,   -- 'agent' | 'user' | 'worker'
    author_id   TEXT,            -- user_id for users, worker_id for workers, null for agent
    body        TEXT NOT NULL,   -- synthesised comment text (2-5 lines)
    worker_id   TEXT,            -- if this comment summarises a worker run, links to that worker
    metadata    TEXT DEFAULT '{}',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX task_comments_task ON task_comments(task_id, created_at);
```

`worker_id` links a comment to a specific worker run. The UI renders it as a pill on the comment — click to expand the full worker output. The comment body is always the agent's synthesised 2-5 line summary; the worker output is available on demand, never inlined.

### `add_task_comment` tool

Available to the autonomy channel and to workers (via the task tools toolset).

```rust
struct AddTaskCommentInput {
    task_id: String,
    body: String,               // synthesised finding — 2-5 lines
    worker_id: Option<String>,  // tag the worker whose output this summarises
}
```

### Enrichment Pattern

> Enrichment gains an explicit task state in
> [autonomy-lifecycle.md](autonomy-lifecycle.md) §4 — `not_needed` /
> `needed` / `in_progress` / `done`, with a plan document written as a task
> attachment and a terminating rule (a run may not re-enrich a `done`
> task). The pattern below is the shipped, state-free version.

```
wake → survey pending_approval tasks
  → pick highest-priority unenriched task
  → spawn 1-3 investigation workers in parallel
  → reason about worker findings
  → add_task_comment: synthesised finding + worker_id(s)
  → repeat for next task within turn budget
  → autonomy_complete → close the epoch and return to idle
```

The autonomy channel system prompt instructs: investigate and comment freely; never execute a task still in `pending_approval`.

### Worker Briefing

When a `ready` task is eventually executed, `WorkerContextMode::Briefed` pulls the task's comments as part of the briefing synthesis alongside memory recall and working memory events. The executing worker walks in knowing what was investigated, what solution was proposed, and what the user said — without re-doing any of the research.

---

## Future Task Selection

The resident channel currently reasons from task summaries, dependency metadata, prior attempts, and recent run summaries. The `last_enriched_at` scheme below is a follow-up design and is not part of the current schema.

### `last_enriched_at`

Tasks gain a `last_enriched_at` timestamp, set by the autonomy channel each time it works on a task. This prevents the same tasks from dominating every run.

```sql
ALTER TABLE tasks ADD COLUMN last_enriched_at TEXT;
```

### Task Ownership

Tasks have an `assigned_agent_id` field. Once an agent claims a task, no other agent can work on it. Ownership is enforced at the query level and set atomically when a task is first enriched or executed.

The global tasks table currently declares `assigned_agent_id TEXT NOT NULL` with assignment at creation time, so the unowned-task model requires making the column nullable. That migration touches the global database and every task consumer; it ships as its own change ahead of this system, not inside it.

```sql
-- Atomic claim: only succeeds if still unassigned or already assigned to this agent
UPDATE tasks SET assigned_agent_id = ?1
WHERE id = ?2 AND (assigned_agent_id IS NULL OR assigned_agent_id = ?1)
```

If the UPDATE affects 0 rows, another agent claimed it first — skip and move on.

`claim_unowned` is an agent-level config flag (default: `true`). Set to `false` for agents that should only work on tasks explicitly assigned to them — useful in multi-agent setups where task routing is intentional. Once claimed, ownership is permanent. Reassignment is user-initiated only.

### Selection Priority

On wake, tasks are ordered by:

1. **User-engaged since last enrichment** — tasks where a user comment exists with `created_at > last_enriched_at`. A user weighing in is the strongest signal to come back.
2. **Never enriched** — `last_enriched_at IS NULL`. Fresh tasks the agent hasn't seen yet.
3. **Stale** — `last_enriched_at < now - (interval_secs * 3)`. Tasks not touched in a while.
4. **Excluded** — `last_enriched_at > last_run_started_at`. Worked on in the most recent run — skip unless the user has commented since.

Rule 4 prevents just-worked-on tasks from floating to the top next wake. Rule 1 clears that exclusion when the user responds — natural gate, no separate flag needed.

The channel selects up to `max_tasks_per_run` from this ordered list, reasoning about which are most valuable given goal context. Not a mechanical top-N pick. All queries filter to `assigned_agent_id = this_agent OR assigned_agent_id IS NULL` (when `claim_unowned = true`).

### Run History as Context

The last few `autonomy_complete` summaries provide a second deduplication layer — even if a task slips through the timestamp filter, the channel can see "I already researched this last run" and move on.

---

## Liveness

Elapsed wall-clock time never ends an autonomy epoch and never cancels useful work. Provider requests, tools, and workers keep their own operation-level limits. Daemon shutdown and process restart are explicit terminal events.

An epoch can remain open while owned work is active. `autonomy_complete` is rejected at the tool boundary until every registered child has produced a result. Child admission and finish requests share one lock, which removes the check-then-spawn race.

On restart, the new supervisor marks any leftover `running` epoch as interrupted before admitting another one. Generation fencing prevents late calls from an older epoch from clearing or completing the current epoch.

---

## Continuity Between Runs

**Task comments** — the primary record of investigation findings. They persist indefinitely and are available through task tools and full task execution context. The compact heartbeat survey does not inline every comment.

**Run summaries** - `autonomy_complete` records what was enriched, executed, created, and which wake events the epoch consumed. The UI renders consumed wakes as provenance.

**The summary is the epoch boundary.** During an epoch the channel carries tool calls, worker results, and intermediate reasoning. The next epoch starts with fresh live history and recent summaries from the run store.

`run_history_count` bounds the recent summaries injected into each heartbeat. The run store is both the continuity source and the provenance index. Live model history is scoped to the current epoch.

One consequence worth stating: wake provenance lives in the run store and the UI, not in the transcript. Read on its own, the transcript is uninterrupted thought with no visible cause — "why did run 47 happen" is answered by run history, not by scrolling back.

Working memory provides broader system context. The transcript provides the autonomy-specific thread.

---

## Lifecycle

> The flow below describes the shipped implementation. See
> [autonomy-lifecycle.md](autonomy-lifecycle.md) for the successor: the
> trigger becomes the previous run's requested `next_wake` (interval as
> floor and crash fallback), and the run opens with a deliberation phase
> that queries rather than reading a rendered dump.

```
Agent startup
  → reconcile interrupted run rows
  → start one resident autonomy channel and supervisor
  → wait for heartbeat or durable wake doorbell
  ↓
Supervisor atomically admits one run epoch
  → claims pending wake rows
  → sends a generation-tagged system heartbeat
  ↓
Autonomy channel wakes with current tasks, goals, workers, and recent summaries
  ↓
  ├─ pending_approval tasks exist?
  │    → enrich: spawn investigation workers, reason about findings, add_task_comment
  │    → repeat within turn/task budget
  │
  ├─ ready tasks exist?
  │    → execute: pick highest-priority, run with full context + briefing from comments
  │
  └─ no tasks worth acting on?
       → create pending_approval tasks from goals, or exit with "nothing to do"
  ↓
Calls autonomy_complete after owned work settles
  → summary and actions recorded once
  ↓
Epoch closes → channel returns to idle; retained worker controls remain in the agent registry
```

If the channel crashes mid-execution, the task returns to `ready`. If a task fails 3 consecutive times, it moves to `failed` and emits a working memory `Error` event. Enrichment runs (comments only) do not count as failures. `failed` is a new `TaskStatus` variant — the current set is pending_approval, backlog, ready, in_progress, done — so adding it includes the transition table, API, and UI sweep.

---

## Configuration

```toml
[autonomy]
level = "off"                      # off | observe | suggest | act
interval_secs = 1800               # How often to wake (seconds)
active_hours = [8, 22]             # Agent-timezone hour range (optional)
max_turns = 20                     # Agentic-loop turns per heartbeat
max_tasks_per_run = 2              # Max tasks to work on per wake
run_history_count = 5              # How many past run summaries to surface on wake
claim_unowned = true               # Pick up tasks with no assigned agent
```

### Validation

Enforced at startup and on config reload — autonomy does not start if any rule is violated:

- `max_tasks_per_run` ≥ 1
- `interval_secs` ≥ 60

---

## Implementation

**Current - Autonomy Channel**
- `AutonomyChannelContext` builder: identity + bulletin + working memory + tasks + goals + run summaries
- `autonomy_channel.md.j2` heartbeat briefing (worker-aware, `autonomy_complete` returns to idle, never execute `pending_approval`)
- Goals table migration + `goal_create`, `goal_update`, `goal_list` tools (see `goals.md`)
- Resident supervisor: interval heartbeat, durable wake claiming, epoch admission, channel lifecycle, and restart reconciliation
- `autonomy_complete` tool + run summary storage + retrieval for run history. Intentionally distinct from `set_outcome`, which is an unpersisted last-write-wins delivery buffer with no completion contract. Model it on `memory_persistence_complete` instead, which already enforces call-the-terminal-tool-before-exit through the hook retry machinery.
- All config fields + validation

**Current - Task Comments**
- `task_comments` table migration
- `add_task_comment` tool (autonomy channel + workers)
- Task comments pulled into `WorkerContextMode::Briefed` pipeline
- API endpoints: list and create comments per task
- UI: chronological comments on task detail, worker pill with expandable full output

**Follow-up**
- `last_enriched_at` and deterministic enrichment selection
- Task retry/failure handling
- Bounded comment signals in the heartbeat survey
- Autonomy UI surface: run history, last wake time, active enrichment progress
- "Quiet while active" flag: suppress autonomy wakes when user channels have been recently active
- Autonomy outcomes surfaced to relevant user channels via working memory synthesis

---

## Non-Goals

- **No talking to users.** No `reply` tool. Output goes to task comments, working memory, and `autonomy_complete`.
- **No recursive autonomy.** The autonomy channel cannot spawn other autonomy channels.
- **No identity or config modification.** Factory tools remain cortex-chat / admin only.
- **Goals are not auto-completed.** The agent proposes completion; the user confirms.
- **Per-goal active hours** are out of scope. `active_hours` applies to the whole system.
