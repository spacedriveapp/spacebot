# Coding Worker Backends

Spacebot should be able to execute an approved task with a builtin worker, a
local coding agent, or a durable cloud agent. The task remains the source of
truth for what should run. A backend adapter decides how that work reaches a
specific coding agent and how its lifecycle maps back into Spacebot.

The first cloud backend is Capy. OpenCode moves behind the same boundary, and
ACP provides broad local-agent coverage after that. Cursor Cloud, Codex,
Claude, Devin, and GitHub Copilot can then be added without changing task
orchestration or the worker UI contract.

This design complements [worker-reliability.md](worker-reliability.md). That
document owns durable outcomes, staged termination, liveness, and supervision.
This document defines the backend boundary those guarantees operate across.

## Document Status

This is a proposed implementation architecture, not a description of shipped
backend-neutral behavior. The **Current System** section names behavior present
in the repository at review time. Later contracts, schemas, state machines,
adapters, APIs, and rollout phases are requirements for tasks #39-#45 unless a
paragraph explicitly says that a component already exists. OpenCode is the only
current external coding-worker implementation; Capy, ACP, generic profiles, the
backend supervisor, and the generic workbench are not yet shipped.

## Goals

- Execute the same task through builtin, local, and cloud coding workers.
- Make manual spawns, autonomy pickup, wakes, and scheduled execution use one
  task execution path.
- Preserve provider-specific features such as Capy steering without leaking
  provider types through the core worker lifecycle.
- Recover durable cloud workers after a Spacebot restart.
- Render every worker through a normalized transcript, with an optional
  backend-specific embed or external link.
- Add backend profiles without adding another task enum variant.
- Keep task approval, workspace policy, secret handling, and terminal outcomes
  under Spacebot's control.

## Non-goals

- A runtime plugin ABI. Backend adapters ship with Spacebot.
- Feature parity between providers. Capabilities are explicit and callers adapt
  to them.
- Using MCP as a worker protocol. MCP supplies tools and context to workers.
- Reproducing a cloud provider's complete web interface.
- Migrating a live session between providers. Cross-provider continuation uses
  a structured handoff and starts a new worker.
- Replacing external session chronicling. That pipeline imports work started
  outside Spacebot. This design controls workers started by Spacebot.

## Terms

**Backend profile** is a configured worker target such as `opencode-local`,
`capy-spacebot`, or `cursor-team`. It selects an adapter kind, credentials,
policy, and provider-specific settings.

**Adapter kind** is the implementation used by a profile, such as `builtin`,
`opencode`, `acp`, or `capy`.

**Worker** is the Spacebot-owned durable session identified by `WorkerId`. It
may accept multiple prompts over its lifetime.

**Attempt** is one provider busy epoch. It starts when an idle worker receives
work and ends when the provider returns to idle or terminal state. Queued and
steering messages are inputs to an attempt, not independent attempts. An
interrupting message changes the active attempt. A successor starts only after
the worker becomes idle and receives new work.

**Input** is an initial prompt, queued follow-up, steering instruction, or
response sent to a worker. Providers differ in whether input submission is
idempotent.

**Backend session** is the provider-owned session, thread, or agent. OpenCode
calls this a session. Capy calls it a thread. Cursor calls it an agent.

**Execution location** describes where code runs: in-process, a local
directory, a remote host, or a provider-managed workspace.

**Identity domains** are separate identifiers for the Spacebot worker, its
attempt, its input and operation records, the backend profile, and the
provider's session, run, message, and request objects. An identifier is only
meaningful in its own domain. Provider IDs never stand in for `WorkerId`,
`WorkerAttemptId`, or authorization to control another worker.

## Current System

Spacebot already has most of the orchestration pieces, but OpenCode is a
special branch rather than an adapter.

| Concern | Current owner |
|---|---|
| Spawn tool and task-plan resolution | `src/tools/spawn_worker.rs` |
| Builtin and OpenCode dispatch | `src/agent/channel_dispatch.rs` |
| Builtin agent loop | `src/agent/worker.rs` |
| OpenCode session loop | `src/opencode/worker.rs` |
| OpenCode server ownership | `src/opencode/server.rs` |
| Process lifecycle events | `src/lib.rs` |
| Worker persistence | `src/conversation/history.rs` |
| Normalized transcripts | `src/conversation/worker_transcript.rs` |
| Worker API | `src/api/workers.rs` |
| OpenCode presentation | `interface/src/components/OpenCodeEmbed.tsx` |
| Task execution plan | `src/tasks/store.rs` |
| Autonomous task execution | `src/agent/autonomy.rs` via `SpawnWorkerTool` |

The central event model is already mostly backend-neutral. `WorkerStarted`,
`WorkerStatus`, `WorkerIdle`, `WorkerInitialResult`, and `WorkerComplete` can
describe any provider. The remaining OpenCode-specific events, persistence
columns, API fields, and UI branches show where the boundary needs extracting.

The current OpenCode path also exposes concrete trust and lifecycle gaps that
this migration must close rather than preserve:

- `OpenCodeServer` inherits the Spacebot process environment and only overlays
  `OPENCODE_CONFIG_CONTENT` and `OPENCODE_PORT`; local backend launch needs an
  explicit environment allowlist plus profile-selected secret references.
- Permission and question events are emitted, but the worker then grants each
  permission once and chooses the first answer. Observation is not approval;
  requests must remain pending until profile policy or a named human response
  resolves them.
- The UI proxy accepts any loopback port in the deterministic range. A numeric
  range is not endpoint ownership; proxying must resolve a live pooled server
  bound to the requested Spacebot worker and backend generation.
- OpenCode has an `/abort` client and some cancellation paths call it, but the
  backend contract must make provider acknowledgement and subsequent terminal
  evidence part of one staged cancellation operation.
- SSE inactivity is a hard-coded 600-second failure and server HTTP/startup
  timeouts are hard-coded. These become capability-aware liveness and profile
  policy, not transport silence interpreted as death.

Autonomy does not have a Cortex ready-task pickup loop. It starts a direct-tool
autonomy channel, presents ready tasks to that channel, and task-bound spawns
currently reach `SpawnWorkerTool`. The remaining execution correctness gap is
the boundary itself: plan resolution, worker creation, and task binding are
coupled to the tool and the task binding happens after the worker starts. A
shared executor is still required before API, scheduling, or another backend
can create work without duplicating or weakening those rules.

## Design Principles

### Tasks Own Intent

The task stores the objective, approval state, project association, required
skills, execution policy, and selected backend profile. Backend sessions store
execution state. Provider metadata never becomes the source of truth for what
Spacebot intended to run.

### Orchestration Owns Outcomes

Adapters report normalized events and attempt outcomes. They do not update
tasks, release concurrency slots, retrigger channels, or decide retry policy.
The orchestration layer performs those effects after persisting the outcome.

### Capabilities Replace Backend Checks

Callers ask whether a worker supports follow-up, steering, cancellation,
resume, artifacts, or an embedded view. They do not compare the backend ID to
`"opencode"` or `"capy"`.

### Idempotency Is Explicit

Every Spacebot session, attempt, and input has a stable operation ID. An
adapter advertises which provider operations accept caller idempotency keys.
Spacebot retries an ambiguous external request only when that operation is
idempotent or the provider offers a reliable reconciliation query. It never
falls back to another creation path after the provider may have accepted the
first request.

### Events Have Exact Authority

Every driver event carries the Spacebot worker and generation it is reporting
for, plus an attempt ID when it concerns an attempt. The supervisor accepts an
event only when that binding matches the persisted backend profile, external
session, current driver generation, and active attempt where required. A
provider event ID, session ID, port, process ID, or browser connection alone
does not authorize a state transition. Events with an unknown or mismatched
binding are recorded as rejected observations and cannot create, resume,
complete, or control a worker.

