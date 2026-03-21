# Global Task System Elevation

Specification for elevating the task system from agent-scoped to instance-level global scope. This replaces per-agent task boards with a single global task board where tasks are created, assigned, delegated, and tracked across all agents.

## Problem Statement

Tasks currently live in per-agent SQLite databases. Each agent has its own `tasks` table, its own `TaskStore`, its own numbering sequence, and its own API surface filtered by `agent_id`. This creates several problems:

1. **No cross-agent visibility.** A human managing multiple agents cannot see all tasks in one place. The only way to view tasks is navigating to each agent's "Tasks" tab individually.

2. **Fragile cross-agent delegation.** The `send_agent_message` tool writes directly to the target agent's SQLite database via a `task_store_registry`. This crosses database boundaries at the application layer, creating implicit coupling between agent databases that can fail if the target agent is not initialized.

3. **No global task numbering.** Task numbers are per-agent (`UNIQUE(agent_id, task_number)`), so `#42` is ambiguous when multiple agents exist. Cross-agent references require qualifying with the agent ID.

4. **UI dead end.** Tasks are buried under agent tab navigation (tab position 8 of 12). There is no sidebar entry, no dashboard widget, no global task page. The Orchestrate page shows cross-agent workers but has no task awareness.

5. **Assignment model is implicit.** The `agent_id` field determines which agent "owns" a task, but there is no explicit assignment model. A task cannot be assigned to a different agent without being recreated on that agent's database.

## Design Goals

- **Single global task database.** One `tasks` table in one SQLite database, shared across all agents, with globally unique task numbers.
- **Explicit ownership and assignment.** Every task has an `owner_agent_id` (who created it) and an `assigned_agent_id` (who will execute it). These can differ.
- **Top-level sidebar entry.** Tasks get a dedicated icon in the sidebar, navigating to a global `/tasks` page. The agent-scoped task tab becomes a filtered view of the same global data.
- **Backward-compatible API.** The existing `/agents/tasks` endpoints continue to work (filtering by agent), while new `/tasks` endpoints provide global access.
- **Eliminate cross-database writes.** The `send_agent_message` tool no longer writes to a foreign agent's database. It creates a task in the shared global store with the appropriate assignment.

## Current Architecture

### Database

Each agent has a `tasks` table in its own SQLite database (`{agent_dir}/data/spacebot.db`):

```sql
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    task_number INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'backlog',
    priority TEXT NOT NULL DEFAULT 'medium',
    subtasks TEXT,           -- JSON array
    metadata TEXT,           -- JSON object
    source_memory_id TEXT,
    worker_id TEXT,
    created_by TEXT NOT NULL,
    approved_at TIMESTAMP,
    approved_by TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    UNIQUE(agent_id, task_number)
);
```

### Rust Types

- `TaskStore` wraps a `SqlitePool` and accepts `agent_id` on every method call
- `Task`, `CreateTaskInput`, `UpdateTaskInput`, `TaskStatus`, `TaskPriority`, `TaskSubtask` in `src/tasks/store.rs`
- Tools: `TaskCreateTool`, `TaskListTool`, `TaskUpdateTool` in `src/tools/task_{create,list,update}.rs`
- Each tool stores a fixed `agent_id` at construction time
- `SendAgentMessageTool` uses `task_store_registry: Arc<ArcSwap<HashMap<String, Arc<TaskStore>>>>` to write to foreign agent databases

### API Endpoints

All under `/agents/tasks`, all require `agent_id` query parameter:
- `GET /agents/tasks` — list (filtered by agent_id)
- `POST /agents/tasks` — create
- `GET /agents/tasks/{number}` — get by number
- `PUT /agents/tasks/{number}` — update
- `DELETE /agents/tasks/{number}` — delete
- `POST /agents/tasks/{number}/approve` — approve
- `POST /agents/tasks/{number}/execute` — execute

### UI

