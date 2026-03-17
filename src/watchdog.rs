//! Startup catch-up and health watchdog.
//!
//! - **Catch-up**: After boot, scans enabled repos for open issues that were
//!   missed during downtime (no `prd-generated` label) and injects synthetic
//!   webhook messages so the agent processes them.
//!
//! - **Watchdog**: Periodically checks whether the daemon is still processing
//!   messages. If no activity is observed for a configurable timeout, the
//!   process exits so systemd can restart it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::registry::RegistryStore;
use crate::{InboundMessage, MessageContent};

// ---------------------------------------------------------------------------
// Startup catch-up
// ---------------------------------------------------------------------------

/// Issue metadata returned by `gh issue list --json`.
#[derive(Debug, serde::Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    body: Option<String>,
    labels: Vec<GhLabel>,
    #[serde(rename = "createdAt")]
    #[allow(dead_code)]
    created_at: String,
    author: Option<GhAuthor>,
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct GhAuthor {
    login: String,
}

/// Scan all enabled repos for the given agent and inject synthetic
/// `issues.opened` messages for any open issues that lack the
/// `prd-generated` label (i.e. were missed during downtime).
///
/// Only considers issues created in the last 24 hours to avoid
/// re-processing ancient issues on first deploy.
pub async fn run_startup_catchup(
    agent_id: &str,
    registry_store: &Arc<RegistryStore>,
    injection_tx: &mpsc::Sender<crate::ChannelInjection>,
) {
    let repos = match registry_store.list_repos(agent_id, true).await {
        Ok(repos) => repos,
        Err(error) => {
            tracing::warn!(%error, "startup catch-up: failed to list repos");
            return;
        }
    };

    if repos.is_empty() {
        tracing::debug!("startup catch-up: no enabled repos, skipping");
        return;
    }

    let mut total_injected = 0u32;

    for repo in &repos {
        let full_name = &repo.full_name;
        let issues = match discover_unprocessed_issues(full_name).await {
            Ok(issues) => issues,
            Err(error) => {
                tracing::warn!(
                    repo = %full_name,
                    %error,
                    "startup catch-up: failed to query issues"
                );
                continue;
            }
        };

        for issue in &issues {
            let author = issue
                .author
                .as_ref()
                .map(|a| a.login.as_str())
                .unwrap_or("unknown");

            let labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
            let labels_str = if labels.is_empty() {
                "none".to_string()
            } else {
                labels.join(", ")
            };

            let body_preview = issue
                .body
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(500)
                .collect::<String>();

            let content = format!(
                "GitHub webhook: issues.opened\n\
                 Repo: {full_name}\n\
                 Issue #{number}: {title}\n\
                 Author: {author}\n\
                 Labels: {labels_str}\n\
                 URL: {url}\n\
                 Body:\n{body}",
                number = issue.number,
                title = issue.title,
                url = issue.url,
                body = body_preview,
            );

            let conversation_id = format!("webhook:github:{full_name}");

            let mut metadata = HashMap::new();
            metadata.insert(
                "webhook_conversation_id".into(),
                serde_json::Value::String(format!("github:{full_name}")),
            );
            metadata.insert(
                "display_name".into(),
                serde_json::Value::String("github-webhook".into()),
            );
            metadata.insert(
                "sender_display_name".into(),
                serde_json::Value::String("github-webhook".into()),
            );
            metadata.insert(
                crate::metadata_keys::CHANNEL_NAME.into(),
                serde_json::Value::String(format!("github:{full_name}")),
            );
            metadata.insert(
                "startup_catchup".into(),
                serde_json::Value::Bool(true),
            );

            let message = InboundMessage {
                id: uuid::Uuid::new_v4().to_string(),
                source: "webhook".into(),
                adapter: Some("webhook".into()),
                conversation_id: conversation_id.clone(),
                sender_id: "github-webhook".into(),
                agent_id: Some(Arc::from(agent_id)),
                content: MessageContent::Text(content),
                timestamp: chrono::Utc::now(),
                metadata,
                formatted_author: Some("github-webhook".into()),
            };

            let injection = crate::ChannelInjection {
                conversation_id,
                agent_id: agent_id.to_string(),
                message,
            };

            if let Err(error) = injection_tx.send(injection).await {
                tracing::warn!(
                    repo = %full_name,
                    issue = issue.number,
                    %error,
                    "startup catch-up: failed to inject issue"
                );
            } else {
                tracing::info!(
                    repo = %full_name,
                    issue = issue.number,
                    title = %issue.title,
                    "startup catch-up: injected missed issue"
                );
                total_injected += 1;
            }
        }
    }

    if total_injected > 0 {
        tracing::info!(
            count = total_injected,
            "startup catch-up complete: injected missed issues"
        );
    } else {
        tracing::info!("startup catch-up complete: no missed issues found");
    }
}

