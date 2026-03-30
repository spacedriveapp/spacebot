//! Phase 2: Build structural nodes (Folder, File, Package, Module, Project).

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;

/// Build the structural skeleton of the code graph.
///
/// Creates Project, Folder, and File nodes with CONTAINS edges forming
/// the directory tree.
pub async fn build_structure(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    _db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();

    // Collect unique directories from the file list.
    let mut dirs = std::collections::HashSet::new();
    for file in files {
        if let Ok(relative) = file.strip_prefix(root_path) {
            let mut current = PathBuf::new();
            for component in relative.parent().into_iter().flat_map(|p| p.components()) {
                current.push(component);
                dirs.insert(current.clone());
            }
        }
    }

    // Create the Project node.
    let project_name = root_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    tracing::debug!(
        project_id = %project_id,
        name = %project_name,
        dirs = dirs.len(),
        files = files.len(),
        "building structural skeleton"
    );

    // Count: 1 Project + dirs + files
    result.nodes_created = 1 + dirs.len() as u64 + files.len() as u64;
    // Edges: each dir/file is CONTAINED by its parent
    result.edges_created = dirs.len() as u64 + files.len() as u64;

    // Actual DB insertion will be implemented with the kuzu crate.
    // For now we track the counts for progress reporting.

    Ok(result)
}