- Single file: `interface/src/routes/AgentTasks.tsx` (643 lines)
- Route: `/agents/$agentId/tasks`
- No sidebar entry, no global page, no dashboard widget
- Kanban board with 5 columns, create dialog, detail dialog
- Real-time updates via SSE `task_updated` events -> `useLiveContext` -> React Query invalidation

### Execution Pipeline

- Cortex `spawn_ready_task_loop()` polls for ready tasks via `task_store.claim_next_ready(agent_id)`
- Worker binds to task via `worker_id` field
- Worker can only update its assigned task (enforced by `TaskUpdateScope::Worker`)
- Completion notifications route back via `metadata.delegating_agent_id` and `metadata.originating_channel`

---

## Global Architecture Design

### New Database: Instance-Level Task Store

A new SQLite database at `{instance_dir}/data/tasks.db`, independent of any agent's database. This follows the precedent set by `secrets.redb` — the only other instance-level datastore.

#### Schema

```sql
-- Global tasks table with instance-wide unique numbering.
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    task_number INTEGER NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'backlog',
    priority TEXT NOT NULL DEFAULT 'medium',

    -- Ownership: who created this task
    owner_agent_id TEXT NOT NULL,
    -- Assignment: who should execute this task (can differ from owner)
    assigned_agent_id TEXT NOT NULL,

    subtasks TEXT,                    -- JSON array of {title, completed}
    metadata TEXT,                    -- JSON object, arbitrary key-value

    source_memory_id TEXT,           -- FK concept to a memory in the owner agent's memory store
    worker_id TEXT,                  -- set when a worker is executing this task

    created_by TEXT NOT NULL,        -- 'cortex', 'human', 'branch', 'agent:<id>'
    approved_at TEXT,
    approved_by TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_owner ON tasks(owner_agent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_assigned ON tasks(assigned_agent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_worker ON tasks(worker_id);
CREATE INDEX IF NOT EXISTS idx_tasks_priority_status ON tasks(status, priority);
CREATE INDEX IF NOT EXISTS idx_tasks_source_memory ON tasks(source_memory_id);
```

Key changes from the current schema:
- **`task_number` is globally unique** (`UNIQUE` constraint, no composite key with agent_id). Simple monotonic sequence across all agents.
- **`agent_id` splits into `owner_agent_id` and `assigned_agent_id`.** Owner is who created it; assigned is who executes it. For self-created tasks, both are the same agent.
- **Timestamps use ISO 8601 strings** (consistent with the rest of the codebase).
- **No per-agent composite index** on task_number — the global uniqueness constraint serves as the index.

#### Task Numbering

Global monotonic sequence. `SELECT COALESCE(MAX(task_number), 0) + 1 FROM tasks`. All agents share the same number space. `#42` is unambiguous across the entire instance.

The retry-on-collision logic in the current `TaskStore::create()` is retained — it handles concurrent inserts gracefully via the `UNIQUE(task_number)` constraint and SQLite transaction isolation.

### Rust Changes

#### New GlobalTaskStore

```
src/tasks/
  store.rs      — rename to global_store.rs (or refactor in-place)
```

The `TaskStore` struct changes from per-agent to global:

```rust
#[derive(Debug, Clone)]
pub struct TaskStore {
    pool: SqlitePool,  // Points to {instance_dir}/data/tasks.db
}
```

Method signature changes (conceptual):

| Current | New |
|---------|-----|
| `create(input)` where input has `agent_id` | `create(input)` where input has `owner_agent_id` + `assigned_agent_id` |
| `list(agent_id, status, priority, limit)` | `list(filter)` where filter has optional `owner_agent_id`, `assigned_agent_id`, `status`, `priority`, `limit` |
| `get_by_number(agent_id, task_number)` | `get_by_number(task_number)` — no agent scoping needed |
| `update(agent_id, task_number, input)` | `update(task_number, input)` — agent validation moves to authorization layer |
| `delete(agent_id, task_number)` | `delete(task_number)` |
| `claim_next_ready(agent_id)` | `claim_next_ready(assigned_agent_id)` — still agent-scoped for execution |
| `get_by_worker_id(agent_id, worker_id)` | `get_by_worker_id(worker_id)` — globally unique worker IDs |
| `list_ready(agent_id, limit)` | `list_ready(assigned_agent_id, limit)` |

