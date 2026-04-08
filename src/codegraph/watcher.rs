//! File watcher for real-time incremental graph updates.
//!
//! Uses the `notify` crate with 500ms debounce to detect file changes,
//! then forwards a batch of absolute paths to the manager via an mpsc
//! channel. The manager drives the incremental indexing pipeline; this
//! module is only responsible for debounced change detection.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc, watch};

use super::db::SharedCodeGraphDb;
use super::events::CodeGraphEvent;
use super::types::CodeGraphConfig;

/// A batch of file paths that changed within a single debounce window.
/// Paths are absolute so downstream consumers can read the files directly.
pub type ChangeBatch = Vec<PathBuf>;

/// Handle to a running file watcher. Drop to stop watching.
pub struct WatcherHandle {
    /// Send `true` to stop the watcher.
    stop_tx: watch::Sender<bool>,
}

impl WatcherHandle {
    /// Stop the file watcher.
    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start watching a project directory for file changes.
///
/// When the debounce window elapses with pending changes, the watcher:
/// 1. Fires a `GraphStale` event on `event_tx` so the UI can show a badge.
/// 2. Sends the batch of changed absolute paths on `change_tx` so the
///    manager can trigger an incremental re-index.
pub fn start_watcher(
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    config: Arc<CodeGraphConfig>,
    event_tx: broadcast::Sender<CodeGraphEvent>,
    change_tx: mpsc::Sender<ChangeBatch>,
) -> Result<WatcherHandle> {
    let (stop_tx, stop_rx) = watch::channel(false);

    let debounce_ms = config.debounce_ms.max(100);

    tokio::spawn(watch_loop(
        project_id,
        root_path,
        db,
        config,
        event_tx,
        change_tx,
        stop_rx,
        debounce_ms,
    ));

    Ok(WatcherHandle { stop_tx })
}

/// The main watcher loop.
#[allow(clippy::too_many_arguments)]
async fn watch_loop(
    project_id: String,
    root_path: PathBuf,
    _db: SharedCodeGraphDb,
    _config: Arc<CodeGraphConfig>,
    event_tx: broadcast::Sender<CodeGraphEvent>,
    change_tx: mpsc::Sender<ChangeBatch>,
    mut stop_rx: watch::Receiver<bool>,
    debounce_ms: u64,
) {
    tracing::info!(
        project_id = %project_id,
        path = %root_path.display(),
        debounce_ms = debounce_ms,
        "starting file watcher"
    );

    // Channel for notify events.
    let (notify_tx, mut notify_rx) = mpsc::channel::<notify::Event>(256);

    // Set up the notify watcher.
    let watcher_result = {
        let notify_tx = notify_tx.clone();
        notify::recommended_watcher(move |res: std::result::Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = notify_tx.blocking_send(event);
            }
        })
    };

    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(err) => {
            tracing::error!(
                project_id = %project_id,
                %err,
                "failed to create file watcher"
            );
            return;
        }
    };

    use notify::Watcher;
    if let Err(err) = watcher.watch(&root_path, notify::RecursiveMode::Recursive) {
        tracing::error!(
            project_id = %project_id,
            %err,
            "failed to start watching directory"
        );
        return;
    }

    let mut pending_changes: HashSet<PathBuf> = HashSet::new();
    let mut debounce_timer: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            // Check for stop signal.
            _ = stop_rx.changed() => {
                if *stop_rx.borrow() {
                    tracing::info!(project_id = %project_id, "file watcher stopped");
                    return;
                }
            }
            // Receive file events.
            Some(event) = notify_rx.recv() => {
                for path in &event.paths {
                    // Only track source files.
                    if let Some(ext) = path.extension().and_then(|e| e.to_str())
                        && super::lang::language_for_extension(ext).is_some()
                    {
                        pending_changes.insert(path.clone());
                    }
                }
                // Reset debounce timer.
                debounce_timer = Some(tokio::time::Instant::now() + Duration::from_millis(debounce_ms));
            }
            // Debounce timer fired.
            _ = async {
                if let Some(deadline) = debounce_timer {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    // No timer set; sleep forever (will be interrupted by select).
                    std::future::pending::<()>().await;
                }
            } => {
                if !pending_changes.is_empty() {
                    let absolute_paths: Vec<PathBuf> =
                        pending_changes.drain().collect();

                    let relative_strings: Vec<String> = absolute_paths
                        .iter()
                        .filter_map(|p| {
                            p.strip_prefix(&root_path)
                                .ok()
                                .map(|r| r.to_string_lossy().to_string())
                        })
                        .collect();

                    tracing::debug!(
                        project_id = %project_id,
                        files = absolute_paths.len(),
                        "file changes detected, dispatching incremental update"
                    );

                    // Fire stale event first (UI shows stale badge).
                    let _ = event_tx.send(CodeGraphEvent::GraphStale {
                        project_id: project_id.clone(),
                        stale_files: relative_strings,
                    });

                    // Dispatch the batch to the manager's incremental worker.
                    // A full receiver means the worker is overloaded; we fall
                    // back to blocking send so we don't drop changes silently.
                    if let Err(err) = change_tx.send(absolute_paths).await {
                        tracing::warn!(
                            project_id = %project_id,
                            %err,
                            "change receiver closed, dropping batch"
                        );
                    }
                }
                debounce_timer = None;
            }
        }
    }
}
