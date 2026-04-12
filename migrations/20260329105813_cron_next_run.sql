-- Persist the scheduler cursor for each cron job so recurring schedules can
-- be claimed and advanced atomically before execution.
ALTER TABLE cron_jobs ADD COLUMN next_run_at TIMESTAMP;
