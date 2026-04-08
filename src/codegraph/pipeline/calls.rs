//! Phase 5: AST-aware call resolution with tiered confidence scoring.
//!
//! Resolution tiers:
//! - Receiver-resolved method call: 0.92
//! - Tier 1 (same-file): 0.95
//! - Tier 2a (import-scoped): 0.90
//! - Tier 3 (project-wide unique): 0.70
//! - Tier 4 (project-wide multi-match): 0.40

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::lang;

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// A resolved symbol entry from the graph.
struct SymbolEntry {
    qualified_name: String,
    source_file: String,
    label: String,
}

/// Resolve function/method call-sites and create CALLS edges.
///
/// Uses AST-extracted call sites from language providers, then resolves
/// each call through multiple tiers of decreasing confidence.
pub async fn resolve_calls(
    project_id: &str,
    db: &SharedCodeGraphDb,
    root_path: &Path,
    files: &[PathBuf],
    import_map: &HashMap<String, HashSet<String>>,
    progress_fn: Option<&super::ProgressFn>,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    tracing::debug!(project_id = %project_id, "resolving call-sites (AST-aware)");

    // 1. Query all Function and Method nodes — build symbol table.
    let mut symbols_by_name: HashMap<String, Vec<SymbolEntry>> = HashMap::new();

    for (node_label, label_str) in &[("Function", "Function"), ("Method", "Method")] {
        let rows = db.query(&format!(
            "MATCH (n:{node_label}) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name, n.source_file"
        )).await?;

        for row in &rows {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(source_file)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                symbols_by_name.entry(name.clone()).or_default().push(SymbolEntry {
                    qualified_name: qname.clone(),
                    source_file: source_file.clone(),
                    label: label_str.to_string(),
                });
            }
        }
    }

    if symbols_by_name.is_empty() {
        tracing::info!(project_id = %project_id, "no callable symbols found, skipping call resolution");
        return Ok(result);
    }

    // 2. Query Class/Struct/Interface/Trait nodes for receiver resolution.
    let mut classes_by_name: HashMap<String, Vec<(String, String)>> = HashMap::new(); // name → [(qname, source_file)]
    let mut classes_by_qname: HashSet<String> = HashSet::new();

    for label in &["Class", "Struct", "Interface", "Trait"] {
        let rows = db.query(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name, n.source_file"
        )).await?;

        for row in &rows {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(sf)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                classes_by_name
                    .entry(name.clone())
                    .or_default()
                    .push((qname.clone(), sf.clone()));
                classes_by_qname.insert(qname.clone());
            }
        }
    }

    // Build a lookup for methods by (class_qname, method_name) → SymbolEntry.
    let mut methods_by_class: HashMap<String, SymbolEntry> = HashMap::new();
    for entries in symbols_by_name.values() {
        for entry in entries {
            if entry.label == "Method" {
                // Derive the parent class qname from method qname:
                // "file::Class::method" → parent = "file::Class"
                if let Some((parent, method_name)) = entry.qualified_name.rsplit_once("::")
                    && classes_by_qname.contains(parent)
                {
                    let key = format!("{parent}::{method_name}");
                    methods_by_class.entry(key).or_insert_with(|| SymbolEntry {
                        qualified_name: entry.qualified_name.clone(),
                        source_file: entry.source_file.clone(),
                        label: entry.label.clone(),
                    });
                }
            }
        }
    }

    // Build a lookup for class fields by (class_qname, field_name) → Variable qname.
    // Used by access resolution to turn `self.x` references into ACCESSES edges.
    let mut variables_by_class: HashMap<String, HashSet<String>> = HashMap::new();
    let var_rows = db
        .query(&format!(
            "MATCH (n:Variable) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name"
        ))
        .await?;
    for row in &var_rows {
        if let (Some(lbug::Value::String(qname)), Some(lbug::Value::String(name))) =
            (row.first(), row.get(1))
            && let Some((parent, _)) = qname.rsplit_once("::")
            && classes_by_qname.contains(parent)
        {
            variables_by_class
                .entry(parent.to_string())
                .or_default()
                .insert(name.clone());
        }
    }

    // 3. Extract call sites from each file using AST analysis, then resolve.
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    let total_files = files.len();
    let report_interval = (total_files / 20).max(1);

    for (file_idx, file_path) in files.iter().enumerate() {
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let provider = match lang::provider_for_extension(ext) {
            Some(p) => p,
            None => continue,
        };

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    file = %file_path.display(),
                    %err,
                    "skipping file in call resolution (read error)"
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

        let imported_files = import_map.get(&relative);
        let call_sites = provider.extract_calls(&relative, &content);
        let access_sites = provider.extract_accesses(&relative, &content);

        // --- Resolve self/this field accesses → ACCESSES edges ---
        for site in &access_sites {
            if site.receiver != "self" && site.receiver != "this" {
                continue; // Only self/this resolution this round.
            }
            let class_qn =
                match find_enclosing_class(&site.caller_qualified_name, &classes_by_qname) {
                    Some(c) => c,
                    None => continue,
                };
            let fields = match variables_by_class.get(class_qn) {
                Some(f) => f,
                None => continue,
            };
            if !fields.contains(&site.field_name) {
                continue;
            }
            let target_qname = format!("{class_qn}::{}", site.field_name);
            // Use a distinct key prefix so ACCESSES dedup doesn't collide
            // with the CALLS edge dedup set.
            let edge_key = format!("ACC:{}->{}", site.caller_qualified_name, target_qname);
            if seen_edges.insert(edge_key) {
                push_access_edge(
                    &mut edge_stmts,
                    &site.caller_qualified_name,
                    &target_qname,
                    &pid,
                );
            }
        }

        for site in &call_sites {
            // --- Receiver-resolved method call (0.92) ---
            if site.is_method_call
                && let Some(recv) = &site.receiver
            {
                let resolved_class_qname = if recv == "self" || recv == "this" {
                    // Enclosing class: strip last segment from caller qname,
                    // walk up until we find a known class.
                    find_enclosing_class(&site.caller_qualified_name, &classes_by_qname)
                } else {
                    // Receiver is a direct class/struct name reference
                    classes_by_name.get(recv.as_str()).and_then(|entries| {
                        // Prefer same-file match
                        entries
                            .iter()
                            .find(|(_, sf)| *sf == relative)
                            .or_else(|| entries.first())
                            .map(|(qn, _)| qn.as_str())
                    })
                };

                if let Some(class_qn) = resolved_class_qname {
                    let method_key = format!("{class_qn}::{}", site.callee_name);
                    if let Some(target) = methods_by_class.get(&method_key) {
                        let edge_key = format!("{}->{}",  site.caller_qualified_name, target.qualified_name);
                        if seen_edges.insert(edge_key) {
                            push_edge(
                                &mut edge_stmts,
                                &site.caller_qualified_name,
                                &target.qualified_name,
                                &target.label,
                                &pid,
                                0.92,
                                "receiver-resolved",
                            );
                        }
                        continue;
                    }
                }
            }

            // --- Name-based tiered resolution ---
            let entries = match symbols_by_name.get(&site.callee_name) {
                Some(e) => e,
                None => continue,
            };

            // Tier 1: same-file (0.95)
            if let Some(target) = entries.iter().find(|e| e.source_file == relative) {
                let edge_key = format!("{}->{}",  site.caller_qualified_name, target.qualified_name);
                if seen_edges.insert(edge_key) {
                    push_edge(
                        &mut edge_stmts,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                        0.95,
                        "same-file",
                    );
                }
                continue;
            }

            // Tier 2a: import-scoped (0.90)
            if let Some(imported) = imported_files
                && let Some(target) = entries.iter().find(|e| imported.contains(&e.source_file))
            {
                let edge_key = format!("{}->{}",  site.caller_qualified_name, target.qualified_name);
                if seen_edges.insert(edge_key) {
                    push_edge(
                        &mut edge_stmts,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                        0.90,
                        "import-scoped",
                    );
                }
                continue;
            }

            // Tier 3: project-wide unique match (0.70)
            if entries.len() == 1 {
                let target = &entries[0];
                let edge_key = format!("{}->{}",  site.caller_qualified_name, target.qualified_name);
                if seen_edges.insert(edge_key) {
                    push_edge(
                        &mut edge_stmts,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                        0.70,
                        "project-unique",
                    );
                }
                continue;
            }

            // Tier 4: project-wide multi-match (0.40) — create edges to all candidates
            for target in entries {
                let edge_key = format!("{}->{}",  site.caller_qualified_name, target.qualified_name);
                if seen_edges.insert(edge_key) {
                    push_edge(
                        &mut edge_stmts,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                        0.40,
                        "project-multi",
                    );
                }
            }
        }

        // Report intermediate progress.
        if let Some(pf) = progress_fn
            && (file_idx + 1) % report_interval == 0
        {
            let pct = (file_idx + 1) as f32 / total_files as f32;
            pf(
                pct * 0.8,
                &format!("Scanning calls ({}/{})", file_idx + 1, total_files),
                &result,
            );
        }
    }

    // 4. Execute edge batch.
    const BATCH_SIZE: usize = 100;
    let total_edge_chunks = (edge_stmts.len() + BATCH_SIZE - 1).max(1) / BATCH_SIZE.max(1);
    for (ci, chunk) in edge_stmts.chunks(BATCH_SIZE).enumerate() {
        let batch = db.execute_batch(chunk.to_vec()).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;

        if let Some(pf) = progress_fn {
            let edge_pct = (ci + 1) as f32 / total_edge_chunks as f32;
            pf(0.8 + edge_pct * 0.2, &format!("Creating call edges ({})", result.edges_created), &result);
        }
    }

    tracing::info!(
        project_id = %project_id,
        edges = result.edges_created,
        errors = result.errors,
        "call resolution complete"
    );

    Ok(result)
}

