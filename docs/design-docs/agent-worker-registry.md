# Agent Worker Registry

Workers belong to an agent. Channels provide spawn provenance and result destinations, but they do not own execution controls.

Spacebot already has one `ProcessControlRegistry` per agent. This design expands it from channel cancellation into the single live control plane for workers. It fixes the ownership split that allowed resident autonomy to see retained workers in SQLite while `route` searched only the autonomy channel's in-memory maps.

Related designs:

- [worker-lifecycle-convergence.md](worker-lifecycle-convergence.md) owns durable lifecycle transitions and terminal convergence.
- [worker-reliability.md](worker-reliability.md) owns terminal outcome persistence and notification ordering.
- [durable-worker-execution.md](durable-worker-execution.md) owns checkpoints and recovery of in-flight execution.
- [autonomy.md](autonomy.md) owns resident autonomy and epoch behavior.
- [autonomy-lifecycle.md](autonomy-lifecycle.md) owns task selection and execution policy.

## Problem

On August 16, 2026, resident autonomy saw two retained OpenCode workers in its heartbeat briefing. Both were durable, nonterminal, and idle. Routing to either worker returned `worker not found`.

The two paths used different scopes:

```text
autonomy heartbeat: agent-wide durable worker query
route tool:         current-channel worker maps
```

The workers belonged to ordinary channels, so those channels held their task handles and input senders. Autonomy could inspect the rows but could not reach the controls.

The same split affects cancellation, status, admission, channel shutdown, and retained-worker restoration. Moving only `route` would leave several competing worker authorities.

## Scope

This design covers live worker ownership:

- agent-scoped worker registration
- spawn admission and start ordering
- idle follow-up routing
- running context injection
- cancellation and terminal cleanup
- live status snapshots
- retained idle worker restoration
- result targeting for interactive follow-ups
- removal of worker controls from `ChannelState`

The registry uses the existing durable worker lifecycle. It does not add a durable operation ledger, result-delivery dispatcher, checkpoint format, task receipt saga, or autonomy no-op circuit.

## Current System

`ProcessControlRegistry` already has agent lifetime through `AgentDeps`. It registers channel control handles with replacement IDs and prunes stale weak handles.

Worker controls currently live in `ChannelState`:

```text
worker_handles
worker_inputs
worker_injections
reserved_tasks
StatusBlock.active_workers
```

`StatusBlock.active_workers` is a presentation structure that `route` currently treats as lifecycle authority. The separate `active_workers` map is vestigial and is not populated reliably.

SQLite already owns durable worker lifecycle, transcripts, terminal outcomes, origin channel IDs, task bindings, and OpenCode session metadata. Existing lifecycle compare-and-swap and terminal convergence remain authoritative.

## Decisions

### Agent ownership

Every runtime-attached worker appears once in the owning agent's registry. A channel ID on a worker row is immutable spawn provenance and the default result destination for the initial operation.

Stopping or archiving an origin channel does not cancel its worker or remove its live controls. Agent shutdown owns worker draining and cancellation. Physical channel deletion is rejected while nonterminal worker rows reference the channel. Deleting a channel may continue removing terminal worker history until the channel foreign-key contract is redesigned separately.

### One live authority

Worker controls cannot exist in both `ChannelState` and `ProcessControlRegistry`. The cutover changes every spawn, route, cancel, status, shutdown, and restoration path together. There is no shadow registry or dual-write period.

### Existing durable authority

SQLite remains authoritative for durable lifecycle and terminal outcomes. The registry is authoritative for runtime attachment, routability, task handles, cancellation senders, interactive inputs, running injections, and live admission.

Broadcast events are notifications. A missing or lagged event cannot create a second live control owner.

### Agent boundary

Each registry is already scoped to one `AgentDeps`. Worker lookup never scans another agent's database or registry.

This cutover does not introduce runtime-issued capability objects. Tool registration and existing channel authority checks remain the authorization boundary. A future shared-agent permission model can add finer worker permissions without changing control ownership.

## Invariants

