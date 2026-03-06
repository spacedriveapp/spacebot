-- Track whether a worker is interactive (accepts follow-up input) or fire-and-forget.
ALTER TABLE worker_runs ADD COLUMN interactive BOOLEAN NOT NULL DEFAULT FALSE;