New `TaskListFilter` struct:

```rust
#[derive(Debug, Clone, Default)]
pub struct TaskListFilter {
    pub owner_agent_id: Option<String>,
    pub assigned_agent_id: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<TaskPriority>,
    pub created_by: Option<String>,
    pub limit: Option<i64>,
}
```

Updated `CreateTaskInput`:

```rust
#[derive(Debug, Clone)]
pub struct CreateTaskInput {
    pub owner_agent_id: String,
    pub assigned_agent_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub subtasks: Vec<TaskSubtask>,
    pub metadata: Value,
    pub source_memory_id: Option<String>,
    pub created_by: String,
}
```

Updated `UpdateTaskInput` — add `assigned_agent_id`:

```rust
#[derive(Debug, Clone, Default)]
pub struct UpdateTaskInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<TaskPriority>,
    pub subtasks: Option<Vec<TaskSubtask>>,
    pub metadata: Option<Value>,
    pub worker_id: Option<String>,
    pub clear_worker_id: bool,
    pub approved_by: Option<String>,
    pub complete_subtask: Option<usize>,
    pub assigned_agent_id: Option<String>,  // NEW: reassign task
}
```

Updated `Task` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub task_number: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub owner_agent_id: String,
    pub assigned_agent_id: String,
    pub subtasks: Vec<TaskSubtask>,
    pub metadata: Value,
    pub source_memory_id: Option<String>,
    pub worker_id: Option<String>,
    pub created_by: String,
    pub approved_at: Option<String>,
    pub approved_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}
