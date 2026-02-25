-- Durable delivery receipts for terminal worker notifications.
--
-- Tracks whether a terminal worker completion notice has been delivered to the
-- user-facing channel, with bounded retry metadata for transient adapter
-- failures.

CREATE TABLE IF NOT EXISTS worker_delivery_receipts (
    id TEXT PRIMARY KEY,
    worker_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    terminal_state TEXT NOT NULL,
    payload_text TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    next_attempt_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    acked_at TIMESTAMP,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(worker_id, kind)
);

CREATE INDEX idx_worker_delivery_receipts_due
    ON worker_delivery_receipts(status, next_attempt_at);

CREATE INDEX idx_worker_delivery_receipts_channel
    ON worker_delivery_receipts(channel_id, created_at);
