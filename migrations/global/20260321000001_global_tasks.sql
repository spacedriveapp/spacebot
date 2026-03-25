-- Global task board: instance-level tasks shared across all agents.
-- Replaces per-agent task tables with a single globally-numbered store.

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    task_number INTEGER NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'backlog',
    priority TEXT NOT NULL DEFAULT 'medium',

    -- Ownership: the agent that created this task.
    owner_agent_id TEXT NOT NULL,
    -- Assignment: the agent responsible for executing this task.
    assigned_agent_id TEXT NOT NULL,

    subtasks TEXT,                    -- JSON array of {title, completed} objects
    metadata TEXT,                    -- JSON object, arbitrary key-value

    source_memory_id TEXT,           -- conceptual FK to a memory in the owner agent's store
    worker_id TEXT,                  -- set when a worker is executing this task

    created_by TEXT NOT NULL,        -- 'cortex', 'human', 'branch', 'agent:<id>'
    approved_at TEXT,
    approved_by TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_owner ON tasks(owner_agent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_assigned ON tasks(assigned_agent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_worker ON tasks(worker_id);
CREATE INDEX IF NOT EXISTS idx_tasks_priority_status ON tasks(status, priority);
CREATE INDEX IF NOT EXISTS idx_tasks_source_memory ON tasks(source_memory_id);
