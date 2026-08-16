# Worker Reliability

A worker must never end without telling anyone what happened. Today spacebot has roughly fifteen distinct ways a worker can terminate, and several of them produce no outcome at all: the requester waits on a run that silently stopped existing, the DB row sits `running` until the next restart, and a concurrency slot leaks forever. The ones that do produce outcomes often destroy work on the way out -- the wall-clock timeout drops the worker future mid-step, losing everything since the last segment boundary.

This design replaces the current termination machinery with two invariants and one measurement change:

1. **Outcome invariant.** Every spawned worker terminates in exactly one `WorkerOutcome`, persisted before anyone is notified. There is no code path -- panic, timeout, channel death, process shutdown -- that ends a worker without a durable record.
2. **Termination is staged.** Nothing goes from "running" to "aborted" in one step. Stalled workers are interrupted cooperatively first, so they can persist their transcript and return partial work. The hard abort exists only as a backstop behind a grace period.
3. **Liveness is progress, not time.** A worker is alive if it is advancing -- making LLM calls, running tools. It is not dead because thirty minutes passed, and it is not dead because it went quiet on an event bus while inside one long streaming call.

A fourth failure mode sits at the other end of the lifecycle: a worker that terminates perfectly can still be worthless because it was spawned blind. By default a worker receives no conversation history -- the task description the channel wrote for it is its entire world. Section 8 makes workers forks of their channel, the way branches already are.

Production data motivating this is in `production-worker-failures.md`: 32 workers failed over 48 hours. That doc's hypothesis was that workers need more defensive limits. This design takes the opposite position on kill-switches specifically: tool-output caps and context monitoring are good (and orthogonal to this doc), but every limit that *terminates* a worker on a clock or a counter is a way to lose work that an outcome-producing path wouldn't lose. A good worker never fails without an outcome; most of our failure modes were built, not inherent.

Related designs: `cortex-implementation.md` (the supervisor this doc modifies), `task-tracking.md` (detached task lifecycle), `dormancy.md` (dormant agents currently have no supervision at all), `skill-lifecycle.md` (the reflection pass section 9 points at worker transcripts).

## The current termination inventory

Why workers "die or time out frequently," concretely:

| # | Path | Where | Outcome produced? | Work preserved? |
|---|------|-------|--------------------|-----------------|
| 1 | 30-min wall clock races the whole run | `worker.rs:512-538` | `Timeout` | No -- future dropped, no transcript persist |
| 2 | Cortex idle sweep, 600s without *events* | `cortex.rs:1227-1418` | Synthetic `WorkerComplete` | Partial -- live transcript drained if API map is shared |
| 3 | `handle.abort()` (supervisor, API, `cancel` tool) | `channel.rs:423-544` | Synthetic `WorkerComplete` | Same caveat |
| 4 | Turn/segment/retry ceilings (6 hard-coded constants) | `worker.rs:26-49`, `hooks/spacebot.rs:94` | Mixed: `Partial`, `Failed`, or `Cancelled` | Varies |
| 5 | Cron channel hard-timeout aborts channel, workers orphaned | `cron/scheduler.rs` | **None, ever** | Worker keeps running into a void |
| 6 | Broadcast lag drops `WorkerComplete` | `lib.rs:349`, `channel.rs:1433-1452` | **Lost in transit** | Slot leaks, self-exiting channels wedge |
| 7 | Process shutdown | `main.rs:1780-1810` | **None** -- next boot marks rows `failed` | No |
| 8 | OpenCode SSE 600s inactivity | `opencode/worker.rs:541-546` | `Failed` | No |

The four structural problems underneath the table:

**The idle sweep measures silence, not health.** Liveness is inferred from `ToolStarted`/`ToolOutput`/`WorkerStatus` events. A worker inside a single long LLM call emits nothing -- and the streaming HTTP timeout is 1800s, so there is a 20-minute window where a healthy worker doing a long generation looks dead and gets killed at the 600s mark. This is the single biggest source of spurious kills.

**Timeouts destroy evidence.** Both the wall clock and `handle.abort()` drop the future at its current await point. `persist_transcript()`, the failure log, and the token-usage flush live inside that future and never run. We punish a slow worker by deleting its work.

