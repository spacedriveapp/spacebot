//! Resolve method overrides via C3 linearization.
//!
//! Runs after [`super::heritage::resolve_heritage`] inside the Heritage
//! phase. Builds C3-linearized Method Resolution Order (MRO) for every
//! class-like node (Class, Interface, Struct, Trait, Impl), then for
//! each method on a child class walks the MRO looking for the nearest
//! ancestor method with the same name and emits an OVERRIDES edge.
//!
//! C3 linearization handles diamond inheritance (Python), multiple
//! interfaces (Java/C#), and trait hierarchies (Rust) correctly by
//! preserving local precedence order + monotonicity.
//!
//! Known limitations:
//!
//! - Single-name match only — no signature comparison. Overloading and
//!   overriding are not distinguished, so a Java overload of
//!   `print(int)` will match a parent's `print(String)`.
//! - Cycle detection caps chain depth at 50 to avoid infinite loops
//!   on broken graphs.

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
    // Ordered Vec instead of HashSet so the declaration order of
    // parents is preserved. This gives leftmost-base MRO behavior
    // which is correct for Java/C#/C++ and a reasonable approximation
    // of C3 for Python's non-diamond cases.
    let mut immediate_parents: HashMap<String, Vec<String>> = HashMap::new();
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
                let parents = immediate_parents
                    .entry(r.qualified_name.clone())
                    .or_default();
                if !parents.contains(&parent_qname) {
                    parents.push(parent_qname);
                }
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

    // ─── 5. For each method, compute C3 MRO and find the nearest match
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    let mut mro_cache: HashMap<String, Vec<String>> = HashMap::new();

    for (child_class_qn, methods) in &class_methods {
        for (method_name, method_qn) in methods {
            if let Some(target_qn) = find_nearest_override(
                child_class_qn,
                method_name,
                &immediate_parents,
                &class_methods,
                &mut mro_cache,
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

/// Compute C3 linearization (MRO) for `class_qn`. Results are memoized
/// in `cache` so each class is linearized at most once.
fn c3_linearize(
    class_qn: &str,
    parents: &HashMap<String, Vec<String>>,
    cache: &mut HashMap<String, Vec<String>>,
    depth: usize,
) -> Vec<String> {
    if let Some(cached) = cache.get(class_qn) {
        return cached.clone();
    }
    if depth > MAX_CHAIN_DEPTH {
        return vec![class_qn.to_string()];
    }

    let direct = match parents.get(class_qn) {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            let result = vec![class_qn.to_string()];
            cache.insert(class_qn.to_string(), result.clone());
            return result;
        }
    };

    let mut to_merge: Vec<Vec<String>> = Vec::new();
    for p in &direct {
        to_merge.push(c3_linearize(p, parents, cache, depth + 1));
    }
    to_merge.push(direct);

    let mut result = vec![class_qn.to_string()];
    c3_merge(&mut result, &mut to_merge);
    cache.insert(class_qn.to_string(), result.clone());
    result
}

/// Standard C3 merge: repeatedly pick the first head that doesn't
/// appear in the tail of any other list. Falls back to draining
/// remaining heads on inconsistent hierarchies.
fn c3_merge(result: &mut Vec<String>, lists: &mut Vec<Vec<String>>) {
    loop {
        lists.retain(|l| !l.is_empty());
        if lists.is_empty() {
            break;
        }

        let mut found = None;
        for list in lists.iter() {
            let head = &list[0];
            let in_tail = lists.iter().any(|l| l.len() > 1 && l[1..].contains(head));
            if !in_tail {
                found = Some(head.clone());
                break;
            }
        }

        match found {
            Some(cls) => {
                result.push(cls.clone());
                for list in lists.iter_mut() {
                    if list.first() == Some(&cls) {
                        list.remove(0);
                    }
                }
            }
            None => {
                for list in lists.iter() {
                    if !list.is_empty() && !result.contains(&list[0]) {
                        result.push(list[0].clone());
                    }
                }
                break;
            }
        }
    }
}

/// Walk the C3-linearized MRO for `start_class_qn`, looking for
/// the nearest ancestor that defines a method with `method_name`.
fn find_nearest_override(
    start_class_qn: &str,
    method_name: &str,
    parents: &HashMap<String, Vec<String>>,
    class_methods: &HashMap<String, Vec<(String, String)>>,
    mro_cache: &mut HashMap<String, Vec<String>>,
) -> Option<String> {
    let mro = c3_linearize(start_class_qn, parents, mro_cache, 0);
    for ancestor in &mro[1..] {
        if let Some(methods) = class_methods.get(ancestor) {
            for (name, qn) in methods {
                if name == method_name {
                    return Some(qn.clone());
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