```

#### Database Initialization

In `main.rs`, the global task database is initialized once during startup alongside the secrets store:

```rust
// Instance-level databases
let secrets_store = SecretsStore::open(&instance_dir.join("data").join("secrets.redb"))?;
let global_task_pool = db::connect_sqlite(&instance_dir.join("data").join("tasks.db")).await?;
db::run_task_migrations(&global_task_pool).await?;
let global_task_store = Arc::new(TaskStore::new(global_task_pool));
```

The `global_task_store` is passed to every `AgentDeps` (replacing the per-agent `task_store`), and stored in `ApiState` as a single `Arc<TaskStore>` (replacing the per-agent `ArcSwap<HashMap<String, Arc<TaskStore>>>`).

#### AgentDeps Changes

```rust
pub struct AgentDeps {
    // REMOVED: pub task_store: Arc<TaskStore>,
    // REMOVED: pub task_store_registry: Arc<ArcSwap<HashMap<String, Arc<TaskStore>>>>,
    pub task_store: Arc<TaskStore>,  // Now points to the GLOBAL store
    // ...rest unchanged
}
```

The `task_store_registry` is eliminated entirely. Every agent shares the same `Arc<TaskStore>` pointing to the global database.

#### Tool Changes

**`TaskCreateTool`** — No structural change, but `agent_id` becomes both `owner_agent_id` and `assigned_agent_id` (self-assignment). The tool signature for the LLM is unchanged.

```rust
pub struct TaskCreateTool {
    task_store: Arc<TaskStore>,    // global store
    agent_id: String,             // this agent's ID (used as owner + assigned)
    created_by: String,
    working_memory: Option<Arc<WorkingMemoryStore>>,
}
```

**`TaskListTool`** — Filter by `assigned_agent_id` by default (show tasks assigned to this agent). Add an optional `include_owned` parameter so the agent can also see tasks it created but assigned elsewhere.

```rust
pub struct TaskListTool {
    task_store: Arc<TaskStore>,    // global store
    agent_id: String,             // this agent's ID
}
```

**`TaskUpdateTool`** — Worker scope validation uses `get_by_worker_id()` (globally unique, no agent_id needed). Branch scope now operates on globally-addressed tasks.

**`SendAgentMessageTool`** — Simplified. No more `task_store_registry`. Creates a task on the same global store with `owner_agent_id = sender` and `assigned_agent_id = target`:

```rust
// Before: look up target agent's TaskStore from registry, write to their DB
// After: write to global store with appropriate assignment
let task = self.task_store.create(CreateTaskInput {
    owner_agent_id: sending_agent_id.to_string(),
    assigned_agent_id: receiving_agent_id.to_string(),
    title,
    description: Some(args.message.clone()),
    status: TaskStatus::Ready,
    priority: TaskPriority::Medium,
    subtasks: Vec::new(),
    metadata,
    source_memory_id: None,
    created_by: format!("agent:{}", sending_agent_id),
}).await?;
```

#### Cortex Ready-Task Loop Changes

`spawn_ready_task_loop()` calls `task_store.claim_next_ready(agent_id)` — the `agent_id` parameter now filters on `assigned_agent_id`. Each agent's cortex still only picks up tasks assigned to it. No behavioral change, just a column rename in the query.

#### Event System Changes

`ProcessEvent::TaskUpdated` and `ApiEvent::TaskUpdated` gain clarity:

```rust
ProcessEvent::TaskUpdated {
    agent_id: AgentId,           // the agent whose cortex/process emitted this event
    owner_agent_id: AgentId,     // who created the task
    assigned_agent_id: AgentId,  // who is assigned to execute
    task_number: i64,
    status: String,
    action: String,
}
```

The SSE event includes both agent IDs so the UI can update the global view, agent-filtered views, and any cross-agent dashboards simultaneously.

### API Changes

#### New Global Endpoints

```
GET    /tasks                     — list all tasks (with optional filters)
POST   /tasks                     — create task (global)
GET    /tasks/{number}            — get by number (globally unique)
PUT    /tasks/{number}            — update task
DELETE /tasks/{number}            — delete task
POST   /tasks/{number}/approve    — approve
POST   /tasks/{number}/execute    — execute
POST   /tasks/{number}/assign     — reassign to different agent
```

#### Backward-Compatible Agent Endpoints

The existing `/agents/tasks` endpoints are retained as thin wrappers that filter by agent:

```
GET    /agents/tasks?agent_id=X        → /tasks?assigned_agent_id=X
POST   /agents/tasks                   → /tasks (with agent_id mapped to owner+assigned)
GET    /agents/tasks/{number}          → /tasks/{number} (validate agent_id matches)
PUT    /agents/tasks/{number}          → /tasks/{number}
DELETE /agents/tasks/{number}          → /tasks/{number}
POST   /agents/tasks/{number}/approve  → /tasks/{number}/approve
POST   /agents/tasks/{number}/execute  → /tasks/{number}/execute
```

These can be deprecated over time once the UI fully migrates to global endpoints.

#### New Endpoint: Assign

```
POST /tasks/{number}/assign
Body: { "assigned_agent_id": "agent-b" }
```

Reassigns a task to a different agent. Validates the target agent exists. Sets `assigned_agent_id` and emits events to both agents' SSE streams.

#### Query Parameters for GET /tasks

| Parameter | Type | Description |
|-----------|------|-------------|
| `owner_agent_id` | string | Filter by task creator |
| `assigned_agent_id` | string | Filter by assigned executor |
| `status` | string | Filter by status |
| `priority` | string | Filter by priority |
| `created_by` | string | Filter by created_by field |
| `limit` | integer | Max results (default 100, max 500) |
| `offset` | integer | Pagination offset |

#### ApiState Changes

```rust
pub struct ApiState {
    // REMOVED: pub task_stores: ArcSwap<HashMap<String, Arc<TaskStore>>>,
    // REMOVED: pub task_store_registry: Arc<ArcSwap<HashMap<String, Arc<TaskStore>>>>,
    pub task_store: Arc<TaskStore>,  // Single global store
    // ...rest unchanged
}
```

### UI Changes

#### Sidebar Addition

Add a "Tasks" icon to the sidebar between "Orchestrate" and "Settings":

```tsx
// In Sidebar.tsx, after the Orchestrate link:
<Link
    to="/tasks"
    className={`flex h-8 w-8 items-center justify-center rounded-md ${
        isTasks ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
    }`}
    title="Tasks"
