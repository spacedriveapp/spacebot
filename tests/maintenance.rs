//! Memory maintenance integration coverage.

use spacebot::memory::maintenance::{run_maintenance, run_maintenance_with_cancel};
use spacebot::memory::{MemoryStore, RelationType, maintenance::MaintenanceConfig};
use tempfile::tempdir;
use tokio::sync::watch;

async fn make_memory_maintenance_fixture() -> (
    std::sync::Arc<MemoryStore>,
    spacebot::memory::EmbeddingTable,
    tempfile::TempDir,
) {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .in_memory(true)
        .create_if_missing(true);
    let pool = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("failed to connect in-memory db");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run migrations");

    let store: std::sync::Arc<MemoryStore> = MemoryStore::new(pool);

    let dir = tempdir().expect("failed to create temp dir");
    let lance_conn = lancedb::connect(dir.path().to_str().expect("temp path"))
        .execute()
        .await
        .expect("failed to connect to lancedb");
    let embedding_table = spacebot::memory::EmbeddingTable::open_or_create(&lance_conn)
        .await
        .expect("failed to create embedding table");

    (store, embedding_table, dir)
}

#[tokio::test]
async fn maintenance_run_merges_duplicate_memory_and_links_updates_edge() {
    let (store, embedding_table, _dir_guard) = make_memory_maintenance_fixture().await;

    let survivor = {
        let memory = spacebot::memory::Memory::new(
            "phase3 maintenance survivor",
            spacebot::memory::MemoryType::Fact,
        )
        .with_importance(0.9);
        store
            .save(&memory)
            .await
            .expect("failed to save survivor memory");
        embedding_table
            .store(&memory.id, &memory.content, &vec![1.0; 384])
            .await
            .expect("failed to store survivor embedding");
        memory
    };

    let duplicate = {
        let memory = spacebot::memory::Memory::new(
            "phase3 maintenance survivor updated",
            spacebot::memory::MemoryType::Fact,
        )
        .with_importance(0.4);
        store
            .save(&memory)
            .await
            .expect("failed to save duplicate memory");
        embedding_table
            .store(&memory.id, &memory.content, &vec![1.0; 384])
            .await
            .expect("failed to store duplicate embedding");
        memory
    };

    let related = {
        let memory =
            spacebot::memory::Memory::new("related memory", spacebot::memory::MemoryType::Fact)
                .with_importance(0.8);
        store
            .save(&memory)
            .await
            .expect("failed to save related memory");
        embedding_table
            .store(&memory.id, &memory.content, &vec![0.0; 384])
            .await
            .expect("failed to store related embedding");
        memory
    };

    store
        .create_association(&spacebot::memory::Association::new(
            &duplicate.id,
            &related.id,
            spacebot::memory::RelationType::RelatedTo,
        ))
        .await
        .expect("failed to create related association");

    store
        .create_association(&spacebot::memory::Association::new(
            &related.id,
            &duplicate.id,
            spacebot::memory::RelationType::PartOf,
        ))
        .await
        .expect("failed to create part-of association");

    let report = run_maintenance(
        &store,
        &embedding_table,
        &MaintenanceConfig {
            prune_threshold: 0.2,
            decay_rate: 0.05,
            min_age_days: 30,
            merge_similarity_threshold: 0.95,
        },
    )
    .await
    .expect("maintenance should succeed");

    assert_eq!(report.merged, 1);

    let survivor_assocs = store
        .get_associations(&survivor.id)
        .await
        .expect("failed to load survivor associations");
    let has_updates = survivor_assocs.iter().any(|association| {
        association.source_id == survivor.id
            && association.target_id == duplicate.id
            && association.relation_type == RelationType::Updates
    });
    assert!(
        has_updates,
        "survivor must keep updates association to merged memory"
    );

    let duplicate_assocs = store
        .get_associations(&duplicate.id)
        .await
        .expect("failed to load duplicate associations");
    assert_eq!(duplicate_assocs.len(), 1);
    assert_eq!(duplicate_assocs[0].source_id, survivor.id);
    assert_eq!(duplicate_assocs[0].relation_type, RelationType::Updates);

    let forgotten = store
        .load(&duplicate.id)
        .await
        .expect("failed to load duplicate memory")
        .expect("duplicate should still exist");
    assert!(forgotten.forgotten);

    let similar_to_merged = embedding_table
        .find_similar(&duplicate.id, 0.0, 10)
        .await
        .expect("failed to query merged embedding");
    assert!(similar_to_merged.is_empty());
}

#[tokio::test]
async fn maintenance_run_can_be_cancelled() {
    let (store, embedding_table, _dir_guard) = make_memory_maintenance_fixture().await;
    let maintenance_config = MaintenanceConfig::default();

    let (cancel_tx, cancel_rx) = watch::channel(false);
    cancel_tx.send_replace(true);

    let maintenance_task = tokio::spawn(async move {
        run_maintenance_with_cancel(&store, &embedding_table, &maintenance_config, cancel_rx).await
    });

    let result = maintenance_task
        .await
        .expect("maintenance task should have completed");

    assert!(
        result.is_err(),
        "maintenance should stop after cancellation is requested"
    );
    let error_message = result.unwrap_err().to_string();
    assert!(
        error_message.contains("memory maintenance cancelled"),
        "expected cancellation error, got: {}",
        error_message,
    );
}

#[tokio::test]
async fn maintenance_config_validation_rejects_negative_min_age() {
    let (store, embedding_table, _dir_guard) = make_memory_maintenance_fixture().await;

    let result = run_maintenance(
        &store,
        &embedding_table,
        &MaintenanceConfig {
            prune_threshold: 0.2,
            decay_rate: 0.05,
            min_age_days: -5,
            merge_similarity_threshold: 0.95,
        },
    )
    .await;

    assert!(result.is_err(), "negative min_age_days must be rejected");
    let error_message = result.unwrap_err().to_string();
    assert!(
        error_message.contains("min_age_days must be >= 0"),
        "unexpected error message: {}",
        error_message
    );
}
