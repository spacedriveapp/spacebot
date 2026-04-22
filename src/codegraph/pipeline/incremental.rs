//! Incremental re-indexing pipeline.
//!
//! Given a set of changed files from the watcher, surgically rebuild the
//! graph for just those files (and their direct dependents) instead of
//! purging and re-running the full pipeline.
//!
//! Execution order:
//! 1. Classify changed files → `still_exist` vs `deleted`.
//! 2. Query files that currently import any of the changed files → `dependents`.
//! 3. Purge all symbol + File + Import nodes sourced from changed files.
//!    `DETACH DELETE` cascades CALLS/DEFINES/HAS_METHOD/IMPORTS edges.
//! 4. Drop outgoing IMPORTS and CALLS edges from dependent files — their
//!    targets may have been deleted or their shape may have changed.
//! 5. Rebuild File + CONTAINS edges for surviving changed files.
//! 6. Re-run parsing for surviving changed files.
//! 7. Re-resolve imports for the union of (changed ∪ dependents) files.
//! 8. Re-resolve calls for the union of (changed ∪ dependents) files.
//! 9. Phases 6–8 (heritage / communities / processes) are **skipped**
//!    unless the change set exceeds `re_index_threshold` percent — in
//!    that case the caller should fall back to a full re-index.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::sync::broadcast;

use super::{calls, imports, parsing, PhaseResult};
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::events::CodeGraphEvent;
use crate::codegraph::lang;
use crate::codegraph::schema::ALL_NODE_LABELS;
use crate::codegraph::types::{CodeGraphConfig, PipelineStats};

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Normalize a path to always use forward slashes (cross-platform).
fn normalize_path(s: &str) -> String {
    s.replace('\\', "/")
}

/// Run the incremental pipeline for a set of changed files.
///
/// Returns `PipelineStats` summarizing the work done. The caller decides
/// whether the change count warrants a full re-index instead (see the
/// `should_full_reindex` helper).
pub async fn run_incremental_pipeline(
    project_id: &str,
    root_path: &Path,
    changed_files: Vec<PathBuf>,
    db: &SharedCodeGraphDb,
    config: &CodeGraphConfig,
    event_tx: &broadcast::Sender<CodeGraphEvent>,
) -> Result<PipelineStats> {
    let mut stats = PipelineStats::default();
    let pid = cypher_escape(project_id);

    // ── Step 1: classify changed files ───────────────────────────────────
    let mut still_exist: Vec<PathBuf> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut changed_rel: HashSet<String> = HashSet::new();

    for file in &changed_files {
        let rel = normalize_path(
            &file
                .strip_prefix(root_path)
                .unwrap_or(file)
                .to_string_lossy(),
        );
        changed_rel.insert(rel.clone());

        if file.exists() {
            still_exist.push(file.clone());
        } else {
            deleted.push(rel);
        }
    }

    tracing::info!(
        project_id = %project_id,
        changed = changed_files.len(),
        still_exist = still_exist.len(),
        deleted = deleted.len(),
        "starting incremental pipeline"
    );

    // ── Step 2: find dependent files (files that import changed files) ──
    let dependents = find_dependent_files(db, &pid, &changed_rel).await?;

    tracing::debug!(
        project_id = %project_id,
        dependents = dependents.len(),
        "resolved dependent files for incremental update"
    );

    // ── Step 3: purge nodes owned by changed files ──────────────────────
    let purge_result = purge_files(db, &pid, &changed_rel).await?;
    stats.nodes_created = stats.nodes_created.saturating_sub(purge_result);

    // ── Step 4: drop outgoing IMPORTS/CALLS edges from dependents ───────
    drop_dependent_edges(db, &pid, &dependents).await?;

    // ── Step 5: rebuild File + CONTAINS for surviving changed files ─────
    let structure_result = rebuild_structure_for_files(db, &pid, root_path, &still_exist).await?;
    stats.nodes_created += structure_result.nodes_created;
    stats.edges_created += structure_result.edges_created;
    stats.errors += structure_result.errors;

    // ── Step 6: re-parse surviving changed files ────────────────────────
    if !still_exist.is_empty() {
        let parse_result =
            parsing::parse_files(project_id, root_path, &still_exist, db, config, None).await?;
        stats.files_parsed += parse_result.files_parsed;
        stats.files_skipped += parse_result.files_skipped;
        stats.nodes_created += parse_result.nodes_created;
        stats.edges_created += parse_result.edges_created;
        stats.errors += parse_result.errors;
    }

    // ── Step 7: re-resolve imports (scoped to changed ∪ dependents) ─────
    // Only process Import nodes whose source_file is in the affected scope
    // so we don't duplicate edges that belong to unchanged files.
    let mut scope_set: HashSet<String> = changed_rel
        .iter()
        .filter(|rel| !deleted.contains(rel))
        .cloned()
        .collect();
    for dep in &dependents {
        scope_set.insert(dep.clone());
    }

    // On the incremental path we don't carry the ConfigContext from
    // the full pipeline — walking the entire tree just to reload
    // manifests on every debounced change would cost more than the
    // incremental run saves. Aliased imports (tsconfig paths, PSR-4,
    // etc.) therefore fall through to the tail heuristics on
    // incremental; a manifest edit triggers a full re-index via the
    // `re_index_threshold` path and the aliases kick back in there.
    let empty_config = crate::codegraph::config::ConfigContext::default();
    let import_result = imports::resolve_imports_scoped(
        project_id,
        db,
        &empty_config,
        Some(&scope_set),
    )
    .await?;
    stats.edges_created += import_result.phase.edges_created;
    stats.errors += import_result.phase.errors;
    // The scoped import_map only covers files we just rebuilt. The full
    // map still lives in the DB for other callers; that's fine because
    // the call-resolution scope below is limited to the same set.
    let import_map = import_result.import_map;

    // ── Step 8: re-resolve calls for (changed ∪ dependents) ─────────────
    let mut scope: Vec<PathBuf> = still_exist.clone();
    for dep_rel in &dependents {
        scope.push(root_path.join(dep_rel));
    }
    // Only keep scope entries whose extension resolves to a language provider.
    scope.retain(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|ext| lang::provider_for_extension(ext).is_some())
            .unwrap_or(false)
    });

    if !scope.is_empty() {
        let call_result =
            calls::resolve_calls(project_id, db, root_path, &scope, &import_map, None).await?;
        stats.edges_created += call_result.edges_created;
        stats.errors += call_result.errors;
    }

    // ── Fire GraphChanged event ─────────────────────────────────────────
    let changed_list: Vec<String> = changed_rel.into_iter().collect();
    let _ = event_tx.send(CodeGraphEvent::GraphChanged {
        project_id: project_id.to_string(),
        changed_files: changed_list,
        added_symbols: Vec::new(),
        removed_symbols: Vec::new(),
        changed_symbols: Vec::new(),
    });

    tracing::info!(
        project_id = %project_id,
        nodes = stats.nodes_created,
        edges = stats.edges_created,
        files_parsed = stats.files_parsed,
        errors = stats.errors,
        "incremental pipeline complete"
    );

    Ok(stats)
}