>
    <HugeiconsIcon icon={TaskIcon} className="h-4 w-4" />
</Link>
```

The sidebar icon shows a notification badge with the count of `pending_approval` tasks across all agents:

```tsx
{pendingCount > 0 && (
    <span className="absolute -right-0.5 -top-0.5 flex h-3.5 w-3.5 items-center justify-center rounded-full bg-amber-500 text-[9px] font-bold text-white">
        {pendingCount > 9 ? "9+" : pendingCount}
    </span>
)}
```

#### New Route: /tasks

```tsx
const globalTasksRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/tasks",
    validateSearch: (search: Record<string, unknown>) => ({
        agent: typeof search.agent === "string" ? search.agent : undefined,
        status: typeof search.status === "string" ? search.status : undefined,
    }),
    component: function GlobalTasksPage() { ... },
});
```

#### Global Tasks Page Layout

The global tasks page has three sections:

1. **Toolbar** — Filter pills (agent, status, priority), search, "Create Task" button. Agent filter shows all agent names as toggleable pills. Status filter is a dropdown. Priority filter is a dropdown.

2. **Kanban Board** — Same 5-column layout as the current agent-scoped board, but showing tasks from all agents (or filtered by selected agents). Each task card shows an **agent badge** indicating which agent owns/is assigned to it. Cards for different agents can use the agent's gradient colors for visual distinction.

3. **Task Detail Dialog** — Enhanced version of the current dialog with:
   - Agent assignment selector (dropdown of all agents)
   - "Reassign" button
   - Owner agent display (if different from assigned)
   - Cross-agent delegation history in metadata

#### Agent-Scoped Task Tab

The existing `/agents/$agentId/tasks` route remains but becomes a filtered view:

```tsx
// AgentTasks.tsx — same component, but queries change:
// Before: api.listTasks(agentId, { limit: 200 })
// After:  api.listGlobalTasks({ assigned_agent_id: agentId, limit: 200 })
```

The agent tab now shows tasks where `assigned_agent_id = agentId` OR `owner_agent_id = agentId`. This gives each agent a view of "my tasks" (assigned to me) and "tasks I created" (delegated to others).

#### TypeScript Type Changes

```typescript
export interface TaskItem {
    id: string;
    task_number: number;           // globally unique
    title: string;
    description?: string;
    status: TaskStatus;
    priority: TaskPriority;
    owner_agent_id: string;        // NEW: replaces agent_id
    assigned_agent_id: string;     // NEW: replaces agent_id
    subtasks: TaskSubtask[];
    metadata: Record<string, unknown>;
    source_memory_id?: string;
    worker_id?: string;
    created_by: string;
    approved_at?: string;
    approved_by?: string;
    created_at: string;
    updated_at: string;
    completed_at?: string;
}

