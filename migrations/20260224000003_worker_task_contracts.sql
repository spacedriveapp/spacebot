-- Deterministic worker task contracts.
--
-- Tracks acknowledgement/progress/terminal guarantees for worker executions so
-- long-running tasks always provide bounded feedback and reach terminal states.

CREATE TABLE IF NOT EXISTS worker_task_contracts (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    worker_id TEXT NOT NULL UNIQUE,
    task_summary TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'created',
    ack_deadline_at TIMESTAMP NOT NULL,
    progress_deadline_at TIMESTAMP NOT NULL,
    terminal_deadline_at TIMESTAMP NOT NULL,
    last_progress_at TIMESTAMP,
    last_status_hash TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    sla_nudge_sent INTEGER NOT NULL DEFAULT 0,
    terminal_state TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_worker_task_contracts_channel_state
    ON worker_task_contracts(channel_id, state);

CREATE INDEX idx_worker_task_contracts_ack_due
    ON worker_task_contracts(state, ack_deadline_at);

CREATE INDEX idx_worker_task_contracts_progress_due
    ON worker_task_contracts(state, progress_deadline_at);

CREATE INDEX idx_worker_task_contracts_terminal_due
    ON worker_task_contracts(state, terminal_deadline_at);
