//! Advisory prefetch queue — pre-generates advisory packets during idle time.
//!
//! A `PrefetchQueue` persists work items in the `prefetch_queue` table so the
//! system can warm up advisory packets before they are needed.  Items are
//! enqueued by intent family, processed in priority order, and cleaned up
//! after a configurable retention window.

use crate::learning::LearningStore;

use anyhow::{Context as _, Result};
use uuid::Uuid;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

// ---------------------------------------------------------------------------
// Intent-to-tool probability map
// ---------------------------------------------------------------------------

/// Probability table mapping an intent family to the tools most likely to be
/// invoked, together with an advisory pre-generation priority (0.0–1.0).
///
/// Higher probability means more benefit from pre-warming advisory packets
/// for that tool before the turn begins.
static INTENT_TOOL_MAP: LazyLock<HashMap<String, Vec<(String, f64)>>> = LazyLock::new(|| {
    let mut map = HashMap::new();

    map.insert(
        "deploy".to_string(),
        vec![("shell".to_string(), 0.85), ("file".to_string(), 0.70)],
    );
    map.insert(
        "search".to_string(),
        vec![("shell".to_string(), 0.90), ("file".to_string(), 0.80)],
    );
    map.insert(
        "edit".to_string(),
        vec![("file".to_string(), 0.95), ("shell".to_string(), 0.40)],
    );
    map.insert(
        "test".to_string(),
        vec![("shell".to_string(), 0.95), ("exec".to_string(), 0.60)],
    );
    map.insert(
        "debug".to_string(),
        vec![("shell".to_string(), 0.85), ("file".to_string(), 0.75)],
    );
    map.insert(
        "build".to_string(),
        vec![("shell".to_string(), 0.90), ("exec".to_string(), 0.70)],
    );

    map
});

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single item dequeued from the prefetch queue, ready for processing.
#[derive(Debug, Clone)]
pub struct PrefetchItem {
    pub id: String,
    pub intent_family: String,
    pub tool_name: String,
    /// Optional sub-operation within the tool (e.g. `"read"`, `"write"`).
    pub tool_operation: Option<String>,
    /// Execution phase this advisory targets (e.g. `"pre_flight"`).
    pub phase: Option<String>,
    /// Pre-generation priority derived from the intent-to-tool probability.
    pub priority: f64,
}

/// Manages the `prefetch_queue` table in `learning.db`.
///
/// Callers enqueue intent families, a background worker dequeues batches and
/// generates advisory packets, then marks items done.  Stale done items are
/// pruned by `cleanup_old`.
pub struct PrefetchQueue {
    store: Arc<LearningStore>,
}

// ---------------------------------------------------------------------------
// Inherent methods
// ---------------------------------------------------------------------------

impl PrefetchQueue {
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    /// Enqueue pre-generation work for every tool associated with an intent
    /// family.
    ///
    /// Looks up `intent_family` in the probability map and inserts one
    /// `prefetch_queue` row per `(tool, priority)` pair.  Returns the IDs of
    /// the newly created rows so callers can track them.  Unknown intent
    /// families produce an empty result without error.
    pub async fn enqueue(
        &self,
        intent_family: &str,
        phase: Option<&str>,
    ) -> Result<Vec<String>> {
        let items = generate_prefetch_items(intent_family);
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let pool = self.store.pool();
        let mut ids = Vec::with_capacity(items.len());

        for (tool_name, tool_operation, priority) in items {
            let id = Uuid::new_v4().to_string();

            sqlx::query(
                r#"
                INSERT INTO prefetch_queue
                    (id, intent_family, tool_name, tool_operation, phase, priority, status)
                VALUES (?, ?, ?, ?, ?, ?, 'pending')
                "#,
            )
            .bind(&id)
            .bind(intent_family)
            .bind(&tool_name)
            .bind(&tool_operation)
            .bind(phase)
            .bind(priority)
            .execute(pool)
            .await
            .with_context(|| {
                format!(
                    "failed to enqueue prefetch for intent '{}' tool '{}'",
                    intent_family, tool_name
                )
            })?;

            ids.push(id);
        }

        tracing::debug!(
            intent_family,
            count = ids.len(),
            "prefetch items enqueued"
        );

        Ok(ids)
    }

