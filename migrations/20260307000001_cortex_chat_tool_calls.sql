-- Add tool_calls column to cortex_chat_messages for persisting tool call data
-- alongside assistant messages. Stored as a JSON array of tool call objects.
ALTER TABLE cortex_chat_messages ADD COLUMN tool_calls TEXT;
