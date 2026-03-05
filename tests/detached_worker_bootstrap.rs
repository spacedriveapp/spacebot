//! Integration coverage for detached worker bootstrap behavior.
//!
//! Verifies that supervision control entries are not leaked when task bootstrap
//! fails after registration.

use spacebot::AgentId;
use spacebot::agent::cortex::register_detached_worker_for_pickup;
use spacebot::agent::process_control::ProcessControlRegistry;
use spacebot::tasks::TaskStore;
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn detached_bootstrap_rolls_back_control_entry_on_database_failure() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("failed to create sqlite memory pool");

    sqlx::query(
        "CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL,
            task_number INTEGER NOT NULL,
            title TEXT NOT NULL,
            description TEXT,
            status TEXT NOT NULL DEFAULT 'backlog',
            priority TEXT NOT NULL DEFAULT 'medium',
            subtasks TEXT,
            metadata TEXT,
            source_memory_id TEXT,
            worker_id TEXT,
            created_by TEXT NOT NULL,
            approved_at TIMESTAMP,
            approved_by TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            UNIQUE(agent_id, task_number)
        )",
    )
    .execute(&pool)
    .await
    .expect("failed to create tasks table");

    let task_store = TaskStore::new(pool.clone());
    let registry = ProcessControlRegistry::new();
    let agent_id: AgentId = "agent-1".into();
    let task_number = 7_i64;
    let worker_id = uuid::Uuid::new_v4();

    sqlx::query(
        "INSERT INTO tasks (
            id, agent_id, task_number, title, description, status, priority,
            subtasks, metadata, source_memory_id, created_by
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&agent_id[..])
    .bind(task_number)
    .bind("detached bootstrap test")
    .bind(Some("integration test"))
    .bind("ready")
    .bind("medium")
    .bind("[]")
    .bind("{}")
    .bind(Option::<String>::None)
    .bind("system")
    .execute(&pool)
    .await
    .expect("failed to insert ready task fixture");

    sqlx::query("DROP TABLE tasks")
        .execute(&pool)
        .await
        .expect("failed to drop tasks table");

    let result = register_detached_worker_for_pickup(
        &registry,
        &task_store,
        &agent_id,
        task_number,
        worker_id,
    )
    .await;

    assert!(
        result.is_err(),
        "bootstrap should fail when the task table is missing"
    );

    assert!(
        !registry.unregister_detached_worker(worker_id).await,
        "registry entry should be cleaned up on bootstrap failure"
    );
}