1. Every runtime-attached worker has exactly one registry entry.
2. A registry entry belongs to one agent and one worker registration ID.
3. Stale cleanup cannot remove a replacement registration.
4. No worker performs task work before its durable row and registry controls exist.
5. A channel exiting cannot make a worker unreachable.
6. Every worker rendered as routable has a live registry control path.
7. A durable nonterminal row without a registry entry is unavailable, not actionable.
8. An idle follow-up records its requester and result target without changing spawn provenance.
9. A result settles only the operation that produced it.
10. Cancellation racing completion preserves the durable outcome that wins lifecycle convergence.
11. Terminal cleanup removes only the matching worker registration.
12. Task reservations are agent-scoped, while configured concurrency quotas preserve their current per-origin-channel behavior.

## Registry

Expand `src/agent/process_control.rs`:

```rust
pub struct ProcessControlRegistry {
    channels: RwLock<HashMap<ChannelId, ChannelControlEntry>>,
    workers: RwLock<HashMap<WorkerId, Arc<LiveWorkerEntry>>>,
    admissions: Mutex<WorkerAdmissions>,
    worker_store: ProcessRunLogger,
    next_channel_registration: AtomicU64,
    next_worker_registration: AtomicU64,
}
```

The worker map lock is held only long enough to clone, insert, or conditionally remove an entry. SQL queries, provider calls, channel sends, task joins, and worker input sends happen after releasing it.

### Live entry

```rust
struct LiveWorkerEntry {
    worker_id: WorkerId,
    registration_id: WorkerRegistrationId,
    provenance: WorkerProvenance,
    backend: WorkerBackend,
    interactive: bool,
    state: RwLock<WorkerRuntimeState>,
    status: RwLock<String>,
    active_operation: RwLock<Option<WorkerOperationContext>>,
    last_completed_operation_id: RwLock<Option<WorkerOperationId>>,
    control: WorkerRuntimeControl,
}

struct WorkerRuntimeControl {
    task_handle: Mutex<Option<JoinHandle<()>>>,
    cancel_tx: watch::Sender<bool>,
    terminal_notify: Arc<Notify>,
    transcript_snapshot: WorkerTranscriptSnapshot,
    opencode_cancellation: Option<OpenCodeCancellationState>,
    input_tx: Option<mpsc::Sender<WorkerFollowUp>>,
    injection_tx: Option<mpsc::Sender<String>>,
}
```

`WorkerProvenance` contains the existing origin channel, origin branch, task, autonomy run, and spawning process information. It is immutable.

`WorkerRegistrationId` is allocated with the admission reservation before worker construction. Workers, hooks, status tools, and OpenCode callbacks receive an immutable `WorkerCallbackContext { worker_id, registration_id }`. The registration ID prevents an old task's cleanup or callback from mutating a newer restored entry. Durable runtime generation fencing remains part of `durable-worker-execution.md` and is not required for this live ownership cutover.

Every callback that mutates live state carries the registration ID. This includes status updates, idle transitions, results, terminal completion, OpenCode session metadata, task-handle cleanup, and registry removal. The registry ignores a callback when its registration ID no longer matches.

Operation callbacks also carry the operation ID. A result or idle transition applies only when it matches `active_operation`. Completing operation A after operation B starts on the same registration cannot change B's state, result target, interaction target, or autonomy child.

### Runtime states

The registry projects the existing runtime states:

```text
Starting
Running
WaitingForInput
Cancelling
Completing
```

Terminal workers are absent from the live registry after their durable terminal outcome commits. Historical inspection comes from SQLite.

The registry snapshot distinguishes:

```text
durable_nonterminal
runtime_attached
routable_idle
routable_running
unavailable
terminal
```

These labels prevent durable existence from being presented as verified liveness.

### API

The registry exposes operations rather than its maps:

```rust
reserve_worker(...)
register_worker(...)
install_task_handle(...)
worker_snapshot(...)
list_worker_snapshots(...)
route_follow_up(...)
inject_context(...)
cancel_worker(...)
update_worker_state(...)
remove_worker_if_registration_matches(...)
close_admission(...)
drain_workers(...)
```

`reserve_worker` returns an ownership token containing the worker ID, registration ID, normalized task reservation, and origin-channel quota slot. Registration consumes that token. Cleanup releases reservations by token or matching registration, never by task text or worker ID alone.

Normal registration rejects an existing worker ID. Restoration also requires the worker ID to be absent from the live map. A restored worker receives a new registration ID only after the previous runtime has detached. The registry never replaces a task that may still execute.

Callers receive structured results:

