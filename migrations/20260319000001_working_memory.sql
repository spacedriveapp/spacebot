-- Working memory events: the append-only temporal event log.
-- Records what happens around conversations (worker lifecycle, branch
-- conclusions, cron executions, decisions, errors) — NOT user messages
-- or agent responses, which live in conversation_messages.
CREATE TABLE IF NOT EXISTS working_memory_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    timestamp TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    channel_id TEXT,
    user_id TEXT,
    summary TEXT NOT NULL,
    detail TEXT,
    importance REAL NOT NULL DEFAULT 0.5,
    day TEXT NOT NULL
);

CREATE INDEX idx_wm_events_day ON working_memory_events(day, timestamp);
CREATE INDEX idx_wm_events_channel ON working_memory_events(channel_id, timestamp);
CREATE INDEX idx_wm_events_type ON working_memory_events(event_type, timestamp);
CREATE INDEX idx_wm_events_user ON working_memory_events(user_id, timestamp);

-- Intra-day synthesis: rolling narrative blocks within a day.
-- Each row is a 50-100 word paragraph covering a batch of events.
CREATE TABLE IF NOT EXISTS working_memory_intraday_syntheses (
    id TEXT PRIMARY KEY,
    day TEXT NOT NULL,
    time_range_start TIMESTAMP NOT NULL,
    time_range_end TIMESTAMP NOT NULL,
    summary TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_wm_intraday_day ON working_memory_intraday_syntheses(day, time_range_start);

-- Daily summaries: cortex-synthesized narratives per day.
-- One row per day, never pruned.
CREATE TABLE IF NOT EXISTS working_memory_daily_summaries (
    day TEXT PRIMARY KEY,
    summary TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
