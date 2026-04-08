//! LanceDB table for Wave 6 code-graph semantic embeddings.
//!
//! Each row stores the embedding for a single code symbol (Function,
//! Method, or Class). The table lives at
//! `<base>/codegraph/<project_id>/embeddings.lance` — one table per
//! project so we can delete it alongside the LadybugDB directory when a
//! project is removed.
//!
//! Schema:
//! - `qualified_name` : STRING  — primary key; matches the LadybugDB qname
//! - `source_file`    : STRING  — relative path for display
//! - `snippet`        : STRING  — raw code that was embedded (used by FTS)
//! - `embedding`      : FLOAT32[384] — fastembed all-MiniLM-L6-v2 output

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::cast::AsArray;
use arrow_array::types::Float32Type;
use arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use futures::TryStreamExt;

/// Table name inside the project-scoped LanceDB directory.
const TABLE_NAME: &str = "code_embeddings";

/// Dimension of the embeddings produced by `fastembed::TextEmbedding`
/// (`all-MiniLM-L6-v2`). Must stay in sync with `memory::lance`.
pub const EMBEDDING_DIM: i32 = 384;

/// Wrapper around the per-project LanceDB table for code symbol
/// embeddings.
pub struct CodeEmbeddingTable {
    table: lancedb::Table,
}

impl CodeEmbeddingTable {
    /// Open or create the embeddings table for a project.
    ///
    /// `project_dir` should be `<base>/codegraph/<project_id>/`. This
    /// function creates a `lance/` subdirectory inside it if needed.
    pub async fn open(project_dir: &Path) -> Result<Self> {
        let lance_dir = project_dir.join("lance");
        tokio::fs::create_dir_all(&lance_dir)
            .await
            .with_context(|| format!("creating lance dir at {}", lance_dir.display()))?;

        let uri = lance_dir.to_string_lossy().to_string();
        let connection = lancedb::connect(&uri)
            .execute()
            .await
            .context("connecting to lancedb for code embeddings")?;

        // Fast path: table already exists.
        match connection.open_table(TABLE_NAME).execute().await {
            Ok(table) => return Ok(Self { table }),
            Err(err) => {
                tracing::debug!(%err, "code embeddings table not found, creating fresh");
            }
        }

        // Slow path: create from an empty RecordBatch.
        let table = Self::create_empty(&connection).await?;
        Ok(Self { table })
    }

    async fn create_empty(connection: &lancedb::Connection) -> Result<lancedb::Table> {
        let schema = Self::schema();
        let batches = RecordBatchIterator::new(vec![].into_iter().map(Ok), Arc::new(schema));

        connection
            .create_table(TABLE_NAME, Box::new(batches))
            .execute()
            .await
            .context("creating code embeddings table")
    }

    /// Insert a batch of symbol embeddings.
    ///
    /// Callers are expected to chunk their workload so a single call
    /// stays under a few thousand rows.
    pub async fn store_batch(&self, rows: &[CodeEmbeddingRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        for row in rows {
            if row.embedding.len() != EMBEDDING_DIM as usize {
                anyhow::bail!(
                    "embedding dimension mismatch: expected {}, got {} for {}",
                    EMBEDDING_DIM,
                    row.embedding.len(),
                    row.qualified_name
                );
            }
        }

        let qnames: Vec<&str> = rows.iter().map(|r| r.qualified_name.as_str()).collect();
        let files: Vec<&str> = rows.iter().map(|r| r.source_file.as_str()).collect();
        let snippets: Vec<&str> = rows.iter().map(|r| r.snippet.as_str()).collect();

        let qname_array = StringArray::from(qnames);
        let file_array = StringArray::from(files);
        let snippet_array = StringArray::from(snippets);

        let embedding_array =
            arrow_array::FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                rows.iter()
                    .map(|r| Some(r.embedding.iter().map(|v| Some(*v)).collect::<Vec<_>>())),
                EMBEDDING_DIM,
            );

        let batch = RecordBatch::try_new(
            Arc::new(Self::schema()),
            vec![
                Arc::new(qname_array) as arrow_array::ArrayRef,
                Arc::new(file_array) as arrow_array::ArrayRef,
                Arc::new(snippet_array) as arrow_array::ArrayRef,
                Arc::new(embedding_array) as arrow_array::ArrayRef,
            ],
        )
        .context("building code embedding record batch")?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], Arc::new(Self::schema()));

        self.table
            .add(Box::new(batches))
            .execute()
            .await
            .context("inserting code embedding batch")?;

        Ok(())
    }

    /// Delete all rows whose `source_file` matches one of the given
    /// relative paths. Used by the incremental pipeline when a changed
    /// file's symbols get re-embedded.
    pub async fn delete_by_source_files(&self, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        // LanceDB accepts SQL-style predicates; escape single quotes.
        let list = files
            .iter()
            .map(|f| format!("'{}'", f.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        let predicate = format!("source_file IN ({list})");

        self.table
            .delete(&predicate)
            .await
            .context("deleting code embeddings by source file")?;
        Ok(())
    }

    /// Nearest-neighbour vector search. Returns
    /// `(qualified_name, source_file, snippet, distance)` tuples sorted
    /// ascending by cosine distance.
    pub async fn vector_search(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, String, String, f32)>> {
        if query.len() != EMBEDDING_DIM as usize {
            anyhow::bail!(
                "query embedding dimension mismatch: expected {}, got {}",
                EMBEDDING_DIM,
                query.len()
            );
        }

        use lancedb::query::{ExecutableQuery, QueryBase};

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .nearest_to(query)
            .context("nearest_to query build failed")?
            .limit(limit)
            .execute()
            .await
            .context("executing code embedding vector search")?
            .try_collect()
            .await
            .context("collecting code embedding vector search results")?;

        let mut results = Vec::new();
        for batch in batches {
            let qname_col = batch.column_by_name("qualified_name");
            let file_col = batch.column_by_name("source_file");
            let snippet_col = batch.column_by_name("snippet");
            let dist_col = batch.column_by_name("_distance");

            if let (Some(qn), Some(sf), Some(sn), Some(dist)) =
                (qname_col, file_col, snippet_col, dist_col)
            {
                let qnames: &StringArray = qn.as_string::<i32>();
                let files: &StringArray = sf.as_string::<i32>();
                let snippets: &StringArray = sn.as_string::<i32>();
                let dists: &arrow_array::PrimitiveArray<Float32Type> = dist.as_primitive();

                for i in 0..qnames.len() {
                    if qnames.is_valid(i) && dists.is_valid(i) {
                        results.push((
                            qnames.value(i).to_string(),
                            files.value(i).to_string(),
                            snippets.value(i).to_string(),
                            dists.value(i),
                        ));
                    }
                }
            }
        }

        Ok(results)
    }

    fn schema() -> arrow_schema::Schema {
        arrow_schema::Schema::new(vec![
            arrow_schema::Field::new("qualified_name", arrow_schema::DataType::Utf8, false),
            arrow_schema::Field::new("source_file", arrow_schema::DataType::Utf8, false),
            arrow_schema::Field::new("snippet", arrow_schema::DataType::Utf8, false),
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
}

/// A single row waiting to be inserted into the embeddings table.
#[derive(Debug, Clone)]
pub struct CodeEmbeddingRow {
    pub qualified_name: String,
    pub source_file: String,
    pub snippet: String,
    pub embedding: Vec<f32>,
}