```text
routed
injected
busy
wait_until_idle
unavailable
terminal
not_found
unauthorized
```

`terminal` includes the durable outcome and is not a retryable failure. `unavailable` means a durable nonterminal worker lacks usable live controls. `not_found` means neither live nor durable state exists for that worker in the current agent.

## Interactive Operations

The registry cutover does not add a durable operation ledger. It still needs an operation identity so a result from an earlier follow-up cannot settle a later one.

```rust
pub struct WorkerOperationId(Uuid);

pub struct WorkerOperationContext {
    pub operation_id: WorkerOperationId,
    pub requester: WorkerOperationRequester,
    pub result_target: WorkerResultTarget,
    pub autonomy_run_id: Option<String>,
}

pub struct WorkerFollowUp {
    pub operation: WorkerOperationContext,
    pub message: String,
}

pub enum WorkerResultTarget {
    Channel { channel_id: ChannelId },
    CortexChat { thread_id: String },
    None,
}
```

Operation IDs are runtime-scoped. They are carried through worker result events and autonomy child tracking. The existing restart behavior remains authoritative for an operation interrupted by process loss.

The initial operation targets the origin channel. An idle follow-up targets the requesting channel unless a system process selects another valid internal target. Routing does not rewrite the worker's origin channel.

OpenCode permission and question events follow the active operation's result target. A channel-targeted operation sends interactions to that channel. A cortex-chat operation sends them to the owning thread. `None` relies only on the existing automatic response policy and emits no interactive destination event.

Running context injection contributes to the current operation. It does not create a new operation or redirect that operation's result. A process that needs an attributable result waits until the worker is idle and submits a follow-up.

## Spawn

Every worker source uses one start sequence:

1. Resolve the backend, task, directory, prompt, and initial result target.
2. Reserve the worker ID, registration ID, initial operation ID, task exclusivity, and origin-channel admission slot.
3. Construct the worker, hook, and tools with the callback context behind a closed start gate.
4. Register all controls in the agent registry as `Starting` and install the task handle.
5. Register the autonomy child when applicable.
6. Persist the worker through the existing `running` lifecycle API.
7. For task-bound work, claim the task attempt and then bind the task pointer at the planned revision.
8. Persist the existing project and worktree links.
9. Mark the registry entry `Running`.
10. Publish `WorkerStarted`.
11. Open the start gate.

The spawn coordinator owns this sequence. `SpawnWorkerTool` passes a prepared task plan into the coordinator instead of binding after a worker has started.

Failure before durable worker persistence settles any autonomy child, removes the matching registry entry, and releases the reservation token without creating a worker row. Failure after worker persistence but before the gate opens commits a terminal failed outcome with a not-started reason. If a task attempt was claimed, it closes as `Interrupted`. If the task pointer was bound, rollback clears it using the revision produced by the successful bind. The worker future is never polled on any failure path.

The cutover covers every current source:

- user channels
- branches
- resident autonomy
- cron channels
- cortex chat
- builtin workers
- OpenCode workers
- retained idle builtin and OpenCode restoration

### Admission

`reserved_tasks` moves into the registry so two channels cannot reserve the same task independently. Concurrency accounting also moves into the registry, but remains bucketed by origin channel and uses the current `max_concurrent_workers` behavior. Agent-global concurrency and separate retained-session capacity are later policy changes.

## Routing

`src/tools/route.rs` becomes a registry client.

| Worker state | Behavior |
|---|---|
| `Starting` | Return `busy` |
| `Running` with injection support | Inject context into the current operation |
| `Running` without injection support | Return `wait_until_idle` |
| `WaitingForInput` | Allocate and send a typed follow-up |
| `Cancelling` or `Completing` | Return `busy` with the transition |
| Durable terminal | Return `terminal` with the outcome |
| Durable nonterminal without controls | Return `unavailable` |
| Missing from registry and SQLite | Return `not_found` |

An idle follow-up:

1. Resolves the worker in the current agent registry.
2. Locks the live entry and verifies `WaitingForInput`.
3. Allocates and installs a `WorkerOperationContext`.
4. Registers the operation as an autonomy child when applicable.
5. Changes live and durable state to `Running` through the existing transition API.
6. Sends the typed follow-up.
7. Returns the worker to `WaitingForInput` when the operation completes, or removes it after terminal convergence.

