# Goals

Goals are user-defined objectives — the "why" behind task work. They're closer to Linear milestones than to tasks: high-level, persistent, and always visible. The agent uses goals as motivational context in every conversation, not just during autonomous operation.

This doc covers the goals data model, tools, context injection, and lifecycle. For how goals drive autonomous work, see [`autonomy.md`](autonomy.md).

---

## Why Goals Are Different from Tasks

Tasks are work to be done. Goals are things to achieve. The distinction matters:

- A task is complete when its output is delivered. A goal is complete when the user says it is.
- Tasks come and go. Goals persist until the objective is met or abandoned.
- Tasks are the mechanism. Goals are the motivation.

Goals are not auto-completed when their linked tasks finish. The agent can note that all linked work is done and suggest the goal is ready for review — but the user closes it. This is intentional. The agent doesn't know when an objective is truly met; the user does.

Goals are also different from `MemoryType::Goal`, which is for agent-internal notes. User goals are top-level, always-visible objectives defined explicitly by the user.

---

## Schema

```sql
CREATE TABLE goals (
    id           TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    title        TEXT NOT NULL,
    description  TEXT,
    status       TEXT NOT NULL DEFAULT 'active',  -- active, paused, completed, abandoned
    priority     TEXT NOT NULL DEFAULT 'medium',  -- critical, high, medium, low
    due_date     TEXT,                             -- ISO 8601 date, nullable
    notes        TEXT,                             -- agent-writable progress notes
    metadata     TEXT DEFAULT '{}',               -- JSON, extensible
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT
);

CREATE INDEX goals_status ON goals(status);
CREATE INDEX goals_priority ON goals(status, priority);
```

### Fields

- `title` — one-line label ("Migrate auth to Clerk", "Ship v2 before May")
- `description` — context and acceptance criteria. What does success look like?
- `status` — `active | paused | completed | abandoned`
- `priority` — same four levels as tasks: `critical | high | medium | low`
- `due_date` — optional deadline, day-granularity. Not a datetime — goals have deadlines, not appointments.
- `notes` — single agent-writable field. The agent replaces this with its current assessment, not appends. Acts as "where this stands right now." Not a history log.
- `metadata` — extensible JSON for integrations (e.g., `{"github_milestone": "v2.0", "linear_id": "OBJ-12"}`)

### Task Linking

```sql
ALTER TABLE tasks ADD COLUMN goal_id TEXT REFERENCES goals(id);
CREATE INDEX tasks_goal ON tasks(goal_id);
```

Tasks link to goals via `goal_id`. One task, one goal (or none). A goal can have many tasks. The goal review process creates tasks with `goal_id` set; users can also set it manually.

---

## Context Injection

Goals appear in agent context at two fidelity levels.

### Short Format — Channel System Prompt (always)

All active goals in a compact list, injected into every channel system prompt. ~200 token budget. Goals inform every conversation, not just autonomous operation.

```
## Active Goals
- [HIGH] Migrate auth to Clerk (due: 2026-05-01) — Tracking task created, research in progress
- [MEDIUM] Ship v2 documentation — No tasks yet
- [LOW] Improve test coverage to 80% — 3 tasks in progress
```

The agent reads these as background context: "these are the things I'm working toward." They don't dominate conversation — they orient it.

### Extended Format — Goal Review and Autonomy Channels

Full goal detail for the linked goal (for autonomy channels) or all goals (for goal review):

```
## Goal: Migrate auth to Clerk
Priority: high | Due: 2026-05-01 | Status: active

Description:
Replace the current custom JWT auth with Clerk. All endpoints should
use Clerk middleware. Session tokens should not be stored server-side.

Progress notes:
Audit complete — 12 endpoints need migration. Started with /api/user
endpoints. Clerk SDK installed and tested in staging.

Linked tasks:
- [done]        Audit auth endpoints
- [in_progress] Migrate /api/user endpoints
- [ready]       Migrate /api/admin endpoints
- [backlog]     Remove legacy session storage
- [backlog]     Update auth docs
```

