-- Run state: a launch that knows how it is going, and a ceiling on what it costs.
--
-- `workflow_runs` was `(id, workflow_id, inputs, launched_by, created_at)`. A
-- run had no status, so every caller that wanted to know how one was going
-- loaded its tasks and reduced them — differently each time, and none of them
-- could answer the question that actually matters.
--
-- That question is not "did it succeed". It is **"is it stuck"**, and it is not
-- derivable from any single task. A loop whose body task is permanently blocked
-- never reaches "all done", so no boundary fires. A `route` gate stuck at
-- `erroring` past GATE_ERROR_LIMIT stops polling, so its branch is never
-- decided. In both cases every task looks individually reasonable and the run
-- as a whole cannot advance. Being unable to advance is a property of the run,
-- which is exactly why the run needs a state of its own rather than a reduction
-- over rows that each look fine.

-- How the run is going. running | succeeded | failed | stuck | cancelled.
--
--   running    something is in flight, promotable, or waiting on a gate that
--              can still open
--   succeeded  every task settled and no failure path was taken
--   failed     a task used its whole failure budget, or a loop ran out of
--              attempts and took its on_exhausted edge
--   stuck      nothing in flight, nothing promotable, no gate that can still
--              open — and not finished
--   cancelled  a person stopped it
--
-- No CHECK constraint, matching `tasks.status`: the parser in
-- `workflows::store::RunStatus` is the single definition and a constraint here
-- would be a second one to keep in step.
--
-- `stuck` and `running` are the pair this column exists to separate, and they
-- are the one-label-two-conditions trap this codebase has now paid for four
-- times. A run waiting on a pollable gate is *waiting*: the world has not
-- answered yet and will. A run whose only unfinished task is behind a gate that
-- stopped polling is *stuck*: nothing will ever answer. Both look like "no
-- progress" from outside, and they recover completely differently — one by
-- waiting, one by a person. A detector that conflates them either parks healthy
-- runs, which trains people to ignore the signal, or reports nothing, which is
-- the silence we have now.
ALTER TABLE workflow_runs ADD COLUMN status TEXT NOT NULL DEFAULT 'running';

-- When the run stopped, in any of the four terminal senses.
--
-- Written by the same conditional UPDATE that writes the terminal status, never
-- on its own: a `finished_at` that can be set while the status still says
-- `running` is a third state nobody declared. NULL for exactly as long as the
-- run is `running`.
ALTER TABLE workflow_runs ADD COLUMN finished_at TEXT;

-- Why the run reached that status, in words, for whoever reads it.
--
-- "Stuck" alone sends someone reading rows, which is the failure this whole
-- feature exists to remove. Every terminal transition names its cause: which
-- task is blocked and what its card says, which gate will not open, which
-- declared limit was hit and what its value is. A run stopped by a ceiling that
-- does not say which ceiling is indistinguishable from a bug.
--
-- It also carries the distinction that does *not* deserve its own status.
-- "Succeeded" and "succeeded with a branch skipped" have identical recovery —
-- none — so they are one status, and the difference is a sentence here rather
-- than a sixth value that every caller would then have to treat as success
-- anyway. The rule is that two conditions share a label only when they share a
-- recovery; these do, and `stuck` versus `running` does not.
ALTER TABLE workflow_runs ADD COLUMN status_reason TEXT;

-- The supervisor's only query: the runs still worth judging.
--
-- Partial, because it is asked on a timer and a terminal run is never asked
-- about again — the index holds the handful of live runs rather than every run
-- this instance has ever launched. Also what makes the terminal transition
-- cheap: it is a conditional UPDATE keyed by id whose `status = 'running'`
-- guard is the once-only latch behind the notification.
CREATE INDEX IF NOT EXISTS idx_workflow_runs_active
    ON workflow_runs(created_at)
    WHERE status = 'running';
