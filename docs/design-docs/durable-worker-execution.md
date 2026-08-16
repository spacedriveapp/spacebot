# Durable Worker Execution

Workers execute assigned tasks until they produce a durable outcome, are
explicitly cancelled by an operator, or encounter a declared external block.
Provider failures, context pressure, process restart, liveness signals, and
runtime budgets are execution conditions. They are not reasons to discard a
worker.

This replaces the current collection of programmatic abort paths with one
durable continuation model. It builds on
[`worker-reliability.md`](worker-reliability.md) and
[`worker-lifecycle-convergence.md`](worker-lifecycle-convergence.md).

## Contract

1. A worker has durable task ownership from creation through a terminal
   outcome.
2. Every active worker has a durable checkpoint containing enough state to
   inspect and resume it after Spacebot restarts.
3. The runtime retries, compacts, reconnects, or waits for recoverable faults.
   It does not convert them directly into `Failed`, `Partial`, `TimedOut`, or
   `Cancelled`.
4. Only an explicit operator cancellation, an affirmed terminal worker
   outcome, or a declared external block may close task ownership.
5. Every worker is inspectable while active from a durable checkpoint and from
   the live event stream when available.
6. A worker that cannot progress remains owned and visible. It enters a
   recoverable `Suspended` state with evidence and a next recovery action.

`Failed` describes an execution attempt, not task ownership. A failed attempt
creates a new durable attempt or waits for operator action. It does not silently
dispose of the task.

## States

```text
Created -> Running <-> Checkpointing
                  -> WaitingForInput
                  -> Suspended
                  -> Completing -> Succeeded | Blocked | Cancelled

Suspended -> Recovering -> Running
WaitingForInput -> Running
```

`Suspended` is nonterminal. It records why progress stopped, the last durable
checkpoint, the recovery action, and the next retry time. `Recovering` fences
the old generation before a resumed driver can emit events. Terminal states are
monotonic and retain the final checkpoint.

## Programmatic Stops To Remove

The following paths currently end an active worker or turn a recoverable
condition into a terminal outcome. Replace each with checkpoint and recovery.

| Current condition | Current effect | Replacement |
|---|---|---|
| 30-minute wall-clock timeout | Drops the built-in worker future | Disable by default. If configured, request a checkpoint and move to `Suspended`. |
| Segment budget | Returns `Partial` | Checkpoint, start a new segment or durable continuation. |
| Tool-nudge exhaustion | Ends the run | Persist the incomplete response and inject a recovery instruction. |
| Context-overflow retry ceiling | Fails the worker | Compact from the last checkpoint, reduce artifact context, then continue on a new generation. |
| Provider retry ceiling | Fails the worker | Persist backoff state, retry through the routing chain, then suspend until the provider is available. |
| OpenCode SSE inactivity timeout | Fails the worker | Reattach by session ID, poll the backend, or suspend with a reconnect action. |
| Cortex liveness intervention | Aborts the handle | Request cooperative checkpointing. Escalate only to `Suspended` after the grace period. |
| Cron channel timeout | Orphans or cancels children | Transfer worker ownership to the durable supervisor. Autonomy has a resident channel and no run timeout. |
| Process shutdown | Leaves active work for startup failure reconciliation | Drain checkpoints before exit. On boot, restore active workers into `Recovering`. |
| Startup reconciliation | Marks `Running` workers failed | Resume from checkpoint or mark `Suspended` with recovery evidence. |
| Backend/session reconnect failure | Fails or retires the worker | Retain resume metadata and suspend for retry or operator remediation. |

The explicit cancellation path remains. It must claim terminal ownership,
request a cooperative checkpoint, and preserve the final transcript before a
forced abort backstop.

## Durable Checkpoint

Checkpoint after every state-changing boundary:

- worker creation and task claim
- before and after an LLM request
- before and after a tool call
- provider retry scheduling and fallback selection
- backend session creation, reconnect, and cursor advancement
- progress intervention, cancellation request, and ownership transfer

The checkpoint contains the worker and task IDs, lifecycle, generation, task
specification, backend/session metadata, normalized transcript cursor, pending
tool or model operation, retry state, context-compaction state, current
workspace, and latest progress status. Transcript payloads may remain bounded,
but their durable cursor and summary must always be available.

Persisting before an external request makes replay explicit. If the outcome of
that request is ambiguous after a crash, the recovery driver reconciles with
the provider or backend before issuing another request. It never blindly
duplicates a non-idempotent operation.

## Recovery Driver

Startup loads every nonterminal worker into a supervisor-owned recovery queue.
For each worker, the driver:

1. Acquires the next generation through a conditional lifecycle update.
2. Reads the latest checkpoint and validates workspace and backend access.
3. Reattaches to an existing provider or OpenCode session when one exists.
4. Replays normalized events from the stored cursor or resumes the built-in
   loop from its persisted history and pending continuation.