No token budget — autonomy channels and goal review get the full picture.

---

## Tools

### Branch and Task Channel Tools

- `goal_create(title, description?, priority?, due_date?)` — create a new goal
- `goal_update(id, status?, priority?, due_date?, notes?, metadata_patch?)` — update fields
- `goal_list(status?)` — list goals, optionally filtered by status
- `goal_note(id, notes)` — update the progress notes field (shorthand for `goal_update`)

### Channel Tools (read-only)

Channels get `goal_list` only. They can read goals and reference them in conversation, but cannot mutate them. Goal mutations go through branches.

### Goal Completion

Goals are not completed by tool — they're completed by the user via the API or UI. The agent can call `goal_update(status: "completed")` only with explicit user instruction. The goal review process does not call this automatically.

What the goal review *can* do: set `notes` to "All linked tasks complete — ready for your review" when it detects all tasks are done. This surfaces the completion candidate to the user without presuming to close it.

---

## Lifecycle

```
goal_create → active
  ↓
Autonomy channel wakes, sees goal with no tasks
  → creates pending_approval tasks, updates goal notes
  ↓
User approves tasks → backlog / ready
  ↓
Autonomy channel executes tasks
  → updates goal notes as progress is made
  ↓
Autonomy channel detects all tasks complete
  → sets notes: "All linked tasks complete — ready for your review"
  ↓
User reviews → goal_update(status: "completed")  ← user-initiated
```

**Status transitions:**
- `active → paused` — user pauses (no autonomous work on paused goals)
- `active → completed` — user confirms objective met
- `active → abandoned` — user drops the goal
- `paused → active` — user resumes

The autonomy channel skips paused and abandoned goals entirely. When picking a task, it checks the linked goal's status — if the goal is paused or abandoned, the task is skipped and flagged.

---

## UI

Goals surface in two places in the interface:

### Goals Tab

A dedicated view alongside Tasks. Goals list with priority indicators, due dates, progress notes, and linked task counts by status. Think Linear milestones — linear progress bar derived from task completion ratio, click through to see linked tasks.

Actions: create, edit, pause, complete, abandon. Goal detail panel shows description, notes, full linked task list with status badges.

### Channel Context

The active goals list is visible in the channel context panel (same panel that shows knowledge synthesis, working memory). The user can see what the agent is oriented toward in the current conversation.

---

## Relationship to Autonomy

Goals don't drive execution directly — the task system does. The autonomy channel reads goals on each wake, uses them to inform which task to pick and what new tasks to create, and updates their notes as progress is made. There is no separate goal review process.

```
Goals → Autonomy Channel → pending_approval tasks → (user approves) → ready tasks → execution
```

Goals are the "why." Tasks are the "what." The autonomy channel is the "how."

A goal without tasks is a signal the autonomy channel acts on — it creates `pending_approval` tasks toward the objective. A goal with all tasks complete is a signal the autonomy channel surfaces to the user via `notes`. In between, goals provide the orienting context that makes individual task work coherent.

---

## Implementation Phases

**Phase 1 — Data Model + Tools**
- `goals` table migration
- `goal_id` FK on tasks migration
- `goal_create`, `goal_update`, `goal_list`, `goal_note` tools
- Active goals injected into channel system prompt (short format)
- API endpoints: CRUD for goals

**Phase 2 — Autonomy Integration**
- Autonomy channel receives all active goals in extended format on wake
- Autonomy channel uses `goal_id` when creating tasks from goals
- Autonomy channel updates `notes` as progress is made
- Autonomy channel sets `notes` when all linked tasks are complete

**Phase 3 — UI**
- Goals tab: list, create, detail panel, linked task counts
- Progress indicator (task completion ratio)
- Channel context panel shows active goals