    /// Atomically fetch the highest-priority pending items and mark them as
    /// `processing`.
    ///
    /// Uses a transaction so no two concurrent callers pull the same batch.
    pub async fn dequeue_batch(&self, limit: i64) -> Result<Vec<PrefetchItem>> {
        let pool = self.store.pool();
        let mut transaction = pool.begin().await.context("failed to begin transaction")?;

        let rows: Vec<(String, String, String, Option<String>, Option<String>, f64)> =
            sqlx::query_as(
                r#"
                SELECT id, intent_family, tool_name, tool_operation, phase, priority
                FROM prefetch_queue
                WHERE status = 'pending'
                ORDER BY priority DESC
                LIMIT ?
                "#,
            )
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await
            .context("failed to fetch pending prefetch items")?;

        let items: Vec<PrefetchItem> = rows
            .into_iter()
            .map(
                |(id, intent_family, tool_name, tool_operation, phase, priority)| PrefetchItem {
                    id,
                    intent_family,
                    tool_name,
                    tool_operation,
                    phase,
                    priority,
                },
            )
            .collect();

        for item in &items {
            sqlx::query(
                "UPDATE prefetch_queue SET status = 'processing' WHERE id = ?",
            )
            .bind(&item.id)
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("failed to mark item '{}' as processing", item.id))?;
        }

        transaction.commit().await.context("failed to commit dequeue transaction")?;

        tracing::debug!(count = items.len(), "prefetch batch dequeued");

        Ok(items)
    }

    /// Mark a prefetch item as successfully completed.
    pub async fn mark_done(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE prefetch_queue SET status = 'done' WHERE id = ?")
            .bind(id)
            .execute(self.store.pool())
            .await
            .with_context(|| format!("failed to mark prefetch item '{}' as done", id))?;

        Ok(())
    }

    /// Delete completed items older than `max_age_hours` hours.
    ///
    /// Returns the number of rows deleted.
    pub async fn cleanup_old(&self, max_age_hours: i64) -> Result<u64> {
        // Build the datetime modifier string (e.g. "-48 hours") at the Rust
        // level so it can be passed as a bound parameter to SQLite's datetime().
        let modifier = format!("-{max_age_hours} hours");

        let result = sqlx::query(
            r#"
            DELETE FROM prefetch_queue
            WHERE status = 'done'
              AND created_at < datetime('now', ?)
            "#,
        )
        .bind(&modifier)
        .execute(self.store.pool())
        .await
        .with_context(|| {
            format!("failed to clean up prefetch items older than {max_age_hours} hours")
        })?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            tracing::debug!(deleted, max_age_hours, "old prefetch items pruned");
        }

        Ok(deleted)
    }
}

impl std::fmt::Debug for PrefetchQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrefetchQueue")
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Public helper
// ---------------------------------------------------------------------------

