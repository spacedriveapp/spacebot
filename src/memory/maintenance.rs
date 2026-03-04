//! Memory maintenance: decay, prune, merge, reindex.

use crate::error::Result;
use crate::memory::{Association, EmbeddingTable, Memory, MemoryStore, MemoryType, RelationType};
use anyhow::Context;

use sqlx::Row;

use std::collections::HashSet;

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
    embedding_table: &EmbeddingTable,
    config: &MaintenanceConfig,
) -> Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();

    // Apply decay to all non-identity memories
    // Fields are assigned sequentially because the values are async — can't use struct literal.
    #[allow(clippy::field_reassign_with_default)]
    {
        report.decayed = apply_decay(memory_store, config.decay_rate).await?;
        report.pruned = prune_memories(memory_store, config).await?;
        report.merged = merge_similar_memories(
            memory_store,
            embedding_table,
            config.merge_similarity_threshold,
        )
        .await?;
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
        let id: String = row.try_get("id")?;
        memory_store.delete(&id).await?;
        pruned_count += 1;
    }

    Ok(pruned_count)
}

/// Merge near-duplicate memories.
async fn merge_similar_memories(
    memory_store: &MemoryStore,
    embedding_table: &EmbeddingTable,
    similarity_threshold: f32,
) -> Result<usize> {
    let memory_ids = fetch_candidate_memory_ids(memory_store).await?;
    if memory_ids.is_empty() {
        return Ok(0);
    }

    let mut merged_count = 0_usize;
    let mut merged_memory_ids = HashSet::new();

    for source_id in memory_ids {
        if merged_memory_ids.contains(&source_id) {
            continue;
        }

        let Some(source_memory) = memory_store.load(&source_id).await? else {
            continue;
        };
        if source_memory.forgotten {
            continue;
        }
        let source_id = source_memory.id.clone();
        if merged_memory_ids.contains(&source_id) {
            continue;
        }

        let similar = match embedding_table
            .find_similar(&source_memory.id, similarity_threshold, 25)
            .await
        {
            Ok(similar) => similar,
            Err(error) => {
                tracing::debug!(
                    memory_id = source_memory.id,
                    %error,
                    "skipping memory maintenance merge due embedding lookup failure"
                );
                continue;
            }
        };

        if similar.is_empty() {
            continue;
        }

        let mut active_survivor = source_memory;
        let mut source_merged = false;

        for (candidate_id, _similarity) in similar {
            if merged_memory_ids.contains(&candidate_id) || candidate_id == active_survivor.id {
                continue;
            }

            let Some(candidate_memory) = memory_store.load(&candidate_id).await? else {
                continue;
            };
            if candidate_memory.forgotten {
                continue;
            }

            let (winner, loser) = choose_merge_pair(&active_survivor, &candidate_memory);
            merge_pair(memory_store, embedding_table, &winner, &loser).await?;
            merged_memory_ids.insert(loser.id.clone());
            merged_count += 1;

            if loser.id == source_id {
                source_merged = true;
                break;
            }

            if winner.id != active_survivor.id {
                active_survivor = winner;
            }
        }

        if source_merged {
            continue;
        }
    }

    Ok(merged_count)
}

fn choose_merge_pair(first: &Memory, second: &Memory) -> (Memory, Memory) {
    let first_wins = first.importance > second.importance
        || (first.importance == second.importance && first.id < second.id);

    if first_wins {
        (first.clone(), second.clone())
    } else {
        (second.clone(), first.clone())
    }
}

fn merged_memory_content(winner: String, loser: &str) -> String {
    let winner_trimmed = winner.trim_end();
    let loser_trimmed = loser.trim_end();

    if loser_trimmed.is_empty() {
        return winner_trimmed.to_string();
    }

    if winner_trimmed.contains(loser_trimmed) {
        return winner_trimmed.to_string();
    }

    if winner_trimmed.is_empty() {
        loser_trimmed.to_string()
    } else {
        format!("{winner_trimmed}\n\n{loser_trimmed}")
    }
}

async fn merge_pair(
    memory_store: &MemoryStore,
    embedding_table: &EmbeddingTable,
    survivor: &Memory,
    merged: &Memory,
) -> Result<()> {
    let mut updated_survivor = survivor.clone();
    updated_survivor.content = merged_memory_content(updated_survivor.content, &merged.content);
    updated_survivor.updated_at = chrono::Utc::now();

    memory_store.update(&updated_survivor).await?;

    let merged_associations = memory_store.get_associations(&merged.id).await?;
    if !merged_associations.is_empty() {
        memory_store
            .delete_associations_for_memory(&merged.id)
            .await?;

        for mut association in merged_associations {
            if association.source_id == updated_survivor.id {
                continue;
            }

            if association.source_id == merged.id {
                association.source_id = updated_survivor.id.clone();
            }
            if association.target_id == merged.id {
                association.target_id = updated_survivor.id.clone();
            }

            if association.source_id == association.target_id {
                continue;
            }

            memory_store.create_association(&association).await?;
        }
    }

    let updates_assoc =
        Association::new(&updated_survivor.id, &merged.id, RelationType::Updates).with_weight(1.0);
    memory_store.create_association(&updates_assoc).await?;

    memory_store.forget(&merged.id).await?;
    embedding_table.delete(&merged.id).await?;
    Ok(())
}

