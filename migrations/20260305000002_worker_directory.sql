-- Persist the working directory for opencode workers so that idle workers
-- can be resumed into the correct directory after a restart.
ALTER TABLE worker_runs ADD COLUMN directory TEXT;
