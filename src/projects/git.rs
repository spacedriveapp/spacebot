//! Git helpers for project management: repo discovery, worktree operations.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use tokio::process::Command;

/// Information about a discovered git repository.
#[derive(Debug, Clone)]
pub struct DiscoveredRepo {
    /// Directory name (e.g., "spacebot-dash").
    pub name: String,
    /// Path relative to the project root.
    pub relative_path: String,
    /// Primary remote URL (origin), if any.
    pub remote_url: String,
    /// Default branch name (remote's default, e.g. "main").
    pub default_branch: String,
    /// Currently checked-out branch (from `git rev-parse --abbrev-ref HEAD`).
    pub current_branch: Option<String>,
}

/// Information about an existing git worktree.
#[derive(Debug, Clone)]
pub struct DiscoveredWorktree {
    /// Absolute path to the worktree.
    pub path: PathBuf,
    /// Branch name checked out in this worktree.
    pub branch: String,
}

/// Scan a directory for git repositories.
///
/// First checks if the project root itself is a git repo (single-repo project).
/// If so, returns it with `relative_path: "."`. Otherwise scans immediate
/// children for git repos (multi-repo project). Skips hidden directories and
/// directories that have a `.git` *file* (worktrees of another repo).
pub async fn discover_repos(project_root: &Path) -> anyhow::Result<Vec<DiscoveredRepo>> {
    // Check if the project root itself is a git repo (single-repo project).
    let root_dot_git = project_root.join(".git");
    if root_dot_git.exists() {
        let name = project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo")
            .to_string();

        let remote_url = get_remote_url(project_root).await.unwrap_or_default();
        let default_branch = get_default_branch(project_root)
            .await
            .unwrap_or_else(|| "main".into());
        let current_branch = get_current_branch(project_root).await;

        return Ok(vec![DiscoveredRepo {
            name,
            relative_path: ".".to_string(),
            remote_url,
            default_branch,
            current_branch,
        }]);
    }

    // Multi-repo project — scan immediate children.
    let mut repos = Vec::new();
    let mut entries = tokio::fs::read_dir(project_root)
        .await
        .with_context(|| format!("failed to read project root: {}", project_root.display()))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .context("failed to read directory entry")?
    {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };

        // Skip hidden directories.
        if name.starts_with('.') {
            continue;
        }

        // Check if this is a git repo by looking for .git *directory*.
        // Worktrees have a .git *file* (not directory) — skip those so they
        // are discovered as worktrees of their parent repo instead.
        let dot_git = path.join(".git");
        if !dot_git.exists() || !dot_git.is_dir() {
            continue;
        }

        let remote_url = get_remote_url(&path).await.unwrap_or_default();
        let default_branch = get_default_branch(&path)
            .await
            .unwrap_or_else(|| "main".into());
        let current_branch = get_current_branch(&path).await;

        repos.push(DiscoveredRepo {
            name: name.clone(),
            relative_path: name,
            remote_url,
            default_branch,
            current_branch,
        });
    }

    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(repos)
}

/// List existing worktrees for a git repo.
///
/// Parses `git worktree list --porcelain` output. Returns worktrees other than
/// the main working tree.
pub async fn list_worktrees(repo_path: &Path) -> anyhow::Result<Vec<DiscoveredWorktree>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_path)
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to run `git worktree list` in {}",
                repo_path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git worktree list failed in {}: {}",
            repo_path.display(),
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;
    let mut is_first = true;

    for line in stdout.lines() {
        if line.is_empty() {
            // Blank line separates worktree entries.
            if let (Some(path), Some(branch)) = (current_path.take(), current_branch.take()) {
                // Skip the first entry — that's the main working tree.
                if !is_first {
                    worktrees.push(DiscoveredWorktree { path, branch });
                }
                is_first = false;
            } else if current_path.is_some() {
                // Detached HEAD or bare — skip.
                current_path = None;
                current_branch = None;
                is_first = false;
            }
            continue;
        }

        if let Some(path_str) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(path_str));
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            // branch refs/heads/feat/foo → feat/foo
            let branch = branch_ref.strip_prefix("refs/heads/").unwrap_or(branch_ref);
            current_branch = Some(branch.to_string());
        }
    }

    // Handle the last entry (porcelain output ends without a trailing blank line).
    if let (Some(path), Some(branch)) = (current_path, current_branch)
        && !is_first
    {
        worktrees.push(DiscoveredWorktree { path, branch });
    }

    Ok(worktrees)
}