/// Walk up the qualified name chain to find an enclosing class/struct/trait.
fn find_enclosing_class<'a>(
    caller_qn: &'a str,
    classes: &'a HashSet<String>,
) -> Option<&'a str> {
    let mut qn = caller_qn;
    while let Some((parent, _)) = qn.rsplit_once("::") {
        if classes.contains(parent) {
            return Some(classes.get(parent).unwrap().as_str());
        }
        qn = parent;
    }
    None
}

/// Build and push a CALLS edge Cypher statement.
fn push_edge(
    stmts: &mut Vec<String>,
    caller_qn: &str,
    target_qn: &str,
    target_label: &str,
    pid: &str,
    confidence: f64,
    reason: &str,
) {
    let src_escaped = cypher_escape(caller_qn);
    let tgt_escaped = cypher_escape(target_qn);

    // Try both Function and Method labels for the caller since we may not know which it is.
    for src_label in &["Function", "Method"] {
        stmts.push(format!(
            "MATCH (a:{src_label}), (b:{target_label}) \
             WHERE a.qualified_name = '{src_escaped}' AND a.project_id = '{pid}' \
             AND b.qualified_name = '{tgt_escaped}' AND b.project_id = '{pid}' \
             CREATE (a)-[:CodeRelation {{type: 'CALLS', confidence: {confidence}, reason: '{reason}', step: 0}}]->(b)",
        ));
    }
}

/// Build and push an ACCESSES edge Cypher statement (callable → Variable).
///
/// The receiver is always `self` / `this` at the moment, so the resolved
/// field is unambiguous and the confidence is fixed at 0.92 to match the
/// receiver-resolved CALLS tier.
fn push_access_edge(stmts: &mut Vec<String>, caller_qn: &str, target_qn: &str, pid: &str) {
    let src_escaped = cypher_escape(caller_qn);
    let tgt_escaped = cypher_escape(target_qn);

    for src_label in &["Function", "Method"] {
        stmts.push(format!(
            "MATCH (a:{src_label}), (b:Variable) \
             WHERE a.qualified_name = '{src_escaped}' AND a.project_id = '{pid}' \
             AND b.qualified_name = '{tgt_escaped}' AND b.project_id = '{pid}' \
             CREATE (a)-[:CodeRelation {{type: 'ACCESSES', confidence: 0.92, reason: 'self-receiver', step: 0}}]->(b)",
        ));
    }
}