export interface CreateTaskRequest {
    title: string;
    description?: string;
    status?: TaskStatus;
    priority?: TaskPriority;
    owner_agent_id: string;        // NEW
    assigned_agent_id?: string;    // NEW (defaults to owner)
    subtasks?: TaskSubtask[];
    metadata?: Record<string, unknown>;
    source_memory_id?: string;
    created_by?: string;
}
```

#### New API Client Methods

```typescript
// Global task endpoints
listGlobalTasks: (params?) => GET /tasks?...
getGlobalTask: (taskNumber) => GET /tasks/{taskNumber}
createGlobalTask: (request) => POST /tasks
updateGlobalTask: (taskNumber, request) => PUT /tasks/{taskNumber}
deleteGlobalTask: (taskNumber) => DELETE /tasks/{taskNumber}
approveGlobalTask: (taskNumber, approvedBy?) => POST /tasks/{taskNumber}/approve
executeGlobalTask: (taskNumber) => POST /tasks/{taskNumber}/execute
assignTask: (taskNumber, assignedAgentId) => POST /tasks/{taskNumber}/assign
```

#### SSE Event Handling

The `task_updated` SSE event now includes `owner_agent_id` and `assigned_agent_id`. The `useLiveContext` hook bumps `taskEventVersion` regardless of which agent is involved, enabling the global tasks page to stay live.

For the agent-scoped view, the existing behavior is preserved — only events where `assigned_agent_id` matches the viewed agent trigger invalidation.

---

## Migration Strategy

### Phase 1: Create Global Database + Dual-Write

1. **Create migration infrastructure.** Add `{instance_dir}/data/tasks.db` with the new schema. The migration runner creates the database alongside secrets.redb initialization.

2. **Create `GlobalTaskStore`.** Implement the new store pointing at the global database. Initially coexists with per-agent `TaskStore` instances.

3. **Dual-write.** During transition, writes go to both the per-agent database AND the global database. Reads come from the per-agent database. This ensures no data loss during migration.

### Phase 2: Data Migration

On first startup after upgrade:

1. **Scan all agent data directories** for `spacebot.db` files containing tasks.
2. **For each agent, read all tasks** from the per-agent `tasks` table.
3. **Assign new global task numbers** sequentially (ordered by `created_at` across all agents).
4. **Map `agent_id` to `owner_agent_id` and `assigned_agent_id`** — for non-delegated tasks, both equal the original `agent_id`. For delegated tasks (those with `metadata.delegating_agent_id`), `owner_agent_id` = the delegating agent, `assigned_agent_id` = the original `agent_id`.
5. **Insert into global database** with new global task numbers.
6. **Record migration state** in a migration marker file (`{instance_dir}/data/.tasks_migrated`) or a redb key, so migration is idempotent.

Task number remapping means existing references to `#42` in conversation history and memories will point to the old per-agent number. This is acceptable because:
- Conversation history is read-only and archived
- The new global numbers start from 1, and the mapping is monotonic (older tasks get lower global numbers)
- A `metadata.legacy_task_number` field preserves the original per-agent number for traceability

```rust
/// Migration record stored in each migrated task's metadata.
metadata: {
    "legacy_agent_id": "original-agent-id",
    "legacy_task_number": 42,
    // ...existing metadata preserved
}
```

### Phase 3: Switch Reads to Global Store

1. **Update all tools** to use the global `TaskStore`.
2. **Update all API handlers** to use the global store.
3. **Update the cortex ready-task loop** to use the global store.
4. **Stop dual-writing** to per-agent databases.

### Phase 4: Deprecate Per-Agent Task Tables

1. **Remove `task_store_registry`** from `AgentDeps` and `ApiState`.
2. **Remove per-agent task store** initialization from agent creation.
3. The per-agent `tasks` table in `spacebot.db` becomes dead data — do not delete it (it serves as a backup), but stop reading/writing.

### Rollback Plan

If migration fails:
- The per-agent databases are never modified (migration is additive)
- Remove the migration marker to retry
- Delete `tasks.db` to start fresh
- Fall back to per-agent stores by reverting code changes

---

## Implementation Roadmap

### Step 1: Global Database Setup

**Files to change:**
- `src/db.rs` — Add `connect_task_db()` function for instance-level task database
- `migrations/` — New migration file for global tasks schema (separate directory from per-agent migrations, e.g., `migrations/global/` or a dedicated prefix)
- `src/main.rs` — Initialize global task database during startup

**New files:**
- `migrations/global/20260321000001_global_tasks.sql` — Global tasks schema

**Effort:** Small. Follows the `secrets.redb` precedent for instance-level database initialization.

### Step 2: TaskStore Refactor

