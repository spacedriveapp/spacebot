-- Projects, repos, and worktrees at the instance level (shared across agents).
-- A project maps to a directory containing one or more git repos + worktrees.

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    icon TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '[]',
    root_path TEXT NOT NULL,
    logo_path TEXT,
    settings TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'active',
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_root_path ON projects(root_path);
CREATE INDEX IF NOT EXISTS idx_projects_status ON projects(status);

CREATE TABLE IF NOT EXISTS project_repos (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    remote_url TEXT NOT NULL DEFAULT '',
    default_branch TEXT NOT NULL DEFAULT 'main',
    current_branch TEXT,
    description TEXT NOT NULL DEFAULT '',
    disk_usage_bytes INTEGER,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_project_repos_project ON project_repos(project_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_project_repos_path ON project_repos(project_id, path);

CREATE TABLE IF NOT EXISTS project_worktrees (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    repo_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    branch TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT 'user',
    disk_usage_bytes INTEGER,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (repo_id) REFERENCES project_repos(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_project_worktrees_project ON project_worktrees(project_id);
CREATE INDEX IF NOT EXISTS idx_project_worktrees_repo ON project_worktrees(repo_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_project_worktrees_path ON project_worktrees(project_id, path);
