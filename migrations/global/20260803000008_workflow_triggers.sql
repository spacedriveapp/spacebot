-- Triggers: something other than a person starting a run.
--
-- The only non-test caller of `WorkflowStore::launch` was the HTTP handler, so
-- a workflow was a reusable procedure that nothing autonomous could reuse. The
-- three triggers all want the same thing — a launch identity that is not a
-- person — and `workflow_runs.launched_by` already accommodates it. Two of them
-- need storage: a schedule is a stored intent that has to survive a restart,
-- and a webhook is a stored secret plus a stored payload mapping. The third,
-- the `launch_workflow` tool, needs none: its launch identity is the filing
-- task it is called from (`task:<n>`), which is the same `created_by` value the
-- existing filing-depth walk already reads, so the recursion guard that stops a
-- worker filing cards without limit stops a workflow launching a workflow with
-- no new column at all.

-- A schedule attached to a workflow, launching with a stored input.
--
-- A sibling of `cron_jobs` rather than an extension of it, for two reasons that
-- are each sufficient. First, `cron_jobs` lives in the *per-agent* database and
-- `workflows` lives in this one — a foreign key between them is not expressible
-- and the join would be a lie. Second, a cron job is job-shaped: it has a
-- `prompt` and a `delivery_target` because what it does is run a model and post
-- the answer. A workflow schedule has neither and never will; forcing it into
-- that table would mean two columns that must be NULL for one kind of row and
-- NOT NULL for the other, with nothing but convention keeping them apart.
--
-- What *is* reused is the scheduler's mechanism rather than its table: the same
-- 5-field cron expression vocabulary, the same `next_run_at` cursor, and the
-- same conditional-UPDATE claim that makes a fire happen exactly once when
-- several processes are watching the same row.
CREATE TABLE IF NOT EXISTS workflow_schedules (
    id TEXT PRIMARY KEY NOT NULL,

    -- Deleting a template deletes its schedules. Unlike a run — which is
    -- history and must outlive the recipe — a schedule is an intent to launch
    -- something, and an intent to launch a workflow that no longer exists is
    -- not history, it is a timer that fires into a refusal forever.
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,

    -- Human-readable name, so a schedule that misfires can be talked about.
    -- Not unique: two schedules of the same pipeline at different times are a
    -- normal thing to want, and the id is what identifies one.
    name TEXT NOT NULL,

    -- Wall-clock schedule, 5-field cron syntax, expanded and parsed by the same
    -- code path `cron_jobs` uses. NULL falls back to `interval_secs`.
    --
    -- Interpreted in **UTC**, deliberately, where a cron job is interpreted in
    -- its owning agent's configured timezone. A cron job has exactly one agent
    -- and therefore exactly one answer; a workflow schedule is instance-level
    -- and is swept by whichever agent's supervisor tick gets there first, so
    -- taking the timezone from the sweeper would make the same row fire at a
    -- different hour depending on who noticed it. A fixed zone is worse for
    -- humans and correct for machines, and correctness is the property a
    -- trigger cannot trade away.
    cron_expr TEXT,

    -- Fallback period when `cron_expr` is NULL. Same meaning as the cron job
    -- column of the same name.
    interval_secs INTEGER NOT NULL DEFAULT 3600,

    -- The launch payload, stored whole. A literal because a schedule cannot
    -- prompt: there is nobody to ask at 03:00, so the input has to have been
    -- decided when the schedule was written. Validated against the workflow's
    -- `input_schema` at every fire rather than once at save time, because the
    -- template can change underneath a schedule that was correct when saved.
    inputs TEXT NOT NULL DEFAULT '{}',

    -- Which agent owns and, absent a step assignment, executes the emitted
    -- tasks. A schedule is not an agent, so it cannot be the answer to "who
    -- runs this" — that has to name something that can pick a card up.
    agent_id TEXT NOT NULL,

    -- Off switch that survives a restart, and the circuit breaker's landing
    -- spot. Set to 0 by the sweep when a fire is *refused* — see
    -- `last_outcome` — because a schedule that fires into the same validation
    -- error every five minutes is a log nobody reads.
    enabled INTEGER NOT NULL DEFAULT 1,

    -- The scheduler cursor: when this schedule is next due.
    --
    -- NULL means "not yet computed" and the first sweep to see it initialises
    -- it, exactly as `cron_jobs.next_run_at` works. It is also the claim token:
    -- firing is an UPDATE that advances this column guarded on its current
    -- value, so when several agents' supervisor ticks see the same due schedule
    -- at the same moment, exactly one of them wins and the other reads the
    -- advanced cursor and moves on. This is the same conditional-UPDATE latch
    -- `claim_next_ready` uses to hand one task to one worker, and reusing it is
    -- what keeps a fire from launching two identical runs.
    next_run_at TEXT,

    -- When this schedule last fired, whatever came of it.
    last_fired_at TEXT,

    -- What came of that fire: launched | refused | errored.
    --
    -- Three values because there are three recoveries, and collapsing them is
    -- the bug this codebase has now paid for five times.
    --
    --   launched  a run started. `last_run_id` names it. Nothing to do.
    --   refused   the launch was *validly* rejected — the template contradicts
    --             itself, or the stored input does not match the schema. This
    --             is deterministic: the next fire refuses identically, and the
    --             one after that. So this outcome disables the schedule and
    --             `last_detail` says which step and which key. Recovery is a
    --             person editing the template or the stored input and switching
    --             it back on.
    --   errored   the launch could not be attempted — a storage failure. This
    --             is transient and says nothing about the template. The
    --             schedule stays enabled and the next fire tries again.
    --             Recovery is waiting.
    --
    -- "The schedule fired and the launch was refused" and "the schedule fired
    -- and the run started" are not the same event, and a single `success`
    -- boolean would have made the first one indistinguishable from the third —
    -- which is the difference between a schedule that needs editing and one
    -- that needs nothing.
    last_outcome TEXT,

    -- Why, in words, for whoever reads it. Carries the `LaunchError` text on a
    -- refusal, which already names the offending step and key.
    last_detail TEXT,

    -- The run the last successful fire produced, so a schedule links to what it
    -- actually did rather than to a search.
    last_run_id TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- The sweep's only query: schedules that are on and due.
--
-- Partial on `enabled`, because a disabled schedule is never asked about and
-- the index should hold the live ones rather than every schedule ever written.
CREATE INDEX IF NOT EXISTS idx_workflow_schedules_due
    ON workflow_schedules(next_run_at)
    WHERE enabled = 1;

CREATE INDEX IF NOT EXISTS idx_workflow_schedules_workflow
    ON workflow_schedules(workflow_id);

-- An inbound endpoint mapping a payload to a run input. What closes the gitops
-- loop: gates can already *wait* on CI, and until now CI could not *start*
-- anything.
--
-- **This table existing is what makes the endpoint work, and that is the whole
-- safety design.** A webhook is an unauthenticated inbound trigger for
-- arbitrary pipeline execution, on a host whose sandbox containment is
-- currently inert, and the instance authentication it was supposed to ship
-- behind does not exist yet. So the posture is off unless somebody explicitly
-- turned it on for one specific workflow: no row means the endpoint refuses,
-- and there is no default row, no global enable, and no way to configure one by
-- accident. The refusal is the easy path because it is the *absence* path.
--
-- One row per workflow. A second webhook onto the same pipeline would be a
-- second secret to rotate for no capability the first does not have.
CREATE TABLE IF NOT EXISTS workflow_webhooks (
    workflow_id TEXT PRIMARY KEY NOT NULL
        REFERENCES workflows(id) ON DELETE CASCADE,

    -- SHA-256 of the shared secret, hex, lowercase. Never the secret itself.
    --
    -- Hashed at rest for the same reason a password is: whoever can read this
    -- database should not thereby be able to *fire* the trigger, and a
    -- plaintext column would put the secret in every backup, every export, and
    -- every debug dump of a row. Comparison is over the two digests in constant
    -- time, which also removes the length side-channel a direct string compare
    -- would have — digests are always 32 bytes whatever the secret was.
    --
    -- Unsalted, and deliberately: this is a high-entropy machine-generated
    -- shared secret, not a human-chosen password, so there is no dictionary to
    -- iterate and a per-row salt would buy nothing but a second column.
    secret_hash TEXT NOT NULL,

    -- How the payload becomes the run input: a JSON object of
    -- `{ "<run input key>": "<RFC 6901 JSON Pointer into the payload>" }`.
    --
    -- Pointers rather than a template language, because pointers are the
    -- vocabulary bindings and gates already use — `/head_commit/id` means the
    -- same thing here as it does in a `task_output` gate, and a second way to
    -- address into JSON would be a second thing to learn and a second thing to
    -- get subtly different.
    --
    -- Explicit rather than "pass the payload through": a run input is validated
    -- against the workflow's `input_schema`, and handing a pipeline the whole
    -- of an unauthenticated third party's POST body is how a payload field
    -- nobody declared ends up inside a step's prompt.
    input_pointers TEXT NOT NULL DEFAULT '{}',

    -- Which agent owns and executes the emitted tasks, for the same reason
    -- `workflow_schedules.agent_id` exists: a webhook cannot pick up a card.
    agent_id TEXT NOT NULL,

    -- The second off switch, and the reason it is not just row deletion: an
    -- operator turning a noisy integration off for an afternoon should not have
    -- to destroy and re-issue the shared secret to do it, because a secret that
    -- has to be re-issued to pause something is a secret that gets left on.
    --
    -- Disabled and absent are one answer to the caller — an identical refusal,
    -- since telling an unauthenticated stranger which workflows exist and which
    -- of them have a webhook waiting to be enabled is itself the leak — and two
    -- different answers in the log, because one needs configuring and the other
    -- needs a switch.
    enabled INTEGER NOT NULL DEFAULT 0,

    -- When something last posted here *and got in*. A rejected delivery
    -- deliberately writes nothing: this column is driven by an unauthenticated
    -- endpoint, and letting a stranger write to a row by failing at it is a
    -- free amplification into the database.
    last_delivery_at TEXT,

    -- What came of that delivery: launched | unmapped | refused | errored.
    -- The same rule as `workflow_schedules.last_outcome` — one label per
    -- recovery, and these are four different recoveries:
    --
    --   launched  a run started; `last_run_id` names it.
    --   unmapped  authenticated, but a configured pointer found nothing in the
    --             payload. Fix the pointer, or the sender.
    --   refused   authenticated and mapped, and `launch` rejected the result.
    --             Fix the template or the pointers.
    --   errored   storage failed. Fix nothing; try again.
    last_outcome TEXT,
    last_detail TEXT,
    last_run_id TEXT,

    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