### Liveness Follows Capabilities

The supervisor derives liveness from the capability snapshot, not from a
generic "last event" timestamp. Each backend declares the progress evidence it
can prove, such as a durable event cursor, a polling revision, a running tool,
or a provider status transition, and the phase-specific intervals at which
that evidence is expected. Transport reconnects, duplicate events, browser
activity, and status rendering do not prove work is advancing. A backend that
cannot prove progress uses the configured observation deadline and enters the
same staged termination and reconciliation path as a stalled worker.

### Identities Do Not Cross Domains

Spacebot allocates worker, attempt, input, operation, and event identities.
Adapters retain provider identities as opaque, backend-scoped metadata. A
profile ID identifies policy and credentials, not an external session. API
requests and driver commands resolve a Spacebot identity first, then verify
the bound backend identity before acting. This prevents a reused provider
identifier, local port, or stale resume token from targeting the wrong worker.

### Spacebot Owns Canonical State

The provider owns its process or cloud session. Spacebot owns the canonical
worker record, task binding, operation IDs, normalized events, attempt
outcomes, and current state projection. A connected browser is only a view.

### Backend Choice Is Explicit

Spacebot does not silently move work from a cloud backend to a local agent, or
between credentials, after a failure. A fallback chain must be configured on
the task or backend profile. Every fallback creates a new attempt with visible
provenance.

## Lessons From Orca

Orca is useful as a source of orchestration invariants, not as the worker
transport for this design. Its lifecycle reconciliation accepts completion and
heartbeat messages only when the exact active dispatch and assignee pane (or a
legacy exact handle) match. Stale dispatches are ignored, wrong-pane signals
are retained as auditable rejections, and late heartbeats cannot refresh a
newer dispatch. Spacebot applies the same pattern with worker, backend,
generation, external-session, and attempt bindings.

Orca's main runtime also demonstrates why presentation cannot own lifecycle.
PTY generations, terminal panes, recent output, OSC status, hooks, and UI
subscriptions are valuable observations, but redraws and browser activity do
not prove agent progress or authorize completion. The transferable boundary is:

- durable dispatch/session identity authorizes lifecycle changes;
- generation fences reject stale process, stream, and pane observations;
- terminal or provider output is normalized and bounded before persistence;
- reconciliation owns recovery after a transport, renderer, or process dies;
- operator-visible panes remain views over canonical orchestration state.

Spacebot therefore does not introduce a PTY/TUI adapter abstraction into the
core contract. ACP and native local adapters may use stdio, hooks, or a PTY
internally, but they must emit the same authority-bound semantic events. Orca
continues to own terminal surfaces and remote runtime topology; Spacebot owns
approved task intent, backend credentials, worker state, outcomes, and policy.

## Architecture

```text
Channel / branch / autonomy / wake
                 |
                 v
          TaskExecutionService
          - approval and claim
          - execution-plan resolution
          - workspace preparation
          - context briefing
          - admission control
          - task binding
                 |
                 v
            WorkerSupervisor
          - durable lifecycle
          - attempts and outcomes
          - command routing
          - retries and recovery
          - normalized event log
                 |
                 v
          WorkerBackendRegistry
          - profile lookup
          - availability
          - capability snapshot
                 |
                 v
             BackendDriver
        builtin | opencode | acp | capy | ...
                 |
                 v
         local process or cloud API
```

The driver is an actor owned by the worker supervisor. It receives normalized
commands and emits normalized events. Streaming and polling providers expose
the same driver contract. A Capy driver implements event delivery with a poll
loop. An OpenCode driver consumes SSE. The supervisor does not care how an
event arrived.

## Backend Profiles

Tasks should select a profile ID rather than a closed provider enum. A profile
allows multiple accounts, organizations, projects, local commands, and policy
sets for the same adapter kind.

```toml
[[worker_backends]]
id = "opencode-local"
kind = "opencode"
enabled = true

[worker_backends.settings]
command = "opencode"

[[worker_backends]]
id = "capy-spacebot"
kind = "capy"
enabled = true
credential = "capy-service-user"

[worker_backends.settings]
project_id = "project_..."
poll_active_secs = 2
poll_idle_secs = 15
machine_size = "medium"

[[worker_backends]]
id = "claude-acp"
kind = "acp"
enabled = true

[worker_backends.settings]
command = "claude-code-acp"
args = []
```

Credentials are secret-store references. Raw API keys never appear in TOML,
API responses, backend metadata, transcripts, or events.

The registry resolves a profile into:

```rust
pub struct WorkerBackendProfile {
    pub id: WorkerBackendId,
    pub kind: WorkerBackendKind,
    pub enabled: bool,
    pub credential: Option<String>,
    pub settings: serde_json::Value,
    pub policy: WorkerBackendPolicy,
}

pub enum WorkerBackendKind {
    Builtin,
    OpenCode,
    Acp,
    Capy,
}
```

`WorkerBackendKind` is an internal exhaustive dispatch enum. Task and project
records store `WorkerBackendId`, a validated string newtype. Adding another
configured ACP command does not change the schema or public API.

## Capabilities

Capabilities are snapshotted onto the worker at creation. A configuration
reload can affect new workers without changing the contract of an active one.

```rust
pub struct WorkerCapabilities {
    pub event_delivery: EventDelivery,
    pub liveness: LivenessCapabilities,
    pub follow_up: FollowUpCapabilities,
    pub cancellation: CancellationCapabilities,
    pub resume: ResumeCapabilities,
    pub workspace: WorkspaceCapabilities,
    pub artifacts: ArtifactCapabilities,
    pub presentation: PresentationCapabilities,
    pub requests: RequestCapabilities,
    pub idempotency: IdempotencyCapabilities,
}

pub enum EventDelivery {
    Stream,
    Poll,
}

pub struct FollowUpCapabilities {
    pub when_idle: bool,
    pub queue_while_running: bool,
    pub steer_while_running: bool,
    pub interrupt_with_message: bool,
}

pub struct CancellationCapabilities {
    pub cancel_attempt: bool,
    pub close_session: bool,
    pub termination_is_destructive: bool,
}

pub enum ResumeCapabilities {
    None,
    SameHost,
    Durable,
}

pub enum WorkspaceCapabilities {
    None,
    LocalDirectory,
    SingleRepository,
    MultipleRepositories,
    ManagedWorkspace,
}

pub struct IdempotencyCapabilities {
    pub create_session: bool,
    pub submit_input: bool,
    pub cancel_attempt: bool,
    pub close_session: bool,
}

pub struct LivenessCapabilities {
    pub progress_evidence: Vec<ProgressEvidence>,
    pub observation_deadline: Duration,
    pub running_deadline: Duration,
    pub waiting_deadline: Option<Duration>,
}

pub enum ProgressEvidence {
    EventCursor,
    PollRevision,
    ProviderStatusTransition,
    ToolProgress,
}
```

Capabilities describe operations, not quality. `event_delivery: Poll` is not a
degraded stream. It tells the supervisor how reconnect and freshness work.

## Shared Task Execution

`TaskExecutionService` becomes the only route from an approved task to a
worker. `SpawnWorkerTool`, autonomy task execution, scheduled wakes, and API
requests call the same service.

```rust
pub struct WorkerSpawnRequest {
    pub owner: WorkerOwner,
    pub task_number: Option<i64>,
    pub task: String,
    pub interactive: bool,
    pub backend_id: Option<WorkerBackendId>,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub suggested_skills: Vec<String>,
}

pub struct ResolvedWorkerSpec {
    pub owner: WorkerOwner,
    pub task_number: Option<i64>,
    pub task: String,
    pub backend: WorkerBackendProfile,
    pub workspace: WorkerWorkspace,
    pub context: WorkerBriefing,
    pub required_skills: Vec<String>,
    pub suggested_skills: Vec<String>,
    pub interactive: bool,
    pub operation_id: String,
}
```

