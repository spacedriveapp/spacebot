//! Provisioning, recording and reaping the checkouts a run creates for itself.
//!
//! A step says it needs its own checkout and gets one. The interesting parts are
//! all in the lifecycle rather than the creation: **a dirty worktree is never
//! deleted**, orphans are reported and never swept, and `per_branch`
//! provisioning happens inside the transaction that emits the branches so a
//! branch task can never exist without its checkout.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

use crate::projects::git;

/// How many worktrees one run may hold at once.
///
/// A fan-out of fifty is fifty checkouts, and worktrees are cheap in git terms
/// and not free on disk. Refusing at expansion with a message naming the cap
/// beats discovering it as ENOSPC in the middle of a pipeline, which is a
/// failure that also takes the rest of the host with it.
pub const MAX_RUN_WORKTREES: i64 = 20;

/// Where a step gets its working directory from.
///
/// `Inherit` is the default and is exactly today's behaviour, which is what lets
/// every template that predates this feature keep working untouched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    /// Use whatever the task binding already says.
    #[default]
    Inherit,
    /// One worktree for this step, created at launch.
    PerRun,
    /// One worktree per fan-out branch, created when the fan-out expands.
    ///
    /// On a step that is not a fan-out this is a template error refused at
    /// launch. Degrading it to `PerRun` would hand an author a pipeline that
    /// looks isolated and is not.
    PerBranch,
}

impl WorktreeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WorktreeMode::Inherit => "inherit",
            WorktreeMode::PerRun => "per_run",
            WorktreeMode::PerBranch => "per_branch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "inherit" => Some(WorktreeMode::Inherit),
            "per_run" => Some(WorktreeMode::PerRun),
            "per_branch" => Some(WorktreeMode::PerBranch),
            _ => None,
        }
    }

    /// Whether this mode creates anything. `Inherit` does not, and every
    /// provisioning path short-circuits on it.
    pub fn provisions(self) -> bool {
        !matches!(self, WorktreeMode::Inherit)
    }
}

impl std::fmt::Display for WorktreeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Why provisioning could not happen.
///
/// Every variant names something a person can act on, because every one of them
/// blocks a task rather than failing it: a missing base ref or an unregistered
/// repo is a broken configuration, and spending a task's failure budget on it
/// would park the card for the wrong reason and hide the fix.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WorktreeError {
    #[error("step `{step_key}` asks for its own worktree but names no repo — set repo_id")]
    NoRepo { step_key: String },
    #[error("repo `{repo_id}` is not registered with any project")]
    UnknownRepo { repo_id: String },
    #[error("repo `{repo_id}` belongs to project `{project_id}`, which does not exist")]
    UnknownProject { repo_id: String, project_id: String },
    #[error(
        "step `{step_key}` forks its worktree from `{base_ref}`, which does not exist in repo \
         `{repo_name}` — a base ref that drifts is a run whose failures cannot be explained"
    )]
    BaseRefMissing {
        step_key: String,
        repo_name: String,
        base_ref: String,
    },
    #[error("git could not create the worktree for step `{step_key}`: {detail}")]
    GitFailed { step_key: String, detail: String },
    #[error(
        "this run already holds {existing} worktrees and {wanted} more would pass the cap of \
         {cap} (limit: MAX_RUN_WORKTREES) — nothing was created"
    )]
    CapExceeded {
        existing: i64,
        wanted: i64,
        cap: i64,
    },
    #[error("worktree storage error: {0}")]
    Storage(String),
}

/// A checkout that exists on disk but has not been recorded yet.
///
/// The split is deliberate. `git worktree add` is not transactional and a SQLite
/// transaction cannot roll it back, so the disk work happens first and the two
/// rows that make it *visible* — `project_worktrees`, which tasks bind to, and
/// `workflow_run_worktrees`, which the reaper walks — are written inside
/// whatever transaction the caller is already in. A crash between the two leaves
/// a directory the orphan report can name, which is strictly better than a task
/// bound to a checkout that was never created.
#[derive(Debug, Clone)]
pub struct PreparedWorktree {
    pub worktree_id: String,
    pub project_id: String,
    pub repo_id: String,
    pub name: String,
    /// Relative to the project root, matching `project_worktrees.path`.
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub branch: String,
    /// Where `git worktree remove` has to be run from.
    pub repo_absolute_path: PathBuf,
}

/// What the reaper found when it offered one worktree for removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedWorktree {
    pub run_id: String,
    pub step_key: String,
    pub branch_key: Option<String>,
    pub path: String,
    pub outcome: ReapOutcome,
    pub detail: Option<String>,
}

