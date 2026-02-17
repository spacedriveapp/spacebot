//! Memory graph storage (SQLite).

use crate::error::{MemoryError, Result};
use crate::memory::search::SearchSort;
use crate::memory::types::{Association, Memory, MemoryType, RelationType};

use anyhow::Context as _;
use sqlx::{Row, SqlitePool};

use std::sync::Arc;

/// Memory store for CRUD and graph operations.
pub struct MemoryStore {
    pool: SqlitePool,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore")
            .field("pool", &"<SqlitePool>")
            .finish()
    }
}

impl MemoryStore {
    /// Create a new memory store with the given SQLite pool.
    pub fn new(pool: SqlitePool) -> Arc<Self> {
        Arc::new(Self { pool })
    }

    /// Get a reference to the SQLite pool.
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Save a new memory to the store.
    pub async fn save(&self, memory: &Memory) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO memories (id, content, memory_type, importance, created_at, updated_at, 
                                 last_accessed_at, access_count, source, channel_id, forgotten)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&memory.id)
        .bind(&memory.content)
        .bind(memory.memory_type.to_string())
        .bind(memory.importance)
        .bind(memory.created_at)
        .bind(memory.updated_at)
        .bind(memory.last_accessed_at)
        .bind(memory.access_count)
        .bind(&memory.source)
        .bind(memory.channel_id.as_ref().map(|id| id.as_ref()))
        .bind(memory.forgotten)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to save memory {}", memory.id))?;

        Ok(())
    }

    /// Load a memory by ID.
    pub async fn load(&self, id: &str) -> Result<Option<Memory>> {
        let row = sqlx::query(
            r#"
            SELECT id, content, memory_type, importance, created_at, updated_at,
                   last_accessed_at, access_count, source, channel_id, forgotten
            FROM memories
            WHERE id = ?
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .with_context(|| format!("failed to load memory {}", id))?;

        Ok(row.map(|row| row_to_memory(&row)))
    }

    /// Update an existing memory.
    pub async fn update(&self, memory: &Memory) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE memories 
            SET content = ?, memory_type = ?, importance = ?, updated_at = ?, 
                last_accessed_at = ?, access_count = ?, source = ?, channel_id = ?,
                forgotten = ?
            WHERE id = ?
            "#,
        )
        .bind(&memory.content)
        .bind(memory.memory_type.to_string())
        .bind(memory.importance)
        .bind(memory.updated_at)
        .bind(memory.last_accessed_at)
        .bind(memory.access_count)
        .bind(&memory.source)
        .bind(memory.channel_id.as_ref().map(|id| id.as_ref()))
        .bind(memory.forgotten)
        .bind(&memory.id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to update memory {}", memory.id))?;

        Ok(())
    }

    /// Delete a memory by ID.
    pub async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM memories WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to delete memory {}", id))?;

        Ok(())
    }

    /// Record access to a memory, updating last_accessed_at and access_count.
    pub async fn record_access(&self, id: &str) -> Result<()> {
        let now = chrono::Utc::now();

        sqlx::query(
            r#"
            UPDATE memories 
            SET last_accessed_at = ?, access_count = access_count + 1
            WHERE id = ?
            "#,
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to record access for memory {}", id))?;

        Ok(())
    }

    /// Mark a memory as forgotten. The memory stays in the database but is
    /// excluded from search results and recall.
    pub async fn forget(&self, id: &str) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE memories SET forgotten = 1, updated_at = ? WHERE id = ? AND forgotten = 0",
        )
        .bind(chrono::Utc::now())
        .bind(id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to forget memory {}", id))?;

        Ok(result.rows_affected() > 0)
    }

    /// Create an association between two memories.
    pub async fn create_association(&self, association: &Association) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO associations (id, source_id, target_id, relation_type, weight, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_id, target_id, relation_type) DO UPDATE SET
                weight = excluded.weight
            "#,
        )
        .bind(&association.id)
        .bind(&association.source_id)
        .bind(&association.target_id)
        .bind(association.relation_type.to_string())
        .bind(association.weight)
        .bind(association.created_at)
        .execute(&self.pool)
        .await
        .with_context(|| {
            format!(
                "failed to create association from {} to {}",
                association.source_id, association.target_id
            )
        })?;

        Ok(())
    }

    /// Get all associations for a memory (both incoming and outgoing).
    pub async fn get_associations(&self, memory_id: &str) -> Result<Vec<Association>> {
        let rows = sqlx::query(
            r#"
            SELECT id, source_id, target_id, relation_type, weight, created_at
            FROM associations
            WHERE source_id = ? OR target_id = ?
            "#,
        )
        .bind(memory_id)
        .bind(memory_id)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("failed to get associations for memory {}", memory_id))?;

        let associations = rows
            .into_iter()
            .map(|row| row_to_association(&row))
            .collect();

        Ok(associations)
    }

    /// Get all associations where both source and target are in the provided set.
    /// Used by the graph view to fetch edges between a known set of visible nodes.
    pub async fn get_associations_between(
        &self,
        memory_ids: &[String],
    ) -> Result<Vec<Association>> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Build a parameterized IN clause. SQLite handles this fine for
        // the sizes we deal with (up to ~500 IDs).
        let placeholders: String = memory_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query_str = format!(
            "SELECT id, source_id, target_id, relation_type, weight, created_at \
             FROM associations \
             WHERE source_id IN ({placeholders}) AND target_id IN ({placeholders})"
        );

        let mut query = sqlx::query(&query_str);
        // Bind once for source_id IN, once for target_id IN
        for id in memory_ids {
            query = query.bind(id);
        }
        for id in memory_ids {
            query = query.bind(id);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .context("failed to get associations between memory set")?;

        Ok(rows
            .into_iter()
            .map(|row| row_to_association(&row))
            .collect())
    }

    /// Get neighbors of a memory: all associations plus the connected memories.
    /// Returns (neighbors, edges) where neighbors excludes any IDs in `exclude_ids`.
    pub async fn get_neighbors(
        &self,
        memory_id: &str,
        depth: u32,
        exclude_ids: &[String],
    ) -> Result<(Vec<Memory>, Vec<Association>)> {
        let mut visited: std::collections::HashSet<String> = exclude_ids.iter().cloned().collect();
        visited.insert(memory_id.to_string());

        let mut all_associations = Vec::new();
        let mut frontier = vec![memory_id.to_string()];

        for _ in 0..depth {
            if frontier.is_empty() {
                break;
            }

            let mut next_frontier = Vec::new();
            for node_id in &frontier {
                let associations = self.get_associations(node_id).await?;
                for assoc in associations {
                    let neighbor_id = if assoc.source_id == *node_id {
                        &assoc.target_id
                    } else {
                        &assoc.source_id
                    };

                    if !visited.contains(neighbor_id) {
                        visited.insert(neighbor_id.clone());
                        next_frontier.push(neighbor_id.clone());
                    }
                    all_associations.push(assoc);
                }
            }
            frontier = next_frontier;
        }

        // Deduplicate associations by id
        let mut seen_assoc_ids = std::collections::HashSet::new();
        all_associations.retain(|a| seen_assoc_ids.insert(a.id.clone()));

        // Load all neighbor memories (everything visited except the excludes and the seed)
        let neighbor_ids: Vec<String> = visited
            .into_iter()
            .filter(|id| !exclude_ids.contains(id))
            .collect();

        let mut neighbors = Vec::new();
        for id in &neighbor_ids {
            if let Some(memory) = self.load(id).await? {
                if !memory.forgotten {
                    neighbors.push(memory);
                }
            }
        }

        Ok((neighbors, all_associations))
    }

    /// Get memories by type.
    pub async fn get_by_type(&self, memory_type: MemoryType, limit: i64) -> Result<Vec<Memory>> {
        let type_str = memory_type.to_string();

        let rows = sqlx::query(
            r#"
            SELECT id, content, memory_type, importance, created_at, updated_at,
                   last_accessed_at, access_count, source, channel_id, forgotten
            FROM memories
            WHERE memory_type = ? AND forgotten = 0
            ORDER BY importance DESC, updated_at DESC
            LIMIT ?
            "#,
        )
        .bind(&type_str)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("failed to get memories by type {:?}", memory_type))?;

        Ok(rows.into_iter().map(|row| row_to_memory(&row)).collect())
    }

    /// Get high-importance memories for injection into context.
    pub async fn get_high_importance(&self, threshold: f32, limit: i64) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            r#"
            SELECT id, content, memory_type, importance, created_at, updated_at,
                   last_accessed_at, access_count, source, channel_id, forgotten
            FROM memories
            WHERE importance >= ? AND forgotten = 0
            ORDER BY importance DESC, updated_at DESC
            LIMIT ?
            "#,
        )
        .bind(threshold)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to get high importance memories")?;

        Ok(rows.into_iter().map(|row| row_to_memory(&row)).collect())
    }

    /// Get memories sorted by a flexible criterion with optional type filter.
    ///
    /// Used by non-hybrid search modes (Recent, Important, Typed) to retrieve
    /// memories directly from SQLite without vector/FTS overhead.
    pub async fn get_sorted(
        &self,
        sort: SearchSort,
        limit: i64,
        memory_type: Option<MemoryType>,
    ) -> Result<Vec<Memory>> {
        let order_clause = match sort {
            SearchSort::Recent => "ORDER BY created_at DESC",
            SearchSort::Importance => "ORDER BY importance DESC, created_at DESC",
            SearchSort::MostAccessed => "ORDER BY access_count DESC, created_at DESC",
        };

        let (query_str, type_filter) = if let Some(ref memory_type) = memory_type {
            (
                format!(
                    "SELECT id, content, memory_type, importance, created_at, updated_at, \
                     last_accessed_at, access_count, source, channel_id, forgotten \
                     FROM memories WHERE memory_type = ? AND forgotten = 0 {order_clause} LIMIT ?"
                ),
                Some(memory_type.to_string()),
            )
        } else {
            (
                format!(
                    "SELECT id, content, memory_type, importance, created_at, updated_at, \
                     last_accessed_at, access_count, source, channel_id, forgotten \
                     FROM memories WHERE forgotten = 0 {order_clause} LIMIT ?"
                ),
                None,
            )
        };

        let rows = if let Some(type_str) = type_filter {
            sqlx::query(&query_str)
                .bind(type_str)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query(&query_str)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        }
        .with_context(|| format!("failed to get sorted memories ({sort:?})"))?;

        Ok(rows.into_iter().map(|row| row_to_memory(&row)).collect())
    }

    /// Create an in-memory store for testing. Each call creates an isolated
    /// database so tests can run in parallel without migration conflicts.
    #[cfg(test)]
    pub async fn connect_in_memory() -> Arc<Self> {
        use sqlx::sqlite::SqliteConnectOptions;

        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);

        // Single-connection pool: each pool gets its own private in-memory db.
        let pool = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("in-memory SQLite");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        Arc::new(Self { pool })
    }
}