Resolution order for a task-bound spawn:

1. Validate that the task is `ready` or already `in_progress` for this worker.
2. Merge the task execution plan over project defaults.
3. Resolve `backend_id` from the task, project, then agent default.
4. Probe backend availability and validate required capabilities.
5. Resolve or provision the workspace.
6. Validate required skills.
7. Build and persist the worker briefing.
8. In one global-database transaction, claim the task, reserve admission, and
   create a `task_execution` row with predetermined worker, attempt, and
   operation IDs.
9. Create the per-agent worker record idempotently from that execution row.
10. Create or resume the provider session with the stable operation ID.

If a failure occurs before provider creation, reservations roll back. If
provider creation may have succeeded, the worker remains in `starting` and
reconciliation repeats only the provider's documented idempotent operation or
uses a provider lookup that proves what happened. It must not start a
replacement blindly.

Tasks and projects live in the global database while `worker_runs` lives in an
agent database. `task_executions` is therefore the global coordination record.
It makes the claim and task-to-worker binding atomic before an external side
effect. A per-agent outcome outbox applies terminal effects to the global
execution and task idempotently. Reconciliation repairs either side after a
crash. Moving all worker history into the global database is unnecessary.

The current `TaskWorkerType` becomes `worker_backend_id`. Spacebot reserves
the built-in profile IDs `builtin-default` and `opencode-default`, so SQL can
backfill task values deterministically without reading mutable configuration.
`worker_runs.worker_type` currently mixes backend and spawn-origin values such
as `task`. Its replacement splits `backend_id` from `spawn_origin` and uses a
versioned post-migration repair for historical rows that cannot be classified
by SQL. Known executor values map to reserved profiles. Origin-only values map
to `spawn_origin` and `builtin-default` when their persisted metadata proves a
builtin worker. Unknown terminal rows are retained as non-resumable historical
records. Unknown non-terminal rows are failed during the migration with their
transcript preserved. Before the supervisor starts, every non-terminal worker
must reference an enabled backend profile. Historical migrations remain
unchanged.

## Backend Selection

Backend selection uses this precedence:

1. Task `worker_backend_id`.
2. Project execution default.
3. Explicit spawn argument for work not attached to a task.
4. Agent `default_worker_backend_id`.

Autonomous execution requires a resolved backend. The autonomy channel cannot
leave the choice to a later turn or an implicit fallback. A missing or
unavailable backend moves the claimed task back to `ready` with a structured
execution error and a bounded retry time. It does not execute through builtin
as an implicit fallback.

Future capability routing can select from an explicit task policy such as
`["capy-spacebot", "opencode-local"]`. The attempt history records each
selection and why the prior backend was abandoned.

## Workspace Model

Local and cloud workers do not share one path model.

```rust
pub enum WorkerWorkspace {
    None,
    Local {
        directory: PathBuf,
        project_id: Option<String>,
        repo_id: Option<String>,
        worktree_id: Option<String>,
    },
    Remote {
        project_id: Option<String>,
        binding_id: String,
        repositories: Vec<RepositoryReference>,
    },
}
```

Local profiles resolve Spacebot project roots and worktrees as they do today.
Cloud profiles use a project binding. The binding maps a Spacebot project to a
provider project or environment and records which repositories are available
there.

```text
Spacebot project: spacebot
Backend profile: capy-spacebot
Provider project: project_123
Repositories:
  github.com/spacedriveapp/spacebot -> core
  github.com/spacedriveapp/spacebot-web -> web
```

A cloud adapter validates that the task's requested repositories exist in the
binding before creating a session. `worktree_mode: create` is a local checkout
policy and is rejected for managed cloud workspaces unless that adapter
explicitly defines an equivalent branch operation.

One-active-worker-per-directory remains a local profile policy, not a global
worker rule. Cloud providers enforce their own workspace concurrency.

## Worker Briefing

Every backend receives the same normalized task briefing. The rendering may
change for a provider, but the source fields and persisted hash do not.

```rust
pub struct WorkerBriefing {
    pub objective: String,
    pub task: Option<TaskBriefing>,
    pub project: Option<ProjectBriefing>,
    pub repositories: Vec<RepositoryReference>,
    pub task_comments: Vec<TaskCommentBriefing>,
    pub required_skills: Vec<ResolvedSkill>,
    pub suggested_skills: Vec<String>,
    pub memory_context: Option<String>,
    pub conversation_context: WorkerConversationContext,
    pub delivery_policy: ContextDeliveryPolicy,
}
```

The briefing fixes two problems. Autonomous pickup receives the same task
context as manual spawning, and cloud adapters do not need Rig history types.
A channel-started builtin worker may receive a forked channel history only when
its explicit delivery policy selects `channel_fork`; it is not the universal
backend default. Detached, scheduled, local-agent, and cloud execution all need
the same bounded briefing to be reproducible without a live channel. Cloud
workers receive only rendered text and attachments allowed by the profile's
context-delivery policy.

This decision supersedes the fork-by-default proposal in section 8 and rollout
phase 7 of `worker-reliability.md`. That document remains authoritative for
outcomes, liveness, staged termination, and bounded transcript recovery; task
#30 owns the bounded briefing contract used here. Full channel history remains
an explicit local policy, never an implicit cloud fallback.

Cloud profiles default to task, project, task comments, and explicitly
resolved memory. Sending a raw channel transcript to a third party requires an
explicit profile policy. Secret scrubbing runs before the rendered briefing
leaves Spacebot.

Cross-provider continuation creates a new briefing with a structured handoff:

```text
objective
current state
decisions
changed files
verification performed
remaining work
known failures
artifacts and pull requests
```

The handoff is persisted as provenance on the new worker. A provider-native
resume token is never passed to a different provider.

## Driver Contract

The registry uses enum dispatch over built-in adapters. This avoids
`async_trait`, keeps adapter construction exhaustive, and does not require a
plugin framework.

Each active driver receives commands:

```rust
pub enum WorkerCommand {
    Submit {
        attempt_id: WorkerAttemptId,
        input_id: WorkerInputId,
        operation_id: String,
        prompt: String,
        delivery: FollowUpDelivery,
    },
    RespondToRequest {
        request_id: String,
        response: WorkerRequestResponse,
    },
    CancelAttempt {
        attempt_id: WorkerAttemptId,
        operation_id: String,
    },
    Close {
        operation_id: String,
    },
}
```

Drivers emit events in an authority envelope:

```rust
pub struct WorkerBackendEventEnvelope {
    pub worker_id: WorkerId,
    pub backend_id: WorkerBackendId,
    pub generation: u64,
    pub observation: WorkerObservation,
    pub event: WorkerBackendEvent,
}

pub enum WorkerBackendEvent {
    SessionCreated {
        external_id: String,
        resume_state: serde_json::Value,
        presentation: WorkerPresentation,
    },
    AttemptStarted {
        attempt_id: WorkerAttemptId,
        external_run_id: Option<String>,
    },
    StatusChanged {
        attempt_id: Option<WorkerAttemptId>,
        status: WorkerExecutionStatus,
        detail: Option<String>,
    },
    TranscriptStep {
        attempt_id: WorkerAttemptId,
        step: TranscriptStep,
    },
    Request {
        attempt_id: WorkerAttemptId,
        request: WorkerRequest,
    },
    ArtifactPublished {
        attempt_id: WorkerAttemptId,
        artifact: WorkerArtifact,
    },
    AttemptCompleted {
        attempt_id: WorkerAttemptId,
        outcome: WorkerAttemptOutcome,
    },
    SessionClosed,
}

pub struct WorkerObservation {
    pub source: WorkerObservationSource,
    pub provider_event_key: Option<String>,
    pub observed_at: String,
    pub source_metadata: serde_json::Value,
}

pub enum WorkerObservationSource {
    Stream,
    Poll { revision: String },
    Reconciliation,
}
```

