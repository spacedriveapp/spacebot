CREATE TABLE IF NOT EXISTS agent_links (
    id TEXT PRIMARY KEY,
    from_agent_id TEXT NOT NULL,
    to_agent_id TEXT NOT NULL,
    direction TEXT NOT NULL DEFAULT 'two_way',
    relationship TEXT NOT NULL DEFAULT 'peer',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(from_agent_id, to_agent_id)
);

CREATE INDEX IF NOT EXISTS idx_agent_links_from ON agent_links(from_agent_id);
CREATE INDEX IF NOT EXISTS idx_agent_links_to ON agent_links(to_agent_id);
