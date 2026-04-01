-- Separate cron execution outcomes from outbound delivery outcomes.
ALTER TABLE cron_executions ADD COLUMN execution_succeeded INTEGER;
ALTER TABLE cron_executions ADD COLUMN delivery_attempted INTEGER;
ALTER TABLE cron_executions ADD COLUMN delivery_succeeded INTEGER;
ALTER TABLE cron_executions ADD COLUMN execution_error TEXT;
ALTER TABLE cron_executions ADD COLUMN delivery_error TEXT;
