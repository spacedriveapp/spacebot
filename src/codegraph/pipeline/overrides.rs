//! Phase 6b: Resolve method overrides via parent-class chain walk.
//!
//! Runs after [`super::heritage::resolve_heritage`] inside the Heritage
//! phase. Builds a transitive parent chain for every class-like node
//! (Class, Interface, Struct, Trait, Impl), then for each method on a
//! child class walks up the chain looking for the nearest ancestor
//! method with the same name and emits an OVERRIDES edge.
//!
//! Limitations (intentional, will be relaxed in follow-up waves):
//!
//! - Single-name match only — no signature comparison. Overloading and
//!   overriding are not distinguished, so a Java overload of
//!   `print(int)` will match a parent's `print(String)`.
//! - Single-parent walk per class qname. Multiple-inheritance chains
//!   (Python diamond, C++ MI) are walked but without C3 linearization,
//!   so the "nearest" definition is whichever the DFS hits first.
//! - Cycle detection caps chain depth at 50 and tracks visited nodes
//!   to avoid infinite loops on broken graphs.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;

/// Maximum inheritance chain depth to walk before bailing. Real
/// inheritance trees rarely exceed 10; 50 is generous defense against
/// pathological graphs.
const MAX_CHAIN_DEPTH: usize = 50;

fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// One node-source row, used to feed the parent resolver.
struct ClassRow {
    qualified_name: String,
    name: String,
    extends_type: String,
}

/// Resolve method overrides and create OVERRIDES edges.
pub async fn resolve_overrides(
    project_id: &str,
    db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    tracing::debug!(project_id = %project_id, "resolving method overrides");

    // ─── 1. Query class-like nodes across all inheritable labels ────────
    //
    // We include `Impl` here so Rust `impl Trait for Type` blocks
    // contribute their `extends_type` (the trait) to the parent chain
    // for the underlying type. Multiple labels can share the same
    // qname (Struct + Impl), in which case we union their extends.
    let labels = &["Class", "Interface", "Struct", "Trait", "Impl"];
    let mut all_rows: Vec<ClassRow> = Vec::new();

    for label in labels {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN n.qualified_name, n.name, n.extends_type"
            ))
            .await?;

        for row in &rows {
            if let (
                Some(lbug::Value::String(qn)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(ext)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                all_rows.push(ClassRow {
                    qualified_name: qn.clone(),
                    name: name.clone(),
                    extends_type: ext.clone(),
                });
            }
        }
    }

    if all_rows.is_empty() {
        tracing::info!(project_id = %project_id, "no class-like nodes, skipping overrides");
        return Ok(result);
    }

    // ─── 2. Build name → qname lookup for resolving extends_type strings ─
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_qname: HashSet<String> = HashSet::new();
    for r in &all_rows {
        by_qname.insert(r.qualified_name.clone());
        by_name
            .entry(r.name.clone())
            .or_default()
            .push(r.qualified_name.clone());
    }

    // ─── 3. For each qname, collect resolved immediate-parent qnames ────
    //
    // A single qname may appear in multiple labels (e.g. Struct Foo and
    // Impl Foo for trait T) — both contribute their extends to the same
    // child. We dedupe at the parent level.
    let mut immediate_parents: HashMap<String, HashSet<String>> = HashMap::new();
    for r in &all_rows {
        if r.extends_type.is_empty() {
            continue;
        }
        for parent_name in r.extends_type.split(',').map(str::trim) {
            if parent_name.is_empty() {
                continue;
            }
            let resolved = if by_qname.contains(parent_name) {
                Some(parent_name.to_string())
            } else if let Some(candidates) = by_name.get(parent_name) {
                // Prefer same-file by string-prefix match if multiple.
                candidates
                    .iter()
                    .find(|c| {
                        let src_file = r.qualified_name.split("::").next().unwrap_or("");
                        let cand_file = c.split("::").next().unwrap_or("");
                        src_file == cand_file
                    })
                    .or_else(|| candidates.first())
                    .cloned()
            } else {
                None
            };

            if let Some(parent_qname) = resolved {
                immediate_parents
                    .entry(r.qualified_name.clone())
                    .or_default()
                    .insert(parent_qname);
            }
        }
    }

    // ─── 4. For each Method node, group by parent class qname ──────────
    let method_rows = db
        .query(&format!(
            "MATCH (n:Method) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name"
        ))
        .await?;

    // class qname → Vec<(method name, method qname)>
    let mut class_methods: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for row in &method_rows {
        if let (Some(lbug::Value::String(method_qn)), Some(lbug::Value::String(method_name))) =
            (row.first(), row.get(1))
            && let Some((parent_qn, _)) = method_qn.rsplit_once("::")
        {
            class_methods
                .entry(parent_qn.to_string())
                .or_default()
                .push((method_name.clone(), method_qn.clone()));
        }
    }

    if class_methods.is_empty() {
        tracing::info!(project_id = %project_id, "no methods found, skipping overrides");
        return Ok(result);
    }

    // ─── 5. For each method, walk parent chain and find the nearest match
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();

    for (child_class_qn, methods) in &class_methods {
        for (method_name, method_qn) in methods {
            // Walk parent chain (ordered, nearest-first) and stop at the
            // first ancestor that defines a method with this name.
            if let Some(target_qn) = find_nearest_override(
                child_class_qn,
                method_name,
                &immediate_parents,
                &class_methods,
            ) && target_qn != *method_qn
            {
                let edge_key = format!("OV:{method_qn}->{target_qn}");
                if seen_edges.insert(edge_key) {
                    push_overrides_edge(&mut edge_stmts, method_qn, &target_qn, &pid);
                }
            }
        }
    }

    // ─── 6. Batch-execute edge statements ──────────────────────────────
    if !edge_stmts.is_empty() {
        const BATCH_SIZE: usize = 100;
        for chunk in edge_stmts.chunks(BATCH_SIZE) {
            let batch = db.execute_batch(chunk.to_vec()).await?;
            result.edges_created += batch.success;
            result.errors += batch.errors;
        }
    }

    tracing::info!(
        project_id = %project_id,
        edges = result.edges_created,
        errors = result.errors,
        "override resolution complete"
    );

    Ok(result)
}

