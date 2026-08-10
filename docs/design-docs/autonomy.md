# Autonomy

Spacebot has all the primitives for autonomous operation but no loop that ties them together. Tasks can be claimed. Workers can be spawned. Memories can be saved. Goals can be stored. The cortex observes everything. But none of this fires without a user message.

This doc defines the autonomy system: how Spacebot wakes up, what it sees, what it can do, and how state is tracked — without a heartbeat.json or heartbeat.md.

---

## Philosophy

Most agent harnesses implement "heartbeat" as: run a prompt on a timer, let the LLM figure out what to do, persist state to a markdown or JSON file for next time.

This has a well-known failure mode: the agent writes malformed JSON, overwrites fields it shouldn't, or drifts from the schema over time. The Spacebot approach inverts this: **state lives in structured storage, not in files the LLM writes.** The database is the source of truth. State is tracked through tasks and the working memory event log. The LLM reads from structured queries and writes through typed tools. There is no heartbeat.md and no heartbeat.json.

---

## The Autonomy Channel

The autonomy channel is the agent's process for self-directed work. It is not per-task. It is one channel that wakes on a configured interval, surveys the task state, does as much enrichment and preparation as useful, executes ready tasks if any exist, and exits. On the next interval it wakes again.

The interval is the default trigger, not the only one. Wake events — schedules, webhooks, internal events like a task approval or a user comment, and idle/staleness conditions — pull the next run forward and appear in its context with their payloads and instructions. The channel is the single consumer of all wake sources; see [`wakes.md`](wakes.md) for the trigger model, queue semantics, and authority rules.

It is structurally similar to a cron channel — periodic, no user present, full agent context. The difference is that it is persistent across runs and has awareness of its own history.

The autonomy channel is the only process that:
- Enriches and researches `pending_approval` tasks without a user present
- Executes `ready` tasks (user-approved) without a user present
- Creates new tasks as part of its run
- Maintains a run history via `autonomy_complete`

It does **not** branch. Branches exist to keep memory tool calls out of user-facing channel context. The autonomy channel has no user context to protect — it uses tools directly.

---

## Context on Wake

The cortex assembles the autonomy channel's context before each wake. It gets:

- **Identity** — SOUL.md, IDENTITY.md, ROLE.md. The agent knows who it is.
- **Memory bulletin** — the cortex's current knowledge synthesis.
- **Working memory** — recent system events. What's been happening across all channels.
- **Wake events** — what pulled this run forward, if anything: the wake's name, instructions, and payload for each pending event since the last run. Surfaced first, because they are usually why the run exists.
- **Task state** — all active tasks: ready, in-progress, backlog, pending_approval, each with its comment count and last enrichment time.
- **Enrichment queue** — the ordered selection for this run, with the recent comments on each entry inlined. Full comment threads are rendered here rather than across the whole board, so context stays bounded as the board grows.
- **Goals** — all active goals with descriptions and notes. Background context and direction, not a work queue. See [`goals.md`](goals.md).
- **Active workers** — what's currently running so it doesn't duplicate work.
- **Last few run summaries** — the `autonomy_complete` output from its previous runs, with timestamps. This is the primary continuity mechanism.

The last run summaries are surfaced up front: "Last run (2h ago): enriched tasks X and Y, created tasks Z for backlog." The autonomy channel wakes with spatial awareness of where things stand and what it did recently.

---

## What It Does

All tasks require human approval before execution. `pending_approval` tasks are waiting for the user to review them. `ready` tasks have been approved and are waiting to be executed. The autonomy channel's primary activity — especially overnight or during long idle windows — is **enrichment**: researching, investigating, and preparing `pending_approval` tasks so they are fully reasoned when the user comes to review them.

The autonomy channel reasons about which tasks to prioritise given goal context and current system state — it is not a FIFO queue.

During a run it can:
- **Enrich pending tasks** — spawn investigation workers, reason about their findings, and record synthesised results with `add_task_comment`. A task that arrived as a title becomes a fully researched brief before the user ever approves it.
- **Execute ready tasks** — tasks the user has approved. Uses execution tools directly (shell, file, browser) with no forced delegation. Workers available for genuine parallelism.
- **Create new tasks** — identifies follow-on work and adds it to `pending_approval`. The agent proposes; the user decides.
- **Update task metadata** — priority, blockers, progress notes.
- **Link work to goals** — set `goal_id` when creating a task toward a goal, and record where a goal now stands in its `notes`.

