//! LanceDB table management and embedding storage with HNSW vector index and FTS.

use crate::error::{DbError, Result};
use arrow_array::cast::AsArray;
use arrow_array::types::Float32Type;
use arrow_array::{Array, RecordBatchIterator};
use futures::TryStreamExt;
use std::sync::Arc;

/// Schema constants for the embeddings table.
const TABLE_NAME: &str = "memory_embeddings";
const EMBEDDING_DIM: i32 = 384; // all-MiniLM-L6-v2 dimension

/// LanceDB table for memory embeddings with HNSW index and FTS.
pub struct EmbeddingTable {
    table: lancedb::Table,
}

impl Clone for EmbeddingTable {
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
        }
    }
}

impl EmbeddingTable {
    /// Open existing table or create a new one.
    ///
    /// If the table exists but is corrupted (e.g. process killed mid-write),
    /// it is dropped and recreated. Embeddings can be regenerated from SQLite.
    pub async fn open_or_create(connection: &lancedb::Connection) -> Result<Self> {
        // Try to open existing table
        match connection.open_table(TABLE_NAME).execute().await {
            Ok(table) => return Ok(Self { table }),
            Err(error) => {
                tracing::debug!(%error, "failed to open embeddings table, will create");
            }
        }

        // Table doesn't exist or is unreadable — try creating it
        match Self::create_empty_table(connection).await {
            Ok(table) => return Ok(Self { table }),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "failed to create embeddings table, attempting recovery from corrupted state"
                );
            }
        }

        // Both open and create failed — table data exists but is corrupted.
        // Drop it and recreate from scratch.
        if let Err(error) = connection.drop_table(TABLE_NAME, &[]).await {
            tracing::warn!(%error, "drop_table failed during recovery, proceeding anyway");
        }

        let table = Self::create_empty_table(connection).await?;
        tracing::info!("embeddings table recovered — embeddings will be rebuilt from memory store");

        Ok(Self { table })
    }

    /// Create an empty embeddings table.
    async fn create_empty_table(connection: &lancedb::Connection) -> Result<lancedb::Table> {
        let schema = Self::schema();
        let batches = RecordBatchIterator::new(vec![].into_iter().map(Ok), Arc::new(schema));

        connection
            .create_table(TABLE_NAME, Box::new(batches))
            .execute()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()).into())
    }

    /// Store an embedding with content for a memory.
    /// The content is stored for FTS search capability.
    pub async fn store(&self, memory_id: &str, content: &str, embedding: &[f32]) -> Result<()> {
        if embedding.len() != EMBEDDING_DIM as usize {
            return Err(DbError::LanceDb(format!(
                "Embedding dimension mismatch: expected {}, got {}",
                EMBEDDING_DIM,
                embedding.len()
            ))
            .into());
        }

        use arrow_array::{RecordBatch, StringArray};

        let schema = Self::schema();

        // Build arrays for the record batch
        let id_array = StringArray::from(vec![memory_id]);
        let content_array = StringArray::from(vec![content]);

        // Convert embedding to FixedSizeListArray
        let embedding_array =
            arrow_array::FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                vec![Some(embedding.iter().map(|v| Some(*v)).collect::<Vec<_>>())],
                EMBEDDING_DIM,
            );

        let batch = RecordBatch::try_new(
            Arc::new(schema),
            vec![
                Arc::new(id_array) as arrow_array::ArrayRef,
                Arc::new(content_array) as arrow_array::ArrayRef,
                Arc::new(embedding_array) as arrow_array::ArrayRef,
            ],
        )
        .map_err(|e| DbError::LanceDb(e.to_string()))?;

        // Create iterator for IntoArrow trait
        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(Self::schema()));

        self.table
            .add(Box::new(batches))
            .execute()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?;

        Ok(())
    }

    /// Delete an embedding by memory ID.
    pub async fn delete(&self, memory_id: &str) -> Result<()> {
        Self::validate_memory_id(memory_id)?;
        let predicate = format!("id = '{}'", memory_id);
        self.table
            .delete(&predicate)
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?;

        Ok(())
    }

    /// Vector similarity search using cosine distance.
    /// Returns (memory_id, distance) pairs sorted by distance (ascending).
    pub async fn vector_search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if query_embedding.len() != EMBEDDING_DIM as usize {
            return Err(DbError::LanceDb(format!(
                "Query embedding dimension mismatch: expected {}, got {}",
                EMBEDDING_DIM,
                query_embedding.len()
            ))
            .into());
        }

        use lancedb::query::{ExecutableQuery, QueryBase};

        // Use query() API with nearest_to for vector search
        let results: Vec<arrow_array::RecordBatch> = self
            .table
            .query()
            .nearest_to(query_embedding)
            .map_err(|e| DbError::LanceDb(e.to_string()))?
            .limit(limit)
            .execute()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?
            .try_collect()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?;

        let mut matches = Vec::new();
        for batch in results {
            if let (Some(id_col), Some(dist_col)) = (
                batch.column_by_name("id"),
                batch.column_by_name("_distance"),
            ) {
                let ids: &arrow_array::StringArray = id_col.as_string::<i32>();
                let dists: &arrow_array::PrimitiveArray<Float32Type> = dist_col.as_primitive();

                for i in 0..ids.len() {
                    if ids.is_valid(i) && dists.is_valid(i) {
                        let id = ids.value(i).to_string();
                        let distance = dists.value(i);
                        matches.push((id, distance));
                    }
                }
            }
        }

        Ok(matches)
    }

    /// Find memories similar to a given memory by its embedding.
    /// Returns (memory_id, similarity) pairs where similarity = 1.0 - cosine_distance.
    /// Results exclude the source memory itself.
    pub async fn find_similar(
        &self,
        memory_id: &str,
        threshold: f32,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        Self::validate_memory_id(memory_id)?;
        // First, retrieve the embedding for this memory
        use lancedb::query::{ExecutableQuery, QueryBase};

        let rows: Vec<arrow_array::RecordBatch> = self
            .table
            .query()
            .only_if(format!("id = '{}'", memory_id))
            .execute()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?
            .try_collect()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?;

        // Extract the embedding from the first matching row
        let Some(batch) = rows.first() else {
            return Ok(Vec::new());
        };
        let Some(embedding_col) = batch.column_by_name("embedding") else {
            return Ok(Vec::new());
        };

        let list_array = embedding_col
            .as_any()
            .downcast_ref::<arrow_array::FixedSizeListArray>();
        let Some(list_array) = list_array else {
            return Ok(Vec::new());
        };
        if list_array.is_empty() {
            return Ok(Vec::new());
        }

        let values = list_array.value(0);
        let float_array = values.as_primitive::<Float32Type>();
        let embedding: Vec<f32> = float_array.values().to_vec();

        // Now search for similar embeddings, fetching extra to account for filtering
        let search_limit = limit + 1;
        let results = self.vector_search(&embedding, search_limit).await?;

        let mut similar = Vec::new();
        for (id, distance) in results {
            if id == memory_id {
                continue;
            }
            let similarity = 1.0 - distance;
            if similarity >= threshold {
                similar.push((id, similarity));
            }
        }
        similar.truncate(limit);

        Ok(similar)
    }

    /// Full-text search using Tantivy FTS.
    /// Returns (memory_id, score) pairs sorted by score (descending).
    pub async fn text_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        use lancedb::query::{ExecutableQuery, QueryBase};

        // Use full_text_search on the content column
        let results: Vec<arrow_array::RecordBatch> = self
            .table
            .query()
            .full_text_search(lance_index::scalar::FullTextSearchQuery::new(
                query.to_string(),
            ))
            .select(lancedb::query::Select::columns(&["id", "_score"]))
            .limit(limit)
            .execute()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?
            .try_collect()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?;

        let mut matches = Vec::new();
        for batch in results {
            if let (Some(id_col), Some(score_col)) =
                (batch.column_by_name("id"), batch.column_by_name("_score"))
            {
                let ids: &arrow_array::StringArray = id_col.as_string::<i32>();
                let scores: &arrow_array::PrimitiveArray<Float32Type> = score_col.as_primitive();

                for i in 0..ids.len() {
                    if ids.is_valid(i) && scores.is_valid(i) {
                        let id = ids.value(i).to_string();
                        let score = scores.value(i);
                        matches.push((id, score));
                    }
                }
            }
        }

        Ok(matches)
    }

    /// Ensure vector and FTS indexes exist, creating them only if they don't already exist.
    ///
    /// This prevents the expensive HNSW index training from running on every startup.
    /// Uses `list_indices()` to check for existing indexes BEFORE attempting creation.
    ///
    /// # Problem this solves
    ///
    /// LanceDB's `create_index()` unconditionally triggers a full rebuild when called,
    /// regardless of whether an index already exists on disk. The previous approach of
    /// catching errors after the fact was too late — the expensive KMeans training had
    /// already completed.
    ///
    /// # Solution
    ///
    /// Check for existing indexes using `list_indices()` before calling `create_index()`.
    /// Only create if no index exists on the target column.
    pub async fn ensure_indexes_exist(&self) -> Result<()> {
        use lancedb::index::Index;

        // Check for existing indexes
        let indices = self
            .table
            .list_indices()
            .await
            .map_err(|e| DbError::LanceDb(e.to_string()))?;

        // Check vector index on embedding column
        let has_vector_index = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|col| col == "embedding"));

        if !has_vector_index {
            tracing::info!("Creating HNSW vector index on embedding column");
            self.table
                .create_index(&["embedding"], Index::Auto)
                .execute()
                .await
                .map_err(|e| DbError::LanceDb(format!("Failed to create vector index: {}", e)))?;
            tracing::info!("Vector index created successfully");
        } else {
            tracing::debug!("Vector index already exists, skipping creation");
        }

        // Check FTS index on content column
        let has_fts_index = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|col| col == "content"));

        if !has_fts_index {
            tracing::info!("Creating FTS index on content column");
            self.table
                .create_index(&["content"], Index::FTS(Default::default()))
                .execute()
                .await
                .map_err(|e| DbError::LanceDb(format!("Failed to create FTS index: {}", e)))?;
            tracing::info!("FTS index created successfully");
        } else {
            tracing::debug!("FTS index already exists, skipping creation");
        }

        Ok(())
    }

    /// Optimize indexes for incremental updates after data insertion.
    ///
    /// This is much faster than a full rebuild and should be called after
    /// significant data changes to maintain query performance.
    pub async fn optimize_indexes(&self) -> Result<()> {
        use lancedb::table::{OptimizeAction, OptimizeOptions};

        tracing::debug!("Optimizing indexes (incremental update)");
        self.table
            .optimize(OptimizeAction::Index(OptimizeOptions::default()))
            .await
            .map_err(|e| DbError::LanceDb(format!("Failed to optimize indexes: {}", e)))?;
        tracing::debug!("Index optimization complete");

        Ok(())
    }

    /// Get the Arrow schema for the embeddings table.
    fn schema() -> arrow_schema::Schema {
        arrow_schema::Schema::new(vec![
            arrow_schema::Field::new("id", arrow_schema::DataType::Utf8, false),
            arrow_schema::Field::new("content", arrow_schema::DataType::Utf8, false),
            arrow_schema::Field::new(
                "embedding",
                arrow_schema::DataType::FixedSizeList(
                    Arc::new(arrow_schema::Field::new(
                        "item",
                        arrow_schema::DataType::Float32,
                        true,
                    )),
                    EMBEDDING_DIM,
                ),
                false,
            ),
        ])
    }

    /// Validate that a memory ID is a well-formed UUID to prevent predicate injection.
    fn validate_memory_id(memory_id: &str) -> Result<()> {
        if memory_id.len() != 36 || !memory_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(
                DbError::LanceDb(format!("invalid memory ID format: {}", memory_id)).into(),
            );
        }
        Ok(())
    }
}