5. Commits subsequent checkpoints under the new generation.

If recovery cannot run immediately, the worker remains `Suspended`. The API
shows the exact blocker, retry time, last checkpoint, and available operator
actions. It is never rewritten as a completed failure merely because Spacebot
restarted.

## Inspection

Worker detail must always return:

- durable lifecycle and attempt/generation
- task ownership and supervisor
- last checkpoint time and transcript cursor
- current operation, progress timestamp, and retry/reconnect state
- durable transcript through the last checkpoint
- labelled live transcript data when the active driver is connected
- recovery and cancellation actions with their current state

Live SSE data improves freshness. It is not the only source of truth. The
durable detail endpoint must render useful state after a restart before any
worker has resumed.

## Autonomy Continuity

An autonomy run is allowed to end while its workers continue under supervisor
ownership. The next run must receive the outcomes that arrived in the gap.
Current active-worker rendering is insufficient: it only shows workers still
running when the next briefing is built. A worker that completed after the
previous channel exited has a durable `worker_runs` result, but no active row,
no channel event handler to enqueue a wake, and no representation in the next
run's context.

Each terminal worker commit therefore creates a durable, owner-addressed outcome
inbox entry. The entry includes the worker ID, task and autonomy run ownership,
outcome kind, bounded result summary, transcript/checkpoint reference, and
outcome version. Its creation is part of the terminal transaction or a
transactional outbox consumed from that commit. It must not depend on a live
channel receiving `WorkerComplete`.

At the start of an autonomy run, the driver claims unacknowledged inbox entries
for that agent before rendering the system context. It renders two separate
sections:

- **Workers still running**: worker ID, task, owner, lifecycle, last progress,
  and recovery state. This prevents duplicate work.
- **Worker outcomes since the previous run**: task, outcome, bounded result,
  and the next action required. This lets the model finish follow-up work,
  update task state, or report a blocker.

Claiming is idempotent by worker ID and outcome version. The run records the
claimed entry IDs with its provenance. If it crashes before its own terminal
summary commits, a later run can reclaim those entries. A completed worker is
never hidden merely because it completed between autonomy runs.

The current run may still receive a live completion retrigger. That is a
latency optimization only. The durable inbox is the source of truth for every
completion that arrives after the parent channel exits or misses an event.

## Delivery Phases

### Phase 1: Stop terminalizing recoverable conditions

Replace wall-clock, segment, tool-nudge, overflow, provider-retry, and OpenCode
inactivity terminal exits with `Suspended` or a durable continuation. Remove
startup reconciliation that marks active workers failed. Keep explicit
cancellation and declared external blocks terminal.

### Phase 2: Persist executable checkpoints

Add a versioned worker checkpoint record and generation fence. Persist it around
model requests, tool calls, backend cursor updates, retry scheduling, and state
transitions. Make terminal completion include the final checkpoint.

### Phase 3: Recover active workers

Add the supervisor recovery queue. Restore built-in workers from persisted
history and continuation state. Reattach OpenCode workers by session and event
cursor. Preserve the task claim throughout recovery.

### Phase 4: Unify intervention

Route liveness, shutdown, channel exit, autonomy settling, and cancellation
through one cooperative checkpoint protocol. Transfer unattended workers to the
supervisor instead of orphaning or cancelling them.

### Phase 5: Make inspection durable

Expose checkpoints, attempts, recovery state, and transcript cursors through
the worker API and `worker_inspect`. Retain live SSE as an additional view.

### Phase 6: Deliver outcomes across autonomy runs

Create the transactional worker-outcome inbox, render active workers and
unacknowledged completed outcomes separately in each autonomy run, and record
idempotent inbox consumption with the run. Move completion wake production from
the parent channel event handler to durable terminal-outcome delivery.

### Phase 7: Retire legacy termination controls

Delete constants and branches that convert resource pressure or transient
execution faults directly into terminal worker outcomes. Keep bounded artifact
size, retry scheduling, and repetition detection as recovery inputs.

## Verification

- Kill Spacebot during an LLM request, a tool request, retry backoff, and an
  OpenCode stream. After restart, verify the same worker ID and task claim
  resume or enter visible `Suspended` state.
- Simulate repeated context overflows, provider failures, and SSE disconnects.
  Verify checkpoints advance and no task becomes terminal without an explicit
  terminal cause.
- Race completion against cancellation, shutdown, recovery, and stale events.
  Verify one terminal owner and generation-fenced events.
- Query worker detail before restart, after restart before recovery, and after
  recovery. Verify a durable transcript and recovery state are available in all
  three cases.
- End an autonomy run with an owned worker still active, let the worker finish
  before the next wake, and verify the next run receives its outcome from the
  durable inbox and does not duplicate the task.
- Verify explicit cancellation preserves the latest checkpoint and does not
  overwrite a concurrent successful completion.
