-- OpenCode worker metadata for embedded web UI.
-- Stores the session ID and server port so the frontend can construct
-- an iframe URL to the OpenCode web UI for a given worker run.
ALTER TABLE worker_runs ADD COLUMN opencode_session_id TEXT;
ALTER TABLE worker_runs ADD COLUMN opencode_port INTEGER;