If the input send fails, the route path settles any newly registered autonomy child and conditionally returns the worker to `WaitingForInput`. It does not modify a worker that has already advanced to another state.

Two concurrent idle follow-ups cannot both claim the same waiting state. The state transition and input send remain a known in-memory boundary until durable operations are implemented. A send accepted immediately before process loss follows current restart semantics and is not blindly replayed.

## Results

Interactive operation results use `WorkerOperationResult`:

```text
worker_id
worker_registration_id
operation_id
result_target
result
```

Interactive worker loops use the current follow-up envelope's result target. They no longer emit every result to the immutable origin channel.

`WorkerComplete` remains the terminal lifecycle event. It carries the registration ID and an optional active operation context. A worker that terminates while executing an operation uses that operation's ID and target so its requester can settle. A worker that terminalizes while idle has no active operation and sends lifecycle presentation to its origin channel without duplicating the previous operation result.

Status, idle, permission, question, OpenCode session, and live transcript events also carry the registration ID. Permission and question events carry the active interaction target. Provenance events may still render against the origin channel, but registry mutation always verifies registration identity.

The destination channel applies the existing result relay and retrigger behavior. This cutover does not claim that an interactive result survives event loss or destination deletion. Durable operation results and destination inboxes are separate reliability work.

The registry records the last completed operation ID while an interactive worker remains attached. Same-process lag recovery may settle the matching autonomy child and direct it to inspect the durable transcript. It never settles a different operation merely because the worker is idle.

Natural completion cleanup belongs to the worker task wrapper, not the destination channel. After the durable terminal transaction commits, the wrapper removes the matching registry entry and releases its admission before publishing `WorkerComplete`. An already-committed terminal outcome follows the same cleanup path. Destination channels present results but never retire controls.

Terminal persistence gets bounded retries in the worker wrapper. If it still fails, the wrapper removes routability and releases admission using the matching registration, then leaves the durable nonterminal row visible as unavailable for reconciliation. A dead task is never left registered as running.

## Cancellation

Worker cancellation resolves the worker directly in the agent registry. Branch cancellation remains channel-owned because branches are channel context forks.

Cancellation follows the existing durable lifecycle contract:

1. Resolve the live entry and registration ID.
2. Claim `Cancelling` through the durable lifecycle compare-and-swap.
3. Send cooperative cancellation.
4. Wait through the configured grace period.
5. Abort the task only as the existing backstop.
6. Commit one terminal outcome.
7. Remove only the matching registry registration.

Cancellation racing completion returns the durable terminal outcome that won. A stale task or cancellation request cannot remove a replacement registration.

`Starting` cancellation is handled before durable lifecycle cancellation. It marks the matching registration cancelling, closes the start gate, and waits for the spawn coordinator to converge. The coordinator checks cancellation before and after durable worker persistence:

- If cancellation wins before persistence, it removes the registration and releases reservations without creating a worker row.
- If persistence wins, the coordinator claims durable cancellation and commits a cancelled terminal outcome without polling the worker future.
- If cancellation arrives after live state becomes `Running` but before the gate opens, the gate remains closed and the same durable cancellation path wins.

The spawn coordinator opens the gate only after a final matching-registration and `Running` state check. Cancellation never removes a `Starting` entry independently while persistence may still be in flight.

## Channel Integration

Remove worker execution authority from `ChannelState`:

```text
worker_handles
worker_inputs
worker_injections
reserved_tasks
active_workers
```

Keep channel-local branches, conversation history, pending result presentation, compaction state, links, and messaging state.

The registry exposes one agent-wide worker snapshot for APIs and autonomy. A channel `StatusBlock` may render an origin-filtered snapshot for prompt presentation, but it is never consulted as worker control authority. APIs do not aggregate worker entries from channel status blocks.

Channel shutdown unregisters the channel control handle but does not touch worker entries. Physical channel deletion returns a conflict while nonterminal worker rows reference the channel. Existing terminal history may still follow the current cascading-delete behavior until that foreign-key contract is redesigned. Agent shutdown closes registry admission, stops channels from spawning, and then drains the worker registry once.

Worker list, detail, inspection, and cancellation APIs resolve an explicit agent ID before consulting the registry or durable store. The current cross-agent worker scan is removed. Branch cancellation remains channel-qualified.

