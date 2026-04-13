-- Projects, repos, and worktrees have moved to the instance-level database
-- (spacebot.db). Drop the per-agent tables after the data migration has run.
-- The migration runs on startup from `projects::migration::migrate_legacy_projects`,
-- which copies rows into the instance DB before this migration drops the tables.

DROP TABLE IF EXISTS project_worktrees;
DROP TABLE IF EXISTS project_repos;
DROP TABLE IF EXISTS projects;
