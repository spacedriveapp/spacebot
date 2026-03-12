# CorPilot MVP

Task-scoped steering surface built on top of Cortex chat.

## Goal

Make tasks steerable after creation without turning the task board into a generic chat product.

CorPilot is a constrained, task-scoped copilot:

- refine a task
- add missing context
- split or reorganize subtasks
- inspect execution
- steer active work

It is not a general admin chat and it should not mutate unrelated tasks.

## Product Shape

CorPilot lives inside the task detail view as a dedicated panel/tab.

The user sees:

- task overview
- CorPilot

CorPilot is attached to one task and one task only.

## MVP Scope

### Supported

- persistent task-scoped thread
- task context injected into the cortex prompt
- conversational refinement of the current task
- task updates via existing `task_update`
- execution steering for in-progress tasks
- worker inspection for the task's active worker

### Explicitly out of scope

- global freeform cortex chat from the task surface
- editing unrelated tasks from CorPilot
- new task comments/events tables
- multi-task planning views
- autonomous execution policy changes

## Behavior Rules

### Backlog / Pending Approval / Ready

CorPilot may:

- rewrite title
- rewrite description
- change priority
- change subtasks
- move status

### In Progress

CorPilot should prefer:

- adding context
- steering the worker
- explaining blockers
- proposing replans

CorPilot should avoid silently rewriting the core task specification while work is already running.

## Technical Design

### Thread identity

Use a deterministic cortex thread per task for MVP.

Format:

`corpilot:task:<task-id>`

This avoids a schema migration and keeps history stable.

### Context injection

Extend cortex chat send requests with `task_number`.

When present, cortex prompt building loads:

- task title
- description
- status
- priority
- subtasks
- created/updated metadata
- active worker id
- latest worker summary if available

### Prompt constraints

When `task_context` exists, the cortex prompt explicitly switches into CorPilot mode:

- stay scoped to this task
- prefer task operations
- do not mutate unrelated tasks
- be conservative while task is `in_progress`

### UI

Embed CorPilot into task detail.

Requirements:

- fixed thread id
- no thread picker
- no new-thread button
- task-specific starter prompts
- task summary header above the chat

## Why this MVP

The backend already has:

- task CRUD and mutation
- cortex chat streaming
- worker inspection

So the main missing pieces are:

- task scoping
- prompt constraints
- task detail embedding

This keeps the first implementation small enough to test quickly.