What it **cannot** do:
- Reply to users (no `reply` tool)
- Execute tasks that are still in `pending_approval`
- Create cron jobs
- Spawn other autonomy channels
- Change a goal's status. `goal_update` is registered without the `status`
  field for autonomy runs, so completing or abandoning a goal is unreachable
  rather than merely discouraged.

At `observe` the task surface is not registered at all: the run reads and
summarises, and has no tool with which to create, update, comment, or claim.

---

## Task Comments

Comments are the primary output of the enrichment loop. When the autonomy channel or a worker completes investigation on a task, findings are written as a comment — not appended to the task description, not stuffed into metadata. Comments are append-only and chronological. The task description remains the stable statement of what needs to be done; comments are everything that has been learned or decided since.

Both the agent and the user can comment on a task. This makes tasks a shared workspace: the agent enriches overnight, the user wakes up to investigated briefs and can weigh in before approving.

### Schema

```sql
CREATE TABLE task_comments (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author_type TEXT NOT NULL,   -- 'agent' | 'user' | 'worker'
    author_id   TEXT,            -- agent id, user id, or worker id
    body        TEXT NOT NULL,   -- synthesised comment text (2-5 lines)
    worker_id   TEXT,            -- if this comment summarises a worker run, links to that worker
    metadata    TEXT NOT NULL DEFAULT '{}',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX task_comments_task ON task_comments(task_id, seq);
CREATE INDEX task_comments_author ON task_comments(task_id, author_type, created_at);
```

`seq` is the ordering key, not `created_at`: two comments inside the same
millisecond would otherwise have no defined order, and pagination cursors would
skip or repeat rows. `seq` is also the cursor the list API returns.

Deletion is explicit as well as declared. `ON DELETE CASCADE` only fires when
the connection has `PRAGMA foreign_keys` on, so `TaskStore::delete` removes a
task's comments in the same transaction as the task.

Comment bodies are capped at 4000 bytes. A finding longer than that is a
transcript, and transcripts belong behind the `worker_id` link.

`worker_id` links a comment to a specific worker run. The UI renders it as a pill on the comment — click to expand the full worker output. The comment body is always the agent's synthesised 2-5 line summary; the worker output is available on demand, never inlined.

### `add_task_comment` tool

Comments are append-only. There is no edit and no delete: the store exposes
neither, and the UI offers no affordance for either.

```rust
struct AddTaskCommentArgs {
    task_number: i64,           // #N, matching every other task tool
    body: String,               // synthesised finding — 2-5 lines
    worker_id: Option<String>,  // tag the worker whose output this summarises
}
```

Registered for the autonomy channel (at `suggest` and above), for branches, for
the cortex chat, and for task workers. A worker's registration drops the
`worker_id` field and stamps its own id, so a worker cannot misattribute a
finding to another run.

