-- Per-attempt execution log for tasks, plus a failure budget on the task itself.
--
-- Before this, a failed picked-up task was requeued straight back to 'ready'
-- with no memory of having failed, so a permanently-failing task looped
-- forever. `consecutive_failures` bounds that; `task_runs` records why.
--
-- One row per attempt. A task retried after a crash or timeout has multiple
-- rows, ordered by `attempt`.

CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY NOT NULL,
    task_number INTEGER NOT NULL,
    attempt INTEGER NOT NULL,

    -- The worker that executed this attempt. NULL when the attempt failed
    -- before a worker was spawned.
    worker_id TEXT,

    -- completed | failed | timeout | cancelled | blocked | rate_limited
    -- NULL while the attempt is still running.
    outcome TEXT,

    -- Human-readable result or failure summary.
    summary TEXT,
    -- Raw error text when the attempt did not succeed.
    error TEXT,

    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ended_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_task_runs_task ON task_runs(task_number, attempt);
CREATE INDEX IF NOT EXISTS idx_task_runs_worker ON task_runs(worker_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_task_runs_task_attempt
    ON task_runs(task_number, attempt);

-- Failure budget. Reset to 0 on any successful completion, and on an operator
-- retry (a human looked at it, so the budget starts over).
ALTER TABLE tasks ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;

-- Per-task override of the instance default failure limit. NULL = use default.
ALTER TABLE tasks ADD COLUMN max_retries INTEGER;

-- Last failure text, kept on the task so the board can show why it is parked
-- without joining task_runs.
ALTER TABLE tasks ADD COLUMN last_error TEXT;