/// Query GitHub for open issues on `repo` that do NOT have the
/// `prd-generated` label and were created in the last 24 hours.
async fn discover_unprocessed_issues(repo: &str) -> anyhow::Result<Vec<GhIssue>> {
    let since = (chrono::Utc::now() - chrono::Duration::hours(24))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    // gh issue list with search query: no prd-generated label, created since cutoff
    let output = tokio::process::Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--search",
            &format!("-label:prd-generated created:>{since}"),
            "--json",
            "number,title,body,labels,createdAt,author,url",
            "--limit",
            "20",
        ])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh issue list failed: {stderr}");
    }

    let issues: Vec<GhIssue> = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse gh output: {e}"))?;

    Ok(issues)
}

// ---------------------------------------------------------------------------
// Health watchdog
// ---------------------------------------------------------------------------

/// Spawn a background task that monitors message processing activity.
///
/// If no messages are processed for `timeout` duration, the process exits
/// with code 1 so systemd can restart it. The watchdog checks every
/// `check_interval`.
///
/// Returns a handle that should be used to report activity via
/// `WatchdogHandle::ping()`.
pub fn spawn_watchdog(timeout: Duration, check_interval: Duration) -> WatchdogHandle {
    let last_activity = Arc::new(std::sync::atomic::AtomicU64::new(
        instant_to_epoch_secs(Instant::now()),
    ));

    let handle = WatchdogHandle {
        last_activity: last_activity.clone(),
    };

    tokio::spawn(async move {
        tracing::info!(
            timeout_secs = timeout.as_secs(),
            check_interval_secs = check_interval.as_secs(),
            "health watchdog started"
        );

        loop {
            tokio::time::sleep(check_interval).await;

            let last = last_activity.load(std::sync::atomic::Ordering::Relaxed);
            let now = instant_to_epoch_secs(Instant::now());
            let elapsed = Duration::from_secs(now.saturating_sub(last));

            if elapsed > timeout {
                tracing::error!(
                    elapsed_secs = elapsed.as_secs(),
                    timeout_secs = timeout.as_secs(),
                    "health watchdog: no activity detected, exiting for restart"
                );
                // Give tracing a moment to flush
                tokio::time::sleep(Duration::from_millis(500)).await;
                std::process::exit(1);
            } else {
                tracing::debug!(
                    elapsed_secs = elapsed.as_secs(),
                    "health watchdog: activity detected, all good"
                );
            }
        }
    });

    handle
}

/// Handle for reporting activity to the watchdog.
#[derive(Clone)]
pub struct WatchdogHandle {
    last_activity: Arc<std::sync::atomic::AtomicU64>,
}

impl WatchdogHandle {
    /// Report that a message was processed, resetting the watchdog timer.
    pub fn ping(&self) {
        self.last_activity.store(
            instant_to_epoch_secs(Instant::now()),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// Convert an `Instant` to a monotonic epoch-seconds value.
/// Uses `Instant::now().elapsed()` trick to get a stable u64 for atomic ops.
fn instant_to_epoch_secs(instant: Instant) -> u64 {
    // We use system time here since Instant doesn't expose raw values.
    // This is fine — we only compare deltas, not absolute values.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        // Adjust for the difference between `instant` and `Instant::now()`
        .wrapping_sub(Instant::now().duration_since(instant).as_secs())
}