**Files to change:**
- `src/tasks/store.rs` — Refactor `TaskStore` to use global schema (owner/assigned agent IDs, global numbering, `TaskListFilter`)
- `src/tasks.rs` — Update re-exports

**Approach:** Modify in-place. The `Task`, `CreateTaskInput`, `UpdateTaskInput` structs gain the new fields. The `agent_id` field is removed from `Task` and replaced with `owner_agent_id` + `assigned_agent_id`. All SQL queries are rewritten against the new schema.

**Status transition rules:** Unchanged. The `can_transition()` function is status-only, not agent-specific.

**Effort:** Medium. Core data access layer rewrite, but the logic (transitions, retry, subtask merge) is preserved.

### Step 3: Tool Updates

**Files to change:**
- `src/tools/task_create.rs` — Use global store, set owner=assigned=self
- `src/tools/task_list.rs` — Filter by assigned_agent_id by default
- `src/tools/task_update.rs` — Remove agent_id from worker validation, use global numbering
- `src/tools/send_agent_message.rs` — Remove `task_store_registry`, use global store with cross-agent assignment
- `src/tools.rs` — Update tool server factories (remove task_store_registry wiring)

**Effort:** Medium. Each tool is a self-contained change. The biggest win is `send_agent_message` simplification.

### Step 4: AgentDeps + Initialization Cleanup

**Files to change:**
- `src/lib.rs` — Remove `task_store_registry` from `AgentDeps`, update `ProcessEvent::TaskUpdated`
- `src/main.rs` — Pass global task store to all agents, remove per-agent task store init, remove registry population

**Effort:** Small-medium. Mostly deletion of registry code.

### Step 5: API Layer

**Files to change:**
- `src/api/tasks.rs` — Rewrite handlers for global endpoints, add backward-compatible agent wrappers
- `src/api/server.rs` — Add new `/tasks` routes alongside existing `/agents/tasks` routes
- `src/api/state.rs` — Replace `task_stores: ArcSwap<HashMap<...>>` with single `task_store: Arc<TaskStore>`

**New endpoints:** `/tasks`, `/tasks/{number}`, `/tasks/{number}/approve`, `/tasks/{number}/execute`, `/tasks/{number}/assign`

**Effort:** Medium. New route definitions and handler implementations, but they are simpler than the current ones (no per-agent store lookup).

### Step 6: Cortex Integration

**Files to change:**
- `src/agent/cortex.rs` — Update `spawn_ready_task_loop()` to use `assigned_agent_id`, update `notify_delegation_completion()`, update event emissions

**Effort:** Small. The cortex already filters by agent — just change the column name.

### Step 7: Event System

**Files to change:**
- `src/lib.rs` — Update `ProcessEvent::TaskUpdated` variant
- `src/api/state.rs` — Update `ApiEvent::TaskUpdated`, update SSE forwarding
- `src/api/system.rs` — SSE serialization (already handles `TaskUpdated`)
- `src/agent/cortex.rs` — Signal coalescing for `TaskUpdated`
- `src/agent/channel_history.rs` — `ProcessEvent::TaskUpdated` match arm

**Effort:** Small. Adding fields to existing event variants.

### Step 8: Data Migration

**New files:**
- `src/tasks/migration.rs` — One-time migration logic: scan per-agent DBs, read tasks, assign global numbers, insert into global DB, write marker

**Files to change:**
- `src/main.rs` — Run migration before normal startup if marker absent

**Effort:** Medium. One-time migration code that runs early in startup.

### Step 9: UI — Global Tasks Page

**Files to change:**
- `interface/src/router.tsx` — Add `/tasks` route
- `interface/src/components/Sidebar.tsx` — Add Tasks icon with pending badge
- `interface/src/routes/AgentTasks.tsx` — Extract shared components, add agent badge to cards
- `interface/src/api/client.ts` — Add global task API methods, update TypeScript types
- `interface/src/hooks/useLiveContext.tsx` — No change needed (taskEventVersion already global)

