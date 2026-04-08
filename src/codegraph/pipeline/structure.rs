//! Phase 2: Build structural nodes (Folder, File, Package, Module, Project).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Normalize a path to always use forward slashes (cross-platform).
fn normalize_path(s: &str) -> String {
    s.replace('\\', "/")
}

/// Build the structural skeleton of the code graph.
///
/// Creates Project, Folder, and File nodes with CONTAINS edges forming
/// the directory tree.
pub async fn build_structure(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    db: &SharedCodeGraphDb,
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

    // ── Create Project node ─────────────────────────────────────────────
    let root_str = cypher_escape(&root_path.to_string_lossy());
    let pid = cypher_escape(project_id);
    let pname = cypher_escape(project_name);

    let project_stmt = format!(
        "CREATE (p:Project {{qualified_name: '{pid}', name: '{pname}', \
         project_id: '{pid}', source: 'pipeline', root_path: '{root_str}'}}) \
         RETURN p.id",
    );

    db.execute(&project_stmt).await?;
    result.nodes_created += 1;

    // ── Create Folder nodes ─────────────────────────────────────────────
    let mut sorted_dirs: Vec<PathBuf> = dirs.into_iter().collect();
    sorted_dirs.sort();

    let mut stmts: Vec<String> = Vec::with_capacity(sorted_dirs.len());
    for dir in &sorted_dirs {
        let dir_str = cypher_escape(&normalize_path(&dir.to_string_lossy()));
        let dir_name = cypher_escape(
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(""),
        );
        let qname = format!("{pid}::{dir_str}");

        stmts.push(format!(
            "CREATE (:Folder {{qualified_name: '{qname}', name: '{dir_name}', \
             project_id: '{pid}', source_file: '{dir_str}', \
             line_start: 0, line_end: 0, source: 'pipeline', written_by: 'pipeline', \
             extends_type: '', import_source: ''}})",
        ));
    }

    if !stmts.is_empty() {
        let batch = db.execute_batch(stmts).await?;
        result.nodes_created += batch.success;
        result.errors += batch.errors;
    }

    // ── Create File nodes ───────────────────────────────────────────────
    let mut stmts: Vec<String> = Vec::with_capacity(files.len());
    for file in files {
        if let Ok(relative) = file.strip_prefix(root_path) {
            let rel_str = cypher_escape(&normalize_path(&relative.to_string_lossy()));
            let file_name = cypher_escape(
                relative
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(""),
            );
            let qname = format!("{pid}::{rel_str}");

            stmts.push(format!(
                "CREATE (:File {{qualified_name: '{qname}', name: '{file_name}', \
                 project_id: '{pid}', source_file: '{rel_str}', \
                 line_start: 0, line_end: 0, source: 'pipeline', written_by: 'pipeline', \
                 extends_type: '', import_source: ''}})",
            ));
        }
    }

    if !stmts.is_empty() {
        let batch = db.execute_batch(stmts).await?;
        result.nodes_created += batch.success;
        result.errors += batch.errors;
    }

    // ── Create CONTAINS edges ───────────────────────────────────────────
    let mut edge_stmts: Vec<String> = Vec::new();

    // Build a map of dir path -> qualified_name for lookups.
    let mut dir_qnames: HashMap<String, String> = HashMap::new();
    for dir in &sorted_dirs {
        let dir_str = normalize_path(&dir.to_string_lossy());
        dir_qnames.insert(dir_str.clone(), format!("{pid}::{}", cypher_escape(&dir_str)));
    }

    // Folder -> child Folder edges.
    for dir in &sorted_dirs {
        let child_qname = cypher_escape(&format!(
            "{pid}::{}",
            normalize_path(&dir.to_string_lossy())
        ));

        if let Some(parent) = dir.parent() {
            if parent.as_os_str().is_empty() {
                edge_stmts.push(format!(
                    "MATCH (p:Project), (f:Folder) WHERE p.project_id = '{pid}' \
                     AND f.qualified_name = '{child_qname}' \
                     CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'structural', step: 0}}]->(f)",
                ));
            } else {
                let parent_qname = cypher_escape(&format!(
                    "{pid}::{}",
                    normalize_path(&parent.to_string_lossy())
                ));
                edge_stmts.push(format!(
                    "MATCH (p:Folder), (c:Folder) WHERE p.qualified_name = '{parent_qname}' \
                     AND c.qualified_name = '{child_qname}' \
                     CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'structural', step: 0}}]->(c)",
                ));
            }
        }
    }

    // Folder/Project -> File edges.
    for file in files {
        if let Ok(relative) = file.strip_prefix(root_path) {
            let file_qname = cypher_escape(&format!(
                "{pid}::{}",
                normalize_path(&relative.to_string_lossy())
            ));

            if let Some(parent) = relative.parent() {
                if parent.as_os_str().is_empty() {
                    edge_stmts.push(format!(
                        "MATCH (p:Project), (f:File) WHERE p.project_id = '{pid}' \
                         AND f.qualified_name = '{file_qname}' \
                         CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'structural', step: 0}}]->(f)",
                    ));
                } else {
                    let parent_qname = cypher_escape(&format!(
                        "{pid}::{}",
                        normalize_path(&parent.to_string_lossy())
                    ));
                    edge_stmts.push(format!(
                        "MATCH (p:Folder), (f:File) WHERE p.qualified_name = '{parent_qname}' \
                         AND f.qualified_name = '{file_qname}' \
                         CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'structural', step: 0}}]->(f)",
                    ));
                }
            }
        }
    }

    if !edge_stmts.is_empty() {
        let batch = db.execute_batch(edge_stmts).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;
    }

    tracing::info!(
        project_id = %project_id,
        nodes = result.nodes_created,
        edges = result.edges_created,
        errors = result.errors,
        "structural skeleton complete"
    );

    Ok(result)
}