**Outcomes ride a lossy transport with no ownership handoff.** `WorkerComplete` is a broadcast event that (a) can be dropped on lag, and (b) is deliberately ignored if the worker id has already left `worker_handles` (`channel.rs:3507-3516`). When a channel dies before its workers, nothing inherits them -- the cron scheduler's own comment acknowledges it doesn't bother cancelling workers because the completion event would be dropped anyway.

**Failure classification is incoherent.** Tool-nudge exhaustion maps to `Cancelled` in the initial loop (`worker.rs:725-732`) and `Failed` in the follow-up loop (`worker.rs:882-896`). Budget exhaustion is `Partial` (a success) but transient-retry exhaustion is `Failed` even when eight segments of good work exist. The same underlying situation produces different outcomes depending on which loop you're in.

## Design

### 1. The outcome outbox

`WorkerOutcome` stops being an event and becomes a database invariant.

Every worker termination -- natural or forced -- follows the same sequence: write the terminal state to `worker_runs` first (outcome kind, result text, error, token usage, `completed_at`), then notify listeners. Notification failure or receiver lag can no longer lose an outcome; the row is the source of truth and the event is a cache-invalidation hint.

The single-writer guarantee generalizes the CAS lifecycle that detached workers already have (`process_control.rs:16-19`: `ACTIVE → KILLING|COMPLETING → TERMINAL`). Channel-owned workers get the same FSM. Whoever wins the CAS writes the row; the loser's outcome is dropped *by the FSM*, not by a guard scanning `worker_handles`. The silent-drop guard at `channel.rs:3507-3516` becomes unnecessary and is removed.

Channels stop being the only party that can retire a worker. A reconciliation pass in the cortex tick (already reading the worker registry) catches any `worker_runs` row whose lifecycle is TERMINAL but whose channel never processed the completion -- because the channel died, lagged, or exited -- and performs the retirement itself: clear the handle entry, release the concurrency slot, deliver the outcome to the task store if the worker was task-attached.

### 2. Progress-based liveness

Workers already export a shared `AtomicUsize` for segment count (`worker.rs:272`). This grows into a `WorkerProgress` struct shared between the worker and the supervisor:

```rust
pub struct WorkerProgress {
    /// Incremented when an LLM call starts and again when it completes.
    llm_calls: AtomicU64,
    /// Incremented on each tool invocation.
    tool_calls: AtomicU64,
    /// Tool currently executing, if any (index into a small registry; 0 = none).
    in_tool: AtomicU32,
    /// Unix seconds of the last time any of the above changed.
    last_progress_at: AtomicI64,
    segments: AtomicUsize,
}
```

The cortex reads this directly from the worker registry on each tick instead of reconstructing activity from bus events. Broadcast lag can no longer make a live worker look dead, and the lag-tick kill-suppression valve (`cortex.rs:1292-1303`) becomes unnecessary for worker liveness.

A worker is **stalled** when no field has advanced for the threshold:

- `stall_idle_secs` (default 450) -- not inside an LLM call or tool
- `stall_in_call_secs` (default 1800, matching the streaming HTTP timeout) -- inside an LLM call; the HTTP layer's own timeout fires first and feeds the transient-retry path, so the supervisor never races a healthy long generation
- `stall_in_tool_secs` (default 1200) -- inside a tool invocation

Starting an LLM call counts as progress; so does finishing one. A worker making steady calls can run for hours. That is the point: the limits are on *stopping*, not on *working*.

The wall-clock timeout (`worker_wall_clock_timeout_secs`) becomes opt-in, default off. It remains available for operators who want a hard cap on cron-driven work, but it routes through staged termination (below) rather than dropping the future.

Because liveness reads a shared struct rather than cortex events, the stall monitor also runs for dormant agents (a lightweight tick that skips everything else in the cortex loop) and covers OpenCode workers, whose SSE-inactivity check moves onto the same progress struct and thresholds instead of its private hard-coded 600s.

### 3. Staged termination

The only lever today is `handle.abort()`, which is why every kill loses work. Termination becomes three stages:

**Stage 1 -- cooperative interrupt.** Each worker owns a `CancellationToken`. `run_inner` observes it at turn boundaries and inside the LLM-call select. On cancellation the worker *returns* rather than being dropped: it persists its transcript, flushes token usage, and produces `WorkerOutcome::Stalled { last_progress_at, phase, partial_result }` -- where `partial_result` is whatever `set_status` and completed segments have produced so far. All the cleanup that currently only runs on the happy path runs here too, because we are unwinding normally.

