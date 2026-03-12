//! Registry sync: discovers GitHub repos via `gh repo list` and reconciles
//! against the local registry store.

use super::store::{RegistryStore, UpsertRepoInput};
use crate::config::RegistryConfig;
use crate::error::Result;
use crate::messaging::MessagingManager;
use crate::messaging::target::parse_delivery_target;
use crate::OutboundResponse;

use anyhow::Context as _;
use arc_swap::ArcSwap;
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;
use tokio::time::Duration;

/// Result of a single sync pass.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncResult {
    pub repos_found: usize,
    pub new: usize,
    pub updated: usize,
    pub archived: usize,
    pub cloned: usize,
    pub errors: Vec<String>,
    /// Names of newly discovered repos (for notifications).
    pub new_repos: Vec<String>,
    /// Names of repos newly marked as archived (for notifications).
    pub archived_repos: Vec<String>,
}

/// Current sync status.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SyncStatus {
    Idle,
    Syncing,
    Failed {
        error: String,
        at: String,
    },
    Completed {
        at: String,
        result: SyncResult,
    },
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::Idle
    }
}

/// JSON shape returned by `gh repo list --json`.
#[derive(Debug, Deserialize)]
struct GhRepo {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "defaultBranchRef")]
    default_branch_ref: Option<GhBranchRef>,
    #[serde(rename = "isArchived", default)]
    is_archived: bool,
    #[serde(rename = "isFork", default)]
    is_fork: bool,
    #[serde(default)]
    visibility: String,
    #[serde(rename = "primaryLanguage")]
    primary_language: Option<GhLanguage>,
    #[serde(default)]
    url: String,
    #[serde(rename = "sshUrl", default)]
    ssh_url: String,
}

#[derive(Debug, Deserialize)]
struct GhBranchRef {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GhLanguage {
    name: String,
}

/// Discover repos for a single GitHub owner using the `gh` CLI.
async fn discover_github_repos(owner: &str) -> Result<Vec<GhRepo>> {
    let output = tokio::process::Command::new("gh")
        .args([
            "repo",
            "list",
            owner,
            "--json",
            "name,description,defaultBranchRef,isArchived,isFork,visibility,primaryLanguage,url,sshUrl",
            "--limit",
            "200",
        ])
        .output()
        .await
        .context("failed to run `gh repo list`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("`gh repo list {}` failed: {}", owner, stderr.trim()).into());
    }

    let repos: Vec<GhRepo> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh repo list output")?;
    Ok(repos)
}

/// Check if a repo matches any exclude pattern (simple glob with `*` support).
fn matches_exclude(full_name: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if pattern == full_name {
            return true;
        }
        // Support trailing wildcard: "owner/*-test" not supported, but "owner/prefix*" is.
        if let Some(prefix) = pattern.strip_suffix('*') {
            if full_name.starts_with(prefix) {
                return true;
            }
        }
    }
    false
}

