-- Human decision steps. Task #30.
--
-- A pipeline could already park for a person (`block_kind = 'needs_input'` plus
-- `POST /tasks/{n}/unblock`). What it could not do is *ask a question and use
-- the answer*: unblocking is a single undifferentiated act, so nothing
-- downstream learns what was decided. The only workaround was an agent step
-- that asks in a channel and reports what it heard, which launders a human
-- decision through a model — after which the run record cannot tell "the
-- operator approved this" from "a model believed the operator approved this".
--
-- The constraint that shapes every column below: **a human must not be able to
-- set an arbitrary task's outputs.** An agent task's outputs are the record of
-- what that agent produced. So this is not "let a person fill in outputs on a
-- blocked task" — it is a third step kind whose *entire product* is the answer,
-- and whose outputs are therefore known to have come from a person because that
-- is the only thing this kind of step can produce.

-- ---------------------------------------------------------------------------
-- The template side: what is being asked
-- ---------------------------------------------------------------------------

-- The question, as the person answering will read it.
--
-- Its own column rather than a reuse of `description`. `description` is what the
-- card is about and is shown everywhere a card is; this is the prompt a person
-- is being asked to respond to, and the two are only the same on the simplest
-- gate. Separate also because launch *requires* this on a decision step: a
-- decision with no question is a form with no label, and a person answering it
-- would be guessing at what they were approving.
ALTER TABLE workflow_steps ADD COLUMN decision_question TEXT;

-- Who may answer, as a JSON array of names. NULL means anyone.
--
-- **Advisory in v1, deliberately, and recorded rather than enforced.** The API
-- has no authenticated caller identity at this layer — the answering endpoint
-- takes `answered_by` in its body, exactly as `POST /tasks/{n}/approve` takes
-- `approved_by`. Checking a self-declared name against this list would be
-- enforcement in name only, and worse than none: the board would read "only Pat
-- may answer this" while anybody could type `pat`. So both this and the actual
-- answerer are recorded, an audit can compare them, and the day there is a real
-- caller identity the check has somewhere to go.
ALTER TABLE workflow_steps ADD COLUMN decision_asked_of TEXT;

-- What happens if nobody answers. wait | default | fail.
--
--   wait     the default. It parks until answered, and the run is legitimately
--            blocked — which is correct behaviour and must not read as stuck.
--   default  a declared answer applies, recorded *as* a default.
--   fail     the decision fails and the failure path routes it.
--
-- Three values rather than "is there a timeout", because the three have three
-- different recoveries: none, none-but-check-who-decided, and a person looking
-- at why the pipeline gave up.
ALTER TABLE workflow_steps ADD COLUMN decision_timeout_action TEXT NOT NULL DEFAULT 'wait';

-- How long, in seconds, from the moment the decision was *asked* — not from
-- launch. Required by `default` and `fail`, refused by `wait`.
--
-- Anchored on the ask because that is the only reading that is honest: a
-- decision three steps into a pipeline may not become answerable for an hour,
-- and a deadline that started at launch would expire before anybody could
-- possibly have seen it.
ALTER TABLE workflow_steps ADD COLUMN decision_timeout_secs INTEGER;

-- The answer that applies when `decision_timeout_action = 'default'`, as JSON.
--
-- Validated against the step's own `output_schema` **at launch**, not only when
-- the timeout fires. A default that cannot satisfy the schema is a template bug
-- that would otherwise surface hours later as a pipeline that neither answered
-- nor defaulted, at the moment nobody is watching.
ALTER TABLE workflow_steps ADD COLUMN decision_default_answer TEXT;

-- What a decision inside a loop does on the second pass. each_pass | once.
--
-- `each_pass` is the default, and the justification is provenance rather than
-- convenience: pass 2 exists precisely because the artefact under review
-- changed, so reusing pass 1's answer would attribute a person's approval to
-- work they never looked at. That is the same erosion this whole feature exists
-- to stop, arriving through the loop instead of through a model.
--
-- `once` is the explicit opt-in for the gates that really are a property of the
-- run and not of the pass — "is this deploy authorised at all" — where three
-- prompts for one deploy is the maddening outcome the design doc names. A
-- carried answer is recorded as *carried*, keeping the original answerer and the
-- original timestamp, so it never reads as a fresh approval.
ALTER TABLE workflow_steps ADD COLUMN decision_ask TEXT NOT NULL DEFAULT 'each_pass';

