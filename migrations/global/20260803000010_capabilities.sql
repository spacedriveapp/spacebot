-- Capability-based task assignment (#28).
--
-- Work is addressed by name today: every task carries an `assigned_agent_id`
-- and the claim query filters on it. That is push, it works, and it stays the
-- default. What is missing is any notion of *what an agent can do*, so there is
-- nothing for a task to be matched against. These three schema changes add it.

-- What each agent can do, as opaque labels the operator chooses.
--
-- A table rather than a column on some agents row because there is no agents
-- table: an agent is defined in config.toml. This is a projection of that file
-- into the database the scheduler reads, written at startup and whenever an
-- agent is created, updated or deleted through the API. The scheduler needs it
-- here because the two questions it has to answer -- "may this agent claim that
-- task" and "can anything in the fleet claim it at all" -- are both predicates
-- inside a query, and a config file is not joinable.
--
-- Deliberately no taxonomy and no validation. Every scheme invented up front is
-- wrong for the fleet that eventually exists, and an opaque label can be
-- renamed without a migration.
CREATE TABLE agent_capabilities (
    -- The agent that declares it. Plain text, not a foreign key, because the
    -- authority for what agents exist is config.toml rather than this database.
    agent_id TEXT NOT NULL,
    -- One label. Free text on purpose -- see above.
    capability TEXT NOT NULL,
    -- Declaring the same capability twice is the same declaration, so the pair
    -- is the key and a re-sync is idempotent without a delete pass.
    PRIMARY KEY (agent_id, capability)
);

-- The claim query asks "which agents hold this capability", and the sweep asks
-- it once per unclaimed pooled task. Both scan by capability, not by agent.
CREATE INDEX idx_agent_capabilities_capability ON agent_capabilities (capability);

-- What a task needs, for the tasks that say what they need instead of who
-- should do it. A JSON array of labels; NULL on every task that names an agent,
-- which is every task that exists today.
--
-- NULL is the discriminator for the whole feature: NULL means pushed, non-NULL
-- means pooled. It is a permanent property of the task, distinct from whether
-- the task is *currently* claimed -- that is `assigned_agent_id` being empty.
-- Keeping those two apart is what lets the reaper return a crashed pooled task
-- to the pool it came from instead of to the agent that died with it.
ALTER TABLE tasks ADD COLUMN required_capabilities TEXT;

-- Only unclaimed pooled tasks are ever scanned as a pool -- by the claim query
-- looking for work and by the sweep looking for work nothing can do. The
-- partial index keeps both off the tasks table proper, so a fleet that never
-- pools anything pays nothing for this.
CREATE INDEX idx_tasks_pooled ON tasks (status)
    WHERE required_capabilities IS NOT NULL;

-- The same choice at the template level: a step names an agent, or states a
-- requirement. NULL means it names one (or inherits the launching agent), which
-- is every step that exists today.
--
-- Frozen onto the emitted task at launch like the command and worktree columns
-- are, so editing a template mid-run cannot change what a run already in flight
-- is allowed to be claimed by.
ALTER TABLE workflow_steps ADD COLUMN required_capabilities TEXT;