/// Run a single sync pass: discover repos, upsert into store, mark absent repos.
pub async fn sync_registry(
    store: &RegistryStore,
    agent_id: &str,
    config: &RegistryConfig,
) -> Result<SyncResult> {
    let mut result = SyncResult::default();
    let mut all_seen: Vec<String> = Vec::new();

    for owner in &config.github_owners {
        match discover_github_repos(owner).await {
            Ok(repos) => {
                for gh_repo in repos {
                    let full_name = format!("{}/{}", owner, gh_repo.name);

                    // Apply exclude patterns
                    if matches_exclude(&full_name, &config.exclude_patterns) {
                        continue;
                    }

                    all_seen.push(full_name.clone());
                    result.repos_found += 1;

                    let was_known = store
                        .get_by_full_name(agent_id, &full_name)
                        .await?
                        .is_some();

                    let input = UpsertRepoInput {
                        agent_id: agent_id.to_string(),
                        owner: owner.clone(),
                        name: gh_repo.name.clone(),
                        full_name: full_name.clone(),
                        description: gh_repo.description,
                        default_branch: gh_repo
                            .default_branch_ref
                            .map(|b| b.name)
                            .unwrap_or_else(|| "main".into()),
                        is_archived: gh_repo.is_archived,
                        is_fork: gh_repo.is_fork,
                        visibility: gh_repo.visibility.to_lowercase(),
                        language: gh_repo.primary_language.map(|l| l.name),
                        clone_url: gh_repo.url.clone(),
                        ssh_url: gh_repo.ssh_url,
                    };

                    store.upsert_repo(input).await?;

                    if was_known {
                        result.updated += 1;
                    } else {
                        result.new += 1;
                        result.new_repos.push(full_name.clone());

                        // Auto-clone if configured
                        if config.auto_clone && !gh_repo.is_archived {
                            let clone_dir = config.clone_base_dir.join(&gh_repo.name);
                            if !clone_dir.exists() {
                                match auto_clone_repo(&full_name, &clone_dir).await {
                                    Ok(()) => {
                                        store
                                            .set_local_path(
                                                agent_id,
                                                &full_name,
                                                &clone_dir.to_string_lossy(),
                                            )
                                            .await?;
                                        result.cloned += 1;
                                        tracing::info!(
                                            repo = %full_name,
                                            path = %clone_dir.display(),
                                            "auto-cloned new repo"
                                        );
                                    }
                                    Err(e) => {
                                        let msg =
                                            format!("failed to clone {}: {}", full_name, e);
                                        tracing::warn!("{}", msg);
                                        result.errors.push(msg);
                                    }
                                }
                            } else {
                                // Directory exists, just record the path
                                store
                                    .set_local_path(
                                        agent_id,
                                        &full_name,
                                        &clone_dir.to_string_lossy(),
                                    )
                                    .await?;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let msg = format!("failed to discover repos for {}: {}", owner, e);
                tracing::warn!("{}", msg);
                result.errors.push(msg);
            }
        }
    }

    // Mark repos not seen in this sync as archived
    if !all_seen.is_empty() {
        // Snapshot non-archived names before marking, so we can report which ones changed.
        let before = store.get_non_archived_names(agent_id).await?;
        result.archived = store
            .mark_absent_as_archived(agent_id, &all_seen)
            .await? as usize;
        if result.archived > 0 {
            let seen_set: std::collections::HashSet<&str> =
                all_seen.iter().map(|s| s.as_str()).collect();
            result.archived_repos = before
                .into_iter()
                .filter(|name| !seen_set.contains(name.as_str()))
                .collect();
        }
    }

    Ok(result)
}

/// Clone a repo using `gh repo clone`.
async fn auto_clone_repo(full_name: &str, target_dir: &Path) -> Result<()> {
    let output = tokio::process::Command::new("gh")
        .args([
            "repo",
            "clone",
            full_name,
            &target_dir.to_string_lossy(),
        ])
        .output()
        .await
        .context("failed to run `gh repo clone`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("gh repo clone failed: {}", stderr.trim()).into());
    }
    Ok(())
}

/// Background loop that periodically syncs the registry.
///
/// Reads the current `RegistryConfig` from `runtime_config` on each iteration
/// so hot-reloads take effect without restarting.
pub async fn registry_sync_loop(
    store: RegistryStore,
    agent_id: String,
    runtime_config: Arc<crate::config::RuntimeConfig>,
    status: Arc<ArcSwap<SyncStatus>>,
    messaging_manager: Option<Arc<MessagingManager>>,
) {
    // Initial delay to let the agent fully start up.
    tokio::time::sleep(Duration::from_secs(30)).await;

    loop {
        let current_config = runtime_config.registry.load();
        if !current_config.enabled {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        let sync_interval = current_config.sync_interval_secs;

        status.store(Arc::new(SyncStatus::Syncing));
        tracing::info!(agent_id = %agent_id, "starting registry sync");

        match sync_registry(&store, &agent_id, &current_config).await {
            Ok(result) => {
                tracing::info!(
                    agent_id = %agent_id,
                    found = result.repos_found,
                    new = result.new,
                    archived = result.archived,
                    cloned = result.cloned,
                    errors = result.errors.len(),
                    "registry sync completed"
                );

                // Send notification if there are new or archived repos.
                if !result.new_repos.is_empty() || !result.archived_repos.is_empty() {
                    send_sync_notification(
                        &current_config,
                        &messaging_manager,
                        &result,
                    )
                    .await;
                }

                status.store(Arc::new(SyncStatus::Completed {
                    at: chrono::Utc::now().to_rfc3339(),
                    result,
                }));
            }
            Err(e) => {
                tracing::error!(agent_id = %agent_id, error = %e, "registry sync failed");
                status.store(Arc::new(SyncStatus::Failed {
                    error: e.to_string(),
                    at: chrono::Utc::now().to_rfc3339(),
                }));
            }
        }

        tokio::time::sleep(Duration::from_secs(sync_interval)).await;
    }
}

/// Build and send a notification about new/archived repos after a sync pass.
async fn send_sync_notification(
    config: &RegistryConfig,
    messaging_manager: &Option<Arc<MessagingManager>>,
    result: &SyncResult,
) {
    let target_str = match &config.notification_target {
        Some(t) => t,
        None => return,
    };
    let mm = match messaging_manager {
        Some(m) => m,
        None => return,
    };
    let target = match parse_delivery_target(target_str) {
        Some(t) => t,
        None => {
            tracing::warn!(
                target = %target_str,
                "invalid notification_target for registry sync"
            );
            return;
        }
    };

    let mut lines = Vec::new();
    lines.push("📦 Registry sync update:".to_string());
    if !result.new_repos.is_empty() {
        lines.push(format!("\nNew repos ({}):", result.new_repos.len()));
        for name in &result.new_repos {
            lines.push(format!("  + {}", name));
        }
    }
    if !result.archived_repos.is_empty() {
        lines.push(format!("\nArchived repos ({}):", result.archived_repos.len()));
        for name in &result.archived_repos {
            lines.push(format!("  − {}", name));
        }
    }
    let text = lines.join("\n");

    if let Err(e) = mm
        .broadcast(&target.adapter, &target.target, OutboundResponse::Text(text))
        .await
    {
        tracing::warn!(error = %e, "failed to send registry sync notification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_exclude() {
        assert!(matches_exclude("marcmantei/liralot-config", &["marcmantei/liralot-config".into()]));
        assert!(matches_exclude("marcmantei/test-repo", &["marcmantei/test*".into()]));
        assert!(!matches_exclude("marcmantei/ChargePilot", &["marcmantei/liralot-config".into()]));
        assert!(!matches_exclude("other/repo", &["marcmantei/*".into()]));
    }

    #[test]
    fn test_gh_repo_deserialization() {
        let json = r#"[
            {
                "name": "test-repo",
                "description": "A test",
                "defaultBranchRef": {"name": "main"},
                "isArchived": false,
                "isFork": false,
                "visibility": "PRIVATE",
                "primaryLanguage": {"name": "Rust"},
                "url": "https://github.com/owner/test-repo",
                "sshUrl": "git@github.com:owner/test-repo.git"
            }
        ]"#;
        let repos: Vec<GhRepo> = serde_json::from_str(json).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "test-repo");
        assert_eq!(repos[0].primary_language.as_ref().unwrap().name, "Rust");
    }
}
