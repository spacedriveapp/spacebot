-- Index on created_at for temporal queries (recent memory retrieval).
CREATE INDEX IF NOT EXISTS idx_memories_created_at ON memories(created_at);
