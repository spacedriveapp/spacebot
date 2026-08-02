-- Drop a redundant index created by 20260802000001_task_runs.sql.
--
-- That migration created both `idx_task_runs_task` and the UNIQUE
-- `idx_task_runs_task_attempt` over exactly the same columns, in the same
-- order. The unique index already serves every lookup the plain one did, so
-- the plain one was pure write amplification.
--
-- Fixed here rather than by editing the original: migration files are
-- immutable once committed (AGENTS.md), because rewriting one changes its
-- checksum and blocks startup for anyone who already applied it.

DROP INDEX IF EXISTS idx_task_runs_task;
