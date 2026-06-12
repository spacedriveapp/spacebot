-- Add user-controlled sort order to projects.
ALTER TABLE projects ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;

-- Initialise from creation order so existing projects have a deterministic baseline.
UPDATE projects
SET sort_order = (
    SELECT COUNT(*)
    FROM projects p2
    WHERE p2.created_at < projects.created_at
        OR (p2.created_at = projects.created_at AND p2.id < projects.id)
);