/// Three outcomes rather than "did it work", because they have three different
/// recoveries: none, a person reading a diff, and a person wondering what broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReapOutcome {
    /// Clean, and gone. The branch and its commits survive.
    Removed,
    /// Dirty, so git refused. **Not an error.** Left on disk on purpose.
    Refused,
    /// Nothing there. Already removed, or never made it to disk.
    Missing,
    /// Git failed for some other reason and a person should look.
    Failed,
}

impl ReapOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ReapOutcome::Removed => "removed",
            ReapOutcome::Refused => "refused",
            ReapOutcome::Missing => "missing",
            ReapOutcome::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "removed" => Some(ReapOutcome::Removed),
            "refused" => Some(ReapOutcome::Refused),
            "missing" => Some(ReapOutcome::Missing),
            "failed" => Some(ReapOutcome::Failed),
            _ => None,
        }
    }
}

/// A directory under `.worktrees/` that nothing alive accounts for.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OrphanWorktree {
    pub project_id: String,
    pub repo_id: String,
    pub path: String,
    pub branch: String,
    /// The run it appears to have belonged to, when the name still says so.
    pub run_id: Option<String>,
    /// Why we think nobody owns it, in words.
    pub reason: String,
}

/// The directory, relative to the project root, that every provisioned worktree
/// lives under.
///
/// Under the project root on purpose. `refresh_project_paths` already injects
/// project roots into the sandbox allowlist, so a worktree here is inside the
/// boundary with no new plumbing — and **no new writable path should ever be
/// added for a worktree**. A worktree outside the project root would need one,
/// which is precisely the reason not to put it there.
pub const WORKTREE_DIR: &str = ".worktrees";

/// Deterministic name for a provisioned worktree: `<run-short>-<step>[-<branch>]`.
///
/// Deterministic so it is greppable, re-derivable after a crash, and obvious in
/// `git worktree list` at three in the morning. A leftover directory with a uuid
/// name tells nobody what made it. Run-scoped so a re-launch cannot collide with
/// a previous run's branch name — `create_worktree` falls back to attaching an
/// existing branch when `-b` fails, which is good behaviour for a human retrying
/// and would silently share history between two runs here.
pub fn worktree_name(run_id: &str, step_key: &str, branch_key: Option<&str>) -> String {
    let run_short: String = run_id.chars().take(8).collect();
    let mut name = format!("{}-{}", sanitize(&run_short), sanitize(step_key));
    if let Some(branch_key) = branch_key {
        name.push('-');
        name.push_str(&sanitize(branch_key));
    }
    name
}

/// Reduce an arbitrary key to something safe as a path segment and a branch name.
fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['-', '.']).to_string();
    if trimmed.is_empty() {
        "step".to_string()
    } else {
        trimmed
    }
}

/// Where a repo lives on disk, and which project owns it.
async fn repo_location(
    pool: &SqlitePool,
    repo_id: &str,
) -> Result<(String, PathBuf, String), WorktreeError> {
    let store = crate::projects::ProjectStore::new(pool.clone());
    let repo = store
        .get_repo(repo_id)
        .await
        .map_err(|error| WorktreeError::Storage(error.to_string()))?
        .ok_or_else(|| WorktreeError::UnknownRepo {
            repo_id: repo_id.to_string(),
        })?;
    let project = store
        .get_project(&repo.project_id)
        .await
        .map_err(|error| WorktreeError::Storage(error.to_string()))?
        .ok_or_else(|| WorktreeError::UnknownProject {
            repo_id: repo_id.to_string(),
            project_id: repo.project_id.clone(),
        })?;

    let project_root = PathBuf::from(&project.root_path);
    let repo_path = project_root.join(&repo.path);
    Ok((repo.project_id, repo_path, repo.name))
}

/// Confirm a step's base ref resolves, while a person is still watching.
///
/// Called from launch validation rather than from provisioning, because that is
/// the last moment the answer can be "fix the template" instead of "an incident".
pub async fn verify_base_ref(
    pool: &SqlitePool,
    step_key: &str,
    repo_id: &str,
    base_ref: Option<&str>,
) -> Result<(), WorktreeError> {
    let Some(base_ref) = base_ref.map(str::trim).filter(|value| !value.is_empty()) else {
        // No explicit ref means the repo's current HEAD, which is whatever it
        // is. Nothing to check, and nothing to refuse.
        return Ok(());
    };

    let (_, repo_path, repo_name) = repo_location(pool, repo_id).await?;
    if git::resolve_ref(&repo_path, base_ref).await.is_none() {
        return Err(WorktreeError::BaseRefMissing {
            step_key: step_key.to_string(),
            repo_name,
            base_ref: base_ref.to_string(),
        });
    }
    Ok(())
}

