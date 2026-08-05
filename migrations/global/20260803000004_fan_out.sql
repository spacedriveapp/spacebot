-- Dynamic fan-out: run a step once per item an upstream step produced.
--
-- Static fan-out already works — two steps sharing a parent run concurrently,
-- and a step with two parents waits for both. What a fixed graph cannot express
-- is a width nobody knows at launch: "the scan found five repos, build each of
-- them". `source_pointer` selects one value; nothing selects many, and the count
-- only exists once the upstream task has finished.
--
-- So the run graph has to grow after launch. That is the whole difficulty:
-- `launch()` is a compile step that emits every task at once, and until now the
-- scheduler never had a node appear underneath it.

-- Which upstream step produces the collection, and where to find it.
--
-- A pointer that resolves to something other than an array is a template
-- mistake, and it is reported as one rather than silently producing zero
-- branches — "it did nothing" and "it iterated an empty list" must not look
-- alike from the outside.
ALTER TABLE workflow_steps ADD COLUMN for_each_step_key TEXT;
ALTER TABLE workflow_steps ADD COLUMN for_each_pointer TEXT;

-- A pointer *within each item* naming that branch, e.g. `/name` over
-- `{"name": "repo-a"}` labels the branch `repo-a`.
--
-- This is what makes fan-in keyed rather than positional. Without it the branch
-- index is used and the keys come out `0`, `1`, `2` — honest, but far less
-- useful in a report, and a positional list silently mismatches the moment one
-- branch is retried or the upstream reorders its output.
ALTER TABLE workflow_steps ADD COLUMN for_each_key TEXT;

-- Which branch a task is, once the fan-out has expanded.
--
-- NULL on every ordinary task, and on the placeholder that holds the shape
-- before expansion.
ALTER TABLE tasks ADD COLUMN fan_out_branch_key TEXT;

-- The placeholder that stands in for a fan-out step between launch and
-- expansion.
--
-- It exists because the downstream steps need something to wait on. Emitting
-- them with no parent would have the first sweep promote them and run the
-- report before anything was built. So the placeholder carries exactly the
-- edges the branches will inherit — source as parent, downstream as children —
-- and is never claimed. The sweep must skip it explicitly; a task that sits in
-- the backlog forever and is invisible to the thing that promotes tasks is a
-- deadlock nobody can see.
ALTER TABLE tasks ADD COLUMN fan_out_placeholder INTEGER NOT NULL DEFAULT 0;

-- Expansion asks "which placeholder is waiting on the task that just
-- finished?", so the lookup is by the source task's number via the edge table
-- plus this flag.
CREATE INDEX IF NOT EXISTS idx_tasks_fan_out_placeholder
    ON tasks(fan_out_placeholder) WHERE fan_out_placeholder = 1;

CREATE INDEX IF NOT EXISTS idx_tasks_fan_out_branch
    ON tasks(workflow_run_id, workflow_step_key);

-- Fan-in: one input collecting every branch's output.
--
-- The task-level binding table addresses a single upstream task by number,
-- which cannot name a set that does not exist yet. This column says "collect
-- from every task in my run that came from this step", resolved at claim time
-- against whatever the expansion produced.
--
-- Resolution fails while any branch is unfinished. That is the ordinary state
-- on every sweep before they finish — "not yet", not "never" — and it must
-- surface as a dependency wait rather than an unresolvable contract, or the
-- report parks itself the moment the pipeline starts.
ALTER TABLE task_input_bindings ADD COLUMN fan_in_step_key TEXT;