/// Helper: Convert a database row to a Memory.
fn row_to_memory(row: &sqlx::sqlite::SqliteRow) -> Memory {
    let mem_type_str: String = row.try_get("memory_type").unwrap_or_default();
    let memory_type = parse_memory_type(&mem_type_str);

    let channel_id: Option<String> = row.try_get("channel_id").ok();

    Memory {
        id: row.try_get("id").unwrap_or_default(),
        content: row.try_get("content").unwrap_or_default(),
        memory_type,
        importance: row.try_get("importance").unwrap_or(0.5),
        created_at: row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        updated_at: row
            .try_get("updated_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        last_accessed_at: row
            .try_get("last_accessed_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        access_count: row.try_get("access_count").unwrap_or(0),
        source: row.try_get("source").ok(),
        channel_id: channel_id.map(|id| Arc::from(id) as crate::ChannelId),
        forgotten: row.try_get::<bool, _>("forgotten").unwrap_or(false),
    }
}

/// Helper: Parse memory type from string.
fn parse_memory_type(s: &str) -> MemoryType {
    match s {
        "fact" => MemoryType::Fact,
        "preference" => MemoryType::Preference,
        "decision" => MemoryType::Decision,
        "identity" => MemoryType::Identity,
        "event" => MemoryType::Event,
        "observation" => MemoryType::Observation,
        "goal" => MemoryType::Goal,
        "todo" => MemoryType::Todo,
        _ => MemoryType::Fact,
    }
}

/// Helper: Convert a database row to an Association.
fn row_to_association(row: &sqlx::sqlite::SqliteRow) -> Association {
    let relation_type_str: String = row.try_get("relation_type").unwrap_or_default();
    let relation_type = parse_relation_type(&relation_type_str);

    Association {
        id: row.try_get("id").unwrap_or_default(),
        source_id: row.try_get("source_id").unwrap_or_default(),
        target_id: row.try_get("target_id").unwrap_or_default(),
        relation_type,
        weight: row.try_get("weight").unwrap_or(0.5),
        created_at: row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
    }
}

/// Helper: Parse relation type from string.
fn parse_relation_type(s: &str) -> RelationType {
    match s {
        "related_to" => RelationType::RelatedTo,
        "updates" => RelationType::Updates,
        "contradicts" => RelationType::Contradicts,
        "caused_by" => RelationType::CausedBy,
        "result_of" => RelationType::ResultOf,
        "part_of" => RelationType::PartOf,
        _ => RelationType::RelatedTo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    /// Insert a memory with a specific created_at timestamp.
    async fn insert_memory_at(
        store: &MemoryStore,
        content: &str,
        memory_type: MemoryType,
        importance: f32,
        created_at: chrono::DateTime<Utc>,
    ) -> Memory {
        let mut memory = Memory::new(content, memory_type).with_importance(importance);
        memory.created_at = created_at;
        memory.updated_at = created_at;
        memory.last_accessed_at = created_at;
        store.save(&memory).await.unwrap();
        memory
    }

    #[tokio::test]
    async fn test_save_and_load() {
        let store = MemoryStore::connect_in_memory().await;
        let memory = Memory::new("Rust is great", MemoryType::Fact);
        store.save(&memory).await.unwrap();

        let loaded = store.load(&memory.id).await.unwrap().unwrap();
        assert_eq!(loaded.content, "Rust is great");
        assert_eq!(loaded.memory_type, MemoryType::Fact);
    }

    #[tokio::test]
    async fn test_get_sorted_recent() {
        let store = MemoryStore::connect_in_memory().await;
        let now = Utc::now();

        let old = insert_memory_at(
            &store,
            "old",
            MemoryType::Fact,
            0.5,
            now - Duration::hours(3),
        )
        .await;
        let mid = insert_memory_at(
            &store,
            "mid",
            MemoryType::Fact,
            0.5,
            now - Duration::hours(1),
        )
        .await;
        let new = insert_memory_at(&store, "new", MemoryType::Fact, 0.5, now).await;

        let results = store
            .get_sorted(SearchSort::Recent, 10, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, new.id);
        assert_eq!(results[1].id, mid.id);
        assert_eq!(results[2].id, old.id);
    }

    #[tokio::test]
    async fn test_get_sorted_importance() {
        let store = MemoryStore::connect_in_memory().await;
        let now = Utc::now();

        let low = insert_memory_at(&store, "low", MemoryType::Fact, 0.2, now).await;
        let high = insert_memory_at(&store, "high", MemoryType::Fact, 0.9, now).await;
        let medium = insert_memory_at(&store, "medium", MemoryType::Fact, 0.5, now).await;

        let results = store
            .get_sorted(SearchSort::Importance, 10, None)
            .await
            .unwrap();
        assert_eq!(results[0].id, high.id);
        assert_eq!(results[1].id, medium.id);
        assert_eq!(results[2].id, low.id);
    }

    #[tokio::test]
    async fn test_get_sorted_most_accessed() {
        let store = MemoryStore::connect_in_memory().await;
        let now = Utc::now();

        let a = insert_memory_at(&store, "rarely accessed", MemoryType::Fact, 0.5, now).await;
        let b = insert_memory_at(&store, "often accessed", MemoryType::Fact, 0.5, now).await;

        // Record 5 accesses for b, 1 for a
        store.record_access(&a.id).await.unwrap();
        for _ in 0..5 {
            store.record_access(&b.id).await.unwrap();
        }

        let results = store
            .get_sorted(SearchSort::MostAccessed, 10, None)
            .await
            .unwrap();
        assert_eq!(results[0].id, b.id);
        assert_eq!(results[1].id, a.id);
    }

    #[tokio::test]
    async fn test_get_sorted_with_type_filter() {
        let store = MemoryStore::connect_in_memory().await;
        let now = Utc::now();

        insert_memory_at(&store, "a fact", MemoryType::Fact, 0.5, now).await;
        insert_memory_at(&store, "a preference", MemoryType::Preference, 0.5, now).await;
        let decision = insert_memory_at(&store, "a decision", MemoryType::Decision, 0.5, now).await;
        insert_memory_at(&store, "an event", MemoryType::Event, 0.5, now).await;

        let results = store
            .get_sorted(SearchSort::Recent, 10, Some(MemoryType::Decision))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, decision.id);
    }

    #[tokio::test]
    async fn test_get_sorted_respects_limit() {
        let store = MemoryStore::connect_in_memory().await;
        let now = Utc::now();

        for i in 0..10 {
            insert_memory_at(
                &store,
                &format!("memory {i}"),
                MemoryType::Fact,
                0.5,
                now - Duration::minutes(i),
            )
            .await;
        }

        let results = store.get_sorted(SearchSort::Recent, 3, None).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_get_sorted_excludes_forgotten() {
        let store = MemoryStore::connect_in_memory().await;
        let now = Utc::now();

        let visible = insert_memory_at(&store, "visible", MemoryType::Fact, 0.5, now).await;
        let forgotten = insert_memory_at(&store, "forgotten", MemoryType::Fact, 0.5, now).await;
        store.forget(&forgotten.id).await.unwrap();

        let results = store
            .get_sorted(SearchSort::Recent, 10, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, visible.id);
    }
}
