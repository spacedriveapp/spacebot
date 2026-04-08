//! Phase 9b (Wave 6): Vector embeddings for code symbols.
//!
//! Generates semantic embeddings for Function, Method, and Class nodes
//! using the shared `fastembed` model, then persists them in the
//! project-scoped `CodeEmbeddingTable` so that `hybrid_search` can later
//! do cosine-similarity matching alongside BM25.
//!
//! The snippet that goes into each embedding is the raw source text
//! between `line_start` and `line_end` inclusive — cheap to compute and
//! gives the model enough structure to build a reasonable representation
//! of the symbol.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::embeddings_table::{CodeEmbeddingRow, CodeEmbeddingTable};
use crate::memory::EmbeddingModel;

/// Maximum number of source lines we feed to the embedding model for a
/// single symbol. Keeps the per-row latency bounded on huge functions
/// without truncating meaningful signal for typical code.
const MAX_SNIPPET_LINES: usize = 80;

/// Number of texts per `EmbeddingModel::embed` call. Tuned low so the
/// pipeline doesn't monopolize the fastembed blocking thread for more
/// than a second or two per batch.
const BATCH_SIZE: usize = 32;

/// Result summary returned to the caller.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddingStats {
    pub embedded: u64,
    pub skipped: u64,
}

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Generate and persist embeddings for every eligible symbol in the
/// project.
pub async fn generate_embeddings(
    project_id: &str,
    root_path: &Path,
    db: &SharedCodeGraphDb,
    embedder: &Arc<EmbeddingModel>,
) -> Result<EmbeddingStats> {
    // 1. Figure out where the LanceDB table lives for this project.
    //    The DB's parent directory is `<base>/codegraph/<project_id>/`
    //    so we lift off the `lbug/` trailer to get the project dir.
    let project_dir = db
        .db_path
        .parent()
        .context("db_path has no parent")?
        .to_path_buf();

    let table = CodeEmbeddingTable::open(&project_dir).await?;

    // 2. Query all Function/Method/Class nodes.
    let pid = cypher_escape(project_id);
    let mut candidates: Vec<SymbolRow> = Vec::new();
    for label in &["Function", "Method", "Class"] {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN n.qualified_name, n.source_file, n.line_start, n.line_end"
            ))
            .await?;
        for row in &rows {
            let qname = match row.first() {
                Some(lbug::Value::String(s)) => s.clone(),
                _ => continue,
            };
            let source_file = match row.get(1) {
                Some(lbug::Value::String(s)) => s.clone(),
                _ => continue,
            };
            let line_start = cell_to_u32(row.get(2)).unwrap_or(0);
            let line_end = cell_to_u32(row.get(3)).unwrap_or(0);
            candidates.push(SymbolRow {
                qualified_name: qname,
                source_file,
                line_start,
                line_end,
            });
        }
    }

    if candidates.is_empty() {
        tracing::info!(project_id = %project_id, "no Function/Method/Class nodes to embed");
        return Ok(EmbeddingStats::default());
    }

    tracing::debug!(
        project_id = %project_id,
        candidates = candidates.len(),
        "starting embedding pass"
    );

    // 3. Batch by source file so we only read each file once.
    let mut by_file: HashMap<String, Vec<SymbolRow>> = HashMap::new();
    for row in candidates {
        by_file.entry(row.source_file.clone()).or_default().push(row);
    }

    let mut pending: Vec<(SymbolRow, String)> = Vec::new();
    let mut skipped = 0u64;

    for (source_file, syms) in by_file {
        let abs_path: PathBuf = root_path.join(&source_file);
        let content = match tokio::fs::read_to_string(&abs_path).await {
            Ok(c) => c,
            Err(err) => {
                tracing::debug!(
                    file = %abs_path.display(),
                    %err,
                    "skipping embedding for unreadable file"
                );
                skipped += syms.len() as u64;
                continue;
            }
        };
        let lines: Vec<&str> = content.lines().collect();

        for sym in syms {
            match snippet_for(&lines, sym.line_start, sym.line_end) {
                Some(snippet) => pending.push((sym, snippet)),
                None => skipped += 1,
            }
        }
    }

    if pending.is_empty() {
        tracing::info!(project_id = %project_id, skipped, "no embeddable symbols remained");
        return Ok(EmbeddingStats {
            embedded: 0,
            skipped,
        });
    }

    // 4. Batch-embed via fastembed (CPU bound → spawn_blocking) and
    //    persist each batch immediately so partial failures don't cost
    //    the whole pass.
    let mut embedded = 0u64;

    for chunk in pending.chunks(BATCH_SIZE) {
        let texts: Vec<String> = chunk.iter().map(|(_, s)| s.clone()).collect();

        let model = embedder.clone();
        let vectors = tokio::task::spawn_blocking(move || model.embed(texts))
            .await
            .context("embedding task panicked")?
            .context("embedding batch failed")?;

        if vectors.len() != chunk.len() {
            tracing::warn!(
                expected = chunk.len(),
                actual = vectors.len(),
                "embedding batch returned mismatched row count — skipping"
            );
            skipped += chunk.len() as u64;
            continue;
        }

        let rows: Vec<CodeEmbeddingRow> = chunk
            .iter()
            .zip(vectors.into_iter())
            .map(|((sym, snippet), embedding)| CodeEmbeddingRow {
                qualified_name: sym.qualified_name.clone(),
                source_file: sym.source_file.clone(),
                snippet: snippet.clone(),
                embedding,
            })
            .collect();

        if let Err(err) = table.store_batch(&rows).await {
            tracing::warn!(%err, rows = rows.len(), "failed to persist embedding batch");
            skipped += rows.len() as u64;
            continue;
        }

        embedded += rows.len() as u64;
    }

    tracing::info!(
        project_id = %project_id,
        embedded,
        skipped,
        "embedding pass complete"
    );

    Ok(EmbeddingStats { embedded, skipped })
}

/// Carry the minimum info we need to read the snippet and build the row.
#[derive(Debug, Clone)]
struct SymbolRow {
    qualified_name: String,
    source_file: String,
    line_start: u32,
    line_end: u32,
}

/// Slice a file's lines for a symbol. Returns `None` if the range is
/// obviously bogus (e.g. `line_start == 0`, past EOF). Caps at
/// `MAX_SNIPPET_LINES` to stay within the embedding model's budget.
fn snippet_for(lines: &[&str], line_start: u32, line_end: u32) -> Option<String> {
    if line_start == 0 {
        return None;
    }
    let start = (line_start as usize).saturating_sub(1);
    if start >= lines.len() {
        return None;
    }
    let end = (line_end as usize).min(lines.len());
    let end = end.max(start + 1);
    let clipped_end = end.min(start + MAX_SNIPPET_LINES);
    Some(lines[start..clipped_end].join("\n"))
}

fn cell_to_u32(cell: Option<&lbug::Value>) -> Option<u32> {
    match cell? {
        lbug::Value::Int64(n) => Some(*n as u32),
        lbug::Value::Int32(n) => Some(*n as u32),
        lbug::Value::Int16(n) => Some(*n as u32),
        _ => None,
    }
}
