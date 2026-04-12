-- Make projects a global concept (not per-agent).
--
-- Previously projects were scoped by agent_id with a unique constraint on
-- (agent_id, root_path). Now projects are shared across all agents with a
-- unique constraint on just root_path.
--
-- SQLite doesn't support DROP CONSTRAINT, so we recreate the table.

CREATE TABLE projects_new (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    icon TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '[]',
    root_path TEXT NOT NULL,
    settings TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'active',
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO projects_new (id, name, description, icon, tags, root_path, settings, status, created_at, updated_at)
SELECT id, name, description, icon, tags, root_path, settings, status, created_at, updated_at
FROM projects;

DROP TABLE projects;
ALTER TABLE projects_new RENAME TO projects;

CREATE UNIQUE INDEX idx_projects_root_path ON projects(root_path);