/// Create a new git worktree.
///
/// Runs `git worktree add <worktree_path> -b <branch> <start_point>` or
/// `git worktree add <worktree_path> <branch>` if the branch already exists.
pub async fn create_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch: &str,
    start_point: Option<&str>,
) -> anyhow::Result<()> {
    let worktree_str = worktree_path
        .to_str()
        .context("worktree path is not valid UTF-8")?;

    // Try creating with a new branch first.
    let start = start_point.unwrap_or("HEAD");
    let output = Command::new("git")
        .args(["worktree", "add", worktree_str, "-b", branch, start])
        .current_dir(repo_path)
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to run `git worktree add` in {}",
                repo_path.display()
            )
        })?;

    if output.status.success() {
        return Ok(());
    }

    // If the branch already exists, try without -b.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("already exists") {
        let output = Command::new("git")
            .args(["worktree", "add", worktree_str, branch])
            .current_dir(repo_path)
            .output()
            .await
            .with_context(|| {
                format!(
                    "failed to run `git worktree add` in {}",
                    repo_path.display()
                )
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git worktree add failed in {}: {}",
            repo_path.display(),
            stderr.trim()
        );
    }

    anyhow::bail!(
        "git worktree add failed in {}: {}",
        repo_path.display(),
        stderr.trim()
    );
}

/// Remove a git worktree.
///
/// Runs `git worktree remove <worktree_path>`.
pub async fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> anyhow::Result<()> {
    let worktree_str = worktree_path
        .to_str()
        .context("worktree path is not valid UTF-8")?;

    let output = Command::new("git")
        .args(["worktree", "remove", "--force", worktree_str])
        .current_dir(repo_path)
        .output()
        .await
        .with_context(|| {
            format!(
                "failed to run `git worktree remove` in {}",
                repo_path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git worktree remove failed in {}: {}",
            repo_path.display(),
            stderr.trim()
        );
    }

    Ok(())
}

/// Strip embedded credentials (userinfo) from a URL.
///
/// `https://user:token@github.com/org/repo.git` → `https://github.com/org/repo.git`
/// Non-URL remotes (e.g., SSH `git@...`) are returned as-is.
fn scrub_remote_url(url: &str) -> String {
    // Only strip from http(s) URLs that contain a `@` before the first `/` after `://`.
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let scheme = if url.starts_with("https://") {
            "https://"
        } else {
            "http://"
        };
        if let Some(at_pos) = rest.find('@') {
            // Only strip if `@` comes before the first `/` (i.e., it's in the authority).
            let slash_pos = rest.find('/').unwrap_or(rest.len());
            if at_pos < slash_pos {
                return format!("{scheme}{}", &rest[at_pos + 1..]);
            }
        }
        return url.to_string();
    }
    url.to_string()
}

/// Get the origin remote URL for a repo. Credentials are scrubbed from HTTPS URLs.
async fn get_remote_url(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        None
    } else {
        Some(scrub_remote_url(&url))
    }
}

/// Get the currently checked-out branch for a repo.
///
/// Returns `None` for detached HEAD or when git is unavailable.
pub async fn get_current_branch(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_path)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

/// Get the default branch for a repo (from origin/HEAD or fallback to "main").
async fn get_default_branch(repo_path: &Path) -> Option<String> {
    // Try symbolic-ref of origin/HEAD first.
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_path)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let refname = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // refs/remotes/origin/main → main
        if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
            return Some(branch.to_string());
        }
    }

    // Fallback: check if HEAD points to a branch.
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_path)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() && branch != "HEAD" {
            return Some(branch);
        }
    }

    None
}