/// Decide whether an incremental update is safe or we should fall back to
/// a full re-index. Uses `config.re_index_threshold` as a percentage of
/// `total_indexed_files`.
pub fn should_full_reindex(
    changed_files: usize,
    total_indexed_files: usize,
    config: &CodeGraphConfig,
) -> bool {
    if total_indexed_files == 0 {
        return true;
    }
    let pct = (changed_files as f64 / total_indexed_files as f64) * 100.0;
    pct > config.re_index_threshold as f64
}

/// Count the total number of File nodes for a project — used by the
/// caller to compute the change percentage for the threshold check.
pub async fn count_indexed_files(db: &SharedCodeGraphDb, project_id: &str) -> Result<usize> {
    let pid = cypher_escape(project_id);
    let rows = db
        .query(&format!(
            "MATCH (f:File) WHERE f.project_id = '{pid}' RETURN count(f)"
        ))
        .await?;

    let count = rows
        .first()
        .and_then(|row| row.first())
        .and_then(|v| match v {
            lbug::Value::Int64(n) => Some(*n as usize),
            lbug::Value::Int32(n) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(0);

    Ok(count)
}

/// Query the set of files that currently have an `IMPORTS` edge into any
/// of the changed files. These are "dependents" — their call targets may
/// have been deleted or their semantic meaning may have shifted.
async fn find_dependent_files(
    db: &SharedCodeGraphDb,
    pid: &str,
    changed_rel: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut dependents: HashSet<String> = HashSet::new();
    if changed_rel.is_empty() {
        return Ok(dependents);
    }

    let list = build_in_list(changed_rel);
    let rows = db
        .query(&format!(
            "MATCH (src:File)-[r:CodeRelation]->(tgt:File) \
             WHERE r.type = 'IMPORTS' AND src.project_id = '{pid}' \
             AND tgt.source_file IN {list} \
             RETURN src.source_file"
        ))
        .await?;

    for row in &rows {
        if let Some(lbug::Value::String(path)) = row.first() {
            // Only keep dependents that aren't themselves in the changed
            // set — those get rebuilt anyway.
            if !changed_rel.contains(path) {
                dependents.insert(path.clone());
            }
        }
    }

    Ok(dependents)
}

/// `DETACH DELETE` every symbol, File, and Import node whose
/// `source_file` is in the changed set. Returns a rough count of deleted
/// nodes (best-effort — LadybugDB doesn't report detach counts).
async fn purge_files(
    db: &SharedCodeGraphDb,
    pid: &str,
    changed_rel: &HashSet<String>,
) -> Result<u64> {
    if changed_rel.is_empty() {
        return Ok(0);
    }

    let list = build_in_list(changed_rel);
    let mut deleted = 0u64;

    // Skip Project, Community, Process — those are project-scoped, not
    // file-scoped. Folders don't have a source_file either. Everything
    // else (File + all symbol tables) gets purged by source_file.
    for label in ALL_NODE_LABELS {
        if matches!(*label, "Project" | "Folder" | "Community" | "Process") {
            continue;
        }
        let stmt = format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
             AND n.source_file IN {list} DETACH DELETE n"
        );
        if let Err(err) = db.execute(&stmt).await {
            tracing::debug!(%err, label, "purge query failed (likely empty)");
        } else {
            deleted += 1;
        }
    }

    Ok(deleted)
}

/// Delete the outgoing IMPORTS and CALLS edges from functions/methods
/// residing in the dependent files. They'll be recreated by the scoped
/// imports + calls phases below.
async fn drop_dependent_edges(
    db: &SharedCodeGraphDb,
    pid: &str,
    dependents: &HashSet<String>,
) -> Result<()> {
    if dependents.is_empty() {
        return Ok(());
    }

    let list = build_in_list(dependents);

    // Outgoing IMPORTS from dependent File nodes.
    let imports_stmt = format!(
        "MATCH (src:File)-[r:CodeRelation]->(tgt:File) \
         WHERE r.type = 'IMPORTS' AND src.project_id = '{pid}' \
         AND src.source_file IN {list} DELETE r"
    );
    if let Err(err) = db.execute(&imports_stmt).await {
        tracing::debug!(%err, "drop dependent IMPORTS failed");
    }

    // Outgoing CALLS from functions/methods in dependent files.
    for src_label in &["Function", "Method"] {
        for tgt_label in &["Function", "Method"] {
            let stmt = format!(
                "MATCH (a:{src_label})-[r:CodeRelation]->(b:{tgt_label}) \
                 WHERE r.type = 'CALLS' AND a.project_id = '{pid}' \
                 AND a.source_file IN {list} DELETE r"
            );
            if let Err(err) = db.execute(&stmt).await {
                tracing::debug!(%err, src_label, tgt_label, "drop dependent CALLS failed");
            }
        }
    }

    Ok(())
}

/// Rebuild the File node and its CONTAINS edge for each changed file.
/// Folder nodes are assumed to already exist (new directories are rare
/// in incremental updates). If a folder is missing we log and move on —
/// the CONTAINS edge will just be absent until the next full reindex.
async fn rebuild_structure_for_files(
    db: &SharedCodeGraphDb,
    pid: &str,
    root_path: &Path,
    files: &[PathBuf],
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    if files.is_empty() {
        return Ok(result);
    }

    // First pass: create File nodes.
    let mut node_stmts: Vec<String> = Vec::with_capacity(files.len());
    for file in files {
        let relative = match file.strip_prefix(root_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = cypher_escape(&normalize_path(&relative.to_string_lossy()));
        let file_name = cypher_escape(
            relative
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(""),
        );
        let qname = format!("{pid}::{rel_str}");

        node_stmts.push(format!(
            "CREATE (:File {{qualified_name: '{qname}', name: '{file_name}', \
             project_id: '{pid}', source_file: '{rel_str}', \
             line_start: 0, line_end: 0, source: 'pipeline', written_by: 'pipeline', \
             extends_type: '', import_source: ''}})",
        ));
    }

    if !node_stmts.is_empty() {
        let batch = db.execute_batch(node_stmts).await?;
        result.nodes_created += batch.success;
        result.errors += batch.errors;
    }

    // Second pass: CONTAINS edges from parent Folder (or Project if root).
    let mut edge_stmts: Vec<String> = Vec::with_capacity(files.len());
    for file in files {
        let relative = match file.strip_prefix(root_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = normalize_path(&relative.to_string_lossy());
        let file_qname = cypher_escape(&format!("{pid}::{rel_str}"));

        match relative.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                let parent_qname =
                    cypher_escape(&format!("{pid}::{}", normalize_path(&parent.to_string_lossy())));
                edge_stmts.push(format!(
                    "MATCH (p:Folder), (f:File) WHERE p.qualified_name = '{parent_qname}' \
                     AND f.qualified_name = '{file_qname}' \
                     CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'structural', step: 0}}]->(f)",
                ));
            }
            _ => {
                edge_stmts.push(format!(
                    "MATCH (p:Project), (f:File) WHERE p.project_id = '{pid}' \
                     AND f.qualified_name = '{file_qname}' \
                     CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'structural', step: 0}}]->(f)",
                ));
            }
        }
    }

    if !edge_stmts.is_empty() {
        let batch = db.execute_batch(edge_stmts).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;
    }

    Ok(result)
}

/// Render a `HashSet<String>` as a Cypher list literal like
/// `['a', 'b', 'c']`, escaping quotes and backslashes.
fn build_in_list(values: &HashSet<String>) -> String {
    let mut items: Vec<String> = values
        .iter()
        .map(|v| format!("'{}'", cypher_escape(v)))
        .collect();
    items.sort(); // deterministic for debugging
    format!("[{}]", items.join(", "))
}