The envelope binds an event to its backend profile and driver generation.
`WorkerObservation` records its provider event key or poll revision, observed
time, and bounded source metadata. It answers where a status came from without
making that source authoritative by itself. The current materialized status
stores the observation that produced it. Transcript, request, artifact, and
terminal events carry equivalent provenance.

Provider payloads are normalized inside the adapter. Core events do not carry
`OpenCodePart`, Capy message types, or Cursor tool payloads. Adapters may store
bounded opaque source metadata for diagnostics, but orchestration and the UI
do not depend on it.

## Lifecycle

The worker session and its attempts have separate state machines.

### Worker Session States

```text
starting -> active -> idle -> active
    |         |        |
    |         +------> closing -> closed
    |                     |
    +---------------------+
                          -> failed
```

`closed` and `failed` are terminal. Closing a terminal worker is idempotent.
An attempt can complete while the worker returns to `idle`.

### Attempt States

```text
queued -> submitting -> running -> waiting -> running
             |            |          |
             |            +------> cancelling
             |                         |
             +-------------------------+
                                       v
               succeeded | partial | blocked | stalled | cancelled | failed
```

The attempt lifecycle state and terminal outcome are separate columns. Every
attempt reaches terminal lifecycle state exactly once and stores one outcome
from the taxonomy in [worker-reliability.md](worker-reliability.md): success,
partial, blocked, stalled, cancelled, or failed. Provider statuses such as
`ready_for_review`, `pending_user`, or `archived` map into lifecycle state and
provider detail without becoming core enum variants.

Allowed transitions are checked with `can_transition_to()`. The database
transition uses a compare-and-swap condition on current state and generation.
Duplicate provider events can update the event cursor but cannot apply task
completion, release admission capacity, or notify the channel twice.

### Canonical Status Mapping

| Canonical status | Meaning |
|---|---|
| `queued` | Accepted by Spacebot, not submitted |
| `submitting` | Provider creation or prompt request in flight |
| `running` | Provider is actively working |
| `waiting` | Waiting for input, approval, capacity, or an external system |
| `cancelling` | Cancellation accepted by Spacebot, terminal result pending |
| `succeeded` | Attempt completed with a usable result |
| `partial` | Attempt ended with usable incomplete work |
| `blocked` | External input or access prevents progress |
| `stalled` | Staged termination ended work that stopped advancing |
| `cancelled` | Cancellation won the terminal transition |
| `failed` | Attempt cannot proceed and has a structured error |

Provider detail remains visible as a label. For example, Capy's
`ready_for_review` maps to `succeeded` and remains available as provider status
and artifact provenance.

## Persistence

Task execution coordination lives in the global database beside tasks:

```sql
CREATE TABLE task_executions (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    worker_id TEXT NOT NULL UNIQUE,
    attempt_id TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    backend_id TEXT NOT NULL,
    status TEXT NOT NULL,
    outcome TEXT,
    error TEXT,
    reservation TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX active_task_execution
    ON task_executions(task_id)
    WHERE status IN (
        'claimed', 'starting', 'running', 'cancelling', 'provider_unknown'
    );
```

The transaction that creates this row also moves the task from `ready` to
`in_progress`. Before provider creation, failures may atomically remove the
execution and return the task to `ready`. After an ambiguous provider request,
the execution remains `starting` for reconciliation.

Tasks with non-terminal executions cannot be deleted. Task deletion becomes
archival or soft deletion once execution history exists, so a live cloud
session cannot lose its coordination record.

The existing `worker_runs` table remains the worker-session record to avoid a
destructive rename. Its Rust type should become `WorkerRecord` as code moves
behind the new boundary.

New worker columns:

```sql
ALTER TABLE worker_runs ADD COLUMN backend_id TEXT;
ALTER TABLE worker_runs ADD COLUMN spawn_origin TEXT;
ALTER TABLE worker_runs ADD COLUMN external_session_id TEXT;
ALTER TABLE worker_runs ADD COLUMN backend_state TEXT;
ALTER TABLE worker_runs ADD COLUMN capabilities TEXT;
ALTER TABLE worker_runs ADD COLUMN presentation TEXT;
ALTER TABLE worker_runs ADD COLUMN operation_id TEXT;
ALTER TABLE worker_runs ADD COLUMN generation INTEGER NOT NULL DEFAULT 1;
ALTER TABLE worker_runs ADD COLUMN event_cursor TEXT;
CREATE UNIQUE INDEX worker_runs_operation_id
    ON worker_runs(operation_id) WHERE operation_id IS NOT NULL;
CREATE UNIQUE INDEX worker_runs_backend_session
    ON worker_runs(backend_id, external_session_id)
    WHERE external_session_id IS NOT NULL;
```

`backend_state`, `capabilities`, and `presentation` are versioned JSON owned by
their respective type. `backend_state` may contain provider project IDs and
resume cursors. It must not contain credentials.

Attempts use a separate table:

```sql
CREATE TABLE worker_attempts (
    id TEXT PRIMARY KEY,
    worker_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    external_run_id TEXT,
    delivery TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL,
    outcome_kind TEXT,
    outcome_payload TEXT,
    result TEXT,
    error TEXT,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (worker_id) REFERENCES worker_runs(id) ON DELETE CASCADE,
    UNIQUE (worker_id, sequence)
);
```

Application validation and database checks require `outcome_kind` and
`outcome_payload` for terminal lifecycle states and prohibit them for
non-terminal states.

Every externally visible create, submit, cancel, close, and request response
also has an operation-ledger row:

```sql
CREATE TABLE worker_operations (
    id TEXT PRIMARY KEY,
    worker_id TEXT NOT NULL,
    attempt_id TEXT,
    kind TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    state TEXT NOT NULL,
    external_receipt TEXT,
    reconciliation_evidence TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT,
    FOREIGN KEY (worker_id) REFERENCES worker_runs(id) ON DELETE RESTRICT,
    FOREIGN KEY (attempt_id) REFERENCES worker_attempts(id) ON DELETE RESTRICT,
    UNIQUE (worker_id, id)
);
```

The ledger records `prepared`, `submitted`, `confirmed`, `ambiguous`, and
`rejected` states. An operation ID can be retried only with its original kind,
worker and attempt binding, and request hash. A changed request is a new
operation. An ambiguous operation fences replacement creation, follow-up, and
terminal effects until an idempotent provider retry or reconciliation proves
the original result. Operation rows and their evidence remain available for at
least the retention lifetime of the worker, attempt, task execution, and any
resume metadata they fence. They are never TTL-pruned while ambiguous or while
the worker is non-terminal.

Inputs are recorded separately because several inputs may affect one active
attempt:

```sql
CREATE TABLE worker_inputs (
    id TEXT PRIMARY KEY,
    worker_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    delivery TEXT NOT NULL,
    status TEXT NOT NULL,
    external_input_id TEXT,
    submitted_at TEXT,
    FOREIGN KEY (worker_id) REFERENCES worker_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (attempt_id) REFERENCES worker_attempts(id) ON DELETE CASCADE
);
```

Normalized events use an append-only table:

```sql
CREATE TABLE worker_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    worker_id TEXT NOT NULL,
    attempt_id TEXT,
    generation INTEGER NOT NULL,
    sequence INTEGER NOT NULL,
    provider_event_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    FOREIGN KEY (worker_id) REFERENCES worker_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (attempt_id) REFERENCES worker_attempts(id) ON DELETE CASCADE,
    UNIQUE (worker_id, generation, sequence),
    UNIQUE (worker_id, provider_event_key)
);
```

