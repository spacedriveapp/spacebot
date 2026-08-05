-- Repo-to-repo dependency edges (#29).
--
-- A project holds many repos and nothing records how they relate. "The web
-- client is generated from the api contract" lives only in whoever wired a
-- particular workflow, and has to be re-remembered every time a pipeline
-- touches both repos.
--
-- This table records the relationship so the system knows it exists. It is
-- read by the project view (to draw the arrows) and by the suggestion query
-- the step editor calls. Nothing here is ever turned into a workflow edge
-- automatically -- see `ProjectStore::repo_dependency_suggestions` for why
-- that boundary is the whole design.
CREATE TABLE IF NOT EXISTS repo_dependencies (
    -- The project both repos belong to. Denormalised out of `project_repos`
    -- because every read is "the declarations for this project" (drawing the
    -- graph in the project view), and because it is the key that makes a
    -- cross-project declaration unrepresentable rather than merely refused:
    -- the row states which project it belongs to, and the store checks both
    -- repos agree with it.
    project_id TEXT NOT NULL,

    -- The dependent repo -- the one that has to change when the other does.
    -- "web depends on api" is repo_id = web.
    repo_id TEXT NOT NULL,

    -- The repo depended upon -- api in the example above. Kept as a separate
    -- column rather than a direction flag so both halves of the suggestion
    -- query ("who depends on me", "who do I depend on") are a plain indexed
    -- lookup instead of a scan with a predicate.
    depends_on_repo_id TEXT NOT NULL,

    -- What sort of dependency: generated_from, consumes, vendors, ... Free
    -- text and nullable on purpose, exactly like agent capabilities. A closed
    -- vocabulary invented now is wrong for the second project that uses this,
    -- and an opaque label can be renamed without a migration. Nothing branches
    -- on the value; it is shown to people.
    kind TEXT,

    -- Why the dependency exists, in the author's words. The declaration is
    -- read by whoever is debugging a pipeline months later, and "the codegen
    -- script reads openapi.json from api" is the difference between a stale
    -- declaration being noticed and being obeyed.
    note TEXT,

    -- When it was declared. Declarations are expected to go stale (a stale one
    -- costs nothing while suggestions are all it produces), so the age of a
    -- declaration is the cheapest signal that it deserves a second look.
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,

    -- Declaring the same edge twice is the same declaration. The triple as the
    -- key makes the duplicate a database-level refusal rather than something
    -- every writer has to remember to check.
    PRIMARY KEY (project_id, repo_id, depends_on_repo_id),

    -- A declaration that points at a repo which no longer exists is invisible
    -- in the project view and confusing in a suggestion, so it dies with
    -- either endpoint -- in both directions, not just the dependent side.
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (repo_id) REFERENCES project_repos(id) ON DELETE CASCADE,
    FOREIGN KEY (depends_on_repo_id) REFERENCES project_repos(id) ON DELETE CASCADE,

    -- A repo depending on itself is never a statement about anything. The
    -- store refuses it with a named error for a usable message; this is the
    -- backstop for every other writer.
    CHECK (repo_id <> depends_on_repo_id)
);

-- "Which repos depend on this one" -- the downstream half of the suggestion
-- query, asked by the step editor with a repo id and no project id. The
-- primary key is prefixed by project_id and cannot serve it.
CREATE INDEX IF NOT EXISTS idx_repo_dependencies_depends_on
    ON repo_dependencies (depends_on_repo_id);

-- The upstream half: "what does this repo depend on", same access pattern in
-- the other direction.
CREATE INDEX IF NOT EXISTS idx_repo_dependencies_repo
    ON repo_dependencies (repo_id);
