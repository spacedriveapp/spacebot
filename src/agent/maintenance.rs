//! Instance-wide memory maintenance for dormant-mode agents.
//!
//! When `CortexConfig.mode == Dormant`, the cortex's tick loop never spawns,
//! so the periodic memory consolidation / decay / pruning that normally runs
//! inside `spawn_cortex_loop` never fires. The janitor is the alternative
//! path: a single instance-wide cron task that walks every registered agent
//! on a slow schedule and runs the same `memory::maintenance` machinery.
//!
//! Active-mode agents are also walked when the janitor is enabled — the
//! maintenance functions are idempotent, so the additional pass costs a
//! small amount of redundant work but cannot corrupt state.
//!
//! Disabled by default (`MemoryJanitorConfig::enabled = false`); operators
//! opt in once they're running enough dormant agents that periodic
//! maintenance via tick is no longer happening.

use crate::AgentDeps;
use crate::AgentId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Spawn the janitor task. Returns the join handle so the caller can keep
/// it alive for the process lifetime.
///
/// `interval` is the gap between full sweeps. Default in `MemoryJanitorConfig`
/// is `86_400` (daily). The first sweep fires after `interval` to give the
/// instance time to settle on startup.
pub fn spawn_memory_janitor(
    registry: Arc<tokio::sync::RwLock<HashMap<AgentId, AgentDeps>>>,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_secs.max(60));
        tracing::info!(interval_secs = interval.as_secs(), "memory janitor started");
        loop {
            tokio::time::sleep(interval).await;
            let agents: Vec<AgentDeps> = {
                let guard = registry.read().await;
                guard.values().cloned().collect()
            };
            tracing::info!(
                agent_count = agents.len(),
                "memory janitor running maintenance pass"
            );
            for deps in agents {
                let agent_id = deps.agent_id.clone();
                if let Err(error) = run_maintenance_for_agent(&deps).await {
                    tracing::warn!(
                        %agent_id,
                        %error,
                        "janitor maintenance pass failed for agent"
                    );
                }
            }
        }
    })
}

async fn run_maintenance_for_agent(deps: &AgentDeps) -> anyhow::Result<()> {
    let cortex = deps.runtime_config.cortex.load();
    let config = crate::memory::maintenance::MaintenanceConfig {
        prune_threshold: cortex.maintenance_prune_threshold,
        decay_rate: cortex.maintenance_decay_rate,
        min_age_days: cortex.maintenance_min_age_days,
        merge_similarity_threshold: cortex.maintenance_merge_similarity_threshold,
    };
    let memory_search = &deps.memory_search;
    crate::memory::maintenance::run_maintenance(
        memory_search.store(),
        memory_search.embedding_table(),
        memory_search.embedding_model_arc(),
        &config,
    )
    .await
    .map_err(|error| anyhow::anyhow!("memory maintenance failed: {error}"))?;
    Ok(())
}
