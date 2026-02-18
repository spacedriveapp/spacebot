//! Memory maintenance: decay, prune, merge, reindex.

use crate::error::Result;
use crate::memory::MemoryStore;
use crate::memory::types::{Memory, MemoryType, RelationType};
use std::sync::Arc;

/// Maintenance configuration.
#[derive(Debug, Clone)]
pub struct MaintenanceConfig {
    /// Importance below which memories are considered for pruning.
    pub prune_threshold: f32,
    /// Decay rate per day (0.0 - 1.0).
    pub decay_rate: f32,
    /// Minimum age in days before a memory can be pruned.
    pub min_age_days: i64,
    /// Similarity threshold for merging memories (0.0 - 1.0).
    pub merge_similarity_threshold: f32,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            prune_threshold: 0.1,
            decay_rate: 0.05,
            min_age_days: 30,
            merge_similarity_threshold: 0.95,
        }
    }
}

/// Run maintenance tasks on the memory store.
pub async fn run_maintenance(
    memory_store: &MemoryStore,
    config: &MaintenanceConfig,
) -> Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();

    // Apply decay to all non-identity memories
    report.decayed = apply_decay(memory_store, config.decay_rate).await?;

    // Prune old, low-importance memories
    report.pruned = prune_memories(memory_store, config).await?;

    // Merge near-duplicate memories
    report.merged = merge_similar_memories(memory_store, config.merge_similarity_threshold).await?;

    Ok(report)
}

/// Apply importance decay based on recency and access patterns.
async fn apply_decay(memory_store: &MemoryStore, decay_rate: f32) -> Result<usize> {
    // Get all non-identity memories
    let all_types: Vec<_> = MemoryType::ALL
        .iter()
        .copied()
        .filter(|t| *t != MemoryType::Identity)
        .collect();

    let mut decayed_count = 0;

    for mem_type in all_types {
        let memories = memory_store.get_by_type(mem_type, 1000).await?;

        for mut memory in memories {
            let now = chrono::Utc::now();
            let days_old = (now - memory.updated_at).num_days();
            let days_since_access = (now - memory.last_accessed_at).num_days();

            // Calculate decay multiplier
            let age_decay = 1.0 - (days_old as f32 * decay_rate).min(0.5);
            let access_boost = if days_since_access < 7 {
                1.1 // Recent access boosts importance
            } else if days_since_access > 30 {
                0.9 // Long time since access reduces importance
            } else {
                1.0
            };

            let new_importance = memory.importance * age_decay * access_boost;

            if (new_importance - memory.importance).abs() > 0.01 {
                memory.importance = new_importance.clamp(0.0, 1.0);
                memory.updated_at = now;
                memory_store.update(&memory).await?;
                decayed_count += 1;
            }
        }
    }

    Ok(decayed_count)
}

/// Prune memories that have fallen below the importance threshold.
async fn prune_memories(memory_store: &MemoryStore, config: &MaintenanceConfig) -> Result<usize> {
    let now = chrono::Utc::now();
    let min_age = chrono::Duration::days(config.min_age_days);
    let cutoff_date = now - min_age;

    // Get all memories below threshold that are old enough
    let candidates = sqlx::query(
        r#"
        SELECT id FROM memories
        WHERE importance < ? 
        AND memory_type != 'identity'
        AND created_at < ?
        "#,
    )
    .bind(config.prune_threshold)
    .bind(cutoff_date)
    .fetch_all(memory_store.pool())
    .await?;

    let mut pruned_count = 0;

    for row in candidates {
        let id: String = sqlx::Row::try_get(&row, "id")?;
        memory_store.delete(&id).await?;
        pruned_count += 1;
    }

    Ok(pruned_count)
}

/// Merge near-duplicate memories.
async fn merge_similar_memories(
    memory_store: &MemoryStore,
    similarity_threshold: f32,
) -> Result<usize> {
    // For now, this is a placeholder
    // Full implementation would:
    // 1. Find pairs of memories with high embedding similarity
    // 2. Merge them, keeping the higher importance one
    // 3. Update associations to point to the merged memory
    let _ = similarity_threshold;
    Ok(0)
}

/// Maintenance report.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub decayed: usize,
    pub pruned: usize,
    pub merged: usize,
}
