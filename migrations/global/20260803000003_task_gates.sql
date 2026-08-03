-- External-state gates: park a task until something outside this system says go.
--
-- Dependency edges already express "wait for that task". A gate expresses "wait
-- for that *fact*" — CI is green, the branch merged, an upstream task returned
-- a particular value. The scheduler needs no new concept for it: a task with an
-- unsatisfied gate is simply not promotable, the same way a task with an
-- unfinished parent is not.
--
-- What a gate is NOT is a second predicate language. Conditional steps belong
-- here too — "run the rollback step only if the deploy step reported failure"
-- is a `task_output` gate — rather than in a parallel mechanism that would
-- immediately need its own evaluator, its own UI, and its own bugs.

CREATE TABLE IF NOT EXISTS task_gates (
    id TEXT PRIMARY KEY NOT NULL,
    task_number INTEGER NOT NULL,

    -- http | task_output
    --
    -- Deliberately no vendor SDKs. `http` polls a URL and asserts on the status
    -- or a JSON Pointer into the body, which covers GitHub, GitLab, Buildkite,
    -- and Jenkins without knowing what any of them are. `task_output` reads an
    -- upstream task's stored outputs, which the contract work already persists.
    kind TEXT NOT NULL,

    -- JSON, shape depends on `kind`. Validated when the gate is created, so a
    -- malformed gate is refused while a person is still looking at it rather
    -- than erroring once a minute forever.
    config TEXT NOT NULL,

    -- Human-readable, set by whoever added the gate. The board shows this
    -- rather than the config: "waiting for CI on main" beats a URL.
    label TEXT,

    poll_interval_secs INTEGER NOT NULL DEFAULT 60,
    last_checked_at TEXT,

    -- pending | satisfied | failed | erroring
    --
    -- Four states, not two, and the distinction is the entire lesson of the
    -- promote/re-block loop this codebase already shipped once: one label
    -- covering two conditions with different recovery is how a scheduler ends
    -- up in an infinite loop.
    --
    --   pending    not yet true. Polling is the right response; it may become
    --              true on its own.
    --   satisfied  true. Never re-polled — a gate is a latch, not a live
    --              condition. CI going red an hour after a task started must
    --              not un-start it.
    --   failed     definitively false. CI went red; the PR was closed. Polling
    --              will NOT fix this, so the task parks for a human instead of
    --              burning a request a minute until the heat death of the
    --              universe.
    --   erroring   we could not tell. The endpoint 404s, DNS fails, the config
    --              is wrong. This is *our* problem, not the graph's, and it
    --              must never be silently read as `failed` — "CI is red" and
    --              "we cannot reach CI" call for different humans.
    last_result TEXT NOT NULL DEFAULT 'pending',

    -- Why, in words, for the board. The reason a person can act on.
    last_detail TEXT,

    -- Consecutive evaluation errors, for backoff and escalation. Reset on any
    -- conclusive answer. This is the only defence against a permanently
    -- unreachable endpoint being polled forever.
    consecutive_errors INTEGER NOT NULL DEFAULT 0,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- The sweep asks "does this task have an unsatisfied gate?" on every tick, and
-- the poller asks "which gates are due?". Both are covered here.
CREATE INDEX IF NOT EXISTS idx_task_gates_task ON task_gates(task_number);
CREATE INDEX IF NOT EXISTS idx_task_gates_due ON task_gates(last_result, last_checked_at);