Every driver delivery receives a durable ingestion disposition before it can
affect materialized state:

```sql
CREATE TABLE worker_event_receipts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    worker_id TEXT NOT NULL,
    attempt_id TEXT,
    generation INTEGER NOT NULL,
    provider_event_key TEXT,
    disposition TEXT NOT NULL,
    reason TEXT,
    observed_at TEXT NOT NULL,
    FOREIGN KEY (worker_id) REFERENCES worker_runs(id) ON DELETE CASCADE
);
```

`accepted`, `duplicate`, `stale_generation`, `unknown_binding`,
`attempt_mismatch`, and `terminal_conflict` are distinct dispositions. The
supervisor records stale and rejected deliveries without applying their state
change. Receipts are bounded diagnostic records, but retain the latest
disposition for every provider event key through the worker's retention
lifetime. This makes ignored events auditable without turning the event stream
into an authority source.

An adapter derives `provider_event_key` from a stable provider ID or stable
semantic identity. When a provider cannot supply either, the adapter reconciles
the latest snapshot and emits only newly derived state transitions. It does not
replay unkeyed append-only events across generations.

Events are bounded semantic records: finalized assistant text, tool summaries,
requests, artifacts, statuses, and terminal outcomes. Token deltas and raw
terminal bytes remain ephemeral. Completion still writes the compressed
transcript projection used by the current worker detail API.

The terminal transition is one per-agent database transaction:

1. Compare-and-swap the attempt into terminal lifecycle state.
2. Write the outcome, terminal event, transcript projection, cursor, and usage.
3. Append an outcome-outbox row for the global `task_execution` when bound.

A losing terminal contender writes nothing. After commit, the supervisor
publishes `ProcessEvent` and API invalidation events, releases local admission,
and drains the outbox into the global database. The global outbox consumer
compare-and-swaps `task_execution` and applies the task transition in one
transaction. Duplicate delivery cannot complete or requeue a task twice.

The database is authoritative. Broadcast events remain live-view hints.

## Recovery

On startup, the worker supervisor loads non-terminal workers and asks the
profile's adapter to restore them from `external_session_id`, `backend_state`,
and `event_cursor`.

An instance-level execution reconciler also scans every non-terminal global
`task_execution`. It recreates missing per-agent worker and attempt rows using
their predetermined IDs. It may roll an execution back only when its persisted
creation phase proves that no provider request began. `starting`,
`cancelling`, and `provider_unknown` executions remain owned and reserved until
adapter reconciliation supplies evidence. This scan runs at startup and
periodically, independent of whether the owning agent is active.

Recovery outcomes:

| Result | Action |
|---|---|
| Session exists and is active | Increment generation, reconnect, replay from cursor |
| Session exists and is idle | Restore worker as idle |
| Provider reports terminal attempt | Persist the missing terminal event and reconcile |
| Provider is temporarily unavailable | Keep worker non-terminal and retry with backoff |
| Provider proves session is gone | Fail the active attempt with `session_not_found` |
| Profile or credential is missing | Mark worker blocked for operator action |

Every driver incarnation receives a monotonically increasing generation.
Events from an older generation receive a `stale_generation` ingestion
disposition after a reconnect. Stable provider event or message IDs deduplicate
replay across generations. A provider cursor commits in the same transaction as
the accepted normalized events it covers.

Local same-host adapters may fail recovery when their process disappeared.
Durable cloud adapters recover independently of the original Spacebot process.

## Cancellation And Races

Cancellation is a request with its own operation ID, not a `JoinHandle::abort()`
call.

The supervisor first transitions the attempt to `cancelling`, then asks the
adapter to cancel the provider operation. The driver continues observing the
provider until it receives terminal evidence. The staged-termination grace
period may restart a local driver or transition a cloud execution to the
non-terminal `provider_unknown` state. That state keeps the task blocked,
admission reservation held, workspace fenced, and external metadata available
for reconciliation. It survives Spacebot restarts. A reliability outcome is
written only after provider termination is proven. An operator can suppress
automatic reconciliation, but cannot mark unknown external work as stopped.

The main race windows converge as follows:

| Race | Convergence rule |
|---|---|
| Completion races cancellation | First terminal compare-and-swap wins |
| Duplicate terminal event | Unique event key and terminal CAS make it inert |
| Timeout races completion | Timeout requests cancellation; it does not write a terminal state directly |
| Reconnect races an old stream | Generation fencing rejects stale events |
| Create response is lost | Retry with the same operation ID and reconcile |
| Follow-up retries after ambiguous response | Retry only when the adapter advertises idempotent input or can reconcile the receipt |
| Worker closes while follow-up queues | Close rejects unsubmitted attempts and cancels submitted ones |
| Task is claimed twice | Existing transactional task claim and duplicate-task reservation select one owner |

OpenCode cancellation must call `/session/{id}/abort` before the local driver
is aborted. Directory claims are released by supervisor-owned cleanup on every
terminal path. A resumed OpenCode session must reacquire the same claim.

Capy interruption stops the active generation but does not delete the durable
thread. Closing a Capy worker archives it through the documented archive
endpoint and retains the thread reference for explicit resume.

## Requests And Human Input

Permissions, questions, approvals, and authentication blockers use one core
request model.

```rust
pub struct WorkerRequest {
    pub id: String,
    pub kind: WorkerRequestKind,
    pub prompt: String,
    pub options: Vec<WorkerRequestOption>,
    pub allows_free_text: bool,
    pub expires_at: Option<String>,
}

pub enum WorkerRequestKind {
    Permission,
    Question,
    Approval,
    Authentication,
}
```

The profile defines an approval policy: require human input, allow a bounded
safe default, or deny. Today OpenCode emits permission and question events,
then automatically grants each permission once and selects the first answer.
Its adapter migration replaces that behavior with the profile policy. A request
awaiting a human maps the attempt to `waiting` and appears in channel, worker,
and Portal views.

Adapters advertise structured requests only when the provider supplies a
stable request ID and response contract. A provider status that merely says it
needs the user maps to `waiting` with provider detail. The user responds
through ordinary follow-up input.

`route` responds according to capabilities. For Capy it may queue, steer, or
interrupt. For OpenCode it sends a follow-up when idle. For a provider without
follow-up support it returns a structured error and offers a new worker with a
handoff.

## Artifacts

Artifacts use a provider-neutral descriptor:

```rust
pub enum WorkerArtifactKind {
    File,
    Diff,
    Branch,
    PullRequest,
    Log,
    ExternalLink,
}

pub struct WorkerArtifact {
    pub id: String,
    pub kind: WorkerArtifactKind,
    pub label: String,
    pub url: Option<String>,
    pub repository: Option<RepositoryReference>,
    pub metadata: serde_json::Value,
}
```

The descriptor does not promise that Spacebot can download the artifact.
Capabilities state whether an artifact can be listed, fetched, or only opened
externally. PR URLs and branch metadata become first-class task outcome data
instead of being parsed only from final prose.

## Presentation

The worker API replaces OpenCode-specific top-level fields with a presentation
descriptor.

```rust
pub enum WorkerPresentation {
    Transcript,
    Embedded {
        kind: String,
        endpoint: String,
        session: String,
    },
    External {
        url: String,
    },
}
```

The frontend workbench selects a renderer:

```text
WorkerWorkbench
  -> NormalizedTranscriptRenderer
  -> OpenCodeEmbedRenderer
  -> TerminalRenderer
  -> ExternalWorkerRenderer
```

Every worker has the normalized transcript renderer. Specialized presentation
is an additional tab when available. The workbench stops filtering for
`worker_type === "opencode"`, and task badges show the backend profile's label
rather than applying provider-specific colors in shared components.

