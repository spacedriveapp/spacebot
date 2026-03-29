//! Native source-based upgrade: fetch, build, swap binary, restart service.
//!
//! This module powers `spacebot upgrade` for native/source installs.
//! It automates: git fetch → git pull → cargo build --release → stop → copy → restart.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Result of checking for available updates.
#[derive(Debug)]
pub struct UpgradeCheck {
    pub has_updates: bool,
    pub commit_count: usize,
    pub commit_summaries: Vec<String>,
    pub local_head: String,
    pub remote_head: String,
}

/// Locate the source directory. Checks (in order):
/// 1. Explicit `--source-dir` argument
/// 2. `SPACEBOT_SRC_DIR` environment variable
/// 3. `~/.spacebot-src` (convention on this machine)
pub fn resolve_source_dir(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = explicit {
        return validate_source_dir(dir);
    }

    if let Ok(env_dir) = std::env::var("SPACEBOT_SRC_DIR") {
        let p = PathBuf::from(env_dir);
        return validate_source_dir(&p);
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let default = home.join(".spacebot-src");
    validate_source_dir(&default)
}

fn validate_source_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let dir = dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("source directory '{}' not found: {e}", dir.display()))?;

    if !dir.join("Cargo.toml").exists() {
        anyhow::bail!(
            "'{}' does not look like a Spacebot source checkout (no Cargo.toml)",
            dir.display()
        );
    }
    if !dir.join(".git").exists() {
        anyhow::bail!("'{}' is not a git repository", dir.display());
    }

    Ok(dir)
}

/// Resolve the install path for the binary.
pub fn resolve_install_path(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }

    // Use the path of the currently running binary
    let current = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cannot determine current binary path: {e}"))?;
    Ok(current)
}

/// Fetch from remote and check if there are new commits.
pub fn check_for_updates(
    source_dir: &Path,
    remote: &str,
    branch: &str,
) -> anyhow::Result<UpgradeCheck> {
    // git fetch
    run_git(source_dir, &["fetch", remote])?;

    let remote_ref = format!("{remote}/{branch}");

    // Get local HEAD
    let local_head = run_git(source_dir, &["rev-parse", "HEAD"])?;
    let local_head = local_head.trim().to_string();

    // Get remote HEAD
    let remote_head = run_git(source_dir, &["rev-parse", &remote_ref])?;
    let remote_head = remote_head.trim().to_string();

    if local_head == remote_head {
        return Ok(UpgradeCheck {
            has_updates: false,
            commit_count: 0,
            commit_summaries: vec![],
            local_head,
            remote_head,
        });
    }

    // Get new commits
    let range = format!("HEAD..{remote_ref}");
    let log_output = run_git(source_dir, &["log", "--oneline", &range])?;
    let summaries: Vec<String> = log_output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(UpgradeCheck {
        has_updates: true,
        commit_count: summaries.len(),
        commit_summaries: summaries,
        local_head,
        remote_head,
    })
}

/// Get the current branch name.
pub fn current_branch(source_dir: &Path) -> anyhow::Result<String> {
    let output = run_git(source_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(output.trim().to_string())
}

/// Pull latest changes.
pub fn pull(source_dir: &Path) -> anyhow::Result<String> {
    run_git(source_dir, &["pull"])
}

/// Build the release binary, streaming cargo output to the callback.
pub fn build_release(source_dir: &Path, on_line: impl Fn(&str)) -> anyhow::Result<PathBuf> {
    let mut child = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(source_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn cargo build: {e}"))?;

    // Cargo writes progress to stderr
    let stderr = child.stderr.take().unwrap();
    let reader = BufReader::new(stderr);
    for line in reader.lines().map_while(Result::ok) {
        on_line(&line);
    }

    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("cargo build failed with exit code {}", status);
    }

    let binary = source_dir.join("target/release/spacebot");
    if !binary.exists() {
        anyhow::bail!("built binary not found at {}", binary.display());
    }

    Ok(binary)
}

/// Install the built binary to the target path.
/// Uses a temp file + rename for atomicity.
pub fn install_binary(built: &Path, install_path: &Path) -> anyhow::Result<()> {
    let parent = install_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("install path has no parent directory"))?;

    let tmp_path = parent.join(".spacebot.upgrade.tmp");

    std::fs::copy(built, &tmp_path)
        .map_err(|e| anyhow::anyhow!("failed to copy binary to {}: {e}", tmp_path.display()))?;

    // Preserve executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)?;
    }

    std::fs::rename(&tmp_path, install_path).map_err(|e| {
        // Clean up tmp file on rename failure
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::anyhow!(
            "failed to rename {} → {}: {e}",
            tmp_path.display(),
            install_path.display()
        )
    })?;

    Ok(())
}

/// Restart the spacebot systemd user service.
pub fn restart_service() -> anyhow::Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "restart", "spacebot"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run systemctl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemctl restart failed: {stderr}");
    }

    Ok(())
}

fn run_git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git {}: {e}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {stderr}", args.join(" "));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
