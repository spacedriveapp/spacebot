//! Memory maintenance: decay, prune, merge, reindex.

use std::collections::HashMap;

use crate::error::Result;
use crate::memory::MemoryStore;
use crate::memory::types::MemoryType;

/// Maintenance configuration.
#[derive(Debug, Clone)]
pub struct MaintenanceConfig {
    /// Importance below which memories are considered for pruning.
    pub prune_threshold: f32,
    /// Fallback decay rate per day (0.0 - 1.0) when no category-specific rate applies.
    pub decay_rate: f32,
    /// Per-category decay rates. Keys match `source` learning category tags or lowercase
    /// MemoryType names. A rate of 0.0 means the category never decays.
    pub category_decay_rates: HashMap<String, f32>,
    /// Minimum age in days before a memory can be pruned.
    pub min_age_days: i64,
    /// Similarity threshold for merging memories (0.0 - 1.0).
    pub merge_similarity_threshold: f32,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        let mut rates = HashMap::new();
        // Foundational / identity-adjacent categories — never decay.
        rates.insert("identity".into(), 0.0);
        rates.insert("preference".into(), 0.0);
        rates.insert("wisdom".into(), 0.0);
        rates.insert("user_model".into(), 0.0);
        rates.insert("relationship".into(), 0.0);
        // Slow decay (~quarterly half-life).
        rates.insert("self_awareness".into(), 0.0077);
        rates.insert("domain_expertise".into(), 0.0077);
        // Moderate decay (~bimonthly half-life).
        rates.insert("communication".into(), 0.0116);
        rates.insert("reasoning".into(), 0.0116);
        // Faster decay (~monthly half-life).
        rates.insert("context".into(), 0.0154);
        // Episodic memories fade quickest.
        rates.insert("event".into(), 0.0231);

        Self {
            prune_threshold: 0.1,
            decay_rate: 0.05,
            category_decay_rates: rates,
            min_age_days: 30,
            merge_similarity_threshold: 0.95,
        }
    }
}

/// Resolve the effective decay rate for a single memory.
///
/// Resolution order:
/// 1. If `source` contains a learning category tag (`learning:<category>`), look that category up
///    in `config.category_decay_rates`.
/// 2. Fall back to a hard-coded mapping from `MemoryType`.
/// 3. Fall back to `config.decay_rate`.
pub fn resolve_decay_rate(
    config: &MaintenanceConfig,
    memory_type: MemoryType,
    source: Option<&str>,
) -> f32 {
    // 1. Learning-category tag in source field.
    if let Some(source) = source
        && let Some(category) = source
            .split(':')
            .nth(1)
            .filter(|_| source.starts_with("learning:"))
        && let Some(&rate) = config.category_decay_rates.get(category)
    {
        return rate;
    }

    // 2. MemoryType mapping.
    let type_key = match memory_type {
        MemoryType::Identity => return 0.0,
        MemoryType::Preference => return 0.0,
        MemoryType::Event => "event",
        MemoryType::Fact => "context",
        MemoryType::Decision | MemoryType::Observation | MemoryType::Goal | MemoryType::Todo => {
            // No specific category override — fall through to global default.
            return config.decay_rate;
        }
    };

    config
        .category_decay_rates
        .get(type_key)
        .copied()
        .unwrap_or(config.decay_rate)
}

/// Run maintenance tasks on the memory store.
pub async fn run_maintenance(
    memory_store: &MemoryStore,
    config: &MaintenanceConfig,
) -> Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();

    // Fields are assigned sequentially because the values are async — can't use struct literal.
    #[allow(clippy::field_reassign_with_default)]
    {
        report.decayed = apply_decay(memory_store, config).await?;
        report.pruned = prune_memories(memory_store, config).await?;
        report.merged =
            merge_similar_memories(memory_store, config.merge_similarity_threshold).await?;
    }

    Ok(report)
}

/// Apply importance decay to all non-identity memories using category-specific rates.
async fn apply_decay(memory_store: &MemoryStore, config: &MaintenanceConfig) -> Result<usize> {
    // Collect all types that are not unconditionally exempt.
    let all_types: Vec<_> = MemoryType::ALL
        .iter()
        .copied()
        .filter(|t| *t != MemoryType::Identity)
        .collect();

    let mut decayed_count = 0;

    for mem_type in all_types {
        let memories = memory_store.get_by_type(mem_type, 1000).await?;

        for mut memory in memories {
            let decay_rate =
                resolve_decay_rate(config, memory.memory_type, memory.source.as_deref());

            // A rate of 0.0 means this category is permanently exempt.
            if decay_rate == 0.0 {
                continue;
            }

            let now = chrono::Utc::now();
            let days_old = (now - memory.updated_at).num_days();
            let days_since_access = (now - memory.last_accessed_at).num_days();

            // Calculate decay multiplier using the resolved per-category rate.
            let age_decay = 1.0 - (days_old as f32 * decay_rate).min(0.5);
            let access_boost = if days_since_access < 7 {
                1.1 // Recent access boosts importance.
            } else if days_since_access > 30 {
                0.9 // Long gap since access reduces importance.
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

    // Get all memories below threshold that are old enough.
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
    _memory_store: &MemoryStore,
    similarity_threshold: f32,
) -> Result<usize> {
    // For now, this is a placeholder.
    // Full implementation would:
    // 1. Find pairs of memories with high embedding similarity.
    // 2. Merge them, keeping the higher importance one.
    // 3. Update associations to point to the merged memory.
    let _ = similarity_threshold;
    Ok(0)
}

/// Statistics from a learning-memory maintenance pass.
#[derive(Debug, Default)]
pub struct LearningMaintenanceReport {
    /// Number of distillation records pruned during the pass.
    pub distillations_pruned: usize,
    /// Number of raw evidence rows cleaned up.
    pub evidence_cleaned: usize,
}

/// Unified maintenance report returned by [`run_maintenance`].
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub decayed: usize,
    pub pruned: usize,
    pub merged: usize,
    /// Present when a learning-memory maintenance pass was also performed.
    pub learning: Option<LearningMaintenanceReport>,
}