The OpenCode proxy only accepts server instances registered in the active
OpenCode pool. A numeric localhost port is not sufficient authorization.

## Capy Adapter

Capy is the first cloud adapter because its public API supports durable
threads, multi-repository projects, service users, follow-up delivery modes,
and PR-oriented work.

### Profile Settings

```rust
pub struct CapyBackendSettings {
    pub project_id: String,
    pub model: Option<String>,
    pub machine_size: Option<String>,
    pub poll_active: Duration,
    pub poll_idle: Duration,
}
```

The profile credential references a Capy service-user API key. The service user
should have a project allowlist and spend cap. The adapter validates the
project during availability probing because the public API does not list or
create projects.

### Creation

`POST /api/v1/threads` receives:

- `requestId` from the Spacebot worker operation ID.
- `projectId` from the backend profile or project binding.
- The rendered worker briefing as the initial message.
- Optional model and machine size from the resolved profile.

The returned thread ID becomes `external_session_id`. The adapter stores no
credential in `backend_state`.

Before sending, Spacebot persists the complete create request and a canonical
request hash beside `requestId`. If the response is lost, reconciliation
reissues the same `POST /api/v1/threads` request with that `requestId`. Capy's
idempotent create returns the canonical thread. A retry with a different
request hash is rejected locally.

### Event Delivery

The public Capy API does not document an SSE stream or outgoing completion
webhook. The driver polls thread status and incrementally requests messages
using the message cursor.

Polling policy:

- Poll quickly while the thread is active.
- Back off while it is waiting or idle.
- Poll immediately after Spacebot submits a follow-up.
- Persist the message cursor after normalized events commit.
- Add jitter so many cloud workers do not synchronize requests.
- Honor provider rate-limit headers and bounded exponential backoff.

Repeated messages are deduplicated by provider message ID before conversion to
`TranscriptStep`.

### Follow-up

Capy exposes three useful delivery modes:

| Capy delivery | Spacebot operation |
|---|---|
| `queue` | Add context for after current work |
| `steer` | Redirect current work without discarding the thread |
| `interrupt` | Interrupt current generation and apply the new instruction |

The `route` tool accepts an optional delivery mode when the worker advertises
more than one. The channel can inspect capabilities before choosing. The UI
offers the same controls.

Capy's message endpoint returns a provider-generated admission receipt but does
not accept a caller idempotency key. `submit_input` is therefore false for the
Capy profile. Spacebot records the receipt when a response arrives, but it does
not automatically retry a message request whose response was lost. That input
becomes `delivery_unknown` and requires reconciliation from subsequent
messages or an explicit operator retry.

### Status Mapping

| Capy status | Canonical state |
|---|---|
| `active` | `running` |
| `waiting` | `waiting` |
| `pending_user` | `waiting`; respond through ordinary message input |
| `ready_for_review` | `succeeded` with review-ready detail |
| `idle` | Current attempt completed; worker becomes idle |
| `error` | `failed` |
| `archived` | Worker closed after any active attempt is reconciled |

An idle thread can accept another attempt. A message to an archived thread may
unarchive it according to Capy's API, but Spacebot only does so through an
explicit resume action.

### Cancellation

`POST /threads/{id}/interrupt` cancels the active generation. It does not
destroy the thread. Queued messages are cancelled individually when their IDs
are known. `POST /threads/{id}/archive` provides a reversible, non-destructive
session close, and the documented unarchive endpoint restores it. The adapter
advertises `cancel_attempt: true`, `close_session: true`, and
`termination_is_destructive: false`.

### Artifacts And UI

Capy tracks PRs and review activity, but the public API does not document a
general diff endpoint, artifact download contract, stable thread URL, or
embeddable UI. The first adapter records PR and external links returned in
messages. Presentation remains the normalized transcript until Capy returns a
documented thread URL.

## OpenCode Adapter Migration

OpenCode remains a native adapter rather than being reduced to ACP. Its HTTP
API provides session diffs, server-side messages, SSE, abort, and the existing
embedded interface.

Migration steps:

1. Move session creation, SSE conversion, follow-up, transcript fetch, and
   abort behind the driver command/event contract.
2. Replace `OpenCodeSessionCreated` with generic session metadata and
   presentation events.
3. Replace `OpenCodePartUpdated` with normalized transcript steps.
4. Move `opencode_port` and `opencode_session_id` into versioned backend state
   and presentation metadata.
5. Route cancellation through the OpenCode abort endpoint.
6. Make directory claims supervisor-owned and safe across failure and resume.
7. Restrict the UI proxy to pooled server identities and verify the worker,
   backend, generation, loopback address, and live server binding on every
   request; a caller-supplied port or allowed numeric range is insufficient.
8. Launch OpenCode from an environment allowlist with only profile-approved
   variables and secret references.
9. Wire configured startup timeout, restart policy, model selection, and
   liveness evidence into the adapter rather than hard-coded constants.

The external behavior should remain unchanged during extraction. OpenCode
continues to provide the embedded UI after the generic workbench lands.

## ACP Adapter

ACP is the broad local coding-agent adapter. It covers agents that implement
the protocol without teaching Spacebot their terminal output or local session
file format.

The adapter performs:

1. Spawn the configured command over newline-delimited JSON-RPC on stdio.
2. `initialize` and record negotiated capabilities.
3. Authenticate when the agent advertises an authentication method.
4. Create or load a session for the resolved local directory.
5. Submit prompts and convert `session/update` notifications.
6. Route permission requests through `WorkerRequest`.
7. Send `session/cancel` for staged cancellation.
8. Persist session identity only when `session/load` is supported.

OpenCode, Cursor CLI, Devin CLI, and GitHub Copilot CLI currently expose ACP
implementations. Provider extensions stay inside the profile's adapter state.
ACP does not define cloud project creation, PR discovery, or durable artifact
storage, so native cloud adapters still add value.

## Other Backends

### Cursor Cloud

Cursor is a strong second cloud implementation. Its durable agent/run split,
resumable SSE, explicit run cancellation, artifacts, PR metadata, service
accounts, and multi-repository environments map directly onto this design.

### Codex

Codex app-server is a rich local JSON-RPC adapter with threads, turns, steering,
interrupts, resume, and file-change events. Its schema is version-coupled, so
Spacebot should generate or vendor types for the supported installed version.
Codex cloud remains separate until it exposes a stable general task API.

### Claude

Claude Agent SDK is suitable for a local adapter with streaming, cancellation,
resume, hooks, and checkpoints. The SDK is first-party TypeScript and Python,
so Rust integration either runs a small bridge or uses the documented
headless CLI protocol. Managed Agents is the cleaner future cloud target.

### Devin

Devin exposes durable cloud sessions, follow-up messages, attachments, PRs,
structured output, and explicit termination. Its status feed is polling-based.
Termination is destructive and must be shown as such through capabilities.

### GitHub Copilot

Copilot cloud agent is initially a fire-and-observe GitHub task adapter. Its
public task API lacks programmatic steering and cancellation parity. Copilot
CLI can be supported sooner through ACP.

## Scheduling

Scheduled execution is a task concern, not a backend timer. An instance-level
durable timer runs independently of Cortex mode, so dormant agents still start
due work. A scheduled task stores `not_before` and a schedule reference.
`claim_next_ready` filters out future tasks.

```text
task approved with not_before
  -> global schedule occurrence persisted
  -> instance timer claims the occurrence
  -> transactional claim of the referenced task
  -> TaskExecutionService resolves backend and context
  -> local or cloud driver starts
```

The scheduler does not reserve a process or cloud machine ahead of time. It
reserves intent. Admission, availability, workspace validation, and spending
policy are checked when execution starts.