/// Return `(tool_name, tool_operation, priority)` tuples for a given intent
/// family by consulting the static probability map.
///
/// `tool_operation` is `None` for all built-in mappings.  Returns an empty
/// vector when the intent family is not recognised — this is intentional; the
/// caller decides whether that warrants logging.
pub fn generate_prefetch_items(
    intent_family: &str,
) -> Vec<(String, Option<String>, f64)> {
    let Some(entries) = INTENT_TOOL_MAP.get(intent_family) else {
        return Vec::new();
    };

    entries
        .iter()
        .map(|(tool_name, priority)| (tool_name.clone(), None, *priority))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Spin up a fresh LearningStore backed by a temporary SQLite file.
    async fn setup_queue() -> PrefetchQueue {
        let path: PathBuf = std::env::temp_dir()
            .join(format!("test_prefetch_{}.db", Uuid::new_v4()));
        let store = LearningStore::connect(&path).await.unwrap();
        PrefetchQueue::new(store)
    }

    // -- generate_prefetch_items --------------------------------------------

    #[test]
    fn test_generate_items_known_intent() {
        let items = generate_prefetch_items("deploy");
        assert_eq!(items.len(), 2, "deploy maps to two tools");

        let (tool_name, tool_operation, priority) = &items[0];
        assert_eq!(tool_name, "shell");
        assert!(tool_operation.is_none());
        assert!((*priority - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_generate_items_unknown_intent_is_empty() {
        let items = generate_prefetch_items("frobnicate");
        assert!(items.is_empty());
    }

    #[test]
    fn test_generate_items_edit_second_tool_is_shell() {
        let items = generate_prefetch_items("edit");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "file");
        assert_eq!(items[1].0, "shell");
    }

    // -- enqueue ------------------------------------------------------------

    #[tokio::test]
    async fn test_enqueue_returns_correct_id_count() {
        let queue = setup_queue().await;
        let ids = queue.enqueue("deploy", None).await.unwrap();
        // deploy → shell + file → 2 IDs
        assert_eq!(ids.len(), 2);
        // IDs should be distinct
        assert_ne!(ids[0], ids[1]);
    }

    #[tokio::test]
    async fn test_enqueue_unknown_intent_returns_empty() {
        let queue = setup_queue().await;
        let ids = queue.enqueue("frobnicate", None).await.unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn test_enqueue_with_phase() {
        let queue = setup_queue().await;
        let ids = queue.enqueue("build", Some("pre_flight")).await.unwrap();
        // build → shell + exec → 2 IDs
        assert_eq!(ids.len(), 2);
    }

    // -- dequeue_batch ------------------------------------------------------

    #[tokio::test]
    async fn test_dequeue_batch_returns_items_ordered_by_priority() {
        let queue = setup_queue().await;
        // build: shell=0.90, exec=0.70  — highest priority first
        queue.enqueue("build", None).await.unwrap();

        let items = queue.dequeue_batch(10).await.unwrap();
        assert!(!items.is_empty());
        assert_eq!(items[0].intent_family, "build");
        assert_eq!(items[0].tool_name, "shell");
        assert!(items[0].priority > items[1].priority);
    }

    #[tokio::test]
    async fn test_dequeue_batch_limits_result_count() {
        let queue = setup_queue().await;
        queue.enqueue("build", None).await.unwrap(); // inserts 2
        queue.enqueue("test", None).await.unwrap(); // inserts 2 more

        let items = queue.dequeue_batch(2).await.unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn test_dequeue_batch_does_not_return_processing_items_twice() {
        let queue = setup_queue().await;
        queue.enqueue("test", None).await.unwrap();

        let first_batch = queue.dequeue_batch(10).await.unwrap();
        let second_batch = queue.dequeue_batch(10).await.unwrap();

        assert!(!first_batch.is_empty());
        // All items were flipped to 'processing' — nothing left for the second call.
        assert!(second_batch.is_empty());
    }

    // -- mark_done + cleanup_old --------------------------------------------

    #[tokio::test]
    async fn test_mark_done_removes_item_from_pending() {
        let queue = setup_queue().await;
        let ids = queue.enqueue("debug", None).await.unwrap();
        let items = queue.dequeue_batch(10).await.unwrap();

        for item in &items {
            queue.mark_done(&item.id).await.unwrap();
        }

        // After marking done, a fresh dequeue should return nothing.
        let remaining = queue.dequeue_batch(10).await.unwrap();
        assert!(remaining.is_empty());

        // IDs returned by enqueue match those we processed.
        for id in &ids {
            assert!(items.iter().any(|item| &item.id == id));
        }
    }

    #[tokio::test]
    async fn test_cleanup_old_returns_zero_when_nothing_done() {
        let queue = setup_queue().await;
        queue.enqueue("search", None).await.unwrap();
        // Items are still pending — cleanup_old only touches 'done' rows.
        let deleted = queue.cleanup_old(0).await.unwrap();
        assert_eq!(deleted, 0);
    }
}
