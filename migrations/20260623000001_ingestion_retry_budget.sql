-- Retry budget for ingestion: bound retries, back them off, and quarantine
-- files that keep failing so the poll loop stops re-processing them forever.
ALTER TABLE ingestion_files ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ingestion_files ADD COLUMN next_attempt_at TIMESTAMP;
-- status now also takes the terminal value 'quarantined' (no CHECK constraint
-- exists on this column, so no schema change is needed beyond documenting it).
