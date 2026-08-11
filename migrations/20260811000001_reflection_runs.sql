-- Durable reflection-run record for the skill-reflection loop.
--
-- Every reflection pass (riding the memory persistence branch) writes one
-- row: agent/channel identity, trigger provenance, referenced workers,
-- start/end timestamps, terminal status, concise user-legible outcome
-- summary, affected skill identifiers/actions, error/no-op reason, and
-- token usage where available.
--
-- The record distinguishes declared rationale (the branch's own summary)
-- from authoritative runtime outcomes (actions actually observed).
-- Chain-of-thought is never stored.
CREATE TABLE reflection_runs (
    id              TEXT PRIMARY KEY,          -- UUID (branch_id of the persistence branch)
    agent_id        TEXT NOT NULL,
    channel_id      TEXT NOT NULL,
    -- Which trigger fired: 'turn_work' | 'worker_success' | 'reflection'
    trigger_source  TEXT NOT NULL,
    -- Workers referenced by this reflection pass (JSON array of {id, success})
    referenced_workers TEXT,
    started_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at    TIMESTAMP,
    -- Terminal status: 'success' | 'no_op' | 'error' | 'cancelled'
    status          TEXT NOT NULL DEFAULT 'running',
    -- Declared by the reflection branch itself (its own summary of what
    -- it did or decided). Distinct from `observed_actions`.
    declared_rationale TEXT,
    -- Authoritative: what skill mutations actually happened, observed
    -- from tool-call results. JSON array of {action, skill_name, detail?}.
    -- Empty array for no-op runs. Null until the pass completes.
    observed_actions TEXT,
    -- User-legible one-line summary (rendered in the UI timeline).
    -- Derived from observed_actions + declared_rationale.
    outcome_summary TEXT,
    -- Human-readable reason when status is 'no_op' or 'error'.
    terminal_reason TEXT,
    -- Token usage for the reflection branch (JSON: {input, output, cache_read, reasoning}).
    token_usage     TEXT,
    -- Created skills, patched skills (comma-separated lowercase canonical names).
    -- Populated from observed_actions for easy querying.
    affected_skills TEXT
);

CREATE INDEX idx_reflection_runs_channel ON reflection_runs(channel_id, started_at);
CREATE INDEX idx_reflection_runs_agent ON reflection_runs(agent_id, started_at);
