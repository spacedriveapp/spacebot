-- Task dependency edges plus a typed reason for why a task is parked.
--
-- Edges key on `task_number`, not `id`, because every other subsystem refers to
-- tasks by number — the tool schemas, the API paths, the prompts a worker sees.
-- `task_number` is UNIQUE but not the primary key, so SQLite will not accept it
-- as a foreign key target; referential integrity is enforced in `link_tasks`,
-- which has to reject self-loops and cycles anyway.

CREATE TABLE IF NOT EXISTS task_dependencies (
    parent_task_number INTEGER NOT NULL,
    child_task_number INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (parent_task_number, child_task_number)
);

-- The sweep asks "which parents does this child have"; the completion path asks
-- "which children does this parent unblock". Both directions are hot.
CREATE INDEX IF NOT EXISTS idx_task_deps_child ON task_dependencies(child_task_number);
CREATE INDEX IF NOT EXISTS idx_task_deps_parent ON task_dependencies(parent_task_number);

-- Why a task is parked. dependency | needs_input | capability | transient
--
-- These are not cosmetic labels: each implies a different recovery. `dependency`
-- and `transient` recover on their own, `needs_input` and `capability` are
-- sticky and only a human releases them. A single undifferentiated "blocked"
-- cannot express that — an auto-recovery sweep would either resurrect cards a
-- human deliberately parked, or never resurrect anything.
ALTER TABLE tasks ADD COLUMN block_kind TEXT;
ALTER TABLE tasks ADD COLUMN block_reason TEXT;

-- How many times this task has been unblocked and re-blocked for the same
-- reason. A cron and a worker can otherwise bounce a card between blocked and
-- ready forever; past the limit it escalates to a human instead.
ALTER TABLE tasks ADD COLUMN block_recurrences INTEGER NOT NULL DEFAULT 0;

-- Tasks parked before this migration existed were all budget-exhaustion cases,
-- which is exactly `transient`. Backfilling means the column is never silently
-- NULL for a blocked task, so a reader does not have to guess.
UPDATE tasks SET block_kind = 'transient' WHERE status = 'blocked' AND block_kind IS NULL;

-- The sweep scans for children whose parents may have finished.
CREATE INDEX IF NOT EXISTS idx_tasks_status_block_kind ON tasks(status, block_kind);