/// How many live worktrees a run already holds.
pub async fn count_run_worktrees(pool: &SqlitePool, run_id: &str) -> Result<i64, WorktreeError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM workflow_run_worktrees WHERE run_id = ? AND reaped_at IS NULL",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(|error| WorktreeError::Storage(error.to_string()))
}

/// Refuse before creating anything if this run would go over the cap.
pub async fn check_cap(pool: &SqlitePool, run_id: &str, wanted: i64) -> Result<(), WorktreeError> {
    let existing = count_run_worktrees(pool, run_id).await?;
    if existing + wanted > MAX_RUN_WORKTREES {
        return Err(WorktreeError::CapExceeded {
            existing,
            wanted,
            cap: MAX_RUN_WORKTREES,
        });
    }
    Ok(())
}

/// Create the checkout on disk. Writes nothing to the database.
///
/// The caller records it — see [`PreparedWorktree`] for why the two halves are
/// split.
pub async fn create_checkout(
    pool: &SqlitePool,
    run_id: &str,
    step_key: &str,
    branch_key: Option<&str>,
    repo_id: &str,
    base_ref: Option<&str>,
) -> Result<PreparedWorktree, WorktreeError> {
    let (project_id, repo_path, _repo_name) = repo_location(pool, repo_id).await?;
    let store = crate::projects::ProjectStore::new(pool.clone());
    let project = store
        .get_project(&project_id)
        .await
        .map_err(|error| WorktreeError::Storage(error.to_string()))?
        .ok_or_else(|| WorktreeError::UnknownProject {
            repo_id: repo_id.to_string(),
            project_id: project_id.clone(),
        })?;

    let name = worktree_name(run_id, step_key, branch_key);
    let relative_path = format!("{WORKTREE_DIR}/{name}");
    let absolute_path = Path::new(&project.root_path).join(&relative_path);

    let base_ref = base_ref.map(str::trim).filter(|value| !value.is_empty());
    git::create_worktree(&repo_path, &absolute_path, &name, base_ref)
        .await
        .map_err(|error| WorktreeError::GitFailed {
            step_key: step_key.to_string(),
            detail: error.to_string(),
        })?;

    Ok(PreparedWorktree {
        worktree_id: uuid::Uuid::new_v4().to_string(),
        project_id,
        repo_id: repo_id.to_string(),
        name: name.clone(),
        relative_path,
        absolute_path,
        branch: name,
        repo_absolute_path: repo_path,
    })
}

