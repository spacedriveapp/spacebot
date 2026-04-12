-- Token usage tracking: one row per process invocation.
CREATE TABLE IF NOT EXISTS token_usage (
    id                  TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    agent_id            TEXT NOT NULL,
    process_type        TEXT NOT NULL,
    conversation_id     TEXT,
    model               TEXT NOT NULL,
    provider            TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens    INTEGER NOT NULL DEFAULT 0,
    request_count       INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd  REAL,
    cost_status         TEXT NOT NULL DEFAULT 'unknown',
    recorded_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_token_usage_agent    ON token_usage(agent_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS idx_token_usage_conv     ON token_usage(conversation_id) WHERE conversation_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_token_usage_recorded ON token_usage(recorded_at DESC);
