-- Add wall-clock cron expression schedule support.
ALTER TABLE cron_jobs ADD COLUMN cron_expr TEXT;
