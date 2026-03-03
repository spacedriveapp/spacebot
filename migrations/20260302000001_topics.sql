-- Topic synthesis: addressable, living context documents maintained by the cortex.

CREATE TABLE IF NOT EXISTS topics (
    id              TEXT PRIMARY KEY,
    agent_id        TEXT NOT NULL,
    title           TEXT NOT NULL,
    content         TEXT NOT NULL DEFAULT '',
    criteria        TEXT NOT NULL,
    pin_ids         TEXT NOT NULL DEFAULT '[]',
    status          TEXT NOT NULL DEFAULT 'active',
    max_words       INTEGER NOT NULL DEFAULT 1500,
    last_memory_at  TEXT,
    last_synced_at  TEXT,
    created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_topics_agent ON topics(agent_id);
CREATE INDEX IF NOT EXISTS idx_topics_status ON topics(status);

CREATE TABLE IF NOT EXISTS topic_versions (
    id              TEXT PRIMARY KEY,
    topic_id        TEXT NOT NULL,
    content         TEXT NOT NULL,
    memory_count    INTEGER NOT NULL,
    created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (topic_id) REFERENCES topics(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_topic_versions_topic ON topic_versions(topic_id);