## Autonomy Integration

Resident autonomy reads two worker sets:

- Registry snapshots for live, routable workers.
- Durable nonterminal rows without registry entries for reconciliation visibility.

Only registry-backed workers are actionable. Durable rows without controls render as unavailable and cannot be described as active work that autonomy should tend.

An autonomy follow-up registers `AutonomyChild::WorkerOperation` with the worker and operation IDs. The matching result settles that child. A result from an earlier operation cannot settle the current epoch.

Retained workers from unrelated origin channels do not consume autonomy's origin-channel quota. They prevent task selection only when the task already has a live attempt or agent-scoped task reservation.

This closes the control-plane half of the August 16 incident. Repeated equivalent no-action epochs remain the responsibility of a separate structured autonomy outcome and circuit design.

## Restart

The cutover changes restoration only for worker states already recoverable today.

Idle retained builtin and OpenCode workers are restored directly into the agent registry. Startup no longer creates their origin channels to hold controls. Origin channel IDs remain provenance and default presentation targets.

Restoration uses an agent-level `WorkerRestorationContext` containing the history store, task store, runtime configuration, model and backend services, filesystem paths, logger, event sender, and process registry. It loads the same portal or channel conversation settings used by current restoration without constructing or registering a `Channel`. Builtin workers rebuild their prompt, model override, browser configuration, paths, and transcript from those resolved settings. Persisted backend and session metadata identify OpenCode sessions.

Restoration is exposed behind a narrow worker-runtime factory so registry orchestration tests do not launch a real OpenCode process. A restored runtime returns its controls and gated future. It becomes routable only after registration and session validation succeed.

Restoration does not use the new-worker start sequence. A restored idle worker:

1. Reserves a new registration ID and its existing origin-channel admission slot.
2. Rebuilds and validates the retained runtime behind a closed gate.
3. Registers controls with no active operation and live state `WaitingForInput`.
4. Installs the task handle and publishes the restored snapshot.
5. Opens the gate into the follow-up loop without replaying the initial task or emitting an initial result.

It allocates no operation ID, autonomy child, task attempt, or durable lifecycle transition until a real follow-up claims `WaitingForInput -> Running`.

Current handling of interrupted running workers remains unchanged. Recovering in-flight execution through `Suspended` and `Recovering` belongs to `durable-worker-execution.md`.

If an idle worker row cannot restore its controls, startup applies the existing explicit retirement policy or leaves it unavailable according to the backend's current behavior. It never renders the row as routable.

## Implementation

Each phase remains build-green. The ownership cutover itself is atomic because dual live authorities are unsafe.

### Phase 1: Preparation

- Extract `WorkerRuntimeControl` from channel-specific code without changing ownership.
- Add callback, reservation, registration, operation, requester, and result-target types.
- Replace string follow-up channels with `WorkerFollowUp` for builtin and OpenCode workers.
- Replace `WorkerInitialResult` with operation-addressed result events.
- Add the worker start gate.
- Add the prepared spawn coordinator and pre-start task rollback path.
- Extract the idle worker runtime restoration factory.
- Add registry entry, snapshot, and structured result types behind unused APIs.
- Add registration-fenced insertion, callback, removal, admission, and start-gate tests.

Primary files:

```text
src/agent/process_control.rs
src/agent/channel_dispatch.rs
src/agent/worker.rs
src/opencode/worker.rs
src/hooks/spacebot.rs
src/tools/set_status.rs
src/tools/spawn_worker.rs
src/lib.rs
```

### Phase 2: Registry cutover

- Move every spawn source to registry registration.
- Move task reservations and origin-channel admission buckets into the registry.
- Change route, injection, cancellation, and status reads to registry operations.
- Send interactive results to the operation result target.
- Route OpenCode permission and question events to the active operation target.
- Restore retained idle builtin and OpenCode workers directly into the registry.
- Move agent shutdown and cleanup to the registry.
- Track autonomy children by operation ID and match result events to that operation.
- Render registry-backed routability in autonomy briefings.
- Distinguish unavailable durable workers from actionable workers.
- Route worker inspection and cancellation APIs through an agent-resolved registry.
- Remove the current cross-agent worker cancellation scan.
- Require agent ID in worker control API and CLI requests.
- Switch the interface and API clients to the agent worker snapshot.
- Remove worker controls and worker authority from `ChannelState` in the same phase.
- Remove old channel-local route and cancellation fallbacks.
- Update channel status presentation to use filtered registry snapshots.
- Update worker, autonomy, daemon, API, and command documentation.

