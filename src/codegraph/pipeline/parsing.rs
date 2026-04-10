//! tree-sitter AST parse, extract symbol nodes.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::lang;
use crate::codegraph::types::CodeGraphConfig;

/// Hard ceiling on file size for tree-sitter parsing. The walker's
/// 512 KB soft cap catches the common case, but this defends against
/// anything that slips through (e.g. when `SPACEBOT_NO_GITIGNORE` is
/// set). tree-sitter allocates a buffer proportional to file size, so
/// unbounded reads risk OOM.
const MAX_PARSE_BYTES: u64 = 32 * 1024 * 1024;

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Normalize a path to always use forward slashes (cross-platform).
fn normalize_path(s: &str) -> String {
    s.replace('\\', "/")
}

/// Parse all source files with tree-sitter and extract symbol nodes.
///
/// For each file, determines the language, parses the AST, and extracts
/// Class, Function, Method, Variable, Interface, Enum, etc. nodes with
/// DEFINES edges from their containing File node.
pub async fn parse_files(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    db: &SharedCodeGraphDb,
    _config: &CodeGraphConfig,
    progress_fn: Option<&super::ProgressFn>,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    // Accumulate ALL nodes first, then ALL edges. This guarantees nodes
    // exist in the DB before any edge MATCH queries reference them.
    const BATCH_SIZE: usize = 100;
    let mut node_stmts: Vec<String> = Vec::new();
    let mut edge_stmts: Vec<String> = Vec::new();
    let total_files = files.len();
    let report_interval = (total_files / 20).max(1);

    for (file_idx, file_path) in files.iter().enumerate() {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let provider = match lang::provider_for_extension(ext) {
            Some(p) => p,
            None => {
                result.files_skipped += 1;
                continue;
            }
        };

        // Defense-in-depth size ceiling — bail before loading the file
        // into RAM if it exceeds MAX_PARSE_BYTES.
        match tokio::fs::metadata(file_path).await {
            Ok(meta) if meta.len() > MAX_PARSE_BYTES => {
                tracing::warn!(
                    file = %file_path.display(),
                    size = meta.len(),
                    max = MAX_PARSE_BYTES,
                    "skipping file (exceeds parse size ceiling)"
                );
                result.files_skipped += 1;
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    file = %file_path.display(),
                    %err,
                    "skipping file (stat error)"
                );
                result.errors += 1;
                continue;
            }
        }

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    file = %file_path.display(),
                    %err,
                    "skipping file (read error)"
                );
                result.errors += 1;
                continue;
            }
        };

        let relative = normalize_path(
            &file_path
                .strip_prefix(root_path)
                .unwrap_or(file_path)
                .to_string_lossy(),
        );

        let symbols = provider.extract_symbols(&relative, &content);

        tracing::trace!(
            file = %relative,
            lang = %provider.language(),
            symbols = symbols.len(),
            "parsed file"
        );

        let rel_escaped = cypher_escape(&relative);
        let file_qname = format!("{pid}::{rel_escaped}");

        for sym in &symbols {
            let label = sym.label.as_str();
            let name = cypher_escape(&sym.name);
            let qname = cypher_escape(&sym.qualified_name);
            // Merge `extends` and `implements` into a single comma-
            // separated field. Heritage.rs splits on commas and uses the
            // target's label to decide EXTENDS vs IMPLEMENTS, so the two
            // sources of parent names are interchangeable here.
            let mut heritage_parts: Vec<&str> = Vec::new();
            if let Some(ref ext) = sym.extends
                && !ext.is_empty()
            {
                heritage_parts.push(ext);
            }
            for imp in &sym.implements {
                if !imp.is_empty() {
                    heritage_parts.push(imp);
                }
            }
            let extends_val = cypher_escape(&heritage_parts.join(", "));
            let import_src = cypher_escape(sym.import_source.as_deref().unwrap_or(""));
            // Type text stashed in metadata by the language provider
            // (typed params / typed fields). Empty string when the
            // symbol doesn't carry a declared type.
            let declared_type = cypher_escape(
                sym.metadata
                    .get("declared_type")
                    .map(String::as_str)
                    .unwrap_or(""),
            );

            node_stmts.push(format!(
                "CREATE (:{label} {{qualified_name: '{qname}', name: '{name}', \
                 project_id: '{pid}', source_file: '{rel_escaped}', \
                 line_start: {ls}, line_end: {le}, \
                 source: 'pipeline', written_by: 'pipeline', \
                 extends_type: '{extends_val}', import_source: '{import_src}', \
                 declared_type: '{declared_type}'}})",
                ls = sym.line_start,
                le = sym.line_end,
            ));

            edge_stmts.push(format!(
                "MATCH (f:File), (s:{label}) WHERE f.qualified_name = '{file_qname}' \
                 AND s.qualified_name = '{qname}' AND s.project_id = '{pid}' \
                 CREATE (f)-[:CodeRelation {{type: 'DEFINES', confidence: 1.0, reason: '', step: 0}}]->(s)",
            ));

            if let Some(ref parent_qn) = sym.parent {
                let parent_escaped = cypher_escape(parent_qn);
                let parent_label = symbols
                    .iter()
                    .find(|s| s.qualified_name == *parent_qn)
                    .map(|s| s.label.as_str())
                    .unwrap_or("Class");

                // Parameters get HAS_PARAMETER rather than the generic
                // CONTAINS so downstream queries can distinguish them.
                let edge_type = match sym.label {
                    crate::codegraph::types::NodeLabel::Method => "HAS_METHOD",
                    crate::codegraph::types::NodeLabel::Variable => "HAS_PROPERTY",
                    crate::codegraph::types::NodeLabel::Parameter => "HAS_PARAMETER",
                    _ => "CONTAINS",
                };

                edge_stmts.push(format!(
                    "MATCH (p:{parent_label}), (c:{label}) \
                     WHERE p.qualified_name = '{parent_escaped}' AND p.project_id = '{pid}' \
                     AND c.qualified_name = '{qname}' AND c.project_id = '{pid}' \
                     CREATE (p)-[:CodeRelation {{type: '{edge_type}', confidence: 1.0, reason: '', step: 0}}]->(c)",
                ));
            }
        }

        result.files_parsed += 1;

        // Flush nodes periodically to keep memory bounded, but NEVER flush
        // edges until all nodes in this batch are committed.
        if node_stmts.len() >= BATCH_SIZE {
            let batch = db.execute_batch(std::mem::take(&mut node_stmts)).await?;
            result.nodes_created += batch.success;
            result.errors += batch.errors;
        }

        // Report intermediate progress so the frontend can show live stats.
        if let Some(pf) = progress_fn
            && (file_idx + 1) % report_interval == 0
        {
            let pct = (file_idx + 1) as f32 / total_files as f32;
            pf(
                pct * 0.8,
                &format!("Parsing files ({}/{})", file_idx + 1, total_files),
                &result,
            );
        }
    }

    // Flush remaining nodes FIRST, then all edges.
    if !node_stmts.is_empty() {
        let batch = db.execute_batch(node_stmts).await?;
        result.nodes_created += batch.success;
        result.errors += batch.errors;
    }

    // Report that node parsing is done, starting edges.
    if let Some(pf) = progress_fn {
        pf(0.85, &format!("Creating symbol edges ({})", edge_stmts.len()), &result);
    }

    // Now all nodes exist — safe to create edges.
    let total_edge_chunks = (edge_stmts.len() + BATCH_SIZE - 1).max(1) / BATCH_SIZE.max(1);
    for (ci, chunk) in edge_stmts.chunks(BATCH_SIZE).enumerate() {
        let batch = db.execute_batch(chunk.to_vec()).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;

        if let Some(pf) = progress_fn {
            let edge_pct = (ci + 1) as f32 / total_edge_chunks as f32;
            pf(0.85 + edge_pct * 0.15, &format!("Creating edges ({})", result.edges_created), &result);
        }
    }

    tracing::info!(
        project_id = %project_id,
        files_parsed = result.files_parsed,
        nodes = result.nodes_created,
        edges = result.edges_created,
        errors = result.errors,
        "AST parsing complete"
    );

    Ok(result)
}