-- ---------------------------------------------------------------------------
-- The task side: the same six, frozen, plus what actually happened
-- ---------------------------------------------------------------------------

-- Frozen onto the task at launch, for the reason the fan-out, loop and command
-- specs are frozen: a template edited mid-run must not change what a run already
-- in flight is asking, a run whose template was deleted must still be
-- answerable, and — most of all — the question a person answered must be the
-- question that is on the record afterwards. A decision that read its wording
-- back from a live template could be edited between the ask and the answer.
ALTER TABLE tasks ADD COLUMN decision_question TEXT;
ALTER TABLE tasks ADD COLUMN decision_asked_of TEXT;
ALTER TABLE tasks ADD COLUMN decision_timeout_action TEXT NOT NULL DEFAULT 'wait';
ALTER TABLE tasks ADD COLUMN decision_timeout_secs INTEGER;
ALTER TABLE tasks ADD COLUMN decision_default_answer TEXT;
ALTER TABLE tasks ADD COLUMN decision_ask TEXT NOT NULL DEFAULT 'each_pass';

-- When this decision became answerable — the moment the sweep would otherwise
-- have promoted it to `ready`.
--
-- Two jobs, and they are the same fact: it is the deadline anchor for the
-- timeout above, and it is the latch that stops the ask being re-issued (and
-- re-notified) on every tick. NULL means the decision has not been asked yet,
-- which is why answering one is refused until it has: answering a question whose
-- inputs do not exist yet is answering about something that has not happened.
ALTER TABLE tasks ADD COLUMN decision_asked_at TEXT;

-- How this decision was settled. answered | defaulted | timed_out | carried.
--
-- **This column is the feature.** Without it a defaulted answer is
-- indistinguishable from a human one in the run record, which is the provenance
-- problem returning through a side door — and the design doc names it as the
-- one to get right.
--
--   answered   a person answered. `decision_answered_by` names them.
--   defaulted  nobody answered, and the declared default applied. There are
--              outputs, and they are nobody's decision.
--   timed_out  nobody answered, and the step declared that failing is the
--              answer. There are *no* outputs.
--   carried    a `once` decision inside a loop, reusing an earlier pass's
--              answer. The answerer and the timestamp are the original ones.
--
-- Four values rather than a boolean or a nullable `answered_by`, for the reason
-- this codebase keeps relearning: NULL would have to mean both "defaulted" and
-- "we did not record who", and those recover differently — one is the pipeline
-- working as designed, the other is a hole in the audit trail.
--
-- NULL means unsettled, which includes the case where a `default` timeout fired
-- and its declared answer would not validate: that decision is back to needing a
-- person, and must not be recorded as having been decided by one.
ALTER TABLE tasks ADD COLUMN decision_outcome TEXT;

-- Who answered, for `answered` and `carried`. NULL for `defaulted` and
-- `timed_out`, where the honest answer is nobody.
ALTER TABLE tasks ADD COLUMN decision_answered_by TEXT;

-- When it was settled. For `carried` this is the *original* answer's timestamp,
-- not the moment it was reused — a carried answer that stamped itself with the
-- current time would read as a fresh approval of work nobody looked at.
ALTER TABLE tasks ADD COLUMN decision_answered_at TEXT;

-- Every unanswered decision, cheaply, for the timeout sweep.
--
-- Partial on `decision_outcome IS NULL` because a settled decision is never
-- swept again, and the overwhelming majority of rows in this table are not
-- decisions at all — a full scan every tick to find the two cards that are
-- waiting on a person is the cost this index removes.
CREATE INDEX IF NOT EXISTS idx_tasks_open_decisions
    ON tasks(decision_asked_at)
    WHERE kind = 'decision' AND decision_outcome IS NULL;