A scheduled task whose backend is unavailable remains `ready`, records the
structured failure, and schedules a bounded retry wake. It does not miss its
execution silently. Recurring schedules create a new task occurrence rather
than reopening a completed worker attempt.

Each occurrence has a unique `(schedule_id, scheduled_for)` key. The timer
claims that exact occurrence and task number rather than waking a generic
highest-priority pickup. Admission may delay a due occurrence, but another task
cannot consume its wake. Occurrence state is `pending`, `claimed`, `started`,
or `failed`, with stale-claim recovery following the existing cron cursor
pattern.

## Admission And Cost Policy

Local and cloud capacity use separate limits:

- Agent-wide active worker limit.
- Per-backend active worker limit.
- Per-local-workspace exclusivity where configured.
- Per-cloud-profile concurrency limit.
- Per-cloud-profile spend ceiling when the provider exposes cost controls.

Admission reservations are persisted for external creation because a process
restart must not forget that a cloud session may already exist. Reconciliation
releases stale reservations only after proving the provider did not accept the
operation.

## Security

Cloud execution crosses a stronger trust boundary than a local subprocess.

- Backend credentials live in the encrypted secret store and profiles store
  references, never raw credential values.
- Local subprocesses start from an explicit environment allowlist. They do not
  inherit the Spacebot daemon environment; only profile-approved variables and
  resolved secret references are injected.
- Profiles restrict projects and repositories that may be sent to a provider.
- Cloud context delivery defaults to task-scoped briefing rather than raw
  channel history.
- Secret-pattern scanning runs before outbound prompts and attachments.
- Provider responses pass through the existing leak scanner before delivery.
- Backend state and diagnostic events redact authorization headers and tokens.
- External URLs are treated as untrusted and rendered without automatic fetch.
- Destructive cancellation or workspace deletion requires an explicit
  capability and policy.
- Backend APIs use bounded response bodies, deadlines, and rate-limit handling.
- Local UI proxies verify an active backend-owned endpoint instead of trusting
  a caller-supplied localhost port.

MCP servers available inside cloud agents are configured through the backend
profile. Spacebot does not assume that a local MCP server is reachable from a
provider-managed VM.

## Availability

Backend availability is richer than whether a binary exists.

```rust
pub enum WorkerBackendAvailability {
    Available,
    Unavailable { reason: String },
    AuthenticationRequired { reason: String },
    Degraded { reason: String },
    PolicyBlocked { reason: String },
}
```

The registry probes profiles at startup, on configuration changes, and on
demand before execution. Concurrent probes are deduplicated. Probe results have
a short TTL and appear in settings, task execution controls, and the channel's
backend capability context.

An existing durable worker can remain recoverable while new work is blocked.
Availability for creation and availability for resume are reported separately
when the provider requires it.

## API

Worker list and detail responses gain:

```json
{
  "backend": {
    "id": "capy-spacebot",
    "kind": "capy",
    "label": "Capy / Spacebot"
  },
  "capabilities": {
    "event_delivery": "poll",
    "follow_up": {
      "queue_while_running": true,
      "steer_while_running": true,
      "interrupt_with_message": true
    }
  },
  "presentation": {
    "kind": "transcript"
  },
  "workspace": {
    "kind": "remote",
    "label": "Spacebot"
  },
  "attempt": {
    "id": "...",
    "status": "running",
    "provider_status": "active"
  },
  "artifacts": []
}
```

