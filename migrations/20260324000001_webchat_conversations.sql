CREATE TABLE IF NOT EXISTS webchat_conversations (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    title TEXT NOT NULL,
    title_source TEXT NOT NULL DEFAULT 'system',
    archived INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_webchat_conversations_agent_updated
    ON webchat_conversations(agent_id, archived, updated_at DESC);
