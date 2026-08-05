-- Bounded loops: run a body again until it converges, up to a ceiling.
--
-- "Patch it, run the tests, and if they fail try again — up to three times."
-- Retry-on-failure cannot express that: the failure budget re-runs *one* task
-- with the same inputs, and a loop re-runs a *body* with the previous
-- iteration's output as its input. That is the difference between trying again
-- and making progress.
--
-- The scheduler is a DAG and stays one. The *template* holds the cycle; each
-- iteration emits a fresh set of tasks with fresh numbers, so the run graph is
-- acyclic and merely grows. Launch-time cycle detection is unchanged and still
-- rejects accidental cycles, because a loop is *declared* here — never inferred
-- from an edge that points backwards.

-- Which body a step belongs to.
--
-- A loop is one or more steps sharing this name. A body of one step is the
-- degenerate case and needs no special handling. NULL on every ordinary step,
-- which is what keeps the cost of this feature zero for pipelines that do not
-- loop.
ALTER TABLE workflow_steps ADD COLUMN loop_group TEXT;

-- How many times the body may run before the loop gives up.
--
-- Author-set, NULL meaning the default of 3. There is a hard ceiling in code as
-- well: this is the one thing in the system that creates tasks in response to
-- tasks finishing, and an unbounded count here is unbounded spend against a
-- live model.
--
-- Read from the body's *terminal* step only. A non-terminal body step that set
-- it would be a field nothing consumes, so launch refuses that outright rather
-- than letting a number sit in a row and do nothing.
ALTER TABLE workflow_steps ADD COLUMN loop_max_iterations INTEGER;

-- The exit predicate, as the same JSON object a `task_output` gate takes:
-- `{"pointer": "/tests/passed", "equals": true}`.
--
-- Deliberately not a second predicate language. Conditional steps, external
-- gating, and loop exit are the same question asked in three places, and three
-- dialects of it would be three sets of bugs — so this is evaluated by the
-- gate evaluator, unchanged.
--
-- Required on the terminal step of every loop group. A body with no exit
-- condition is a body that always burns its whole budget, which is a template
-- mistake worth refusing while the author is still looking at it.
ALTER TABLE workflow_steps ADD COLUMN loop_until TEXT;

-- What an edge *means*, now that "the loop finished" is two outcomes.
--
--   normal        follow when the loop converged
--   on_exhausted  follow when it ran out of attempts
--
-- Converging and giving up have opposite meanings, and routing both into one
-- downstream step is the single-label-two-conditions bug that has already cost
-- this codebase three separate incidents. A pipeline that merges after three
-- successful attempts must not also merge after three failed ones.
--
-- Not part of the primary key, on purpose: two steps have one relationship.
-- Wanting the same pair wired both ways is precisely the merge this column
-- exists to prevent.
ALTER TABLE workflow_step_edges ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal';

-- Which body an emitted task belongs to, and which pass it is.
--
-- `loop_iteration` is 1-based and identifies the pass, not the attempt: a task
-- retried under the failure budget keeps its iteration, because retrying is not
-- looping. NULL on every ordinary task.
ALTER TABLE tasks ADD COLUMN loop_group TEXT;
ALTER TABLE tasks ADD COLUMN loop_iteration INTEGER;

-- Whether this task is the body's exit point.
--
-- The terminal task is the one whose outputs `loop_until` reads and the one the
-- iteration boundary is decided on, so the sweep has to find it without parsing
-- every body task's metadata. Exactly the tasks carrying a frozen loop spec
-- have this set.
ALTER TABLE tasks ADD COLUMN loop_terminal INTEGER NOT NULL DEFAULT 0;

-- What the boundary decided for this iteration, once it has decided.
--
--   converged          the predicate held; the normal edges were followed
--   iterated           it did not hold and the next iteration was emitted
--   exhausted_routed   out of attempts, and an on_exhausted edge took it
--   exhausted_blocked  out of attempts with nowhere to go; parked for a person
--
-- Four values rather than one "handled" flag, for the same reason the edge has
-- a kind: these recover differently, and a run that gave up must not read like
-- a run that succeeded.
--
-- It is also the concurrency guard. The boundary is reached by two paths — the
-- sweep, which runs every tick, and task completion — and an emit path that can
-- fire twice for one iteration creates tasks without bound. Setting this is a
-- conditional update inside the emit transaction (`WHERE loop_resolution IS
-- NULL`); zero rows affected means another caller got there first and the
-- transaction rolls back having emitted nothing. Structural, not check-then-act.
ALTER TABLE tasks ADD COLUMN loop_resolution TEXT;

-- This task is downstream of a loop and waits on its verdict.
--
-- A loop's exit is a *branch*, and both arms of it are conditional. Neither can
-- simply wait on the body: the body finishes whether the loop converged or gave
-- up, so a task released by mere completion runs on both paths — which is the
-- merge the edge kind above exists to prevent, arriving by the back door. Nor
-- can either be left with no parent, which would have the first sweep run it
-- before the loop had run at all.
--
-- So both are emitted held, with the reason on the card, and the ready sweep
-- skips a task while this is set. The boundary releases the arm that was taken
-- and leaves the other set, rewriting its reason to say which way the loop went
-- and that this step will not run. Held rather than deleted, because a branch
-- that was not taken is part of what happened.
ALTER TABLE tasks ADD COLUMN awaiting_loop_group TEXT;

-- Which arm of that branch this task is on: `normal` or `on_exhausted`.
--
-- Separate from the group, because "waiting on a loop" is not the condition —
-- there are two conditions, they are opposites, and one column holding both
-- would release the wrong half. The pair is what the boundary matches on.
ALTER TABLE tasks ADD COLUMN awaiting_loop_arm TEXT;

-- The second guard on double emission, independent of `loop_resolution`.
--
-- One step of one run produces at most one task per iteration. If two callers
-- ever reached the emit path for the same iteration, the loser's INSERT fails
-- here and its transaction unwinds — so the worst case is a rolled-back
-- transaction rather than a body that quietly ran twice.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_loop_iteration
    ON tasks(workflow_run_id, workflow_step_key, loop_iteration)
    WHERE loop_iteration IS NOT NULL;

-- The boundary asks "is every task of this body's current iteration done?", and
-- the emit path then reads that whole iteration back to copy it.
CREATE INDEX IF NOT EXISTS idx_tasks_loop_body
    ON tasks(workflow_run_id, loop_group, loop_iteration)
    WHERE loop_group IS NOT NULL;

-- The boundary releases one arm of one loop's branch, and rewrites the reason
-- on the other.
CREATE INDEX IF NOT EXISTS idx_tasks_awaiting_loop
    ON tasks(workflow_run_id, awaiting_loop_group, awaiting_loop_arm)
    WHERE awaiting_loop_group IS NOT NULL;