Primary files:

```text
src/agent/channel.rs
src/agent/channel_history.rs
src/agent/status.rs
src/agent/autonomy.rs
src/agent/cortex_chat.rs
src/tools/route.rs
src/tools/cancel.rs
src/tools/worker_inspect.rs
src/main.rs
src/api/state.rs
src/api/workers.rs
src/api/channels.rs
src/api/system.rs
src/conversation/channels.rs
src/cli/channel.rs
interface/src/
prompts/en/
docs/content/docs/
```

No database migration is required for this cutover. The existing channel ID remains worker provenance. Durable operation and recovery migrations land with their own designs.

## Verification

### Registry

- Every spawn source creates one registry entry.
- Duplicate worker registration is rejected or replaces only through an explicit restoration path.
- Stale cleanup cannot remove a replacement registration.
- Origin-channel concurrency limits preserve current behavior after moving into the registry.
- Concurrent task reservations admit one worker.
- A worker cannot perform task work before the start gate opens.
- Cancellation before durable start prevents the worker future from being polled.
- Cancellation racing durable start either creates no row or commits a cancelled terminal row.
- Stale status, idle, result, completion, and cleanup callbacks cannot mutate a replacement registration.
- A failed old registration cannot release a replacement's admission or task reservation.
- Terminal persistence failure removes routability and leaves the durable row unavailable.

### Routing

- Resident autonomy routes an idle worker spawned by another channel.
- The follow-up result targets autonomy and settles the matching operation child.
- OpenCode interactions target the operation requester rather than the origin channel.
- A stale result cannot settle a later operation.
- A delayed result from operation A cannot mutate active operation B on the same registration.
- Two concurrent follow-ups claim an idle worker at most once.
- Running OpenCode workers return `wait_until_idle` rather than `not_found`.
- A durable nonterminal row without controls returns `unavailable`.
- A worker ID from another agent returns `not_found`.

### Lifecycle

- Channel shutdown leaves its workers registered and controllable.
- Physical channel deletion is rejected while nonterminal worker rows reference it.
- Agent shutdown cancels or drains each worker once.
- Natural completion retires controls without a destination channel consuming the event.
- Cancellation racing completion commits one terminal outcome.
- Old task cleanup cannot remove a restored worker registration.
- Terminal workers disappear from live snapshots after durable convergence.

### Restoration

- Idle retained builtin and OpenCode workers restore without creating their origin channels.
- Restoration preserves resolved conversation settings and model overrides.
- Restoration starts idle with no active operation and does not replay the initial task.
- A restored worker accepts a follow-up from resident autonomy.
- Failed restoration never renders a worker routable.
- Current interrupted-running-worker behavior remains unchanged.

Run focused tests while the phases land:

```bash
cargo test agent::process_control
cargo test agent::channel_dispatch
cargo test agent::channel
cargo test agent::autonomy
cargo test tools::route
cargo test api::workers
cargo test api::channels
```

Run repository gates before pushing or updating a PR:

```bash
just preflight
just gate-pr
```

## Acceptance Criteria

The registry cutover is complete when:

1. No channel stores worker task handles or routing senders.
2. Every runtime-attached worker appears once in its agent registry.
3. Any same-agent process with the worker control tool can route or cancel through the registry.
4. Worker provenance remains independent from the current operation's result target.
5. Resident autonomy can control retained workers spawned by ordinary channels.
6. Durable but unattached workers are never presented as actionable.
7. Channel shutdown has no worker lifecycle effect.
8. Lifecycle race tests and repository gates pass.

## Non-Goals

- Durable interactive operation storage
- Result delivery leases or destination inboxes
- In-flight worker checkpoint recovery
- Durable runtime generation fencing
- Cross-database task execution receipts
- New worker lifecycle states
- OpenCode one-shot or retention policy changes
- Runtime-issued worker capability objects
- Fine-grained shared-agent worker permissions
- Autonomy no-op fingerprinting and interval suppression
- Ordinary-channel autonomy steering
- Exact-once external messaging delivery
