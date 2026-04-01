//! Memory maintenance: decay, prune, merge, reindex.

use crate::error::Result;
use crate::memory::{EmbeddingModel, EmbeddingTable, Memory, MemoryStore, MemoryType};
use anyhow::Context;

use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use tokio::sync::watch;

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

const MAX_MAINTENANCE_MERGE_SOURCE_MEMORIES: i64 = 2_000;
const MAX_MAINTENANCE_MERGES_PER_PASS: usize = 500;
const MAX_MAINTENANCE_SIMILAR_CANDIDATES: usize = 25;
const MAX_MERGED_MEMORY_CONTENT_BYTES: usize = 50_000;

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
    embedding_model: &Arc<EmbeddingModel>,
    config: &MaintenanceConfig,
) -> Result<MaintenanceReport> {
    let (_maintenance_cancel_tx, maintenance_cancel_rx) = watch::channel(false);
    run_maintenance_with_cancel(
        memory_store,
        embedding_table,
        embedding_model,
        config,
        maintenance_cancel_rx,
    )
    .await
}

/// Run maintenance tasks with a cancellation signal.
///
/// The signal allows maintenance to exit quickly when the caller decides to stop it.
pub async fn run_maintenance_with_cancel(
    memory_store: &MemoryStore,
    embedding_table: &EmbeddingTable,
    embedding_model: &Arc<EmbeddingModel>,
    config: &MaintenanceConfig,
    mut maintenance_cancel_rx: watch::Receiver<bool>,
) -> Result<MaintenanceReport> {
    let mut report = MaintenanceReport::default();
    check_maintenance_cancellation(&mut maintenance_cancel_rx).await?;
    validate_maintenance_config(config)?;

    // Apply decay to all non-identity memories
    // Fields are assigned sequentially because the values are async — can't use struct literal.
    #[allow(clippy::field_reassign_with_default)]
    {
        report.decayed =
            apply_decay(memory_store, config.decay_rate, &mut maintenance_cancel_rx).await?;
        report.pruned = prune_memories(memory_store, config, &mut maintenance_cancel_rx).await?;
        report.merged = merge_similar_memories(
            memory_store,
            embedding_table,
            embedding_model,
            config.merge_similarity_threshold,
            &mut maintenance_cancel_rx,
        )
        .await?;
    }

    // Optimize indexes to incorporate all changes from decay, prune, and merge.
    // This ensures the ANN index stays current with the full dataset.
    if let Err(error) = embedding_table.optimize_indexes().await {
        tracing::warn!(
            %error,
            "failed to optimize indexes during maintenance — index may be stale"
        );
        // Don't fail the entire maintenance — optimization is best-effort
    } else {
        tracing::info!("Index optimization complete after maintenance");
    }

    Ok(report)
}

/// Apply importance decay based on recency and access patterns.
async fn apply_decay(
    memory_store: &MemoryStore,
    decay_rate: f32,
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
) -> Result<usize> {
    check_maintenance_cancellation(maintenance_cancel_rx).await?;

    // Get all non-identity memories
    let all_types: Vec<_> = MemoryType::ALL
        .iter()
        .copied()
        .filter(|t| *t != MemoryType::Identity)
        .collect();

    let mut decayed_count = 0;

    for mem_type in all_types {
        let memories = maintenance_cancelable_op(
            maintenance_cancel_rx,
            memory_store.get_by_type(mem_type, 1000),
        )
        .await?;

        for mut memory in memories {
            check_maintenance_cancellation(maintenance_cancel_rx).await?;

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
                maintenance_cancelable_op(maintenance_cancel_rx, memory_store.update(&memory))
                    .await?;
                decayed_count += 1;
            }
        }
    }

    Ok(decayed_count)
}

/// Prune memories that have fallen below the importance threshold.
async fn prune_memories(
    memory_store: &MemoryStore,
    config: &MaintenanceConfig,
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
) -> Result<usize> {
    check_maintenance_cancellation(maintenance_cancel_rx).await?;

    let now = chrono::Utc::now();
    let min_age = chrono::Duration::days(config.min_age_days);
    let cutoff_date = now - min_age;

    // Get all memories below threshold that are old enough
    let candidates: Vec<SqliteRow> = maintenance_cancelable_op(
        maintenance_cancel_rx,
        sqlx::query(
            r#"
        SELECT id FROM memories
        WHERE importance < ? 
        AND memory_type != 'identity'
        AND created_at < ?
        "#,
        )
        .bind(config.prune_threshold)
        .bind(cutoff_date)
        .fetch_all(memory_store.pool()),
    )
    .await?;

    let mut pruned_count = 0;

    for row in candidates {
        let id: String = row.try_get("id")?;
        check_maintenance_cancellation(maintenance_cancel_rx).await?;
        maintenance_cancelable_op(maintenance_cancel_rx, memory_store.delete(&id)).await?;
        pruned_count += 1;
    }

    Ok(pruned_count)
}