Commenting is also where autonomous ownership is settled — see
[Task Ownership](#task-ownership).

### Enrichment Pattern

```
wake → survey pending_approval tasks
  → pick highest-priority unenriched task
  → spawn 1-3 investigation workers in parallel
  → reason about worker findings
  → add_task_comment: synthesised finding + worker_id(s)
  → repeat for next task within turn budget
  → set_outcome → exit
```

The autonomy channel system prompt instructs: investigate and comment freely; never execute a task still in `pending_approval`.

### Worker Briefing

When a `ready` task is executed, its comments are appended to the worker's task
prompt as a "Prior findings" block: oldest first, capped at 12 comments, 600
bytes each, 4000 bytes total. The executing worker walks in knowing what was
investigated, what solution was proposed, and what the user said — without
re-doing any of the research.

`WorkerContextMode::Briefed` does not exist in source. `WorkerContextMode` is a
struct of `history` / `memory` / `wiki_write` modes, and the enum in
[`worker-briefing.md`](worker-briefing.md) was never built. Comments are
injected at the ready-task pickup path instead, which is where a worker is
actually bound to a task — the only place the briefing has a task's comments to
carry. Worker transcripts are never inlined; they stay behind the `worker_id`
link on each comment.

---

## Task Selection and Deduplication

### `last_enriched_at`

Tasks gain a `last_enriched_at` timestamp, set by the autonomy channel each time it works on a task. This prevents the same tasks from dominating every run.

```sql
ALTER TABLE tasks ADD COLUMN last_enriched_at TEXT;
```

### Task Ownership

Tasks have an `assigned_agent_id` field. Once an agent claims a task, no other agent can work on it. Ownership is enforced at the query level and set atomically when a task is first enriched or executed.

`assigned_agent_id` is nullable as of `20260809000001_tasks_nullable_assignment`, so tasks can exist unowned until an agent claims them.

```sql
-- Atomic claim: only succeeds if still unassigned or already assigned to this agent
UPDATE tasks SET assigned_agent_id = ?1
WHERE id = ?2 AND (assigned_agent_id IS NULL OR assigned_agent_id = ?1)
```

If the UPDATE affects 0 rows, another agent claimed it first — the tool returns
a skip ("task #N was claimed by <agent> first"), and the run moves on.

The claim happens when the agent *acts* on a task, not when it lists one:
`add_task_comment` claims before it writes, and `claim_next_ready` takes
assignment and `in_progress` in one guarded UPDATE. Surveying the board claims
nothing, and `observe` runs get no mutation tools at all, so they cannot claim
by any path.

`claim_unowned` is an agent-level config flag (default: `true`). Set to `false` for agents that should only work on tasks explicitly assigned to them — useful in multi-agent setups where task routing is intentional. Once claimed, ownership is permanent. Reassignment is user-initiated only.

### Selection Priority

On wake, tasks are ordered by:

1. **User-engaged since last enrichment** — tasks where a user comment exists with `created_at > last_enriched_at`. A user weighing in is the strongest signal to come back.
2. **Never enriched** — `last_enriched_at IS NULL`. Fresh tasks the agent hasn't seen yet.
3. **Stale** — `last_enriched_at < now - (interval_secs * 3)`. Tasks not touched in a while.
4. **Excluded** — `last_enriched_at > last_run_started_at`. Worked on in the most recent run — skip unless the user has commented since.

Rule 4 prevents just-worked-on tasks from floating to the top next wake. Rule 1 clears that exclusion when the user responds — natural gate, no separate flag needed.

This ordering is settled in SQL (`TaskStore::select_for_enrichment`) and
rendered into the run briefing as an **Enrichment Queue**, so the channel opens
with a list that already excludes last run's work. Visibility filters to
`assigned_agent_id = this_agent OR assigned_agent_id IS NULL` (the latter only
when `claim_unowned = true`).

`max_tasks_per_run` is enforced, not requested: `add_task_comment` holds the
run's allowance and refuses a task beyond it. The allowance is spent per task,
so a run can keep commenting on work it already started. Within the allowance
the channel still reasons about which tasks are most valuable given goal
context — it is not a mechanical top-N pick.

### Run History as Context

The last few `autonomy_complete` summaries provide a second deduplication layer — even if a task slips through the timestamp filter, the channel can see "I already researched this last run" and move on.

---

## Soft Timeout

Hard timeouts waste runs — cutting off mid-task leaves work in undefined state. Spacebot uses a soft warning instead.

The cortex monitors elapsed time. At `warn_secs`, it injects an addendum into the channel's live context:

```
You have approximately 2 minutes remaining in this run.
Finish your current task, add any final comments, and call set_outcome.
Do not start a new task.
```

The channel wraps up gracefully and exits cleanly. The hard `timeout_secs` remains as a safety net but should almost never fire.

Delivery mechanism: a synthetic system message between turns, the same pattern the cron scheduler uses for its timeout wrap-up injection. The worker-style mid-turn injection hook (`inject_rx` on `SpacebotHook`) is wired only for workers today; giving `Channel` the same pair would let the warning land inside a live turn, but that is an enhancement, not a prerequisite.

---

## Continuity Between Runs

**Task comments** — the primary record of what has been investigated and found. Persist indefinitely. The next run sees all prior comments when it reads task state on wake, so it does not duplicate completed investigation.

**Run summaries** — on exit, `autonomy_complete` records what was enriched, what was executed, what was created, and which wake events the run consumed. The next wake receives the last `run_history_count` summaries as part of its context, and the UI renders the consumed wakes as "woken by" provenance per run.

Working memory provides broader system context. Run summaries provide the autonomy-specific thread.

---

## Lifecycle

```
Cortex tick
  → elapsed since last autonomy run >= interval_secs
    OR unconsumed wake events are pending
  → no autonomy channel currently running
  → autonomy.enabled = true
  ↓
Cortex assembles context (identity + bulletin + working memory + tasks + goals + run summaries)
  ↓
Autonomy channel wakes with full context
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
Calls set_outcome → summary recorded
  ↓
Channel exits → cortex records last_run_at, cleans up
```

If the channel crashes mid-execution, the task returns to `ready`. If a task fails 3 consecutive times, it moves to `failed` and emits a working memory `Error` event. Enrichment runs (comments only) do not count as failures. `failed` is a new `TaskStatus` variant — the current set is pending_approval, backlog, ready, in_progress, done — so adding it includes the transition table, API, and UI sweep.

---

## Configuration

```toml
[autonomy]
enabled = false                    # Off by default
interval_secs = 1800               # How often to wake (seconds)
active_hours = [8, 22]             # UTC hour range — suppress wakes outside this window (optional)
max_turns = 20                     # Turn budget per run — on exhaustion, behaves like soft timeout
max_tasks_per_run = 2              # Max tasks to work on per wake
timeout_secs = 600                 # Hard timeout — last resort, should rarely fire
warn_secs = 480                    # Cortex injects soft warning at this point
run_history_count = 5              # How many past run summaries to surface on wake
claim_unowned = true               # Pick up tasks with no assigned agent
```

### Validation

Enforced at startup and on config reload — autonomy does not start if any rule is violated:

- `timeout_secs` ≤ `interval_secs` — a run cannot outlast the interval. If equal, runs are back-to-back (continuous operation) — valid but intentional.
- `warn_secs` < `timeout_secs` — warning must fire before hard timeout. Clamped to `timeout_secs - 60` if violated.
- `max_tasks_per_run` ≥ 1
- `interval_secs` ≥ 60 — minimum 1 minute, prevents accidental tight loops.

---

## Implementation

**Phase 1 — Autonomy Channel**
- `AutonomyChannelContext` builder: identity + bulletin + working memory + tasks + goals + run summaries
- `autonomy_channel.md.j2` system prompt (enrichment-first, `autonomy_complete` on exit, never execute `pending_approval`)
- Goals table migration + `goal_create`, `goal_update`, `goal_list` tools (see `goals.md`)
- Cortex: interval trigger, `last_run_at` tracking, context assembly, channel lifecycle, soft timeout addendum at `warn_secs`
- `autonomy_complete` tool + run summary storage + retrieval for run history. Intentionally distinct from `set_outcome`, which is an unpersisted last-write-wins delivery buffer with no completion contract. Model it on `memory_persistence_complete` instead, which already enforces call-the-terminal-tool-before-exit through the hook retry machinery.
- `last_enriched_at` column on tasks; atomic claim via `assigned_agent_id`; selection query with priority ordering
- Task retry/failure handling (3 strikes → `failed`, working memory error event)
- All config fields + validation

**Phase 2 — Task Comments — shipped**
- `task_comments` table migration
- `add_task_comment` tool (autonomy channel, branches, cortex chat, task workers)
- Task comments in the wake briefing: counts on every surveyed task, full
  recent comments on every enrichment-queue entry
- Task comments injected into the executing worker's briefing at ready-task
  pickup, bounded per comment and in total
- `last_enriched_at` column, transactional with the comment that sets it
- Atomic claim on first comment and on ready-task pickup
- API endpoints: list and create comments per task
- UI: chronological comments on task detail, worker pill with expandable full output

**Phase 3 — Polish**
- Autonomy UI surface: run history, last wake time, active enrichment progress
- Autonomy outcomes surfaced to relevant user channels via working memory synthesis

**Rejected — "quiet while active"**

Suppressing autonomy wakes while user channels have been recently active is
rejected, not deferred. Enrichment is exactly the work that is most useful
while the user is around to react to it, and the wake already carries the
signals that matter: a user comment on a task pulls the next run forward
rather than pushing it away. A global "someone is talking, stand down" flag
would suppress the run for reasons unrelated to the task it was going to work
on. Do not reintroduce it.

---

## Non-Goals

- **No talking to users.** No `reply` tool. Output goes to task comments, working memory, and `autonomy_complete`.
- **No recursive autonomy.** The autonomy channel cannot spawn other autonomy channels.
- **No identity or config modification.** Factory tools remain cortex-chat / admin only.
- **Goals are not auto-completed.** The agent proposes completion; the user confirms.
- **Per-goal active hours** are out of scope. `active_hours` applies to the whole system.