/// Walk up the parent chain (BFS, nearest-first) starting from
/// `start_class_qn`, looking for an ancestor class that has a method
/// with `method_name`. Returns that method's qname.
///
/// Cycles are bounded by both `MAX_CHAIN_DEPTH` and a visited set.
fn find_nearest_override(
    start_class_qn: &str,
    method_name: &str,
    parents: &HashMap<String, HashSet<String>>,
    class_methods: &HashMap<String, Vec<(String, String)>>,
) -> Option<String> {
    use std::collections::VecDeque;

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    // Seed with immediate parents — we don't want to match a method
    // against itself in the same class.
    if let Some(direct) = parents.get(start_class_qn) {
        for p in direct {
            if visited.insert(p.clone()) {
                queue.push_back((p.clone(), 1));
            }
        }
    }

    while let Some((class_qn, depth)) = queue.pop_front() {
        if depth > MAX_CHAIN_DEPTH {
            continue;
        }

        if let Some(methods) = class_methods.get(&class_qn) {
            for (name, qn) in methods {
                if name == method_name {
                    return Some(qn.clone());
                }
            }
        }

        if let Some(grandparents) = parents.get(&class_qn) {
            for gp in grandparents {
                if visited.insert(gp.clone()) {
                    queue.push_back((gp.clone(), depth + 1));
                }
            }
        }
    }

    None
}

/// Build and push an OVERRIDES edge Cypher statement (Method → Method).
///
/// Confidence is fixed at 0.85 — overrides resolution uses single-name
/// matching without signature checks, so we don't claim full certainty.
fn push_overrides_edge(stmts: &mut Vec<String>, child_qn: &str, parent_qn: &str, pid: &str) {
    let src_escaped = cypher_escape(child_qn);
    let tgt_escaped = cypher_escape(parent_qn);

    stmts.push(format!(
        "MATCH (a:Method), (b:Method) \
         WHERE a.qualified_name = '{src_escaped}' AND a.project_id = '{pid}' \
         AND b.qualified_name = '{tgt_escaped}' AND b.project_id = '{pid}' \
         CREATE (a)-[:CodeRelation {{type: 'OVERRIDES', confidence: 0.85, reason: 'name-match', step: 0}}]->(b)",
    ));
}
