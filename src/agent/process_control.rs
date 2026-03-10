//! Supervision control plane for channel and detached worker cancellation.

use crate::agent::channel::WeakChannelControlHandle;
use crate::{AgentId, BranchId, ChannelId, WorkerId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlActionResult {
    Cancelled,
    NotFound,
    AlreadyTerminal,
}

pub const DETACHED_WORKER_LIFECYCLE_ACTIVE: u8 = 0;
pub const DETACHED_WORKER_LIFECYCLE_COMPLETING: u8 = 1;
pub const DETACHED_WORKER_LIFECYCLE_KILLING: u8 = 2;
pub const DETACHED_WORKER_LIFECYCLE_TERMINAL: u8 = 3;

#[derive(Debug, Clone)]
pub struct DetachedWorkerControlSnapshot {
    pub worker_id: WorkerId,
    pub agent_id: AgentId,
    pub task_number: i64,
    pub lifecycle: u8,
}

#[derive(Clone)]
struct ChannelControlEntry {
    handle: WeakChannelControlHandle,
    registration_id: u64,
}

enum ChannelLookupResult {
    Found(crate::agent::channel::ChannelControlHandle),
    Stale(u64),
    Missing,
}

pub struct DetachedWorkerControl {
    pub worker_id: WorkerId,
    pub agent_id: AgentId,
    pub task_number: i64,
    pub cancel_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub lifecycle: Arc<AtomicU8>,
}

impl DetachedWorkerControl {
    pub fn new(
        worker_id: WorkerId,
        agent_id: AgentId,
        task_number: i64,
        cancel_tx: tokio::sync::oneshot::Sender<()>,
        lifecycle: Arc<AtomicU8>,
    ) -> Self {
        Self {
            worker_id,
            agent_id,
            task_number,
            cancel_tx: Mutex::new(Some(cancel_tx)),
            lifecycle,
        }
    }
}

pub struct ProcessControlRegistry {
    channels: tokio::sync::RwLock<HashMap<ChannelId, ChannelControlEntry>>,
    detached_workers: tokio::sync::RwLock<HashMap<WorkerId, DetachedWorkerControl>>,
    next_channel_registration: AtomicU64,
}

impl Default for ProcessControlRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessControlRegistry {
    pub fn new() -> Self {
        Self {
            channels: tokio::sync::RwLock::new(HashMap::new()),
            detached_workers: tokio::sync::RwLock::new(HashMap::new()),
            next_channel_registration: AtomicU64::new(1),
        }
    }

    pub async fn register_channel(
        &self,
        channel_id: ChannelId,
        handle: WeakChannelControlHandle,
    ) -> u64 {
        let registration_id = self
            .next_channel_registration
            .fetch_add(1, Ordering::AcqRel);
        self.channels.write().await.insert(
            channel_id,
            ChannelControlEntry {
                handle,
                registration_id,
            },
        );
        registration_id
    }

    pub async fn unregister_channel(&self, channel_id: &ChannelId, registration_id: u64) -> bool {
        let mut channels = self.channels.write().await;
        let should_remove = channels
            .get(channel_id)
            .is_some_and(|entry| entry.registration_id == registration_id);
        if should_remove {
            channels.remove(channel_id);
        }
        should_remove
    }

    pub async fn prune_dead_channels(&self) -> usize {
        let mut channels = self.channels.write().await;
        let before = channels.len();
        channels.retain(|_, entry| entry.handle.upgrade().is_some());
        before.saturating_sub(channels.len())
    }

    pub async fn register_detached_worker(&self, control: DetachedWorkerControl) {
        self.detached_workers
            .write()
            .await
            .insert(control.worker_id, control);
    }

    pub async fn unregister_detached_worker(&self, worker_id: WorkerId) -> bool {
        self.detached_workers
            .write()
            .await
            .remove(&worker_id)
            .is_some()
    }

    pub async fn detached_worker_snapshots(&self) -> Vec<DetachedWorkerControlSnapshot> {
        let workers = self.detached_workers.read().await;
        workers
            .values()
            .map(|control| DetachedWorkerControlSnapshot {
                worker_id: control.worker_id,
                agent_id: control.agent_id.clone(),
                task_number: control.task_number,
                lifecycle: control.lifecycle.load(Ordering::Acquire),
            })
            .collect()
    }

    async fn lookup_channel_handle(&self, channel_id: &ChannelId) -> ChannelLookupResult {
        let handle_entry = {
            let channels = self.channels.read().await;
            let Some(handle_entry) = channels.get(channel_id).cloned() else {
                return ChannelLookupResult::Missing;
            };

            handle_entry
        };

        match handle_entry.handle.upgrade() {
            Some(handle) => ChannelLookupResult::Found(handle),
            None => ChannelLookupResult::Stale(handle_entry.registration_id),
        }
    }

    pub async fn cancel_channel_worker(
        &self,
        channel_id: &ChannelId,
        worker_id: WorkerId,
        reason: &str,
    ) -> ControlActionResult {
        for _ in 0..2 {
            match self.lookup_channel_handle(channel_id).await {
                ChannelLookupResult::Found(handle) => {
                    return handle.cancel_worker_with_reason(worker_id, reason).await;
                }
                ChannelLookupResult::Stale(registration_id) => {
                    self.remove_stale_channel_if_matches(channel_id, registration_id)
                        .await;
                }
                ChannelLookupResult::Missing => return ControlActionResult::NotFound,
            }
        }
        ControlActionResult::NotFound
    }

    pub async fn cancel_channel_branch(
        &self,
        channel_id: &ChannelId,
        branch_id: BranchId,
        reason: &str,
    ) -> ControlActionResult {
        for _ in 0..2 {
            match self.lookup_channel_handle(channel_id).await {
                ChannelLookupResult::Found(handle) => {
                    return handle.cancel_branch_with_reason(branch_id, reason).await;
                }
                ChannelLookupResult::Stale(registration_id) => {
                    self.remove_stale_channel_if_matches(channel_id, registration_id)
                        .await;
                }
                ChannelLookupResult::Missing => return ControlActionResult::NotFound,
            }
        }
        ControlActionResult::NotFound
    }

    async fn remove_stale_channel_if_matches(
        &self,
        channel_id: &ChannelId,
        expected_registration_id: u64,
    ) -> bool {
        let mut channels = self.channels.write().await;
        let should_remove = channels
            .get(channel_id)
            .is_some_and(|current| current.registration_id == expected_registration_id);

        if should_remove {
            channels.remove(channel_id);
        }

        should_remove
    }

    pub async fn cancel_detached_worker(
        &self,
        worker_id: WorkerId,
        reason: &str,
    ) -> ControlActionResult {
        let mut workers = self.detached_workers.write().await;
        let Some(control) = workers.get_mut(&worker_id) else {
            return ControlActionResult::NotFound;
        };

        if control
            .lifecycle
            .compare_exchange(
                DETACHED_WORKER_LIFECYCLE_ACTIVE,
                DETACHED_WORKER_LIFECYCLE_KILLING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return ControlActionResult::AlreadyTerminal;
        }

        let mut cancel_tx_guard = control.cancel_tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel_tx) = cancel_tx_guard.take()
            && cancel_tx.send(()).is_err()
        {
            tracing::debug!(
                worker_id = %worker_id,
                "detached worker cancel signal receiver already dropped"
            );
        }

        tracing::info!(
            worker_id = %worker_id,
            task_number = control.task_number,
            reason,
            "supervisor sent detached worker cancel signal"
        );

        ControlActionResult::Cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ControlActionResult, DETACHED_WORKER_LIFECYCLE_ACTIVE, DetachedWorkerControl,
        DetachedWorkerControlSnapshot, ProcessControlRegistry,
    };
    use crate::agent::channel::WeakChannelControlHandle;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};

    #[tokio::test]
    async fn register_and_unregister_detached_worker() {
        let registry = ProcessControlRegistry::new();
        let worker_id = uuid::Uuid::new_v4();
        let lifecycle = Arc::new(AtomicU8::new(DETACHED_WORKER_LIFECYCLE_ACTIVE));
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();

        registry
            .register_detached_worker(DetachedWorkerControl::new(
                worker_id,
                Arc::from("agent"),
                7,
                cancel_tx,
                lifecycle,
            ))
            .await;

        assert!(registry.unregister_detached_worker(worker_id).await);
        assert!(!registry.unregister_detached_worker(worker_id).await);
    }

    #[tokio::test]
    async fn prune_dead_channels_removes_stale_entries() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("channel-1");
        let registration_id = registry
            .register_channel(
                channel_id.clone(),
                crate::agent::channel::WeakChannelControlHandle::dangling(),
            )
            .await;

        let pruned = registry.prune_dead_channels().await;

        assert_eq!(pruned, 1);
        assert!(
            !registry
                .unregister_channel(&channel_id, registration_id)
                .await
        );
    }

    #[tokio::test]
    async fn stale_channel_entry_cleanup_only_removes_matching_registration_id() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("channel-stale-race");
        let stale_handle = WeakChannelControlHandle::dangling();

        let stale_registration_id = registry
            .register_channel(channel_id.clone(), stale_handle)
            .await;

        let active_registration_id = registry
            .register_channel(channel_id.clone(), WeakChannelControlHandle::dangling())
            .await;

        assert!(
            !registry
                .remove_stale_channel_if_matches(&channel_id, stale_registration_id)
                .await
        );
        assert!(
            !registry
                .unregister_channel(&channel_id, stale_registration_id)
                .await
        );
        assert!(
            registry
                .unregister_channel(&channel_id, active_registration_id)
                .await
        );
    }

    #[tokio::test]
    async fn cancel_missing_entries_is_idempotent_not_found() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("missing-channel");
        let worker_id = uuid::Uuid::new_v4();
        let branch_id = uuid::Uuid::new_v4();

        assert_eq!(
            registry
                .cancel_channel_worker(&channel_id, worker_id, "test")
                .await,
            ControlActionResult::NotFound
        );
        assert_eq!(
            registry
                .cancel_channel_branch(&channel_id, branch_id, "test")
                .await,
            ControlActionResult::NotFound
        );
        assert_eq!(
            registry.cancel_detached_worker(worker_id, "test").await,
            ControlActionResult::NotFound
        );
    }

    #[tokio::test]
    async fn cancel_stale_channel_entry_prunes_then_returns_not_found() {
        let registry = ProcessControlRegistry::new();
        let channel_id: crate::ChannelId = Arc::from("stale-channel");
        let worker_id = uuid::Uuid::new_v4();

        let registration_id = registry
            .register_channel(channel_id.clone(), WeakChannelControlHandle::dangling())
            .await;

        assert_eq!(
            registry
                .cancel_channel_worker(&channel_id, worker_id, "test")
                .await,
            ControlActionResult::NotFound
        );
        assert!(
            !registry
                .unregister_channel(&channel_id, registration_id)
                .await,
            "stale entry should be pruned during cancellation retry path"
        );
    }

    #[tokio::test]
    async fn cancel_detached_worker_is_single_winner_and_idempotent() {
        let registry = ProcessControlRegistry::new();
        let worker_id = uuid::Uuid::new_v4();
        let lifecycle = Arc::new(AtomicU8::new(DETACHED_WORKER_LIFECYCLE_ACTIVE));
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        registry
            .register_detached_worker(DetachedWorkerControl::new(
                worker_id,
                Arc::from("agent"),
                42,
                cancel_tx,
                lifecycle.clone(),
            ))
            .await;

        let first = registry.cancel_detached_worker(worker_id, "timeout").await;
        assert_eq!(first, ControlActionResult::Cancelled);
        assert!(cancel_rx.try_recv().is_ok());

        let second = registry.cancel_detached_worker(worker_id, "timeout").await;
        assert_eq!(second, ControlActionResult::AlreadyTerminal);

        assert_eq!(
            lifecycle.load(Ordering::Acquire),
            super::DETACHED_WORKER_LIFECYCLE_KILLING
        );
    }

    #[tokio::test]
    async fn detached_worker_snapshots_capture_state() {
        let registry = ProcessControlRegistry::new();
        let worker_id = uuid::Uuid::new_v4();
        let lifecycle = Arc::new(AtomicU8::new(DETACHED_WORKER_LIFECYCLE_ACTIVE));
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();

        registry
            .register_detached_worker(DetachedWorkerControl::new(
                worker_id,
                Arc::from("agent"),
                99,
                cancel_tx,
                lifecycle,
            ))
            .await;

        let snapshots = registry.detached_worker_snapshots().await;
        assert_eq!(snapshots.len(), 1);

        let snapshot: DetachedWorkerControlSnapshot = snapshots[0].clone();
        assert_eq!(snapshot.worker_id, worker_id);
        assert_eq!(snapshot.agent_id.as_ref(), "agent");
        assert_eq!(snapshot.task_number, 99);
        assert_eq!(snapshot.lifecycle, DETACHED_WORKER_LIFECYCLE_ACTIVE);
    }
}
