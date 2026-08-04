-- Command steps, and worktrees that provision themselves.
--
-- Two features in one migration because they interlock at exactly one point:
-- `bun run lint` run in a checkout another step is mid-edit reports on a state
-- that never existed. A deterministic check is only worth having if the tree it
-- read is the tree the author meant, so isolation is part of the check, not an
-- optimisation layered on afterwards.

-- ---------------------------------------------------------------------------
-- Command steps
-- ---------------------------------------------------------------------------

-- What a step *is*: `agent` (a task claimed by a worker with a full tool loop)
-- or `command` (a process, an exit code, and its output).
--
-- Named rather than inferred from "does `command` have a value". A NULL command
-- on a step somebody meant to be a command step is a template bug, and inferring
-- the kind would silently turn it into an agent step that runs a model against
-- an empty instruction — expensive, slow, and wrong in a way nothing reports.
-- With an explicit kind, launch refuses it and says which field is missing.
ALTER TABLE workflow_steps ADD COLUMN kind TEXT NOT NULL DEFAULT 'agent';

-- The command line, run through `sh -c` in the step's bound directory.
--
-- NULL on every agent step, and launch refuses an agent step that carries one:
-- a command line nothing executes is the dead-config shape this codebase keeps
-- paying for, and here it would read on the canvas as a step that runs a command
-- and does not.
ALTER TABLE workflow_steps ADD COLUMN command TEXT;

-- Hard wall-clock ceiling, in seconds. Required on a command step.
--
-- Required rather than inherited from a default, because a stored command runs
-- unattended and forever: the author is the only person who knows whether this
-- is a two-second linter or a four-minute build, and a default that fits one
-- fits the other badly. A timeout is *not* an exit code — it means the command
-- never reported, which fails the task and spends its failure budget.
ALTER TABLE workflow_steps ADD COLUMN command_timeout_secs INTEGER;

-- The exit code that means success, for the steps where non-zero really is a
-- failure — `git push` should not quietly "succeed" with exit 1.
--
-- NULL by default, and that default is the whole feature. A command that ran and
-- reported a problem (`exit 1` from a linter) is a task that **succeeded** with
-- `{"exit_code": 1}` as its answer; only a command that could not run at all is a
-- task failure. Conflating the two makes a lint step burn two attempts of its
-- failure budget and park before the fix loop has run twice — the loop dying of
-- the exact condition it exists to fix.
ALTER TABLE workflow_steps ADD COLUMN expect_exit_code INTEGER;

-- The same four fields, frozen onto the task the step compiles into.
--
-- Frozen rather than read back from `workflow_steps` at pickup, for the reason
-- the fan-out and loop specs are frozen: a template edited mid-run must not
-- change what a run already in flight does, and a run whose template was deleted
-- still has to finish. It is also what lets a command task exist without a
-- template at all, which is what the tests and the API create.
ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'agent';
ALTER TABLE tasks ADD COLUMN command TEXT;
ALTER TABLE tasks ADD COLUMN command_timeout_secs INTEGER;
ALTER TABLE tasks ADD COLUMN expect_exit_code INTEGER;

-- ---------------------------------------------------------------------------
-- Worktree provisioning
-- ---------------------------------------------------------------------------

-- What checkout a step needs. inherit | per_run | per_branch.
--
--   inherit     use whatever the task binding already says — today's behaviour,
--               unchanged, which is why it is the default: every existing
--               template keeps working, and a pipeline that genuinely wants one
--               shared checkout can still have one
--   per_run     one worktree for this step, created at launch
--   per_branch  one worktree per fan-out branch, created when the fan-out
--               expands — inside the same transaction that emits the branches
--
-- `per_branch` on a step that is not a fan-out is refused at launch rather than
-- degraded to `per_run`. Silently degrading it would hand an author a pipeline
-- that looks isolated and is not, which is worse than the error: the failure it
-- produces is two agents editing one working tree, and that does not produce a
-- bad result, it produces an incoherent one.
ALTER TABLE workflow_steps ADD COLUMN worktree_mode TEXT NOT NULL DEFAULT 'inherit';