/// Write the two rows that make a prepared checkout visible.
///
/// Takes a connection rather than a pool so the caller can put this inside the
/// fan-out expansion transaction. That is the whole point: a branch task that
/// commits without its `project_worktrees` row would run in the repo's own
/// checkout — precisely the failure cwd enforcement was built to prevent, one
/// layer up.
pub async fn record_worktree(
    conn: &mut sqlx::SqliteConnection,
    run_id: &str,
    step_key: &str,
    branch_key: Option<&str>,
    prepared: &PreparedWorktree,
) -> Result<(), WorktreeError> {
    sqlx::query(
        "INSERT INTO project_worktrees (id, project_id, repo_id, name, path, branch, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&prepared.worktree_id)
    .bind(&prepared.project_id)
    .bind(&prepared.repo_id)
    .bind(&prepared.name)
    .bind(&prepared.relative_path)
    .bind(&prepared.branch)
    .bind(format!("workflow_run:{run_id}"))
    .execute(&mut *conn)
    .await
    .map_err(|error| WorktreeError::Storage(error.to_string()))?;

    sqlx::query(
        "INSERT INTO workflow_run_worktrees \
             (id, run_id, step_key, branch_key, repo_id, worktree_id, path, branch) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(run_id)
    .bind(step_key)
    .bind(branch_key)
    .bind(&prepared.repo_id)
    .bind(&prepared.worktree_id)
    .bind(prepared.absolute_path.to_string_lossy().to_string())
    .bind(&prepared.branch)
    .execute(&mut *conn)
    .await
    .map_err(|error| WorktreeError::Storage(error.to_string()))?;

    Ok(())
}

/// Undo checkouts created for an expansion that then failed to commit.
///
/// Safe to force nothing: these were created seconds ago and nothing has run in
/// them, so git removes them without complaint. If one has somehow become dirty
/// in that window, git refuses and we leave it — the prohibition does not get an
/// exception for cleanup paths.
pub async fn discard_checkouts(prepared: &[PreparedWorktree]) {
    for worktree in prepared {
        match git::offer_worktree_removal(&worktree.repo_absolute_path, &worktree.absolute_path)
            .await
        {
            Ok(git::WorktreeRemoval::Removed) | Ok(git::WorktreeRemoval::Missing { .. }) => {}
            Ok(other) => tracing::warn!(
                path = %worktree.absolute_path.display(),
                ?other,
                "left a just-created worktree behind after a failed expansion"
            ),
            Err(error) => tracing::warn!(
                %error,
                path = %worktree.absolute_path.display(),
                "failed to remove a just-created worktree after a failed expansion"
            ),
        }
    }
}

/// Offer every worktree this run provisioned for removal, once the run is over.
///
/// Keyed on the *run* being finished, never on the step: a retry after a
/// step-scoped reap would find no checkout. Git's refusal on a dirty tree is
/// recorded and respected — it is the expected outcome for any run that failed
/// with work in progress, and the whole reason it is worth keeping.
pub async fn reap_run(pool: &SqlitePool, run_id: &str) -> Vec<ReapedWorktree> {
    let rows = match sqlx::query(
        "SELECT id, step_key, branch_key, repo_id, worktree_id, path \
         FROM workflow_run_worktrees WHERE run_id = ? AND reaped_at IS NULL",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, run_id, "failed to list the worktrees a finished run provisioned");
            return Vec::new();
        }
    };

    let mut reaped = Vec::new();
    for row in rows {
        let row_id: String = match row.try_get("id") {
            Ok(value) => value,
            Err(_) => continue,
        };
        let step_key: String = row.try_get("step_key").unwrap_or_default();
        let branch_key: Option<String> = row.try_get("branch_key").ok().flatten();
        let repo_id: String = row.try_get("repo_id").unwrap_or_default();
        let worktree_id: String = row.try_get("worktree_id").unwrap_or_default();
        let path: String = row.try_get("path").unwrap_or_default();

        let repo_path = match repo_location(pool, &repo_id).await {
            Ok((_, repo_path, _)) => repo_path,
            Err(error) => {
                tracing::warn!(%error, run_id, repo_id, "cannot reap a worktree whose repo is gone");
                continue;
            }
        };

        let removal = git::offer_worktree_removal(&repo_path, Path::new(&path)).await;
        let (outcome, detail) = match removal {
            Ok(git::WorktreeRemoval::Removed) => (ReapOutcome::Removed, None),
            Ok(git::WorktreeRemoval::Refused { detail }) => (ReapOutcome::Refused, Some(detail)),
            Ok(git::WorktreeRemoval::Missing { detail }) => (ReapOutcome::Missing, Some(detail)),
            Ok(git::WorktreeRemoval::Failed { detail }) => (ReapOutcome::Failed, Some(detail)),
            Err(error) => (ReapOutcome::Failed, Some(error.to_string())),
        };

        if let Err(error) = sqlx::query(
            "UPDATE workflow_run_worktrees SET reaped_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'), \
             reap_outcome = ?, reap_detail = ? WHERE id = ?",
        )
        .bind(outcome.as_str())
        .bind(detail.as_deref())
        .bind(&row_id)
        .execute(pool)
        .await
        {
            tracing::warn!(%error, run_id, "failed to record what reaping a worktree decided");
        }

        // The `project_worktrees` row goes only when the directory did. A row
        // for a checkout that is still on disk is what makes a left-behind
        // worktree visible in the UI and re-bindable by a person who wants to
        // go and look at it.
        if matches!(outcome, ReapOutcome::Removed | ReapOutcome::Missing)
            && let Err(error) = sqlx::query("DELETE FROM project_worktrees WHERE id = ?")
                .bind(&worktree_id)
                .execute(pool)
                .await
        {
            tracing::warn!(%error, run_id, worktree_id, "failed to drop a reaped worktree row");
        }

        reaped.push(ReapedWorktree {
            run_id: run_id.to_string(),
            step_key,
            branch_key,
            path,
            outcome,
            detail,
        });
    }

    reaped
}

