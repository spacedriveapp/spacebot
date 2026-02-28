-- Durable outbox for branch/worker completion retriggers.
-- Ensures completion relays can be replayed after queue failures or restarts.

CREATE TABLE IF NOT EXISTS channel_retrigger_outbox (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    result_payload TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_error TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    delivered_at TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_retrigger_outbox_pending
    ON channel_retrigger_outbox(agent_id, channel_id, delivered_at, next_attempt_at);

CREATE INDEX IF NOT EXISTS idx_retrigger_outbox_created
    ON channel_retrigger_outbox(created_at);
