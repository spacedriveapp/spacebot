-- Add transcript storage and worker metadata to worker_runs.
ALTER TABLE worker_runs ADD COLUMN worker_type TEXT NOT NULL DEFAULT 'builtin';
ALTER TABLE worker_runs ADD COLUMN agent_id TEXT;
ALTER TABLE worker_runs ADD COLUMN transcript BLOB;

CREATE INDEX idx_worker_runs_agent ON worker_runs(agent_id, started_at);
