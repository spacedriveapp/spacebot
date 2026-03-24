-- Monotonic high-water-mark for task numbering.
-- Prevents number reuse after hard deletes.

CREATE TABLE IF NOT EXISTS task_number_seq (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    next_number INTEGER NOT NULL DEFAULT 1
);

-- Seed from existing data so this is safe to apply to databases that already
-- have tasks.
INSERT OR IGNORE INTO task_number_seq (id, next_number)
VALUES (1, COALESCE((SELECT MAX(task_number) + 1 FROM tasks), 1));
