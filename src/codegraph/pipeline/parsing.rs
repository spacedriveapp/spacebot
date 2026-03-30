//! Phase 3: tree-sitter AST parse, extract symbol nodes.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::languages;
use crate::codegraph::types::CodeGraphConfig;

/// Parse all source files with tree-sitter and extract symbol nodes.
///
/// For each file, determines the language, parses the AST, and extracts
/// Class, Function, Method, Variable, Interface, Enum, etc. nodes with
/// DEFINES and CONTAINS edges.
pub async fn parse_files(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    _db: &SharedCodeGraphDb,
    _config: &CodeGraphConfig,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();

    for file_path in files {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let provider = match languages::provider_for_extension(ext) {
            Some(p) => p,
            None => continue,
        };

        // Read the file content.
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

        let relative = file_path
            .strip_prefix(root_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        // Extract symbols using the language provider.
        let symbols = provider.extract_symbols(&relative, &content);

        tracing::trace!(
            file = %relative,
            lang = %provider.language(),
            symbols = symbols.len(),
            "parsed file"
        );

        // Each symbol is a node with a DEFINES edge from its file.
        result.nodes_created += symbols.len() as u64;
        result.edges_created += symbols.len() as u64;
        result.files_parsed += 1;

        // Actual DB insertion will happen when kuzu is integrated.
        // For now we have the parsed symbols in memory and track counts.
    }

    tracing::debug!(
        project_id = %project_id,
        files_parsed = result.files_parsed,
        symbols = result.nodes_created,
        errors = result.errors,
        "AST parsing complete"
    );

    Ok(result)
}