**New files:**
- `interface/src/routes/GlobalTasks.tsx` — Global tasks page with multi-agent kanban board

**Effort:** Medium-large. New page component with agent filtering, but reuses kanban/card/dialog components from `AgentTasks.tsx`.

### Step 10: UI — Agent-Scoped View Update

**Files to change:**
- `interface/src/routes/AgentTasks.tsx` — Switch from per-agent API to global API with agent filter
- `interface/src/components/AgentTabs.tsx` — Keep "Tasks" tab, optionally add pending count badge

**Effort:** Small. Change API calls from `api.listTasks(agentId)` to `api.listGlobalTasks({ assigned_agent_id: agentId })`.

### Step 11: Cleanup + Deprecation

**Files to change:**
- Remove `task_store_registry` from all remaining references
- Remove per-agent `TaskStore` initialization from `create_agent_internal()` in `src/api/agents.rs`
- Update `AGENTS.md` module map and architecture docs
- Update `docs/design-docs/task-tracking.md` to reference global architecture

**Effort:** Small. Deletion and documentation.

---

## Component Dependency Graph

```
Step 1 (DB setup)
  └─ Step 2 (TaskStore refactor)
       ├─ Step 3 (Tool updates)
       │    └─ Step 4 (AgentDeps cleanup)
       │         └─ Step 6 (Cortex integration)
       ├─ Step 5 (API layer)
       │    └─ Step 9 (UI — Global page)
       │         └─ Step 10 (UI — Agent view update)
       ├─ Step 7 (Event system)
       └─ Step 8 (Data migration)

Step 11 (Cleanup) depends on all above
```

Steps 3-7 can be parallelized after Step 2. Step 8 (migration) is independent of the tool/API changes and can be developed in parallel. Step 9 depends on Step 5 (API must exist for UI to call). Step 10 is a small follow-up to Step 9.

---

## Risk Assessment

### Low Risk
- **Global database creation** — follows established patterns (secrets.redb)
- **Event system changes** — additive field additions
- **Sidebar UI change** — small, isolated component

### Medium Risk
- **TaskStore refactor** — core data layer, many consumers, but well-tested
- **Tool changes** — each tool is isolated, but all must be updated atomically
- **API migration** — backward compatibility requires careful testing

### High Risk
- **Data migration** — one-shot, irreversible (in the forward direction). Requires thorough testing with multi-agent fixtures containing delegated tasks, varied statuses, and edge cases (tasks with workers, tasks in progress).
- **Cortex ready-task loop** — critical path for autonomous execution. The `claim_next_ready` query must correctly filter by `assigned_agent_id` and handle the atomic status transition across the global database.

### Mitigations
- Migration is additive (per-agent data is never modified)
- Dual-write phase allows testing reads from global DB while per-agent DB remains source of truth
- Integration test: create multi-agent fixture, migrate, verify all tasks accessible via global API with correct numbering and assignment
- The cortex loop already handles concurrency (atomic claim with WHERE guard) — just changing the column name

---

## Open Questions

1. **Should the global tasks page replace or supplement the agent task tab?** Current recommendation: supplement. The agent tab becomes a filtered view showing "my tasks" for that agent. Removing it would reduce discoverability for users who primarily work with one agent.

2. **Should tasks support multiple assignees?** Not in this iteration. A task has one assigned agent. For multi-agent coordination, the delegating agent creates separate tasks for each target agent and tracks them via metadata.

3. **Should the dashboard (Overview page) show a task summary widget?** Yes, but as a follow-up. A simple "X tasks pending approval, Y in progress" counter per agent in the topology view would be valuable but is out of scope for the core migration.

4. **Should the Create Task dialog on the global page allow selecting the assigned agent?** Yes. The global page create dialog should have an "Agent" dropdown defaulting to the first agent (or the most recently selected agent).

5. **Archival strategy for done tasks.** Deferred. The global database will accumulate done tasks from all agents. A future `archived` status or auto-archive policy should be designed separately.
