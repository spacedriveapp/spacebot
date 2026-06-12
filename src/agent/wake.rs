//! Wake infrastructure for dormant-mode agents.
//!
//! When `CortexConfig.mode == Dormant`, the cortex's periodic loops don't
//! spawn — the agent stays idle until an external event delivers a wake.
//! Wake triggers send the receiving agent's ID over an unbounded mpsc;
//! `WakeManager` consumes from that channel and dispatches to the agent's
//! `cortex::wake()` entry point.
//!
//! Active-mode agents are also valid wake targets — wake is a no-op (idempotent)
//! when the cortex's normal loops are already covering pickup. Triggers fire
//! the same wake call regardless of mode; the receiving agent decides whether
//! the wake is meaningful.
//!
//! This module owns no business logic — it's purely the dispatch substrate.
//! The "do work" happens in `cortex::wake_one`, which `WakeManager` calls.

use crate::AgentDeps;
use crate::AgentId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Sender side of the wake channel. Cloned into every `AgentDeps` and into
/// any tool / API endpoint that needs to deliver a wake.
pub type WakeSender = mpsc::UnboundedSender<AgentId>;

/// Spawn the wake-dispatch loop. Returns a `WakeSender` that can be cloned
/// into `AgentDeps` and other contexts.
///
/// `registry` is a shared map of agent ID → deps. The dispatcher reads it
/// each time a wake arrives, so registering / unregistering agents at
/// runtime is safe (cortex chat session pushes / removes can happen
/// without restarting the dispatcher).
pub fn spawn_wake_manager(
    registry: Arc<tokio::sync::RwLock<HashMap<AgentId, AgentDeps>>>,
) -> WakeSender {
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentId>();
    tokio::spawn(async move {
        tracing::info!("wake manager started");
        while let Some(agent_id) = rx.recv().await {
            let deps = {
                let guard = registry.read().await;
                guard.get(&agent_id).cloned()
            };
            let Some(deps) = deps else {
                tracing::debug!(%agent_id, "wake delivered for unknown agent — dropping");
                continue;
            };
            tokio::spawn(async move {
                if let Err(error) = crate::agent::cortex::wake_one(&deps).await {
                    tracing::warn!(
                        agent_id = %deps.agent_id,
                        %error,
                        "cortex wake failed",
                    );
                }
            });
        }
        tracing::info!("wake manager exiting (channel closed)");
    });
    tx
}

/// Best-effort wake delivery. Logs and drops when the receiver is gone
/// (the wake manager task panicked or was never spawned).
pub fn fire_wake(sender: &WakeSender, agent_id: &AgentId) {
    if let Err(error) = sender.send(agent_id.clone()) {
        tracing::warn!(
            agent_id = %agent_id,
            %error,
            "wake send failed — wake manager not running"
        );
    }
}
