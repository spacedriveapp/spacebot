//! Memory maintenance: decay, prune, merge, reindex.

use crate::error::Result;
use crate::memory::MemoryStore;
use crate::memory::types::{Association, Memory, MemoryType};

use anyhow::anyhow;
use sqlx::Row as _;

use std::collections::{HashMap, HashSet};

type MergeGroupKey = (MemoryType, Option<String>);
type MergeCandidate = (Memory, String);
type MergeGroups = HashMap<MergeGroupKey, Vec<MergeCandidate>>;

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
    // Fields are assigned sequentially because the values are async — can't use struct literal.
    #[allow(clippy::field_reassign_with_default)]
    {
        report.decayed = apply_decay(memory_store, config.decay_rate).await?;
        report.pruned = prune_memories(memory_store, config).await?;
        report.merged =
            merge_similar_memories(memory_store, config.merge_similarity_threshold).await?;
    }

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
    if !(0.0..=1.0).contains(&similarity_threshold) {
        return Err(anyhow!(
            "merge_similarity_threshold must be within [0.0, 1.0], got {similarity_threshold}"
        )
        .into());
    }

    let rows = sqlx::query(
        r#"
        SELECT id, content, memory_type, importance, created_at, updated_at,
               last_accessed_at, access_count, source, channel_id, forgotten
        FROM memories
        WHERE forgotten = 0
        ORDER BY updated_at DESC, importance DESC
        "#,
    )
    .fetch_all(memory_store.pool())
    .await?;

    let memories = rows
        .into_iter()
        .map(row_to_memory)
        .collect::<Result<Vec<_>>>()?;
    let mut groups: MergeGroups = HashMap::new();

    for memory in memories {
        let normalized = normalize_memory_content(&memory.content);
        if normalized.is_empty() {
            continue;
        }

        let channel_key = memory
            .channel_id
            .as_ref()
            .map(|channel_id| channel_id.to_string());
        groups
            .entry((memory.memory_type, channel_key))
            .or_default()
            .push((memory, normalized));
    }

    let mut merged_count = 0;
    for candidates in groups.into_values() {
        let mut remaining = candidates;
        remaining.sort_by(|left, right| canonical_memory_cmp(&left.0, &right.0).reverse());

        while let Some((seed_memory, seed_normalized)) =
            (!remaining.is_empty()).then(|| remaining.remove(0))
        {
            let mut duplicates = vec![seed_memory];
            let mut cluster_queue = std::collections::VecDeque::from([seed_normalized]);
            let mut visited_normalized = HashSet::new();

            while let Some(current_normalized) = cluster_queue.pop_front() {
                if !visited_normalized.insert(current_normalized.clone()) {
                    continue;
                }

                let mut next_remaining = Vec::new();
                for (memory, normalized) in remaining {
                    if normalized_similarity(&current_normalized, &normalized)
                        >= similarity_threshold
                    {
                        cluster_queue.push_back(normalized.clone());
                        duplicates.push(memory);
                    } else {
                        next_remaining.push((memory, normalized));
                    }
                }
                remaining = next_remaining;
            }

            if duplicates.len() < 2 {
                continue;
            }

            let canonical = select_canonical_memory(&duplicates);
            let mut seen_duplicates = HashSet::new();

            for duplicate in duplicates {
                if duplicate.id == canonical.id || !seen_duplicates.insert(duplicate.id.clone()) {
                    continue;
                }

                rewrite_associations(memory_store, &canonical, &duplicate).await?;
                memory_store.delete(&duplicate.id).await?;
                merged_count += 1;
            }
        }
    }

    Ok(merged_count)
}

fn normalize_memory_content(content: &str) -> String {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_lowercase()
}

fn select_canonical_memory(memories: &[Memory]) -> Memory {
    memories
        .iter()
        .max_by(|left, right| canonical_memory_cmp(left, right))
        .expect("duplicate group must be non-empty")
        .clone()
}

fn canonical_memory_cmp(left: &Memory, right: &Memory) -> std::cmp::Ordering {
    left.importance
        .total_cmp(&right.importance)
        .then_with(|| left.access_count.cmp(&right.access_count))
        .then_with(|| left.updated_at.cmp(&right.updated_at))
        .then_with(|| left.created_at.cmp(&right.created_at))
}

