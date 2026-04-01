-- Rename webchat_conversations to portal_conversations
-- This is Phase 0 of the conversation-settings feature

-- Drop any existing indexes on the old table first (idempotent)
DROP INDEX IF EXISTS idx_webchat_conversations_agent_updated;

-- Rename the table
ALTER TABLE webchat_conversations RENAME TO portal_conversations;

-- Recreate the index with the new name
CREATE INDEX IF NOT EXISTS idx_portal_conversations_agent_updated 
    ON portal_conversations(agent_id, archived, updated_at DESC);
