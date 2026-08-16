# Autonomy Lifecycle: Deliberation, Task Readiness, and Sleep

The autonomy loop today is a fixed-interval heartbeat that renders whatever
fits in a prompt and asks the model to decide. That works on an empty
instance and degrades as the instance matures: the interval is the same
whether there is urgent work or nothing has changed in two weeks, the
decision is made from a truncated snapshot rather than a query, and task
execution is split across two independent systems that don't coordinate.

This doc closes those three gaps and states the succession plan for the
cortex.

Related: [autonomy.md](autonomy.md), [wakes.md](wakes.md),
[task-comments.md](task-comments.md),
[task-dependencies.md](task-dependencies.md),
[prompt-audit-2026-08-12.md](prompt-audit-2026-08-12.md),
[dormancy.md](dormancy.md).

## Current state (source-grounded)

- A resident per-agent supervisor owns the autonomy channel. Its bounded
  doorbell coalesces external wake notifications, and its interval heartbeat
  calls `autonomy_run_due(...)` when no epoch is active. `interval_secs`
  defaults to 1800 and never varies.
- **Ready tasks are picked up by the cortex, not by autonomy.**
  `spawn_ready_task_loop` → `pickup_one_ready_task` claims the
  highest-priority ready task on the cortex tick interval, gated only by
  `ready_pickup_allowed(level) == Act`. It shares the autonomy *level* but
  nothing else: no run record, no deliberation, no `max_tasks_per_run`, no
  entry in run history. A task flipped to `ready` starts executing within a
  tick, whether or not that is the most valuable thing to do.
- The autonomy prompt renders task state as a flat list of every task in
  four status buckets (limit 200 each), plus 5 run summaries, wake events,
  goals, and active workers. No retrieval — everything the run knows is what
  was rendered.
- The epoch ends with `autonomy_complete { summary, actions[] }`. The resident
  channel returns to idle. The completion says nothing about what should
  happen next or when.