Control endpoints operate on capabilities:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/agents/workers/{id}/inputs` | Submit follow-up with delivery mode; starts an attempt only from idle |
| `POST` | `/agents/workers/{id}/attempts/{attempt}/cancel` | Cancel current attempt |
| `POST` | `/agents/workers/{id}/requests/{request}/respond` | Answer question or approval |
| `POST` | `/agents/workers/{id}/close` | Close the Spacebot worker session |
| `GET` | `/agents/worker-backends` | Profiles, availability, and capabilities |

SSE emits normalized worker events. Snapshot endpoints return the current
materialized state so reconnect does not depend on replaying browser events.

## Observability

Every log and metric includes `agent_id`, `worker_id`, `attempt_id`,
`backend_id`, and the provider's external ID when safe.

Required metrics:

- Session and attempt creation latency by backend.
- Active, waiting, and cancelling workers by backend.
- Poll request count, latency, and rate-limit responses.
- Stream reconnects and replayed event count.
- Ambiguous creates reconciled by operation ID.
- Cancellation acknowledgement and terminal latency.
- Duplicate events rejected.
- Stale-generation events rejected.
- Event ingestion dispositions by backend and reason.
- Liveness deadline breaches by backend, phase, and progress evidence.
- Backend availability failures.
- Attempt outcomes and retries.

Provider errors normalize into structured categories such as authentication,
rate limit, unavailable, invalid workspace, policy blocked, session missing,
and protocol error. Raw provider text is retained only after redaction and
within a bounded diagnostic field.

## Verification

The adapter contract has a shared conformance suite. Every adapter runs the
same lifecycle fixtures where its capabilities apply.

Required orchestration tests:

- Manual spawn and autonomy task execution resolve an identical `ResolvedWorkerSpec`.
- Task and project backend precedence is deterministic.
- An unavailable backend never triggers implicit fallback.
- Duplicate task claims produce one worker.
- The global task execution record exists before a provider request.
- Recovery creates a missing per-agent worker from a global execution record.
- Outcome outbox replay applies one global task transition.
- Ambiguous creation retries use one operation ID.
- An ambiguous operation fences a changed request and replacement provider call.
- Completion racing cancellation produces one terminal outcome.
- Timeout racing completion converges through cancellation.
- Unconfirmed cloud cancellation remains non-terminal and keeps its reservation.
- Duplicate terminal events do not double-complete a task.
- Old-generation events cannot mutate a resumed worker.
- An unknown worker, backend, session, or attempt binding cannot mutate state.
- Rejected and stale events retain their ingestion disposition without entering
  the normalized event projection.
- Materialized status identifies the observation that produced it.
- A backend cannot reset liveness without capability-declared progress evidence.
- Restart recovery restores active and idle durable workers.
- Missing credentials block without destroying resume metadata.
- Scheduled tasks cannot be claimed before `not_before`.
- A scheduled occurrence starts its referenced task while the agent is dormant.
- Concurrent schedule timers claim one occurrence and one task execution.

Required adapter tests:

- Captured protocol fixtures normalize into the expected transcript and status
  events.
- Follow-up modes match advertised capabilities.
- Cancellation calls the provider and waits for terminal evidence.
- Event replay and polling are idempotent.
- Provider cursors persist only after event commit.
- Event authority binding is checked before transcript, status, or terminal
  state can be written.
- Secrets are absent from backend state, events, logs, and API responses.
- Artifact and presentation metadata are bounded and validated.

Required Capy tests:

- Thread creation sends a stable `requestId`.
- Polling resumes from the last committed message cursor.
- Duplicate messages do not duplicate transcript steps.
- `queue`, `steer`, and `interrupt` map to distinct commands.
- `pending_user` waits for ordinary follow-up input without inventing a request ID.
- `ready_for_review` records a successful attempt and PR artifacts.
- Interrupt racing `ready_for_review` reaches one terminal state.
- Rate limits back off without marking the attempt failed.
- An ambiguous message response is not retried automatically.
- Closing a Capy worker archives the thread and preserves resume metadata.

Required OpenCode tests:

- Cancellation invokes `/abort` before driver teardown.
- Directory claims release after success, failure, cancellation, and panic.
- Resumed workers reacquire directory claims.
- Proxy requests reject ports outside the active pool.
- SSE replay cannot duplicate terminal effects.

Frontend tests cover capability-based controls and renderer selection. No
generic worker component may branch directly on `opencode_port` or a provider
name after the migration.

## Rollout And Work Ownership

The implementation board is the executable decomposition of this design. Task
#38 owns this document and closes when the design, dependencies, and ownership
map agree. The reliability tasks are prerequisites rather than duplicate
backend work: #20 owns durable outcome/CAS semantics, #21 owns supervision and
staged termination, #30 owns bounded briefings, and #22 owns bounded transcript
persistence and recovery. Workspace correctness is split across #28 (registry
reconciliation), #36 (durable task/worktree binding), and #35 (atomic execution
and attempt identity).

| Phase | Primary task | Scope and exit criterion | Required predecessors |
|---|---:|---|---|
| Foundation A | #20 | Persist every terminal outcome before notification; one terminal CAS winner and replay-safe retirement | #38 and existing #3 |
| Foundation B | #28 -> #36 -> #35 | Reconcile registered worktrees, persist task/worktree ownership, then create global execution and predetermined worker/attempt IDs atomically | #20, then the preceding task in the chain |
| 1 | #39 | `TaskExecutionService` is the only approved-task start path for tool, autonomy, API, and later timers; task binding precedes provider I/O | #35, #36, #38 |
| 2 | #40 | Durable profiles, capability snapshots, commands/events, operation records, generation fencing, supervisor, fake adapter, and conformance suite | #20, #39 |
| Reliability integration | #21, #30, #22 | Capability-aware liveness and staged cancellation; bounded briefing; bounded transcript/outcome recovery | #21 after #20/#40; #30 after #39/#40; #22 after #20/#30/#40 |
| 3 | #41 | OpenCode runs behind the contract; abort, claims, environment, requests, recovery, proxy ownership, and configuration are hardened | #21, #22, #30, #40 |
| 4 | #42 | Backend-neutral API, normalized transcript workbench, capability controls, artifacts, and retained OpenCode embed | #40, #41 |
| 5 | #43 | Capy profile/authentication, project binding, idempotent creation, cursor polling, follow-up modes, interruption, recovery, and PR artifacts | #21, #39, #40, #42 |
| 6 | #44 | Durable due occurrences start their exact task through `TaskExecutionService`, including dormant-agent and retry behavior | #35, #39, #40, #43 |
| 7 | #45 | Generic ACP subprocess adapter with negotiated capabilities, load, turns, updates, permission requests, and cancellation | #21, #40, #41, #42, #44 |
| 8 | Future tasks | Cursor Cloud first, then Devin and other demanded providers; Codex and Claude cloud wait for stable service APIs | Production evidence from #43/#45 |

The ordering intentionally proves the contract with the fake backend, then
migrates the existing OpenCode path, then opens the UI/API boundary, and only
then sends source and context to Capy. Scheduling follows the first cloud
adapter so its dormant recovery path is exercised against a real durable
provider. ACP lands after the generic workbench and supervision behavior are
stable; it must not become an alternate shortcut around those layers.

### Architecture Ownership Matrix

| Architecture concern | Owning task | Boundary |
|---|---:|---|
| Durable terminal outcome and lifecycle CAS | #20 | Persists one outcome before notification; does not define provider protocols |
| Progress, cancellation, ownership transfer, and shutdown | #21 | Owns generic supervision policy; adapters supply capability-specific evidence and termination calls |
| Initial briefing and context delivery | #30 | Builds, redacts, bounds, hashes, and authorizes context before execution |
| Retained transcript, inspection, reflection, and recovery | #22 | Owns execution evidence after start; does not construct the initial briefing |
| Worktree registry reconciliation | #28 | Proves local worktree identity and health |
| Durable task/worktree binding | #36 | Records which workspace an approved task owns before execution |
| Global execution and predetermined attempt identity | #35 | Atomically reserves intent before provider I/O |
| Shared task start path | #39 | Resolves approval, plan, backend, workspace, briefing, and admission |
| Backend contract and supervisor persistence | #40 | Defines profiles, capabilities, commands, events, operations, attempts, authority, and conformance |
| OpenCode adapter and trust boundary | #41 | Migrates and hardens current OpenCode behavior |
| Backend-neutral API and workbench | #42 | Exposes canonical state and capability controls |
| Capy adapter | #43 | Implements the first durable cloud profile |
| Scheduled execution | #44 | Claims due occurrences and invokes #39's service |
| ACP adapter | #45 | Implements local ACP without bypassing #40 |

### Change Ownership Rules

- #39 owns orchestration entry points and `ResolvedWorkerSpec`; adapters do not
  recreate approval, task claim, skill, workspace, or briefing decisions.
- #40 owns backend-neutral persistence and runtime contracts. Provider tasks may
  extend versioned opaque backend state but not core lifecycle enums ad hoc.
- #21 owns generic kill sites, progress policy, and shutdown drain. #41, #43,
  and #45 implement provider cancellation hooks under that policy.
- #30 owns briefing construction, redaction, size bounds, hashes, and delivery
  policy. Provider adapters only render the approved briefing.
- #22 owns normalized transcript retention and recovery projection. #42 owns
  its API and Portal presentation, not its durability semantics.
- #41 owns all remaining OpenCode-specific branches and trust-boundary fixes.
  Generic components stop reading `opencode_port` or provider names after #42.
- #43 and #45 cannot weaken authority, idempotency, cancellation, environment,
  or secret policies to match a provider limitation; unsupported behavior is a
  capability or availability result.

### Rollout Gates And Rollback

Each phase ships behind profile/creation feature flags while recovery for
already-created workers remains enabled. A phase advances only when its
required conformance tests, restart fixtures, race tests, redaction checks, and
metrics are present. Rollback disables new creation for that adapter; it does
not delete canonical rows, release reservations for unconfirmed external work,
or discard resume metadata. Operators can drain local workers and reconcile
cloud workers before removing a profile.

Before Capy is enabled outside development, #41 must have removed inherited
daemon environment from local backend launch and #42 must have removed generic
UI dependence on OpenCode fields. Before scheduling is enabled, #44 must prove
that duplicate timers produce one occurrence, one task claim, and one global
execution. Before ACP profiles ship, each command is pinned or version-probed
and its advertised capability fixture passes the same supervisor contract.

## Open Questions

- Capy's public API does not document a stable thread URL. The adapter should
  use transcript-only presentation until the API returns one.
- Capy's API exposes PR behavior but not a general artifact or diff download
  contract. The first adapter should model links it can verify and leave fetch
  capabilities disabled.
- Cursor v1 webhooks are not currently available. Its first adapter should use
  resumable SSE rather than mixing v0 webhooks with v1 lifecycle calls.
- Claude Managed Agents may become a better cloud target than Claude Code
  cloud sessions. The adapter decision waits for stable service access and
  authentication terms.

## Source Material

- [Capy API overview](https://docs.capy.ai/api-reference/overview)
- [Capy OpenAPI document](https://docs.capy.ai/openapi.json)
- [Capy threads](https://docs.capy.ai/threads)
- [OpenCode server API](https://opencode.ai/docs/server/)
- [OpenCode ACP](https://opencode.ai/docs/acp/)
- [Agent Client Protocol](https://agentclientprotocol.com/protocol/overview)
- [Cursor Cloud Agent API](https://cursor.com/docs/cloud-agent/api/endpoints)
- [Codex app-server](https://learn.chatgpt.com/codex/app-server)
- [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview)
- [Devin API](https://docs.devin.ai/api-reference/overview)
- [GitHub Copilot agent tasks](https://docs.github.com/en/rest/agent-tasks/agent-tasks)
- Orca agent startup and remote ownership under
  `/Users/jamespine/Projects/orca/src/shared/` and
  `/Users/jamespine/Projects/orca/src/main/daemon/`