/// Merge near-duplicate memories.
async fn merge_similar_memories(
    memory_store: &MemoryStore,
    embedding_table: &EmbeddingTable,
    embedding_model: &Arc<EmbeddingModel>,
    similarity_threshold: f32,
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
) -> Result<usize> {
    let memory_ids = fetch_candidate_memory_ids(memory_store, maintenance_cancel_rx).await?;
    if memory_ids.is_empty() {
        return Ok(0);
    }

    let mut merged_count = 0_usize;
    let mut merged_memory_ids = HashSet::new();

    for source_id in memory_ids {
        if merged_count >= MAX_MAINTENANCE_MERGES_PER_PASS {
            break;
        }
        check_maintenance_cancellation(maintenance_cancel_rx).await?;

        if merged_memory_ids.contains(&source_id) {
            continue;
        }

        let Some(source_memory) =
            maintenance_cancelable_op(maintenance_cancel_rx, memory_store.load(&source_id)).await?
        else {
            continue;
        };
        if source_memory.forgotten {
            continue;
        }
        let source_id = source_memory.id.clone();
        if merged_memory_ids.contains(&source_id) {
            continue;
        }

        let similar = maintenance_cancelable_op(
            maintenance_cancel_rx,
            embedding_table.find_similar(
                &source_memory.id,
                similarity_threshold,
                MAX_MAINTENANCE_SIMILAR_CANDIDATES,
            ),
        )
        .await
        .with_context(|| {
            format!(
                "failed to lookup similar memories during maintenance for source memory {}",
                source_memory.id
            )
        })?;

        if similar.is_empty() {
            continue;
        }

        let mut active_survivor = source_memory;
        let mut source_merged = false;

        for (candidate_id, _similarity) in similar {
            if merged_count >= MAX_MAINTENANCE_MERGES_PER_PASS {
                break;
            }
            check_maintenance_cancellation(maintenance_cancel_rx).await?;
            if merged_memory_ids.contains(&candidate_id) || candidate_id == active_survivor.id {
                continue;
            }

            let Some(candidate_memory) =
                maintenance_cancelable_op(maintenance_cancel_rx, memory_store.load(&candidate_id))
                    .await?
            else {
                continue;
            };
            if candidate_memory.forgotten {
                continue;
            }

            let (winner, loser) = choose_merge_pair(&active_survivor, &candidate_memory);
            let merged_survivor = merge_pair(
                memory_store,
                embedding_table,
                embedding_model,
                &winner,
                &loser,
                maintenance_cancel_rx,
            )
            .await?;
            merged_memory_ids.insert(loser.id.clone());
            merged_count += 1;

            if loser.id == source_id {
                source_merged = true;
                break;
            }

            active_survivor = merged_survivor;
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

    let merged = if winner_trimmed.is_empty() {
        loser_trimmed.to_string()
    } else {
        format!("{winner_trimmed}\n\n{loser_trimmed}")
    };

    if merged.len() <= MAX_MERGED_MEMORY_CONTENT_BYTES {
        return merged;
    }

    let boundary = merged.floor_char_boundary(MAX_MERGED_MEMORY_CONTENT_BYTES);
    merged[..boundary].to_string()
}

async fn merge_pair(
    memory_store: &MemoryStore,
    embedding_table: &EmbeddingTable,
    embedding_model: &Arc<EmbeddingModel>,
    survivor: &Memory,
    merged: &Memory,
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
) -> Result<Memory> {
    check_maintenance_cancellation(maintenance_cancel_rx).await?;

    let mut updated_survivor = survivor.clone();
    updated_survivor.content = merged_memory_content(updated_survivor.content, &merged.content);
    updated_survivor.updated_at = chrono::Utc::now();

    maintenance_cancelable_op(
        maintenance_cancel_rx,
        memory_store.merge_memories_atomic(&updated_survivor, merged),
    )
    .await?;

    let updated_survivor_embedding = maintenance_cancelable_op(
        maintenance_cancel_rx,
        embedding_model.embed_one(&updated_survivor.content),
    )
    .await?;
    maintenance_cancelable_op(
        maintenance_cancel_rx,
        embedding_table.delete(&updated_survivor.id),
    )
    .await?;
    maintenance_cancelable_op(
        maintenance_cancel_rx,
        embedding_table.store(
            &updated_survivor.id,
            &updated_survivor.content,
            &updated_survivor_embedding,
        ),
    )
    .await?;
    maintenance_cancelable_op(maintenance_cancel_rx, embedding_table.delete(&merged.id)).await?;
    Ok(updated_survivor)
}

async fn fetch_candidate_memory_ids(
    memory_store: &MemoryStore,
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
) -> Result<Vec<String>> {
    check_maintenance_cancellation(maintenance_cancel_rx).await?;

    let rows: Vec<SqliteRow> = maintenance_cancelable_op(
        maintenance_cancel_rx,
        sqlx::query(
            "SELECT id FROM memories WHERE forgotten = 0 ORDER BY importance DESC, created_at DESC, id ASC LIMIT ?",
        )
        .bind(MAX_MAINTENANCE_MERGE_SOURCE_MEMORIES)
        .fetch_all(memory_store.pool()),
    )
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

fn validate_maintenance_config(config: &MaintenanceConfig) -> Result<()> {
    validate_unit_interval("prune_threshold", config.prune_threshold)?;
    validate_unit_interval("decay_rate", config.decay_rate)?;
    validate_unit_interval(
        "merge_similarity_threshold",
        config.merge_similarity_threshold,
    )?;
    if config.min_age_days < 0 {
        return Err(anyhow::anyhow!(
            "maintenance min_age_days must be >= 0, got {}",
            config.min_age_days
        )
        .into());
    }
    Ok(())
}

fn validate_unit_interval(name: &str, value: f32) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(anyhow::anyhow!(
            "maintenance {name} must be finite and between 0.0 and 1.0, got {value}"
        )
        .into());
    }
    Ok(())
}

async fn check_maintenance_cancellation(
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
) -> Result<()> {
    if *maintenance_cancel_rx.borrow() {
        return Err(anyhow::anyhow!("memory maintenance cancelled").into());
    }

    if maintenance_cancel_rx
        .has_changed()
        .map_err(|error| anyhow::anyhow!(error))?
    {
        maintenance_cancel_rx
            .changed()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        if *maintenance_cancel_rx.borrow() {
            return Err(anyhow::anyhow!("memory maintenance cancelled").into());
        }
    }

    Ok(())
}

async fn maintenance_cancelable_op<T, E>(
    maintenance_cancel_rx: &mut watch::Receiver<bool>,
    operation: impl Future<Output = std::result::Result<T, E>>,
) -> Result<T>
where
    E: Into<crate::error::Error>,
{
    check_maintenance_cancellation(maintenance_cancel_rx).await?;

    tokio::select! {
        biased;
        _ = maintenance_cancel_rx.changed() => {
            Err(anyhow::anyhow!("memory maintenance cancelled").into())
        }
        result = operation => result.map_err(Into::into),
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
    use super::*;
    use crate::memory::{Association, RelationType};
    use std::sync::{Arc, OnceLock};
    use tempfile::tempdir;
    use tokio::time::Duration;

    fn shared_embedding_model() -> Arc<crate::memory::EmbeddingModel> {
        static MODEL: OnceLock<Arc<crate::memory::EmbeddingModel>> = OnceLock::new();
        Arc::clone(MODEL.get_or_init(|| {
            let cache_dir = std::env::temp_dir().join("spacebot-test-embedding-cache");
            std::fs::create_dir_all(&cache_dir).expect("failed to create embedding cache dir");
            Arc::new(
                crate::memory::EmbeddingModel::new(&cache_dir)
                    .expect("failed to initialize embedding model"),
            )
        }))
    }

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

        let embedding_model = shared_embedding_model();
        let report = run_maintenance(&store, &embedding_table, &embedding_model, &config)
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

    #[tokio::test]
    async fn merges_multiple_duplicates_into_one_survivor_in_single_pass() {
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
            "durable rust maintenance note",
            MemoryType::Fact,
            0.9,
            vec![1.0; 384],
        )
        .await;

        let duplicate_a = create_memory_with_embedding(
            &store,
            &embedding_table,
            "durable rust maintenance note update A",
            MemoryType::Fact,
            0.6,
            vec![1.0; 384],
        )
        .await;
        let duplicate_b = create_memory_with_embedding(
            &store,
            &embedding_table,
            "durable rust maintenance note update B",
            MemoryType::Fact,
            0.5,
            vec![1.0; 384],
        )
        .await;

        let related_a = create_memory_with_embedding(
            &store,
            &embedding_table,
            "related A",
            MemoryType::Fact,
            0.7,
            {
                let mut embedding = vec![0.0; 384];
                embedding[0] = 1.0;
                embedding
            },
        )
        .await;
        let related_b = create_memory_with_embedding(
            &store,
            &embedding_table,
            "related B",
            MemoryType::Fact,
            0.7,
            {
                let mut embedding = vec![0.0; 384];
                embedding[1] = 1.0;
                embedding
            },
        )
        .await;

        store
            .create_association(&Association::new(
                &duplicate_a.id,
                &related_a.id,
                RelationType::RelatedTo,
            ))
            .await
            .expect("failed to create duplicate_a association");
        store
            .create_association(&Association::new(
                &related_b.id,
                &duplicate_b.id,
                RelationType::PartOf,
            ))
            .await
            .expect("failed to create duplicate_b association");

        let embedding_model = shared_embedding_model();
        let report = run_maintenance(
            &store,
            &embedding_table,
            &embedding_model,
            &MaintenanceConfig {
                prune_threshold: 0.2,
                decay_rate: 0.05,
                min_age_days: 30,
                merge_similarity_threshold: 0.95,
            },
        )
        .await
        .expect("maintenance should succeed");

        assert_eq!(report.merged, 2);

        let refreshed_survivor = store
            .load(&survivor.id)
            .await
            .expect("failed to load survivor")
            .expect("survivor should exist");
        assert!(
            refreshed_survivor
                .content
                .contains("durable rust maintenance note update A")
        );
        assert!(
            refreshed_survivor
                .content
                .contains("durable rust maintenance note update B")
        );

        for duplicate_id in [&duplicate_a.id, &duplicate_b.id] {
            let duplicate = store
                .load(duplicate_id)
                .await
                .expect("failed to load duplicate")
                .expect("duplicate should exist");
            assert!(duplicate.forgotten);

            let duplicate_embeddings = embedding_table
                .find_similar(duplicate_id, 0.0, 10)
                .await
                .expect("failed to search duplicate embeddings");
            assert!(duplicate_embeddings.is_empty());
        }

        let survivor_associations = store
            .get_associations(&survivor.id)
            .await
            .expect("failed to load survivor associations");
        assert!(
            survivor_associations.iter().any(|association| {
                association.source_id == survivor.id && association.target_id == related_a.id
            }),
            "expected duplicate_a associations to rewire to survivor"
        );
        assert!(
            survivor_associations.iter().any(|association| {
                association.source_id == related_b.id && association.target_id == survivor.id
            }),
            "expected duplicate_b associations to rewire to survivor"
        );
    }

    #[tokio::test]
    async fn run_maintenance_with_cancel_stops_when_cancel_requested() {
        let store = MemoryStore::connect_in_memory().await;

        let dir = tempdir().expect("failed to create temp dir");
        let lance_conn = lancedb::connect(dir.path().to_str().expect("temp path"))
            .execute()
            .await
            .expect("failed to connect to lancedb");
        let embedding_table = crate::memory::EmbeddingTable::open_or_create(&lance_conn)
            .await
            .expect("failed to create embedding table");

        let (_cancel_tx, maintenance_cancel_rx) = tokio::sync::watch::channel(true);
        let embedding_model = shared_embedding_model();
        let result = run_maintenance_with_cancel(
            &store,
            &embedding_table,
            &embedding_model,
            &MaintenanceConfig::default(),
            maintenance_cancel_rx,
        )
        .await;

        assert!(
            result.is_err(),
            "maintenance should stop immediately when cancellation is requested"
        );
        assert!(
            result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("memory maintenance cancelled"),
            "expected explicit maintenance cancellation error"
        );

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn run_maintenance_rejects_invalid_configuration_ranges() {
        let store = MemoryStore::connect_in_memory().await;
        let dir = tempdir().expect("failed to create temp dir");
        let lance_conn = lancedb::connect(dir.path().to_str().expect("temp path"))
            .execute()
            .await
            .expect("failed to connect to lancedb");
        let embedding_table = crate::memory::EmbeddingTable::open_or_create(&lance_conn)
            .await
            .expect("failed to create embedding table");

        let invalid_config = MaintenanceConfig {
            prune_threshold: 0.2,
            decay_rate: 0.05,
            min_age_days: -1,
            merge_similarity_threshold: 0.95,
        };

        let embedding_model = shared_embedding_model();
        let result =
            run_maintenance(&store, &embedding_table, &embedding_model, &invalid_config).await;
        assert!(result.is_err(), "expected invalid config to fail");
        assert!(
            result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("min_age_days must be >= 0")
        );
    }
}
