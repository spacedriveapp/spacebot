CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    task_number INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'backlog',
    priority TEXT NOT NULL DEFAULT 'medium',
    subtasks TEXT,
    metadata TEXT,
    source_memory_id TEXT,
    worker_id TEXT,
    created_by TEXT NOT NULL,
    approved_at TIMESTAMP,
    approved_by TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    UNIQUE(agent_id, task_number)
);

CREATE INDEX IF NOT EXISTS idx_tasks_agent ON tasks(agent_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_number ON tasks(agent_id, task_number);
CREATE INDEX IF NOT EXISTS idx_tasks_source_memory ON tasks(source_memory_id);
CREATE INDEX IF NOT EXISTS idx_tasks_worker ON tasks(worker_id);