**Stage 2 -- grace.** The supervisor waits `stall_grace_secs` (default 120) for the cooperative interrupt to land. Most stalls resolve here.

**Stage 3 -- abort backstop.** Only if the worker ignores the token past the grace period: `handle.abort()`, followed by the compensating persist that supervisor-cancel already does (drain `live_worker_transcripts` into `worker_runs`) -- made unconditional by having the channel share the live-transcript map with the supervisor rather than only with the API layer (`channel.rs:385-393`). Then the outbox write, so even the hard path yields a durable `Stalled` outcome.

Every existing kill site routes through this: the cortex sweep, the `cancel` tool, the API cancel endpoint, and the opt-in wall clock. The `cancel` tool and API may skip to stage 3 after a shorter grace when the caller says so (a human clicking "stop" means stop), but they still get the transcript drain and the outbox write.

### 4. One budget, and exhaustion is not death

The six hard-coded ceilings collapse into a single per-worker iteration budget with one rule: **exhausting a budget ends the run with the work it produced.**

- `MAX_SEGMENTS` already does this correctly (`Partial`, counts as success). It becomes the configurable `worker_segment_budget`.
- Tool-nudge exhaustion stops mapping to `Cancelled`-or-`Failed`-depending-on-loop. Both loops classify it as `Partial` carrying the accumulated text, with a structured reason.
- Transient-retry and overflow-retry exhaustion keep producing `Failed` -- those are genuine inability to proceed, not budget exhaustion -- but `Failed` now always carries the partial transcript reference and a machine-readable `failure_reason`, mirroring what the detached path already writes.

Retry ceilings stay (they end in outcomes, which is the property we care about), but move from constants to config so operators on flaky providers can raise them without a rebuild.

The detached-worker requeue asymmetry gets the same treatment: `Failed` outcomes currently requeue to `Ready` forever (`cortex.rs:4169-4177`) while only supervisor timeouts have a retry counter. One retry counter covers both paths; exhaustion sends the task to `Backlog` with the accumulated failure history attached.

### 5. The cortex as stall inspector

With deterministic stall detection in place, the cortex gets a role only an LLM can fill: deciding what a stall *means* before the machinery acts.

When the monitor flags a stalled worker, before stage 1 fires, the cortex may inspect the live transcript and choose:

- **wait** -- the worker is mid-way through something legitimately slow; extend the threshold once
- **steer** -- inject guidance through the existing injection channel (`worker.rs:307`) and reset the stall clock
- **interrupt** -- proceed with staged termination now

Two hard rules keep this from becoming a new failure mode. The inspector is *fail-open*: if the cortex is dormant, lagging, errors out, or exceeds its own decision timeout, staged termination proceeds on schedule -- a broken judge must not wedge cleanup, and deterministic stage 3 is always the backstop. And the inspector can only *delay within bounds*: at most one `wait` per stall episode, so a confused judge cannot keep a dead worker alive indefinitely.

This inverts the current relationship between the cortex and workers. Today the cortex's only lever is a kill based on a signal that can't distinguish "stuck" from "thinking." After this design, the mechanical layer detects stalls reliably, and the judgment call -- the part that actually benefits from reading the transcript -- is the LLM's.

### 6. Ownership survives channel death

Autonomy no longer has a channel or run timeout. Its resident channel retains worker handles across heartbeats and consumes their results while active or idle. Cron still needs an ownership transfer path when its hard timeout fires. That abort path should enumerate `worker_handles` and re-register each worker as detached so useful work continues under stall monitoring and its outcome lands in the outbox.

The resident autonomy supervisor now owns shutdown convergence. Final shutdown and agent deletion cancel and persist every retained child before the database closes. Restart cancels active epoch work but preserves idle interactive workers for session recovery.

### 7. Shutdown drains

The resident autonomy supervisor performs its drain before agent databases close. A future process-wide worker supervisor still needs the staged `shutdown_drain_secs` contract for user channels and cron-owned workers.

### 8. Workers are forks

