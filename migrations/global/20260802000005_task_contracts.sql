-- Typed input/output contracts between tasks.
--
-- Dependency edges (the previous migration) say *that* one task waits for
-- another. They say nothing about what actually passes between them, so a
-- downstream task discovers a missing field at runtime by reading a prompt and
-- guessing. These columns make the handoff declared and checked.
--
-- Every column is nullable and every check is skipped when the schema is
-- absent, so a task without a contract behaves exactly as it does today.
-- Contracts are opt-in and become the norm once there is a builder to author
-- them.

-- JSON Schema describing what this task requires before it can run.
ALTER TABLE tasks ADD COLUMN input_schema TEXT;

-- JSON Schema describing what this task must produce to be considered done.
ALTER TABLE tasks ADD COLUMN output_schema TEXT;

-- The resolved input object, written at claim time once every binding has been
-- read from its source. Persisted rather than recomputed so the value a worker
-- actually saw survives a crash and stays auditable after upstream tasks change.
ALTER TABLE tasks ADD COLUMN inputs TEXT;

-- The validated output object, written on completion. This is what downstream
-- tasks read from.
ALTER TABLE tasks ADD COLUMN outputs TEXT;

-- Where each of a task's inputs comes from.
--
-- A binding is either a pointer into an upstream task's outputs, or a literal
-- baked into the graph. `source_task_number` NULL means literal. Keeping both
-- in one table means the resolver has a single code path and the UI has a
-- single place to show "where does this value come from".
CREATE TABLE IF NOT EXISTS task_input_bindings (
    child_task_number INTEGER NOT NULL,
    -- Key in the child's input object.
    input_key TEXT NOT NULL,
    -- Upstream task to read from. NULL for a literal.
    source_task_number INTEGER,
    -- RFC 6901 JSON Pointer into that task's outputs, e.g. "/image/tag".
    -- Empty string means the whole outputs object.
    source_pointer TEXT,
    -- JSON literal, used when source_task_number IS NULL.
    literal_value TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (child_task_number, input_key)
);

-- Resolution walks every binding for one child at claim time.
CREATE INDEX IF NOT EXISTS idx_task_bindings_child
    ON task_input_bindings(child_task_number);

-- Completing a task asks which downstream inputs just became resolvable.
CREATE INDEX IF NOT EXISTS idx_task_bindings_source
    ON task_input_bindings(source_task_number);
