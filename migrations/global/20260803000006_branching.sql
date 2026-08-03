-- Workflow branching: a step declares the condition under which it runs.
--
-- Three questions about a step were being squeezed into two mechanisms. What
-- order does this run in? — dependency edges. Is the outside world ready? —
-- gates. *Should this run at all?* — nothing, so it got expressed as a gate.
--
-- But "is CI green yet?" and "should this branch run?" have the same predicate
-- and opposite failure modes. Waiting forever is correct for the first and a
-- deadlock for the second: a `rollback` step gated on `deploy` reporting `red`
-- can never open once deploy reports `green` and is done, and any step below
-- both branches waits on a parent that is never going to finish.
--
-- That is the one-label-two-conditions bug this codebase has now paid for three
-- times. The fix is one field — what a false answer *means* — plus a task
-- status for "settled and will never run", plus the ability for a template to
-- declare a gate at all.

-- Why a task will never run, in words, for whoever reads the card.
--
-- Its own column rather than a reuse of `block_reason`, and that is the whole
-- point. `block_reason` comes with the block machinery — `block_kind`,
-- `block_recurrences`, the sticky kinds, the unblock path, the recurrence
-- limiter — none of which has anything to do with a branch that was not taken.
-- Overloading it would tie skip to escalation rules that would then fire on a
-- pipeline behaving exactly as designed, which is the same overloading this
-- migration exists to stop repeating.
--
-- `skipped` itself is a value of the existing `status` column: there is no
-- CHECK constraint on it, so the seventh status needs no schema change. It is
-- terminal. There is deliberately no un-skip in v1 — a task that could un-skip
-- makes "settled" meaningless and puts the ready sweep straight back into the
-- promote/re-block territory it escaped.
ALTER TABLE tasks ADD COLUMN skip_reason TEXT;

-- What a *false* answer from this gate means. NULL = derive it.
--
--   wait   not yet. Poll again. This is every gate's behaviour today.
--   route  no. This step does not apply; it is settled and will never run.
--
-- Nullable because the right answer is nearly always derivable, and a field the
-- author must set correctly for the graph not to deadlock is a field that will
-- eventually be set wrong. The derivation is a fact rather than a heuristic:
-- if the source is a task output and that task has settled, nothing can change
-- the answer, so a false answer is final and routes. If the source is http, or
-- the source task has not finished, the answer can still change, so it waits.
--
-- The override exists for what the derivation cannot see: an http gate polling
-- a decision endpoint that really is final, or a task_output condition that
-- should hold the whole pipeline rather than skip past it.
--
-- What `route` must NEVER act on is `erroring`. Being unable to reach CI is not
-- CI saying no, and skipping a branch because DNS failed is the worst thing
-- this feature could do. Only `pending` and `failed` — the two states that mean
-- "we asked and the answer was not yes" — can route.
ALTER TABLE task_gates ADD COLUMN disposition TEXT;

-- A gate declared by a *template* rather than against one run's task numbers.
--
-- The mechanical half of the problem, and a prerequisite for the rest: today
-- `task_gates` is keyed by `task_number` and a template has only step keys, so
-- branching on the dev board had to be assembled by a script that launched the
-- run, read back the emitted numbers, and POSTed gates against them. Branching
-- was therefore a property of one run, not of the template — launch it again
-- and there were no branches.
--
-- This mirrors `workflow_step_bindings` exactly, for the same reason and with
-- the same translation: addressed by name here, compiled into real `task_gates`
-- rows at launch, with `source_step_key` becoming the task number that step was
-- compiled into. Well-trodden ground rather than a new idea.
CREATE TABLE IF NOT EXISTS workflow_step_gates (
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,

    -- The step this gate holds back. Not a row id, for the same reason edges
    -- and bindings use step keys: a template is edited by hand and a stable
    -- human-readable name survives that.
    step_key TEXT NOT NULL,

    -- Author-named, and part of the primary key, so saving the same gate twice
    -- is an edit rather than a second gate. A generated id would make an
    -- idempotent PUT impossible and let a form submitted twice hold a step
    -- behind two copies of one condition.
    gate_key TEXT NOT NULL,

    -- http | task_output. The same two kinds `task_gates` has; this table adds
    -- no evaluator and no second predicate language.
    kind TEXT NOT NULL,

    -- For `task_output`: whose output to read, by name. This is the column that
    -- does not exist at the task level, because at the task level it has
    -- already been resolved into `config.task_number`.
    source_step_key TEXT,

    -- JSON, exactly the shape `task_gates.config` takes — RFC 6901 pointer plus
    -- `equals` / `any_of`. For `task_output` the compiler injects
    -- `task_number`, which is the entire translation step.
    config TEXT NOT NULL,

    -- What the board should call this. "needs legal review" beats a pointer.
    label TEXT,

    poll_interval_secs INTEGER NOT NULL DEFAULT 60,

    -- NULL = derive; wait | route. Same field, same meaning, and same reason
    -- for being nullable as on `task_gates` above. Set at authoring time
    -- because that is when the author knows.
    disposition TEXT,

    PRIMARY KEY (workflow_id, step_key, gate_key)
);

-- The editor loads every gate of one template alongside its steps and edges,
-- and launch reads them all once. Both are covered by the workflow id.
--
-- Nothing is added for the dependency rule itself. A skipped parent settles a
-- child exactly as a done one does, so the promote and claim paths changed from
-- `status <> 'done'` to `status NOT IN ('done', 'skipped')` — a different
-- predicate over the same rows, reached through the same existing indexes.
CREATE INDEX IF NOT EXISTS idx_workflow_gates_workflow ON workflow_step_gates(workflow_id);