Delegation is currently lossy by construction. `WorkerHistoryMode` defaults to `None` (`conversation/settings.rs:64`), so the channel agent's task description is the only context a worker gets, and the agent writes that description by compressing the conversation at the moment of highest information need -- without knowing which details the downstream task will turn out to require. The other modes don't rescue it: `Summary` is a stub that logs a warning and hands the worker nothing (`channel_dispatch.rs:737-743`), and `Recent(n)` pays for history without the guarantee that matters -- whatever was load-bearing has no reason to be in the last N messages.

The observed shape: a channel spends a session researching a problem across several workers, accumulating transcripts and evidence, and is then asked to produce a design doc. It spawns a worker whose entire context is a one-paragraph restatement of the session's conclusions. The conclusions survive; everything underneath them -- the evidence the document exists to justify -- dies at the handoff. The operator, who mentally tracks what the session knows, expects the delegated work to be written from that same context and gets something strictly worse.

The fix is a default flip, not new machinery. `WorkerHistoryMode::Full` already clones `state.history` under the worker's own system prompt (`channel_dispatch.rs:754-758`), which is byte-for-byte the branch fork semantic (`branch.rs:30-31`). Workers become forks: the difference between a worker and a branch is the tools it gets, not the context it has. The worker system prompt stays -- role and tools come from the prompt, knowledge comes from the history.

The mode set collapses to two:

- **`fork` (default)** -- full history clone under the worker system prompt.
- **`clean`** -- task description only. This stays as an explicit opt-out for fan-out (N parallel workers each forking a long history is a real token multiplier, and the differing system prompt breaks the prompt-cache prefix, so each fork pays full input cost once) and for mechanical tasks where history is noise. Detached workers, which have no channel to fork, are `clean` by definition rather than a special case (`spawn_worker.rs:495`).

`Recent` and `Summary` are deleted, not deprecated.

Three consequences ride along:

- The `spawn_worker` tool description tells the agent to "include all context needed since the worker can't see your conversation" (`spawn_worker.rs:407-408`). It changes to the opposite -- the worker shares the conversation; describe the task, not the background. Without this the channel keeps writing the lossy summaries out of habit, now as pure waste.
- The fork path gains the pre-flight compaction branches already do (`branch.rs:110`), so an oversized history is compacted before the worker's first LLM call instead of tripping the overflow-recovery ladder.
- `WorkerMemoryMode` is untouched -- memory access stays an independent axis of the same `WorkerContextMode` struct.

Scope note: forking fixes intra-session handoff. Knowledge that should outlive the session -- environment quirks a worker discovered, working recipes for the sandbox -- is not carried by a fork of what the channel currently holds; that is a separate design.

### 9. Reflection reads worker transcripts

Section 8 fixes what flows into a worker. This section fixes what flows out of one into the learning loop, and it is independent of section 8: even with workers as forks, the return path is a summary.

Skill reflection rides the memory-persistence branch. The signal is a bare `AtomicBool` (`channel.rs:759`) set by a turn crossing `min_tool_iterations` tool calls or by any successful worker completion (`channel.rs:3562-3573`); the next persistence branch spawns with the reflection prompt section and the skill tools. Because it is a branch, it forks channel history -- and for a worker, channel history holds the task description and the final result text (`[Background worker <uuid> completed]: ...`). The lesson-bearing material -- what the worker actually tried, what failed, what sequence finally worked -- lives only in the worker transcript, which never enters channel history.

The bridge already exists: every branch toolset, including the persistence branch, carries `worker_inspect` (`tools.rs:938`, `tools/worker_inspect.rs`), which renders a completed worker's full transcript inline as markdown -- every reasoning step, tool call, and result -- and lists recent runs when called without an id. Nothing connects it to reflection:

- The reflection prompt section (`prompts/en/memory_persistence.md.j2:72-100`) never mentions `worker_inspect`. The pass has to spontaneously decide the transcript matters.
- The same prompt argues against it: memory extraction rule 7 excludes "worker process chatter." Right for memories, wrong for reflection -- the retries and dead ends are the raw material a procedural skill is distilled from. A model reading the whole prompt will reasonably conclude worker internals are out of scope.
- The signal drops the worker id, so even a willing pass has to scan forked history for `[Background worker ...]` lines to find out what to inspect.

The result is a learning loop that learns from the lossy layer: a worker can spend eleven minutes discovering a working procedure, report a clean two-line success, and reflection sees nothing to teach.

The fix, in three parts:

