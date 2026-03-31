-- Per-channel conversation settings for platform channels (Discord, Slack, etc.)
-- Portal conversations store settings in portal_conversations.settings instead.
CREATE TABLE IF NOT EXISTS channel_settings (
    agent_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    settings TEXT NOT NULL DEFAULT '{}',
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (agent_id, conversation_id)
);
