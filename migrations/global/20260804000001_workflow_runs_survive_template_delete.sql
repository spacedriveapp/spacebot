-- Run history survives the recipe it came from.
--
-- `workflow_runs.workflow_id` was declared `REFERENCES workflows(id) ON DELETE
-- CASCADE`, which made deleting a template delete every run it ever launched —
-- the exact outcome the schema's own comment on `tasks.workflow_run_id` rules
-- out: "deleting a workflow must not cascade into deleting the history of work
-- that was actually done." A run is the record of work that happened; throwing
-- the recipe away changes nothing about that, and the run view (`GET
-- /workflows/{id}/runs` and everything that reads `workflow_runs` by id) is
-- how a failed launch is explained after the fact.
--
-- SQLite cannot drop a foreign key, so the table is rebuilt the way
-- `20260404000001_projects_global.sql` rebuilt `projects`: copy into a new
-- table, drop, rename. `workflow_id` becomes plain text — kept, not nulled,
-- because a run that no longer names its template cannot be explained — and
-- the join back to `workflows` is best-effort, matching
-- `tasks.workflow_run_id` and `workflow_run_worktrees.run_id`, which were
-- deliberately never foreign keys for the same reason. Nothing references
-- `workflow_runs` by foreign key, so the drop cannot trip enforcement.
--
-- The template-scoped tables — steps, edges, bindings, gates, schedules,
-- webhooks — keep their cascades. They describe the recipe and are meaningless
-- without it; the runs are not and are not.

CREATE TABLE workflow_runs_new (
    id TEXT PRIMARY KEY NOT NULL,
    -- Plain text, not a foreign key: a run outlives its template.
    workflow_id TEXT NOT NULL,
    inputs TEXT NOT NULL,
    launched_by TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    finished_at TEXT,
    status_reason TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

INSERT INTO workflow_runs_new
    (id, workflow_id, inputs, launched_by, status, finished_at, status_reason, created_at)
SELECT id, workflow_id, inputs, launched_by, status, finished_at, status_reason, created_at
FROM workflow_runs;

DROP TABLE workflow_runs;
ALTER TABLE workflow_runs_new RENAME TO workflow_runs;

-- The indexes dropped with the old table, recreated as they were: the run
-- view's grouping, and the supervisor's partial index over live runs.
CREATE INDEX idx_workflow_runs_workflow ON workflow_runs(workflow_id);
CREATE INDEX idx_workflow_runs_active
    ON workflow_runs(created_at)
    WHERE status = 'running';
