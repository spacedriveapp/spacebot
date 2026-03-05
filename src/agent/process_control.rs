//! Supervision control plane for channel and detached worker cancellation.

use crate::agent::channel::WeakChannelControlHandle;
use crate::{AgentId, BranchId, ChannelId, WorkerId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
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
    channels: tokio::sync::RwLock<HashMap<ChannelId, WeakChannelControlHandle>>,
    detached_workers: tokio::sync::RwLock<HashMap<WorkerId, DetachedWorkerControl>>,
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
        }
    }

    pub async fn register_channel(&self, channel_id: ChannelId, handle: WeakChannelControlHandle) {
        self.channels.write().await.insert(channel_id, handle);
    }

    pub async fn unregister_channel(&self, channel_id: &ChannelId) -> bool {
        self.channels.write().await.remove(channel_id).is_some()
    }

    pub async fn prune_dead_channels(&self) -> usize {
        let mut channels = self.channels.write().await;
        let before = channels.len();
        channels.retain(|_, handle| handle.upgrade().is_some());
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

    async fn lookup_channel_handle(
        &self,
        channel_id: &ChannelId,
    ) -> std::result::Result<crate::agent::channel::ChannelControlHandle, ControlActionResult> {
        let mut channels = self.channels.write().await;
        let Some(weak_handle) = channels.get(channel_id).cloned() else {
            return Err(ControlActionResult::NotFound);
        };

        let Some(handle) = weak_handle.upgrade() else {
            channels.remove(channel_id);
            return Err(ControlActionResult::NotFound);
        };

        Ok(handle)
    }

    pub async fn cancel_channel_worker(
        &self,
        channel_id: &ChannelId,
        worker_id: WorkerId,
        reason: &str,
    ) -> ControlActionResult {
        let handle = match self.lookup_channel_handle(channel_id).await {
            Ok(handle) => handle,
            Err(result) => return result,
        };

        handle.cancel_worker_with_reason(worker_id, reason).await
    }

    pub async fn cancel_channel_branch(
        &self,
        channel_id: &ChannelId,
        branch_id: BranchId,
        reason: &str,
    ) -> ControlActionResult {
        let handle = match self.lookup_channel_handle(channel_id).await {
            Ok(handle) => handle,
            Err(result) => return result,
        };

        handle.cancel_branch_with_reason(branch_id, reason).await
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
        ProcessControlRegistry,
    };
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
        registry
            .register_channel(
                channel_id.clone(),
                crate::agent::channel::WeakChannelControlHandle::dangling(),
            )
            .await;

        let pruned = registry.prune_dead_channels().await;

        assert_eq!(pruned, 1);
        assert!(!registry.unregister_channel(&channel_id).await);
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
}
