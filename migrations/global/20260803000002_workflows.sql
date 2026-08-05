-- Workflow templates: a pipeline defined once and launched with one input.
--
-- Nothing here executes anything. The scheduler already runs a graph of tasks
-- with edges and bindings unattended — `recompute_ready` decides eligibility,
-- `claim_next_ready` re-checks the parent invariant, `resolve_inputs` assembles
-- a task's inputs, `submit_outputs` validates what it produced, and the reaper
-- and failure budget handle the ways it can go wrong. Launching a workflow is a
-- compile step that emits those rows. That is the whole design.
--
-- The one thing the existing schema cannot express is a binding by *name*.
-- `task_input_bindings.source_task_number` points at a task number, and a
-- template has no task numbers — only step keys, which become numbers at
-- launch. `workflow_step_bindings` below is that same table addressed by name,
-- and instantiation translates one into the other.

CREATE TABLE IF NOT EXISTS workflows (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    description TEXT,

    -- JSON Schema for the input handed to a whole run. Validated once, at
    -- launch, so a bad input is rejected while a human is still looking at it
    -- rather than surfacing inside step three as an unresolvable binding.
    input_schema TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- One step of a pipeline. Mirrors the task fields it will become.
--
-- `step_key` rather than a row id is what bindings and edges reference, because
-- a template is edited by hand and a stable human-readable name survives that;
-- a generated id means every edit risks repointing an edge at the wrong step.
CREATE TABLE IF NOT EXISTS workflow_steps (
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    step_key TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,

    -- NULL means "whoever launched the run". Naming an agent here is how a
    -- pipeline hands a step to a different specialist.
    assigned_agent_id TEXT,
    priority TEXT NOT NULL DEFAULT 'medium',

    input_schema TEXT,
    output_schema TEXT,

    -- Per-step instructions, appended to the worker prompt at pickup. A step is
    -- the natural home for this: it is the only place that knows both what the
    -- work is and what shape it must return.
    system_prompt TEXT,

    -- Multi-repo pipelines: a step names the repo it acts on, so "regenerate
    -- clients in web after the contract lands in api" is two steps and an edge.
    repo_id TEXT,

    -- Display order only. Execution order comes from the edges.
    position INTEGER NOT NULL DEFAULT 0,

    PRIMARY KEY (workflow_id, step_key)
);

CREATE TABLE IF NOT EXISTS workflow_step_edges (
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    parent_step_key TEXT NOT NULL,
    child_step_key TEXT NOT NULL,
    PRIMARY KEY (workflow_id, parent_step_key, child_step_key)
);

-- Where a step's input comes from, addressed by name.
--
-- `source` is explicit rather than inferred from which column is non-NULL. The
-- task-level table infers "literal" from a NULL source_task_number, which works
-- but means a malformed row is indistinguishable from a deliberate one. Three
-- named cases cannot be misread:
--   step      -- read another step's output at source_pointer
--   literal   -- a value baked into the template
--   run_input -- read the run's own input at source_pointer
--
-- `run_input` is what makes a single input drive a whole pipeline: a step binds
-- straight to a pointer into the launch payload, so the entry point is
-- declarative and there is no special-cased "first step".
CREATE TABLE IF NOT EXISTS workflow_step_bindings (
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    step_key TEXT NOT NULL,
    input_key TEXT NOT NULL,

    source TEXT NOT NULL,
    source_step_key TEXT,
    source_pointer TEXT,
    literal_value TEXT,

    PRIMARY KEY (workflow_id, step_key, input_key)
);

-- One launch. Gives the emitted tasks a shared identity and records exactly
-- what was fed in, which is the only way to explain a run after the fact.
CREATE TABLE IF NOT EXISTS workflow_runs (
    id TEXT PRIMARY KEY NOT NULL,
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    inputs TEXT NOT NULL,
    launched_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Which run a task belongs to, and which step it came from.
--
-- Deliberately not a foreign key to `workflow_runs`: a task outlives its
-- template. Deleting a workflow must not cascade into deleting the history of
-- work that was actually done, and `ON DELETE SET NULL` would quietly detach
-- the run grouping instead. The columns are plain text and the join is
-- best-effort, which is the honest representation of "this happened, and the
-- recipe it came from may since have been thrown away".
ALTER TABLE tasks ADD COLUMN workflow_run_id TEXT;
ALTER TABLE tasks ADD COLUMN workflow_step_key TEXT;

-- Per-task instructions, appended to the worker prompt at pickup.
--
-- This is the deferred "per-step system prompt", and it lands here rather than
-- separately because a step is where it belongs: the only place that knows both
-- what the work is and what shape it has to come back in. A workflow step
-- carries one and stamps it onto the task it compiles into, but the column is
-- on `tasks` so a hand-built task can carry one too — the graph does not have
-- to come from a template to want instructions.
--
-- Appended, never substituted. It is task instructions, not an identity
-- override, and a filed card's prompt is model-authored text that becomes
-- another model's system prompt.
ALTER TABLE tasks ADD COLUMN system_prompt TEXT;

-- The run view groups every task of one launch.
CREATE INDEX IF NOT EXISTS idx_tasks_workflow_run ON tasks(workflow_run_id);
CREATE INDEX IF NOT EXISTS idx_workflow_steps_workflow ON workflow_steps(workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflow_edges_workflow ON workflow_step_edges(workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflow_bindings_workflow ON workflow_step_bindings(workflow_id);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_workflow ON workflow_runs(workflow_id);
