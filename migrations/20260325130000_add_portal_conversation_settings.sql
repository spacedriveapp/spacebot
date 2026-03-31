-- Add settings column to portal_conversations table
-- This stores per-conversation settings (JSON) that control behavior

ALTER TABLE portal_conversations ADD COLUMN settings TEXT;

-- Create index for efficient filtering by settings (if we query by specific settings later)
CREATE INDEX IF NOT EXISTS idx_portal_conversations_settings ON portal_conversations(id, settings) WHERE settings IS NOT NULL;