- Enrichment exists only as a prompt instruction ("enrich pending_approval
  tasks"), with no state marking a task as researched. Nothing stops a run
  from re-enriching the same task forever — and the 2026-08-12 audit found
  exactly that failure mode in its cousin, repeated project investigations.

## 1. The run decides when the next run happens

The interval becomes a floor and a fallback, not the schedule. Each run ends
by declaring when it wants to be woken next, and why.

`autonomy_complete` gains:

```
next_wake: { after_secs: u64, reason: String }
```

The run answers one question — *how soon could there plausibly be something
worth doing?* — not *what will I do then*. Bounded by config
(`min_interval_secs`, default 300; `max_interval_secs`, default 86400) and
clamped on the way in, so a bad number degrades to the old behavior rather
than parking the agent for a month.

Stored on the run row; `autonomy_run_due` consults the last run's
`next_wake_at` instead of `last_started_at + interval_secs`. A run that
never called `autonomy_complete` falls back to the configured interval —
the crash path keeps the heartbeat.

This is safe precisely because sleeping is not going dark: **wake events
still interrupt**. `pending_wake_events > 0` short-circuits the due check
today and continues to. A tagged task comment, a schedule wake, a completed
worker, an inbound message — all of these wake the agent regardless of how
long the last run asked to sleep. Long sleeps are only long in the absence
of anything happening.

The design bet: a run that just surveyed the board is the best-informed
thing in the system about whether the next hour matters. "Three tasks are
mid-execution, wake in 10 minutes" and "nothing has changed in two weeks,
wake in 6 hours" are both correct, and only the run knows which it is.

This is also the missing economic half of [dormancy.md](dormancy.md). That
doc argues an idle agent should cost disk rather than a machine; a fixed
30-minute heartbeat guarantees the opposite by manufacturing a reason to be
resident forever. A run that can say "nothing for six hours" produces the
long quiet windows dormancy needs, and `next_wake_at` is exactly the durable
value a supervisor would use to decide when to rehydrate a sleeping agent.

## 2. A deliberation phase that queries

Today's briefing is a dump; the model picks from what fits. At scale the
dump is both too big and too incomplete — the 2026-08-12 audit already
measured the same failure in the run-history window, where the fix was a
character budget over a count.

Split the run into two phases with an explicit boundary:

**Phase A — deliberate.** A bounded opening segment whose only job is to
answer "what is the most valuable thing to do right now?" It receives a
*summary* rather than the full board: counts by status, the tasks that
changed since the last run, unseen comment counts, goals, in-flight work.
It has read-only retrieval tools — `task_list` with real filters,
`task_get`, `memory_recall`, `channel_recall`, the chronicle tools — so it
can *ask* instead of being told. It ends by naming its intent: which tasks,
which mode (execute / enrich / neither), and why.

**Phase B — act.** Execution proceeds against the declared intent under the
existing level rules and `max_tasks_per_run`.

The value is not the split for its own sake — it's that the decision stops
being a side effect of what fit in a prompt window. A mature instance can
have thousands of tasks and years of memory; "render 200 rows of each
status" is not a plan, it's a fixed-size window that silently stops being
representative.

Phase A's declared intent is recorded on the run, which also makes the
loop auditable: run history stops reading "I surveyed and found nothing" and
starts reading "I chose #14 over #9 because…".

## 3. Ready tasks are picked up by autonomy, not the cortex

Delete `spawn_ready_task_loop` / `pickup_one_ready_task`. Ready-task
execution moves inside the autonomy run, where it belongs:

- Marking five tasks `ready` no longer starts five executions on the next
  cortex tick. It puts five tasks in front of the next deliberation, which
  chooses *which* to run, in what order, respecting `max_tasks_per_run` and
  dependency readiness ([task-dependencies.md](task-dependencies.md)).
- Every execution gets a run record, a decision, and a line in run history.
  There is one path from "approved work exists" to "work is running."
- The user-visible contract improves: hitting **go** on a task means "this
  is available to be chosen," not "this starts within 30 seconds." When the
  loop next fires, it decides.

The autonomy level rules are unchanged — execution is still Act-only. What
changes is that execution is a *decision made by a run* rather than a
background loop that happens to share a config field.

## 4. Task enrichment as an explicit state

Some tasks carry everything needed in the description. Others need research
before anyone can sensibly approve or execute them. Today that difference is
invisible, so the loop either ignores it or re-researches forever.

Add to `tasks`:

```
enrichment TEXT NOT NULL DEFAULT 'not_needed'
    -- 'not_needed' | 'needed' | 'in_progress' | 'done'
```

Set at creation: a task the author knows is under-specified is created with
`enrichment = 'needed'`. The tools' descriptions teach the distinction —
small, fully-specified work is `not_needed`; anything requiring
investigation before it can be executed or judged is `needed`.

**A task created from a design document is already enriched.** When a task
is created with an attachment (§5), the research that enrichment would have
produced is the thing it was created from, so it starts at `not_needed`.
This is the common case for the work that matters most: a design doc gets
written, tasks are cut from it, each carries a copy of the doc, and the loop
spends its enrichment budget on the tasks that actually lack a plan. An
attachment can still be marked `needed` explicitly when the doc raises
questions it doesn't answer — the default follows the attachment, it isn't
imposed by it.

**The enrichment pass.** When deliberation selects a `needed` task, the run
spawns a regular worker (per the task's execution plan) to research it. The
worker's contract:

1. Produce a plan document as a task attachment (§5).
2. Leave a comment summarizing findings and what it changed
   ([task-comments.md](task-comments.md)).
3. Answer the positioning questions explicitly: does this depend on other
   tasks (propose `depends_on` edges), is its execution plan right, is it
   the right size, does it still make sense to do at all?
4. Set `enrichment = 'done'`.

The state transition is what makes this a state machine rather than a
treadmill: `needed → in_progress` on spawn, `→ done` on the worker's
successful completion, `→ needed` only if a human or the task's owner
explicitly asks for more research. **A run may never re-enrich a `done`
task.** Iteration after that point is human-driven — Jamie comments,
requests changes, and the agent responds through the comment/mention path,
which is a reply, not an enrichment pass.

That gives the loop a terminating gradient: every enrichment pass strictly
decreases the number of `needed` tasks, and the loop cannot manufacture more
work for itself by re-examining what it already examined.

The end state Jamie described falls out of this: a board of tasks that are
enriched, positioned, dependency-ordered, and specced — waiting on one human
decision. Approval becomes cheap because everything under it is already
answered.

## 5. Task attachments

Enrichment produces documents, and a plan document does not belong in a
description field or a comment body. Neither does a design doc a task was
cut from.

```
{workspace_dir}/tasks/{task_number}/
```

Plain files on disk, in the agent's workspace next to `notes/`, `research/`,
and `skills/` — the same convention those already follow. Workers edit them
with the existing `file` tool: no new write API, no new storage layer, and
the file tool's workspace-scoped path validation already contains them.

- Conventional entry point `plan.md`; anything else in the directory is
  attachment content too.
- The task API lists a task's attachments (name, size, mtime) by reading the
  directory; the UI renders the list and the file contents.
- The executing worker's briefing includes `plan.md` when present — the
  research a task accumulated over weeks reaches the worker that finally
  runs it, which is the entire point of doing the research early.
- Deleting a task leaves its directory (cheap, recoverable, and deletion of
  work product should be deliberate); a maintenance pass can reap orphans.

### Attachments are copies, never references

`task_create` accepts source paths (`attach: ["docs/design-docs/foo.md"]`)
and **copies** their contents into the task directory at creation. It never
stores a path and reads through it later. Four reasons, any one sufficient:

- **The source may not be durable.** Design docs live in a repo only if
  their author commits them — many are scratch files, and the ones outside
  a repo are exactly the ones that vanish.
- **Worktrees are ephemeral by design.** A task with
  `worktree_mode: create` runs in a worktree that gets provisioned and
  reaped ([task-dependencies.md](task-dependencies.md)). A plan living at a
  path inside one would be gone before the work finished.
- **The worker may not have the source checked out.** Cross-repo tasks and
  builtin workers running outside any project would resolve the path to
  nothing.
- **A task's plan should be stable.** The plan a task was scoped and
  approved against shouldn't silently change because someone edited the
  source doc weeks later. Approval means something only if what was
  approved holds still.

Provenance is recorded rather than depended on — task `metadata` carries the
source path, and the repo and commit when the source was inside a checkout:

```json
{"attachments": {"plan.md": {
    "source": "docs/design-docs/autonomy-lifecycle.md",
    "source_repo": "spacebot",
    "source_commit": "9949c5db",
    "copied_at": "2026-08-12T21:40:00Z"
}}}
```

That keeps the trail — an agent or human can diff the copy against the
current source, or refresh it — while the task's own copy stays the
authority. Refreshing is an explicit action, never automatic.

## 6. Cortex succession

The cortex predates all of this. What it currently owns:

| Responsibility | Disposition |
|---|---|
| Ready-task pickup | **Removed** — moves to autonomy (§3) |
| Autonomy dispatch (`maybe_run_autonomy`) | Moves to its own scheduler task; no reason for autonomy's cadence to be a cortex tick |
| Schedule-wake firing | Moves with it — wakes and autonomy are one system |
| Maintenance scheduling | Keep for now; independent of deliberation |
| Working-memory pruning | Keep; cheap SQL housekeeping |
| Intraday / daily synthesis | Superseded in principle by session chronicles; retire once chronicle rollups cover the same ground |
| Association loop, warmup | Keep; unrelated to work orchestration |
| Cortex chat | Already hidden from the UI and used only in the create-agent flow; retire with the create-agent rework |

The through-line: **the cortex stops being the thing that decides what work
happens.** It keeps housekeeping that genuinely wants a periodic tick, and
everything about choosing and running work lives in the autonomy lifecycle.
Nothing here requires a big-bang removal — §3 alone moves the load-bearing
piece, and the rest can be retired as their successors prove out.

## Interaction with existing designs

- **Dependencies** ([task-dependencies.md](task-dependencies.md)): with
  autonomy owning pickup, `claim_next_ready`'s dependency gating stops being
  a filter on a background loop and becomes an input to deliberation — the
  run sees "ready but blocked by #2" and can choose to unblock instead.
- **Comments** ([task-comments.md](task-comments.md)): the mention-wake path
  is what makes long sleeps safe, and the enrichment worker's report is a
  comment. Unseen-comment counts are a deliberation input.
- **Prompt audit** ([prompt-audit-2026-08-12.md](prompt-audit-2026-08-12.md)):
  §3 of that doc asked for tasks to become the spine of autonomous work and
  §4 for character-budgeted run history. This doc is the completion of both
  — deliberation replaces "render everything and hope."
- **Goals** ([goals-as-authority.md](goals-as-authority.md)): deliberation
  ranks by goal contribution, and a goal's authority is what admits derived
  work without per-task approval. An instance with an active goal is also
  never in the empty-instance cold start — the goal is the direction that
  branch exists to substitute for.

## Failure modes

- **A run sleeps too long and something urgent arrives** — wake events
  interrupt; that is the invariant the whole sleep design rests on. Any new
  urgency source must be a wake producer, not a polling loop.
- **A run always asks for the minimum** — bounded by `min_interval_secs`,
  and run history makes a pathological pattern visible.
- **Deliberation spends the whole turn budget** — Phase A is turn-capped
  separately from the run's `max_turns`; exhausting it falls through to the
  old behavior (pick by priority) rather than ending the run empty.
- **Enrichment worker fails** — `in_progress` reverts to `needed` on worker
  failure, with the failure recorded as a task comment. Retry is a later
  run's decision, not an immediate loop.
- **Attachment path traversal** — the file tool's workspace scoping already
  governs this; task directories are ordinary workspace paths.

## Phases

1. **Autonomy owns ready tasks.** Remove the cortex ready-task loop; execute
   selected ready tasks inside the run. One path to execution.
2. **Enrichment state.** `enrichment` column, tool/API surface, the
   enrichment worker contract, transition rules, briefing render.
3. **Attachments.** `tasks/{n}/` convention, `attach:` copying with
   provenance metadata at creation, API listing, worker briefing inclusion
   of `plan.md`, UI rendering.
4. **Deliberation phase.** Summary-first briefing, retrieval tools in Phase
   A, declared intent recorded on the run.
5. **Run-scheduled wakes.** `next_wake` on `autonomy_complete`, run row
   column, `autonomy_run_due` consulting it, config bounds.
6. **Cortex retirement.** Move autonomy dispatch and schedule wakes out;
   retire synthesis once chronicle rollups cover it.