fn normalized_similarity(left: &str, right: &str) -> f32 {
    if left == right {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let left_bigrams = bigram_counts(left);
    let right_bigrams = bigram_counts(right);
    let left_total = left_bigrams.values().sum::<usize>();
    let right_total = right_bigrams.values().sum::<usize>();
    if left_total == 0 || right_total == 0 {
        return 0.0;
    }

    let intersection = left_bigrams
        .iter()
        .map(|(bigram, count)| {
            right_bigrams
                .get(bigram)
                .map_or(0_usize, |other| (*count).min(*other))
        })
        .sum::<usize>();

    (2.0 * intersection as f32) / (left_total + right_total) as f32
}

fn bigram_counts(value: &str) -> HashMap<(char, char), usize> {
    let chars = value.chars().collect::<Vec<_>>();
    let mut counts = HashMap::new();
    for bigram in chars.windows(2) {
        let key = (bigram[0], bigram[1]);
        *counts.entry(key).or_default() += 1;
    }
    counts
}

async fn rewrite_associations(
    memory_store: &MemoryStore,
    canonical: &Memory,
    duplicate: &Memory,
) -> Result<()> {
    let associations = memory_store.get_associations(&duplicate.id).await?;
    for association in associations {
        let source_id = if association.source_id == duplicate.id {
            canonical.id.clone()
        } else {
            association.source_id.clone()
        };
        let target_id = if association.target_id == duplicate.id {
            canonical.id.clone()
        } else {
            association.target_id.clone()
        };

        if source_id == target_id {
            continue;
        }

        let combined_weight = existing_association_weight(
            memory_store,
            &source_id,
            &target_id,
            association.relation_type,
        )
        .await?
        .map_or(association.weight, |existing_weight| {
            existing_weight.max(association.weight)
        });

        let rewritten = Association::new(source_id, target_id, association.relation_type)
            .with_weight(combined_weight);
        memory_store.create_association(&rewritten).await?;
    }

    Ok(())
}

async fn existing_association_weight(
    memory_store: &MemoryStore,
    source_id: &str,
    target_id: &str,
    relation_type: crate::memory::RelationType,
) -> Result<Option<f32>> {
    let row = sqlx::query(
        r#"
        SELECT weight
        FROM associations
        WHERE source_id = ? AND target_id = ? AND relation_type = ?
        "#,
    )
    .bind(source_id)
    .bind(target_id)
    .bind(relation_type.to_string())
    .fetch_optional(memory_store.pool())
    .await?;

    Ok(row.map(|row| row.get("weight")))
}

fn row_to_memory(row: sqlx::sqlite::SqliteRow) -> Result<Memory> {
    let id: String = row.get("id");
    let memory_type_raw: String = row.get("memory_type");

    Ok(Memory {
        id: id.clone(),
        content: row.get("content"),
        memory_type: parse_memory_type(&memory_type_raw)
            .map_err(|error| anyhow!("failed to parse memory_type for memory {id}: {error}"))?,
        importance: row.get("importance"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        last_accessed_at: row.get("last_accessed_at"),
        access_count: row.get("access_count"),
        source: row.get("source"),
        channel_id: row.get::<Option<String>, _>("channel_id").map(Into::into),
        forgotten: row.get("forgotten"),
    })
}

fn parse_memory_type(memory_type: &str) -> Result<MemoryType> {
    match memory_type {
        "fact" => Ok(MemoryType::Fact),
        "preference" => Ok(MemoryType::Preference),
        "decision" => Ok(MemoryType::Decision),
        "identity" => Ok(MemoryType::Identity),
        "event" => Ok(MemoryType::Event),
        "observation" => Ok(MemoryType::Observation),
        "goal" => Ok(MemoryType::Goal),
        "todo" => Ok(MemoryType::Todo),
        _ => Err(anyhow!("unknown memory type '{memory_type}'").into()),
    }
}

/// Maintenance report.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub decayed: usize,
    pub pruned: usize,
    pub merged: usize,
}

#[cfg(test)]
mod tests {
    use super::{MaintenanceConfig, merge_similar_memories};
    use crate::memory::{Association, Memory, MemoryStore, MemoryType, RelationType};
    use std::sync::Arc;

    #[tokio::test]
    async fn merges_normalized_duplicate_memories_and_rewrites_edges() {
        let store = MemoryStore::connect_in_memory().await;

        let canonical = Memory::new("Deploy the worker", MemoryType::Todo).with_importance(0.9);
        let duplicate =
            Memory::new("  deploy   the worker ", MemoryType::Todo).with_importance(0.4);
        let neighbor = Memory::new("CI checks are green", MemoryType::Observation);

        store.save(&canonical).await.expect("save canonical");
        store.save(&duplicate).await.expect("save duplicate");
        store.save(&neighbor).await.expect("save neighbor");

        store
            .create_association(&Association::new(
                &neighbor.id,
                &duplicate.id,
                RelationType::RelatedTo,
            ))
            .await
            .expect("create association");

        let merged = merge_similar_memories(
            &store,
            MaintenanceConfig::default().merge_similarity_threshold,
        )
        .await
        .expect("merge");

        assert_eq!(merged, 1);
        assert!(store.load(&duplicate.id).await.expect("load").is_none());

        let associations = store
            .get_associations(&canonical.id)
            .await
            .expect("get associations");
        assert!(associations.iter().any(|association| {
            association.source_id == neighbor.id
                && association.target_id == canonical.id
                && association.relation_type == RelationType::RelatedTo
        }));
    }

    #[tokio::test]
    async fn skips_merge_when_threshold_exceeds_exact_match() {
        let store = MemoryStore::connect_in_memory().await;
        let first = Memory::new("deploy worker now", MemoryType::Fact);
        let second = Memory::new("deploy workers tomorrow", MemoryType::Fact);
        store.save(&first).await.expect("save first");
        store.save(&second).await.expect("save second");

        let merged = merge_similar_memories(&store, 0.95).await.expect("merge");
        assert_eq!(merged, 0);
        assert!(store.load(&second.id).await.expect("load").is_some());
    }

    #[tokio::test]
    async fn invalid_threshold_returns_error() {
        let store = MemoryStore::connect_in_memory().await;
        let error = merge_similar_memories(&store, 1.1)
            .await
            .expect_err("invalid threshold");
        assert!(
            error
                .to_string()
                .contains("merge_similarity_threshold must be within [0.0, 1.0]"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn does_not_merge_identical_content_across_channels() {
        let store = MemoryStore::connect_in_memory().await;
        let first =
            Memory::new("same content", MemoryType::Fact).with_channel_id(Arc::<str>::from("a"));
        let second =
            Memory::new("same content", MemoryType::Fact).with_channel_id(Arc::<str>::from("b"));
        store.save(&first).await.expect("save first");
        store.save(&second).await.expect("save second");

        let merged = merge_similar_memories(&store, 0.95).await.expect("merge");
        assert_eq!(merged, 0);
    }

    #[tokio::test]
    async fn preserves_stronger_existing_association_weight() {
        let store = MemoryStore::connect_in_memory().await;

        let canonical = Memory::new("shared task", MemoryType::Todo).with_importance(0.9);
        let duplicate = Memory::new("shared  task", MemoryType::Todo).with_importance(0.5);
        let neighbor = Memory::new("neighbor", MemoryType::Observation);

        store.save(&canonical).await.expect("save canonical");
        store.save(&duplicate).await.expect("save duplicate");
        store.save(&neighbor).await.expect("save neighbor");

        store
            .create_association(
                &Association::new(&neighbor.id, &canonical.id, RelationType::RelatedTo)
                    .with_weight(0.9),
            )
            .await
            .expect("canonical association");
        store
            .create_association(
                &Association::new(&neighbor.id, &duplicate.id, RelationType::RelatedTo)
                    .with_weight(0.3),
            )
            .await
            .expect("duplicate association");

        let merged = merge_similar_memories(&store, 0.95).await.expect("merge");
        assert_eq!(merged, 1);

        let associations = store
            .get_associations(&canonical.id)
            .await
            .expect("associations");
        let preserved = associations
            .iter()
            .find(|association| {
                association.source_id == neighbor.id
                    && association.target_id == canonical.id
                    && association.relation_type == RelationType::RelatedTo
            })
            .expect("rewritten association");
        assert_eq!(preserved.weight, 0.9);
    }

    #[tokio::test]
    async fn merges_transitive_duplicate_cluster() {
        let store = MemoryStore::connect_in_memory().await;

        let first = Memory::new("deploy worker now", MemoryType::Todo).with_importance(0.9);
        let second = Memory::new("deploy worker right now", MemoryType::Todo).with_importance(0.7);
        let third = Memory::new("deploy right now", MemoryType::Todo).with_importance(0.6);

        store.save(&first).await.expect("save first");
        store.save(&second).await.expect("save second");
        store.save(&third).await.expect("save third");

        let merged = merge_similar_memories(&store, 0.55).await.expect("merge");
        assert_eq!(merged, 2);
        assert!(store.load(&second.id).await.expect("load second").is_none());
        assert!(store.load(&third.id).await.expect("load third").is_none());
    }
}