- **The signal carries worker ids.** The `AtomicBool` becomes a small drained set; the persistence spawn renders it into the prompt as "these workers completed since the last pass."
- **The reflection section instructs the pull.** Before deciding whether a lesson exists, `worker_inspect` the listed workers -- and their failed predecessors from the list view, because the trials usually live in the worker that did not succeed just before the one that did.
- **Rule 7 gets scoped.** "Worker process chatter" stays excluded from memory extraction, explicitly not from skill reflection.

Two adjacent details: `worker_inspect` truncates each tool result to 500 bytes, which keeps most error lines but clips longer output -- the cap should be higher when the caller is a reflection pass; and the tool covers workers only, so branch transcripts (`branch_runs`) have no equivalent inspection path -- reflection sees down workers, not down sibling branches. The latter is noted, not solved here.

The existing negative-capture bans in the reflection prompt already guard the failure mode this enables: transcripts full of environment flukes must not harden into "tool X doesn't work" skills. The bans were written before the pass could see transcripts; once it can, they stop being theoretical.

## Outcome taxonomy after this design

```rust
pub enum WorkerOutcome {
    Success { result: String },
    /// Budget exhausted or interrupted with usable work. is_success() == true.
    Partial { result: String, reason: PartialReason },
    /// Stopped by staged termination. Carries whatever work existed.
    Stalled { partial_result: Option<String>, phase: StallPhase, last_progress_at: i64 },
    /// Explicitly cancelled by a human, the channel LLM, or the API.
    Cancelled { reason: String, partial_result: Option<String> },
    /// External blocker (captcha, auth wall). Unchanged.
    Blocked { reason: String, url: Option<String>, evidence: Option<String> },
    /// Genuine inability to proceed. Always carries failure_reason + transcript ref.
    Failed { error: String, failure_reason: FailureReason, partial_result: Option<String> },
}
```

`Timeout` is gone as a first-class outcome -- the opt-in wall clock produces `Stalled { phase: WallClock }`. Every variant either is success or carries the partial work; there is no variant that means "it's gone."

## What this deliberately does not change

- **Admission control** (`max_concurrent_workers`, duplicate-task reservation) -- correct as-is, and slot leaks stop once retirement no longer depends on a live channel.
- **Segment compaction and the context-overflow ladder** -- orthogonal; tool-output caps and context monitoring from `production-worker-failures.md` remain worth doing separately.
- **The `blocked_signal` path** -- already the model this doc wants everything to follow: a detected condition producing a structured outcome with evidence.
- **The channel LLM's `cancel` tool** -- humans and channel agents keep an immediate stop; it just stops losing the transcript.

## Phases

**Phase 1 -- outcome outbox.** Persist-then-notify on every termination path, lifecycle CAS for channel-owned workers, cortex reconciliation pass, remove the `worker_handles` completion guard. This alone fixes the orphan and lost-slot classes.

**Phase 2 -- progress liveness.** `WorkerProgress` struct, registry-based stall detection replacing event tracking, per-phase thresholds, dormant-mode lightweight tick, OpenCode workers on the same struct. Wall clock becomes opt-in.

**Phase 3 -- staged termination.** Cancellation token through `run_inner`, `Stalled` outcome variant, grace period, abort backstop with unconditional transcript drain. All kill sites route through it.

**Phase 4 -- budget consolidation.** Single segment budget, tool-nudge reclassification, configurable retry ceilings, unified detached retry counter with backlog exhaustion.

**Phase 5 -- ownership transfer and shutdown drain.** Channel-death adoption into cortex supervision, bounded shutdown drain, reconciliation honesty at boot.

**Phase 6 -- cortex stall inspector.** wait/steer/interrupt on stall flags, fail-open, bounded delay. Last because everything before it must be trustworthy without it.

**Phase 7 -- fork-by-default spawn context.** Default flip to `Full`, mode collapse to `fork` | `clean`, `spawn_worker` description rewrite, pre-flight fork compaction, settings/API/UI surface updated to the two modes. Independent of every phase above -- it touches spawn, not termination -- and can land ahead of them.

**Phase 8 -- reflection transcript access.** Reflection signal carries drained worker ids, persistence prompt lists them and instructs `worker_inspect` on listed workers plus failed predecessors, rule 7 scoped to memory extraction, larger per-result truncation cap for reflection callers. Depends on nothing above; pairs naturally with phase 7 since both correct what crosses the worker boundary.
