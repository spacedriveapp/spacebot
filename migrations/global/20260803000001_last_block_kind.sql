-- Remember what a task was last blocked for, across promotion.
--
-- `block_recurrences` exists to catch a task being released and re-blocked for
-- the same reason forever. Deciding whether a block is a *recurrence* meant
-- comparing the incoming kind against `block_kind` — but the ready sweep clears
-- `block_kind` when it promotes a task, so the comparison always saw NULL, the
-- counter reset every cycle, and the limiter could never fire.
--
-- Found by running it: a task with an unresolvable input binding cycled
-- ready -> claimed -> blocked -> promoted indefinitely, with the counter
-- pinned at zero the whole time.
--
-- This column is written whenever a task is blocked and deliberately never
-- cleared on promotion. `block_kind` still answers "why is this task parked
-- right now" and goes to NULL when it is not; `last_block_kind` answers "what
-- did it park for last time", which is the question the limiter is asking.

ALTER TABLE tasks ADD COLUMN last_block_kind TEXT;

-- Existing rows: whatever they are parked for now is also the last thing they
-- parked for, so the counter starts from a truthful baseline rather than
-- treating the next block as a first offence.
UPDATE tasks SET last_block_kind = block_kind WHERE block_kind IS NOT NULL;