/// List directories under a project's `.worktrees/` that nothing alive owns.
///
/// Deliberately a **report, not a sweep**. The one thing worse than a stale
/// worktree is a background process that deletes directories, so this returns
/// rows for a person and does not touch the disk. The deterministic naming
/// scheme is what makes them findable at all.
pub async fn list_orphans(pool: &SqlitePool, project_id: &str) -> Vec<OrphanWorktree> {
    let store = crate::projects::ProjectStore::new(pool.clone());
    let Ok(repos) = store.list_repos(project_id).await else {
        return Vec::new();
    };
    let Ok(Some(project)) = store.get_project(project_id).await else {
        return Vec::new();
    };
    let worktree_root = Path::new(&project.root_path).join(WORKTREE_DIR);

    let mut orphans = Vec::new();
    for repo in repos {
        let repo_path = Path::new(&project.root_path).join(&repo.path);
        let Ok(worktrees) = git::list_worktrees(&repo_path).await else {
            continue;
        };

        for worktree in worktrees {
            if !worktree.path.starts_with(&worktree_root) {
                // Not ours. A worktree a person made somewhere else is their
                // business, and reporting it as an orphan trains people to
                // ignore the report.
                continue;
            }

            let path = worktree.path.to_string_lossy().to_string();
            let owner: Option<(String, Option<String>)> = sqlx::query(
                "SELECT w.run_id, r.id FROM workflow_run_worktrees w \
                 LEFT JOIN workflow_runs r ON r.id = w.run_id \
                 WHERE w.path = ?",
            )
            .bind(&path)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .map(|row| {
                (
                    row.try_get::<String, _>(0).unwrap_or_default(),
                    row.try_get::<Option<String>, _>(1).ok().flatten(),
                )
            });

            let (run_id, reason) = match owner {
                // A live run still owns it. Not an orphan; it is in use.
                Some((run_id, Some(_))) => {
                    let live: Option<String> = sqlx::query_scalar(
                        "SELECT status FROM workflow_runs WHERE id = ? AND status = 'running'",
                    )
                    .bind(&run_id)
                    .fetch_optional(pool)
                    .await
                    .ok()
                    .flatten();
                    if live.is_some() {
                        continue;
                    }
                    (
                        Some(run_id),
                        "the run that created it has finished and the checkout is still on disk — \
                         most likely it had uncommitted changes when it was offered for removal"
                            .to_string(),
                    )
                }
                Some((run_id, None)) => (
                    Some(run_id),
                    "the run that created it no longer exists".to_string(),
                ),
                None => (
                    None,
                    "nothing recorded creating it — most likely a crash between `git worktree add` \
                     and the row that would have owned it"
                        .to_string(),
                ),
            };

            orphans.push(OrphanWorktree {
                project_id: project_id.to_string(),
                repo_id: repo.id.clone(),
                path,
                branch: worktree.branch,
                run_id,
                reason,
            });
        }
    }

    orphans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskStatus, TaskStore};
    use crate::workflows::store::{StepKind, WorkflowStep, WorkflowStore};

    /// A pool with the real migrations, a real git repo under a real project
    /// root, and both registered.
    ///
    /// Real git rather than a fake: the load-bearing behaviour here is *git's*
    /// refusal to delete a dirty worktree, and a stub would let us assert a
    /// promise nobody keeps.
    async fn fixture() -> (SqlitePool, tempfile::TempDir, String, String) {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::migrate!("./migrations/global")
            .run(&pool)
            .await
            .expect("migrations");

        let root = tempfile::tempdir().expect("temp project root");
        let repo_path = root.path().join("app");
        std::fs::create_dir_all(&repo_path).expect("repo dir");
        git_in(&repo_path, &["init", "-b", "main"]);
        git_in(&repo_path, &["config", "user.email", "test@example.com"]);
        git_in(&repo_path, &["config", "user.name", "Test"]);
        std::fs::write(repo_path.join("README.md"), "hello\n").expect("seed file");
        git_in(&repo_path, &["add", "."]);
        git_in(&repo_path, &["commit", "-m", "first"]);

        let store = crate::projects::ProjectStore::new(pool.clone());
        let project = store
            .create_project(crate::projects::CreateProjectInput {
                name: "sandbox".into(),
                description: String::new(),
                icon: String::new(),
                tags: Vec::new(),
                root_path: root.path().to_string_lossy().to_string(),
                settings: serde_json::json!({}),
            })
            .await
            .expect("project");
        let repo = store
            .create_repo(crate::projects::CreateRepoInput {
                project_id: project.id.clone(),
                name: "app".into(),
                path: "app".into(),
                remote_url: String::new(),
                default_branch: "main".into(),
                current_branch: Some("main".into()),
                description: String::new(),
            })
            .await
            .expect("repo");

        (pool, root, project.id, repo.id)
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git should be installed for worktree tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn base_step(workflow_id: &str, key: &str, repo_id: &str) -> WorkflowStep {
        WorkflowStep {
            workflow_id: workflow_id.to_string(),
            step_key: key.to_string(),
            title: key.to_string(),
            description: None,
            assigned_agent_id: None,
            required_capabilities: None,
            priority: crate::tasks::TaskPriority::Medium,
            input_schema: None,
            output_schema: None,
            system_prompt: None,
            repo_id: Some(repo_id.to_string()),
            position: 0,
            for_each_step_key: None,
            for_each_pointer: None,
            for_each_key: None,
            loop_group: None,
            loop_max_iterations: None,
            loop_until: None,
            kind: StepKind::Agent,
            command: None,
            command_timeout_secs: None,
            expect_exit_code: None,
            worktree_mode: WorktreeMode::Inherit,
            worktree_base_ref: None,
            decision_question: None,
            decision_asked_of: None,
            decision_timeout_action: crate::tasks::DecisionTimeoutAction::Wait,
            decision_timeout_secs: None,
            decision_default_answer: None,
            decision_ask: crate::tasks::DecisionAsk::EachPass,
        }
    }

    /// The plainest case, and the one every other worktree behaviour is built
    /// on: a step that asks for its own checkout gets exactly one, and the task
    /// it became is *bound* to it — not merely told about it in a prompt, which
    /// is the difference between isolation that is enforced and isolation that
    /// is requested politely.
    #[tokio::test]
    async fn a_per_run_step_provisions_one_checkout_and_binds_its_task_to_it() {
        let (pool, root, project_id, repo_id) = fixture().await;
        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool.clone());

        let workflow = workflows
            .create_workflow("build in isolation", None, None)
            .await
            .expect("workflow");
        let mut step = base_step(&workflow.id, "build", &repo_id);
        step.worktree_mode = WorktreeMode::PerRun;
        workflows.put_step(&step).await.expect("put step");

        let run = workflows
            .launch(&tasks, &workflow.id, &serde_json::json!({}), "agent-1")
            .await
            .expect("launch");

        let rows = sqlx::query(
            "SELECT path, branch, worktree_id FROM workflow_run_worktrees WHERE run_id = ?",
        )
        .bind(&run.run.id)
        .fetch_all(&pool)
        .await
        .expect("read provisioned rows");
        assert_eq!(rows.len(), 1, "one step, one checkout");

        let path: String = rows[0].try_get("path").expect("path");
        assert!(
            Path::new(&path).is_dir(),
            "the checkout has to exist on disk: {path}"
        );
        assert!(
            path.starts_with(&root.path().to_string_lossy().to_string()),
            "worktrees live under the project root so the sandbox already covers them: {path}"
        );

        let worktree_id: String = rows[0].try_get("worktree_id").expect("worktree_id");
        let task = tasks
            .get_by_number(run.task_numbers["build"])
            .await
            .expect("read")
            .expect("task");
        assert_eq!(task.worktree_id.as_deref(), Some(worktree_id.as_str()));
        assert_eq!(task.project_id.as_deref(), Some(project_id.as_str()));
    }

    /// A clean checkout is removed when the run is over, and the row records
    /// that it was. Nothing here is forced — git simply had no objection.
    #[tokio::test]
    async fn a_clean_worktree_is_removed_when_the_run_finishes() {
        let (pool, _root, _project_id, repo_id) = fixture().await;
        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool.clone());

        let workflow = workflows
            .create_workflow("build", None, None)
            .await
            .expect("workflow");
        let mut step = base_step(&workflow.id, "build", &repo_id);
        step.worktree_mode = WorktreeMode::PerRun;
        workflows.put_step(&step).await.expect("put step");
        let run = workflows
            .launch(&tasks, &workflow.id, &serde_json::json!({}), "agent-1")
            .await
            .expect("launch");

        let path: String =
            sqlx::query_scalar("SELECT path FROM workflow_run_worktrees WHERE run_id = ?")
                .bind(&run.run.id)
                .fetch_one(&pool)
                .await
                .expect("path");

        let reaped = reap_run(&pool, &run.run.id).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].outcome, ReapOutcome::Removed);
        assert!(
            !Path::new(&path).exists(),
            "a clean checkout is gone: {path}"
        );

        let outcome: Option<String> =
            sqlx::query_scalar("SELECT reap_outcome FROM workflow_run_worktrees WHERE run_id = ?")
                .bind(&run.run.id)
                .fetch_one(&pool)
                .await
                .expect("outcome");
        assert_eq!(outcome.as_deref(), Some("removed"));
    }

    /// **The prohibition.** Uncommitted work from a failed run is evidence, not
    /// garbage — it is the thing you want when the question is "what did it
    /// actually do before it broke". Git refuses to delete a dirty tree and the
    /// reaper lets that refusal stand, records it, and moves on. If this ever
    /// starts passing because `--force` appeared somewhere, the checkout is gone
    /// and so is the only record of what the run was doing.
    #[tokio::test]
    async fn a_dirty_worktree_survives_reaping_and_the_refusal_is_recorded_against_the_run() {
        let (pool, _root, _project_id, repo_id) = fixture().await;
        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool.clone());

        let workflow = workflows
            .create_workflow("build", None, None)
            .await
            .expect("workflow");
        let mut step = base_step(&workflow.id, "build", &repo_id);
        step.worktree_mode = WorktreeMode::PerRun;
        workflows.put_step(&step).await.expect("put step");
        let run = workflows
            .launch(&tasks, &workflow.id, &serde_json::json!({}), "agent-1")
            .await
            .expect("launch");

        let path: String =
            sqlx::query_scalar("SELECT path FROM workflow_run_worktrees WHERE run_id = ?")
                .bind(&run.run.id)
                .fetch_one(&pool)
                .await
                .expect("path");

        // The work a failed run left behind.
        std::fs::write(
            Path::new(&path).join("README.md"),
            "half-finished edit nobody committed\n",
        )
        .expect("dirty the tree");

        let reaped = reap_run(&pool, &run.run.id).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(
            reaped[0].outcome,
            ReapOutcome::Refused,
            "a dirty tree must be refused, not deleted"
        );
        assert!(
            Path::new(&path).join("README.md").exists(),
            "the evidence is still there"
        );
        assert_eq!(
            std::fs::read_to_string(Path::new(&path).join("README.md")).expect("read"),
            "half-finished edit nobody committed\n"
        );

        let (outcome, detail): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT reap_outcome, reap_detail FROM workflow_run_worktrees WHERE run_id = ?",
        )
        .bind(&run.run.id)
        .fetch_one(&pool)
        .await
        .expect("row");
        assert_eq!(outcome.as_deref(), Some("refused"));
        assert!(
            detail.is_some_and(|value| !value.is_empty()),
            "the refusal records git's own words, so nobody has to guess which files are dirty"
        );

        // And the worktree stays visible as a project worktree, so a person can
        // still find it in the UI and go and look at the diff.
        let still_listed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_worktrees WHERE id = \
             (SELECT worktree_id FROM workflow_run_worktrees WHERE run_id = ?)",
        )
        .bind(&run.run.id)
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(still_listed, 1);
    }

    /// A branch task that exists without its checkout runs in the wrong
    /// directory — precisely the failure cwd enforcement was built to prevent,
    /// reintroduced one layer up. So the checkouts and the branch tasks land in
    /// the same transaction, and every branch comes out bound to its own.
    #[tokio::test]
    async fn a_per_branch_fan_out_provisions_one_checkout_per_branch_inside_the_expansion() {
        let (pool, _root, _project_id, repo_id) = fixture().await;
        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool.clone());

        let workflow = workflows
            .create_workflow("scan then build each", None, None)
            .await
            .expect("workflow");

        let scan = base_step(&workflow.id, "scan", &repo_id);
        workflows.put_step(&scan).await.expect("put scan");

        let mut build = base_step(&workflow.id, "build", &repo_id);
        build.position = 1;
        build.for_each_step_key = Some("scan".into());
        build.for_each_pointer = Some("/targets".into());
        build.for_each_key = Some("/name".into());
        build.worktree_mode = WorktreeMode::PerBranch;
        workflows.put_step(&build).await.expect("put build");
        workflows
            .link_steps(&workflow.id, "scan", "build")
            .await
            .expect("edge");

        let run = workflows
            .launch(&tasks, &workflow.id, &serde_json::json!({}), "agent-1")
            .await
            .expect("launch");

        let scan_number = run.task_numbers["scan"];
        tasks
            .submit_outputs(
                scan_number,
                &serde_json::json!({"targets": [{"name": "alpha"}, {"name": "beta"}]}),
            )
            .await
            .expect("outputs");
        tasks
            .update(
                scan_number,
                crate::tasks::UpdateTaskInput {
                    status: Some(TaskStatus::Ready),
                    ..Default::default()
                },
            )
            .await
            .expect("ready");
        tasks
            .update(
                scan_number,
                crate::tasks::UpdateTaskInput {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .await
            .expect("in progress");
        tasks
            .update(
                scan_number,
                crate::tasks::UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("done");

        let outcomes = tasks
            .expand_fan_outs_for(scan_number)
            .await
            .expect("expand");
        assert_eq!(outcomes.len(), 1, "one placeholder expanded: {outcomes:?}");

        let rows = sqlx::query(
            "SELECT branch_key, path, worktree_id FROM workflow_run_worktrees \
             WHERE run_id = ? ORDER BY branch_key",
        )
        .bind(&run.run.id)
        .fetch_all(&pool)
        .await
        .expect("rows");
        assert_eq!(rows.len(), 2, "one checkout per branch");
        for row in &rows {
            let path: String = row.try_get("path").expect("path");
            assert!(Path::new(&path).is_dir(), "{path} should exist");
        }

        // Every branch task is bound to *its own* checkout, and no two share.
        let bound: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT fan_out_branch_key, worktree_id FROM tasks \
             WHERE workflow_run_id = ? AND workflow_step_key = 'build' \
             ORDER BY fan_out_branch_key",
        )
        .bind(&run.run.id)
        .fetch_all(&pool)
        .await
        .expect("branches");
        assert_eq!(bound.len(), 2);
        assert_eq!(bound[0].0, "alpha");
        assert_eq!(bound[1].0, "beta");
        assert!(bound.iter().all(|(_, id)| id.is_some()));
        assert_ne!(
            bound[0].1, bound[1].1,
            "two branches sharing one checkout is the trampling this exists to stop"
        );

        // The reaper takes them all, and they are clean, so they all go.
        let reaped = reap_run(&pool, &run.run.id).await;
        assert_eq!(reaped.len(), 2);
        assert!(reaped.iter().all(|r| r.outcome == ReapOutcome::Removed));
    }

    /// A crash between `git worktree add` and the row that would have owned it
    /// leaves a directory nothing accounts for. The deterministic naming scheme
    /// is what makes it findable at all — and it is *listed*, never swept. The
    /// one thing worse than a stale worktree is a background process that
    /// deletes directories.
    #[tokio::test]
    async fn a_checkout_nothing_recorded_creating_is_reported_and_not_touched() {
        let (pool, root, project_id, _repo_id) = fixture().await;
        let repo_path = root.path().join("app");
        let orphan_path = root.path().join(WORKTREE_DIR).join("deadbeef-build");
        git_in(
            &repo_path,
            &[
                "worktree",
                "add",
                orphan_path.to_str().expect("utf-8"),
                "-b",
                "deadbeef-build",
                "HEAD",
            ],
        );

        let orphans = list_orphans(&pool, &project_id).await;
        assert_eq!(orphans.len(), 1, "{orphans:?}");
        assert_eq!(orphans[0].path, orphan_path.to_string_lossy());
        assert!(orphans[0].run_id.is_none());
        assert!(
            orphans[0].reason.contains("crash"),
            "the report says why nobody owns it: {}",
            orphans[0].reason
        );
        assert!(
            orphan_path.is_dir(),
            "reporting must not have removed anything"
        );
    }

    /// A worktree a person made somewhere else is their business. Reporting it
    /// as an orphan is how a report becomes one nobody reads.
    #[tokio::test]
    async fn a_worktree_outside_the_managed_directory_is_not_reported_as_an_orphan() {
        let (pool, root, project_id, _repo_id) = fixture().await;
        let repo_path = root.path().join("app");
        let mine = root.path().join("my-own-checkout");
        git_in(
            &repo_path,
            &[
                "worktree",
                "add",
                mine.to_str().expect("utf-8"),
                "-b",
                "mine",
                "HEAD",
            ],
        );

        assert!(list_orphans(&pool, &project_id).await.is_empty());
    }

    /// The name is the only thing that tells a person at 3am what made a
    /// leftover directory. If it ever becomes random, orphan reporting and
    /// crash re-derivation both stop working.
    #[test]
    fn a_worktree_name_is_derived_from_the_run_the_step_and_the_branch() {
        let run = "0191d3f2-4c1a-7000-8000-000000000000";
        assert_eq!(worktree_name(run, "lint", None), "0191d3f2-lint");
        assert_eq!(
            worktree_name(run, "build", Some("repo-a")),
            "0191d3f2-build-repo-a"
        );
    }

    /// Branch keys come from model-produced JSON, so they contain slashes,
    /// spaces and worse. A key that escaped into a path segment would create a
    /// worktree somewhere nobody asked for.
    #[test]
    fn a_branch_key_with_path_characters_cannot_escape_its_directory() {
        let name = worktree_name("0191d3f2", "build", Some("../../etc/passwd"));
        assert!(!name.contains('/'), "{name} must be one path segment");
        assert!(!name.contains(".."), "{name} must not traverse upwards");
    }
}
