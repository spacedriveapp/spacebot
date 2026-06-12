-- Instance-level notifications store.
-- Surfaces actionable events to the dashboard Inbox card.

CREATE TABLE IF NOT EXISTS notifications (
    id TEXT PRIMARY KEY,
    -- 'task_approval' | 'worker_failed' | 'cortex_observation'
    kind TEXT NOT NULL,
    -- 'info' | 'warn' | 'error'
    severity TEXT NOT NULL DEFAULT 'info',
    title TEXT NOT NULL,
    body TEXT,
    -- Source agent (NULL means instance-level)
    agent_id TEXT,
    -- Type of the related entity: 'task' | 'worker' | 'cortex_event'
    related_entity_type TEXT,
    -- ID/number of the related entity
    related_entity_id TEXT,
    -- Deep-link the UI action button should navigate to
    action_url TEXT,
    metadata TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    read_at TEXT,
    dismissed_at TEXT
);

-- Fast inbox query: undismissed notifications newest-first
CREATE INDEX IF NOT EXISTS idx_notifications_inbox
    ON notifications(dismissed_at, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_notifications_agent
    ON notifications(agent_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_notifications_entity
    ON notifications(related_entity_type, related_entity_id);

-- Prevent duplicate active notifications for the same entity.
-- When the same event fires repeatedly (e.g. task toggled back to
-- pending_approval), INSERT OR IGNORE silently skips the duplicate.
CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_entity_active
    ON notifications(kind, related_entity_type, related_entity_id)
    WHERE dismissed_at IS NULL;
