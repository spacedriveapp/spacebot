-- Bind tasks to the codebases they act on.
--
-- `projects` already models one project as many repos, each with many
-- worktrees, so a dependency edge between two tasks in *different repos of the
-- same project* becomes expressible once tasks carry these columns. That is the
-- multi-repo / microservice case the task board previously could not represent
-- at all: tasks were agent-scoped, never codebase-scoped.
--
-- All three are nullable. A task with no binding behaves exactly as before.
--
-- ON DELETE SET NULL rather than CASCADE: deleting a project must not silently
-- destroy the task history that referenced it. The task survives, unbound.

ALTER TABLE tasks ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
ALTER TABLE tasks ADD COLUMN repo_id TEXT REFERENCES project_repos(id) ON DELETE SET NULL;
ALTER TABLE tasks ADD COLUMN worktree_id TEXT REFERENCES project_worktrees(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
CREATE INDEX IF NOT EXISTS idx_tasks_repo ON tasks(repo_id);
CREATE INDEX IF NOT EXISTS idx_tasks_worktree ON tasks(worktree_id);

-- Board queries filter by project and then group by repo.
CREATE INDEX IF NOT EXISTS idx_tasks_project_status ON tasks(project_id, status);