-- What the provisioned worktree forks from — a branch, a tag, or a sha.
--
-- NULL means the repo's current HEAD. Explicit is preferred because "whatever
-- was checked out when the run happened" is not reproducible, and a pipeline
-- whose starting point drifts under it is one whose failures cannot be explained
-- afterwards. Checked at launch, not at run time: it is knowable from the
-- template, and a bad ref discovered three steps in has already cost the run.
ALTER TABLE workflow_steps ADD COLUMN worktree_base_ref TEXT;

-- The same two, frozen onto the task, and for the same reason as the command
-- columns above. A fan-out placeholder carries them to its branches, which is
-- how expansion knows to provision one checkout per branch.
ALTER TABLE tasks ADD COLUMN worktree_mode TEXT NOT NULL DEFAULT 'inherit';
ALTER TABLE tasks ADD COLUMN worktree_base_ref TEXT;

-- What a run provisioned, and what became of it.
--
-- The reaper's whole worklist. Without this table a finished run has no way to
-- know which of the directories under `.worktrees/` were its own, and the only
-- available answer would be "delete anything that looks stale" — a background
-- process that deletes directories, which is the one outcome worse than a
-- leftover worktree.
CREATE TABLE IF NOT EXISTS workflow_run_worktrees (
    id TEXT PRIMARY KEY NOT NULL,
    -- Plain text, not a foreign key, matching `tasks.workflow_run_id`: the
    -- record of what was created on disk must outlive the run row, or a deleted
    -- run leaves directories nothing can account for.
    run_id TEXT NOT NULL,
    step_key TEXT NOT NULL,
    -- NULL for `per_run`, the fan-out branch key for `per_branch`. The pair
    -- (run, step, branch) is what makes the name deterministic and the row
    -- re-derivable after a crash.
    branch_key TEXT,
    -- The repo the worktree was forked from, and the `project_worktrees` row
    -- that tasks bind to via `tasks.worktree_id`. Provisioning writes both, so a
    -- provisioned checkout is indistinguishable from one a person made by hand
    -- everywhere downstream of here.
    repo_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    -- Absolute path and branch name, recorded as they were at creation. "Which
    -- checkout did this actually run in" is the first question anyone asks about
    -- a failed run, and re-deriving it later needs a project row that may since
    -- have been edited.
    path TEXT NOT NULL,
    branch TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- When the reaper offered this worktree for removal. NULL means it has not
    -- been offered yet — reaping is keyed on the *run* being finished, never on
    -- the step, because a retry after a step-scoped reap would find no checkout.
    reaped_at TEXT,
    -- What git said. `removed` | `refused` | `missing`.
    --
    -- `refused` is not an error and must never be recorded as one. It means the
    -- tree had uncommitted changes and `git worktree remove` declined — which is
    -- the behaviour we want and are deliberately not overriding, because
    -- uncommitted work from a failed run is evidence, not garbage. Three values
    -- rather than a boolean because they have three different recoveries: none,
    -- a person looking at the diff, and a person wondering who deleted it.
    reap_outcome TEXT,
    -- Git's own stderr, verbatim. A refusal that does not say which files are
    -- dirty sends somebody reading rows.
    reap_detail TEXT
);

-- The reaper's only query: what did this run provision that is still on disk.
CREATE INDEX IF NOT EXISTS idx_workflow_run_worktrees_live
    ON workflow_run_worktrees(run_id)
    WHERE reaped_at IS NULL;

-- Orphan reporting walks every row for a project's repos, live or not, so that
-- a directory under `.worktrees/` can be matched against what we believe we
-- created. Anything on disk with no row here, or whose row belongs to a run that
-- no longer exists, is listed for a person — and only listed.
CREATE INDEX IF NOT EXISTS idx_workflow_run_worktrees_repo
    ON workflow_run_worktrees(repo_id);