async fn fetch_candidate_memory_ids(memory_store: &MemoryStore) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT id FROM memories WHERE forgotten = 0 ORDER BY importance DESC, created_at DESC, id ASC",
    )
    .fetch_all(memory_store.pool())
    .await
    .with_context(|| "failed to fetch candidate memories for maintenance")?;

    let ids: Vec<String> = rows
        .into_iter()
        .map(|row| {
            let memory_id: String = row.get("id");
            memory_id
        })
        .collect();

    Ok(ids)
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
    use super::*;
    use tempfile::tempdir;

    async fn create_memory_with_embedding(
        store: &MemoryStore,
        embedding_table: &crate::memory::lance::EmbeddingTable,
        content: &str,
        memory_type: MemoryType,
        importance: f32,
        embedding: Vec<f32>,
    ) -> Memory {
        let memory = Memory::new(content, memory_type).with_importance(importance);
        store.save(&memory).await.expect("failed to save memory");

        embedding_table
            .store(&memory.id, &memory.content, &embedding)
            .await
            .expect("failed to store embedding");

        memory
    }

    #[tokio::test]
    async fn merges_near_duplicate_memories_and_transfers_associations() {
        let store = MemoryStore::connect_in_memory().await;

        let dir = tempdir().expect("failed to create temp dir");
        let lance_conn = lancedb::connect(dir.path().to_str().expect("temp path"))
            .execute()
            .await
            .expect("failed to connect to lancedb");
        let embedding_table = crate::memory::EmbeddingTable::open_or_create(&lance_conn)
            .await
            .expect("failed to create embedding table");

        let survivor = create_memory_with_embedding(
            &store,
            &embedding_table,
            "rust memory maintenance",
            MemoryType::Fact,
            0.9,
            vec![1.0; 384],
        )
        .await;

        let duplicate = create_memory_with_embedding(
            &store,
            &embedding_table,
            "rust memory maintenance updated",
            MemoryType::Fact,
            0.4,
            vec![1.0; 384],
        )
        .await;

        let related = create_memory_with_embedding(
            &store,
            &embedding_table,
            "related memory",
            MemoryType::Fact,
            0.7,
            vec![0.0; 384],
        )
        .await;

        store
            .create_association(&Association::new(
                &duplicate.id,
                &related.id,
                RelationType::RelatedTo,
            ))
            .await
            .expect("failed to create related-to association");

        store
            .create_association(&Association::new(
                &related.id,
                &duplicate.id,
                RelationType::PartOf,
            ))
            .await
            .expect("failed to create part-of association");

        let config = super::MaintenanceConfig {
            prune_threshold: 0.2,
            decay_rate: 0.05,
            min_age_days: 30,
            merge_similarity_threshold: 0.95,
        };

        let report = run_maintenance(&store, &embedding_table, &config)
            .await
            .expect("maintenance should succeed");

        assert_eq!(report.merged, 1);

        let updated_survivor = store
            .load(&survivor.id)
            .await
            .expect("failed to load survivor")
            .expect("survivor should exist");
        assert_eq!(updated_survivor.id, survivor.id);
        assert!(
            updated_survivor
                .content
                .contains("rust memory maintenance updated")
        );

        let forgotten_duplicate = store
            .load(&duplicate.id)
            .await
            .expect("failed to load duplicate")
            .expect("duplicate should still exist");
        assert!(forgotten_duplicate.forgotten);

        let duplicate_embeddings = embedding_table
            .find_similar(&duplicate.id, 0.0, 10)
            .await
            .expect("failed to search for missing duplicate embeddings");
        assert!(duplicate_embeddings.is_empty());

        let survivor_associations = store
            .get_associations(&survivor.id)
            .await
            .expect("failed to fetch survivor associations");

        let has_updates = survivor_associations.iter().any(|assoc| {
            assoc.source_id == survivor.id
                && assoc.target_id == duplicate.id
                && assoc.relation_type == RelationType::Updates
        });
        assert!(has_updates);

        assert!(
            survivor_associations
                .iter()
                .any(|assoc| { assoc.source_id == survivor.id && assoc.target_id == related.id })
        );

        assert!(
            survivor_associations
                .iter()
                .any(|assoc| assoc.source_id == related.id && assoc.target_id == survivor.id)
        );

        let duplicate_associations = store
            .get_associations(&duplicate.id)
            .await
            .expect("failed to load duplicate associations");
        assert_eq!(duplicate_associations.len(), 1);
        assert_eq!(
            duplicate_associations[0].source_id, survivor.id,
            "only expected survivor updates edge to duplicate after merge"
        );
        assert_eq!(duplicate_associations[0].target_id, duplicate.id);
        assert_eq!(
            duplicate_associations[0].relation_type,
            RelationType::Updates
        );
    }
}
