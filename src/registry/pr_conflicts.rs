//! PR conflict checker: periodically scans open PRs across registered repos
//! for merge conflicts and injects a message into the webhook channel so the
//! agent can spawn a worker to resolve them.

use crate::registry::store::RegistryStore;

use serde::Deserialize;
use std::sync::Arc;
use tokio::time::Duration;

/// Default interval between conflict check passes (10 minutes).
const DEFAULT_CHECK_INTERVAL_SECS: u64 = 600;

/// JSON shape returned by `gh pr list --json`.
#[derive(Debug, Deserialize)]
struct GhPr {
    number: u64,
    title: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    url: String,
    mergeable: String,
}

/// Query open PRs for a single repo using `gh pr list`.
async fn list_open_prs(repo: &str) -> Result<Vec<GhPr>, String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--json",
            "number,title,headRefName,baseRefName,url,mergeable",
            "--limit",
            "50",
        ])
        .output()
        .await
        .map_err(|e| format!("failed to run `gh pr list`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`gh pr list --repo {repo}` failed: {}",
            stderr.trim()
        ));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("failed to parse gh pr list output for {repo}: {e}"))
}

/// Build a message that instructs the agent to fix a conflicting PR.
fn build_conflict_message(repo: &str, pr: &GhPr) -> String {
    format!(
        "PR conflict detected — automatic resolution requested.\n\
         Repo: {repo}\n\
         PR #{}: {}\n\
         Branch: {} -> {}\n\
         URL: {}\n\
         Status: CONFLICTING\n\n\
         Resolve the merge conflicts on this PR. Check out the branch, \
         rebase or merge the base branch, fix any conflicts, and push.",
        pr.number, pr.title, pr.head_ref_name, pr.base_ref_name, pr.url,
    )
}

/// Run a single conflict check pass across all registered repos.
async fn check_all_repos(store: &RegistryStore, agent_id: &str) -> Vec<(String, GhPr)> {
    let repos = match store.list_repos(agent_id, true).await {
        Ok(repos) => repos,
        Err(e) => {
            tracing::warn!(error = %e, "failed to list repos for PR conflict check");
            return Vec::new();
        }
    };

    let mut conflicting = Vec::new();

    for repo in &repos {
        match list_open_prs(&repo.full_name).await {
            Ok(prs) => {
                for pr in prs {
                    if pr.mergeable == "CONFLICTING" {
                        tracing::info!(
                            repo = %repo.full_name,
                            pr_number = pr.number,
                            pr_title = %pr.title,
                            "PR has merge conflicts"
                        );
                        conflicting.push((repo.full_name.clone(), pr));
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    repo = %repo.full_name,
                    error = %e,
                    "skipping repo for conflict check"
                );
            }
        }
    }

    conflicting
}

/// Forward a conflict message to the Spacebot webhook adapter so it enters
/// the normal message routing pipeline.
async fn forward_to_webhook(webhook_url: &str, repo: &str, message: &str) -> Result<(), String> {
    let conversation_id = format!("github:{repo}");
    let payload = serde_json::json!({
        "conversation_id": conversation_id,
        "sender_id": "pr-conflict-checker",
        "content": message,
    });

    let client = reqwest::Client::new();
    client
        .post(webhook_url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("failed to forward conflict message: {e}"))?;

    Ok(())
}

/// Background loop that checks for PR conflicts at a regular interval.
///
/// When a conflicting PR is found, a message is forwarded to the
/// webhook adapter at `webhook_url`, creating a conversation in the
/// `github:{repo}` channel so the agent can spawn a worker to fix it.
pub async fn pr_conflict_check_loop(
    store: RegistryStore,
    agent_id: String,
    runtime_config: Arc<crate::config::RuntimeConfig>,
    webhook_port: u16,
) {
    // Initial delay to let the agent fully start up and registry sync to run first.
    tokio::time::sleep(Duration::from_secs(90)).await;

    let webhook_url = format!("http://127.0.0.1:{webhook_port}/send");

    // Track which PRs we've already sent a conflict message for,
    // so we don't spam the channel on every check pass.
    let mut notified: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();

    loop {
        let current_config = runtime_config.registry.load();
        if !current_config.enabled {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        let check_interval = current_config
            .pr_conflict_check_interval_secs
            .unwrap_or(DEFAULT_CHECK_INTERVAL_SECS);

        if check_interval == 0 {
            // Disabled via config.
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        let conflicting = check_all_repos(&store, &agent_id).await;

        for (repo, pr) in &conflicting {
            let key = (repo.clone(), pr.number);
            if notified.contains(&key) {
                continue;
            }

            let message = build_conflict_message(repo, pr);
            match forward_to_webhook(&webhook_url, repo, &message).await {
                Ok(()) => {
                    tracing::info!(
                        repo = %repo,
                        pr_number = pr.number,
                        "sent conflict resolution request to channel"
                    );
                    notified.insert(key);
                }
                Err(e) => {
                    tracing::warn!(
                        repo = %repo,
                        pr_number = pr.number,
                        error = %e,
                        "failed to send conflict resolution request"
                    );
                }
            }
        }

        // Clean up notified set: remove entries for PRs that are no longer conflicting.
        let current_conflicts: std::collections::HashSet<(String, u64)> = conflicting
            .iter()
            .map(|(repo, pr)| (repo.clone(), pr.number))
            .collect();
        notified.retain(|key| current_conflicts.contains(key));

        tokio::time::sleep(Duration::from_secs(check_interval)).await;
    }
}
