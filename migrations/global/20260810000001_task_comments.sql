-- Append-only task comments plus the enrichment cadence timestamp.
--
-- Comments are the durable record of what has been investigated or decided on
-- a task. The task description stays the stable statement of the work itself;
-- everything learned since lands here, chronologically, never edited.

CREATE TABLE task_comments (
    -- Monotonic sequence: breaks ties inside a millisecond so chronological
    -- ordering and cursor pagination are stable across reads.
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author_type TEXT NOT NULL,               -- 'agent' | 'user' | 'worker'
    author_id   TEXT,                        -- agent id, user id, or worker id
    body        TEXT NOT NULL,
    worker_id   TEXT,                        -- worker run this comment summarises
    metadata    TEXT NOT NULL DEFAULT '{}',  -- JSON object
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX task_comments_task ON task_comments(task_id, seq);
CREATE INDEX task_comments_author ON task_comments(task_id, author_type, created_at);

-- Set every time an autonomy run successfully enriches a task. Drives the
-- selection ordering: never-enriched first, then stale, with tasks worked in
-- the previous run held back unless a user has commented since.
--
-- Millisecond precision matches task_comments.created_at so the two compare
-- lexicographically without a format mismatch.
ALTER TABLE tasks ADD COLUMN last_enriched_at TEXT;
CREATE INDEX tasks_last_enriched ON tasks(status, last_enriched_at);
