-- Durable worker event journal for debugging and UX timeline recovery.
--
-- Captures lifecycle checkpoints (started/progress/tool activity/completed) as
-- append-only records tied to worker_runs.

CREATE TABLE IF NOT EXISTS worker_events (
    id TEXT PRIMARY KEY,
    worker_id TEXT NOT NULL,
    channel_id TEXT,
    agent_id TEXT,
    event_type TEXT NOT NULL,
    payload_json TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (worker_id) REFERENCES worker_runs(id) ON DELETE CASCADE
);

CREATE INDEX idx_worker_events_worker
    ON worker_events(worker_id, created_at);

CREATE INDEX idx_worker_events_channel
    ON worker_events(channel_id, created_at);

CREATE INDEX idx_worker_events_agent
    ON worker_events(agent_id, created_at);
