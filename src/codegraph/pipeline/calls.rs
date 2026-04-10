//! AST-aware call resolution with tiered confidence scoring.
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

    // 1. Query all Function and Method nodes.
    let mut symbols_by_name: HashMap<String, Vec<SymbolEntry>> = HashMap::new();
    // qname → (return_type_text, src_label) — keyed by qname because
    // we emit one USES edge per callable, and src_label is stashed so
    // the edge MATCH clause can use the correct source label without
    // re-deriving it from the qname.
    let mut function_return_types: HashMap<String, (String, String)> = HashMap::new();

    for (node_label, label_str) in &[("Function", "Function"), ("Method", "Method")] {
        let rows = db.query(&format!(
            "MATCH (n:{node_label}) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name, n.source_file, n.declared_type"
        )).await?;

        for row in &rows {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(source_file)),
                declared_type_val,
            ) = (row.first(), row.get(1), row.get(2), row.get(3))
            {
                symbols_by_name.entry(name.clone()).or_default().push(SymbolEntry {
                    qualified_name: qname.clone(),
                    source_file: source_file.clone(),
                    label: label_str.to_string(),
                });
                if let Some(lbug::Value::String(ty)) = declared_type_val
                    && !ty.is_empty()
                {
                    function_return_types
                        .insert(qname.clone(), (ty.clone(), label_str.to_string()));
                }
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
    // Also builds the field-type environment: for each class field with a
    // non-empty declared_type, record (class_qname, field_name) → type.
    let mut variables_by_class: HashMap<String, HashSet<String>> = HashMap::new();
    let mut field_types: HashMap<(String, String), String> = HashMap::new();
    let var_rows = db
        .query(&format!(
            "MATCH (n:Variable) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name, n.declared_type"
        ))
        .await?;
    for row in &var_rows {
        if let (
            Some(lbug::Value::String(qname)),
            Some(lbug::Value::String(name)),
            declared_type_val,
        ) = (row.first(), row.get(1), row.get(2))
            && let Some((parent, _)) = qname.rsplit_once("::")
            && classes_by_qname.contains(parent)
        {
            variables_by_class
                .entry(parent.to_string())
                .or_default()
                .insert(name.clone());
            if let Some(lbug::Value::String(ty)) = declared_type_val
                && !ty.is_empty()
            {
                field_types.insert((parent.to_string(), name.clone()), ty.clone());
            }
        }
    }

    // Build the parameter-type environment.
    // For each Parameter node with a non-empty declared_type, record
    // (enclosing_function_qname, param_name) → type_text. Parameter qnames
    // are "function_qname::param_name", so the enclosing function is the
    // rsplit prefix. This lets the resolver bind `param.method()` calls
    // where the receiver type is known from its annotation.
    let mut param_types: HashMap<(String, String), String> = HashMap::new();
    let param_rows = db
        .query(&format!(
            "MATCH (p:Parameter) WHERE p.project_id = '{pid}' \
             RETURN p.qualified_name, p.name, p.declared_type"
        ))
        .await?;
    for row in &param_rows {
        if let (
            Some(lbug::Value::String(qname)),
            Some(lbug::Value::String(name)),
            Some(lbug::Value::String(ty)),
        ) = (row.first(), row.get(1), row.get(2))
            && !ty.is_empty()
            && let Some((parent_fn, _)) = qname.rsplit_once("::")
        {
            param_types.insert((parent_fn.to_string(), name.clone()), ty.clone());
        }
    }

    // Explicit symbol import map: source_file → (imported_name → SymbolEntry).
    // Every per-symbol Import node ends up here when its name matches a
    // Function/Method in the same project. The call loop consults this
    // before falling through to the looser import-scoped-file tier, so
    // `foo()` in a file that did `import { foo } from './bar'` binds
    // directly to the specific `bar.ts::foo` even when multiple unrelated
    // `foo` functions exist elsewhere in the graph.
    let mut imported_symbols_by_file: HashMap<String, HashMap<String, SymbolEntry>> = HashMap::new();
    let import_rows = db
        .query(&format!(
            "MATCH (i:Import) WHERE i.project_id = '{pid}' \
             RETURN i.source_file, i.name, i.import_source"
        ))
        .await?;
    for row in &import_rows {
        if let (
            Some(lbug::Value::String(src_file)),
            Some(lbug::Value::String(local_name)),
            Some(lbug::Value::String(_module)),
        ) = (row.first(), row.get(1), row.get(2))
            && !local_name.is_empty()
            && local_name != "*"
            && let Some(entries) = symbols_by_name.get(local_name.as_str())
        {
            // Prefer the Function/Method whose qname leaf matches the
            // imported name exactly so that collisions across unrelated
            // files don't map all of them to the same import binding.
            if let Some(entry) = entries.iter().find(|e| {
                e.qualified_name
                    .rsplit("::")
                    .next()
                    .map(|leaf| leaf == local_name.as_str())
                    .unwrap_or(false)
            }) {
                imported_symbols_by_file
                    .entry(src_file.clone())
                    .or_default()
                    .insert(
                        local_name.clone(),
                        SymbolEntry {
                            qualified_name: entry.qualified_name.clone(),
                            source_file: entry.source_file.clone(),
                            label: entry.label.clone(),
                        },
                    );
            }
        }
    }

    // 3. Extract call sites from each file using AST analysis, then resolve.
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();
    // (enclosing_function_qname, local_var_name) → declared type text.
    // Populated inline during the file loop via provider.extract_locals
    // and consulted by the call resolver before params and fields, so a
    // local binding shadows an outer param or class field of the same
    // name just like it would during real scope resolution.
    let mut local_types: HashMap<(String, String), String> = HashMap::new();
    // Qnames of functions classified as tests by their language
    // provider. Every call whose caller is in this set also gets a
    // TESTED_BY edge from the resolved callee back to the test.
    let mut test_fns: HashSet<String> = HashSet::new();
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
            .replace('\\', "/");

        let imported_files = import_map.get(&relative);
        let call_sites = provider.extract_calls(&relative, &content);
        let access_sites = provider.extract_accesses(&relative, &content);

        for binding in provider.extract_locals(&relative, &content) {
            if binding.declared_type.is_empty() {
                continue;
            }
            local_types.insert(
                (binding.function_qualified_name, binding.name),
                binding.declared_type,
            );
        }

        for test_qn in provider.extract_tests(&relative, &content) {
            test_fns.insert(test_qn);
        }

        // Detect HTTP client calls (fetch, axios, requests, etc.) and
        // emit FETCHES edges from the caller to a synthetic URL target.
        // The URL is stored in the reason field since it isn't a graph
        // node — queries can filter on reason to find all fetch sites.
        for site in &call_sites {
            let is_fetch = match (site.callee_name.as_str(), site.receiver.as_deref()) {
                ("fetch", None) => true,
                ("get" | "post" | "put" | "delete" | "patch" | "head" | "request",
                 Some("axios" | "requests" | "http" | "https" | "client" | "httpClient" | "HttpClient")) => true,
                ("Get" | "Post" | "Put" | "Delete" | "Do",
                 Some("http" | "client" | "resp")) => true,
                ("send" | "execute",
                 Some(r)) if r.contains("request") || r.contains("Request") || r.contains("client") || r.contains("Client") => true,
                _ => false,
            };
            if is_fetch {
                let fetch_key = format!("FETCH:{}::{}", site.caller_qualified_name, site.line);
                if seen_edges.insert(fetch_key) {
                    let caller_escaped = cypher_escape(&site.caller_qualified_name);
                    // Emit a self-referencing FETCHES edge on the caller
                    // with the call details in the reason field so
                    // queries can discover which functions make HTTP calls.
                    for src_label in &["Function", "Method"] {
                        edge_stmts.push(format!(
                            "MATCH (a:{src_label}) WHERE a.qualified_name = '{caller_escaped}' \
                             AND a.project_id = '{pid}' \
                             CREATE (a)-[:CodeRelation {{type: 'FETCHES', confidence: 0.80, \
                             reason: '{callee}', step: {line}}}]->(a)",
                            callee = cypher_escape(&format!("{}.{}", site.receiver.as_deref().unwrap_or(""), site.callee_name)),
                            line = site.line,
                        ));
                    }
                }
            }
        }

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
                            maybe_emit_tested_by(
                                &mut edge_stmts,
                                &mut seen_edges,
                                &test_fns,
                                &site.caller_qualified_name,
                                &target.qualified_name,
                                &target.label,
                                &pid,
                            );
                        }
                        continue;
                    }
                }
            }

            // --- Typed-receiver method call (0.88) ---
            // For calls like `param.method()`, `self.field.method()`, or
            // `this.field.method()`, look up the receiver's declared type
            // and resolve the method on that class. This handles the bulk
            // of cross-file method calls that the receiver-resolved tier
            // above can't reach because the receiver isn't a class name.
            if site.is_method_call
                && let Some(recv) = &site.receiver
                && let Some(type_text) = resolve_receiver_type(
                    recv,
                    &site.caller_qualified_name,
                    &classes_by_qname,
                    &param_types,
                    &field_types,
                    &local_types,
                )
                .or_else(|| resolve_chained_receiver_type(
                    recv,
                    &site.caller_qualified_name,
                    &classes_by_qname,
                    &symbols_by_name,
                    &methods_by_class,
                    &function_return_types,
                    &relative,
                ))
                && let Some(base) = base_type_name(&type_text)
                && let Some(class_entries) = classes_by_name.get(&base)
            {
                let class_qn = class_entries
                    .iter()
                    .find(|(_, sf)| *sf == relative)
                    .or_else(|| class_entries.first())
                    .map(|(qn, _)| qn.as_str());
                if let Some(class_qn) = class_qn {
                    let method_key = format!("{class_qn}::{}", site.callee_name);
                    if let Some(target) = methods_by_class.get(&method_key) {
                        let edge_key =
                            format!("{}->{}", site.caller_qualified_name, target.qualified_name);
                        if seen_edges.insert(edge_key) {
                            push_edge(
                                &mut edge_stmts,
                                &site.caller_qualified_name,
                                &target.qualified_name,
                                &target.label,
                                &pid,
                                0.88,
                                "typed-receiver",
                            );
                            maybe_emit_tested_by(
                                &mut edge_stmts,
                                &mut seen_edges,
                                &test_fns,
                                &site.caller_qualified_name,
                                &target.qualified_name,
                                &target.label,
                                &pid,
                            );
                        }
                        continue;
                    }
                }
            }

            // --- Name-based tiered resolution ---
            let entries = match symbols_by_name.get(&site.callee_name) {
                Some(e) => e,
                None => {
                    // No Function/Method with this name exists. If the
                    // name matches a class/struct, treat it as a
                    // constructor call and emit a USES edge from the
                    // caller to the class. Covers `new Foo()`, struct
                    // literals, and Python-style `Foo()` constructors.
                    if !site.is_method_call
                        && let Some(class_entries) = classes_by_name.get(&site.callee_name)
                    {
                        for (class_qn, _sf) in class_entries.iter().take(3) {
                            let edge_key = format!("INST:{}->{}", site.caller_qualified_name, class_qn);
                            if seen_edges.insert(edge_key) {
                                push_uses_edge(
                                    &mut edge_stmts,
                                    "Function",
                                    &site.caller_qualified_name,
                                    class_qn,
                                    &pid,
                                    0.85,
                                    "instantiation",
                                );
                                push_uses_edge(
                                    &mut edge_stmts,
                                    "Method",
                                    &site.caller_qualified_name,
                                    class_qn,
                                    &pid,
                                    0.85,
                                    "instantiation",
                                );
                            }
                        }
                    }
                    continue;
                }
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
                    maybe_emit_tested_by(
                        &mut edge_stmts,
                        &mut seen_edges,
                        &test_fns,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                    );
                }
                continue;
            }

            // Tier 1b: explicitly imported symbol (0.93). An imported
            // symbol match is strictly tighter than the bulk "imported-
            // file" tier below — we know not just which file exports the
            // callee but that the current file named it in its import
            // list, so same-name collisions in unrelated files can't
            // confuse resolution.
            if let Some(file_imports) = imported_symbols_by_file.get(relative.as_str())
                && let Some(target) = file_imports.get(&site.callee_name)
            {
                let edge_key = format!("{}->{}", site.caller_qualified_name, target.qualified_name);
                if seen_edges.insert(edge_key) {
                    push_edge(
                        &mut edge_stmts,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                        0.93,
                        "imported-symbol",
                    );
                    maybe_emit_tested_by(
                        &mut edge_stmts,
                        &mut seen_edges,
                        &test_fns,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
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
                    maybe_emit_tested_by(
                        &mut edge_stmts,
                        &mut seen_edges,
                        &test_fns,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
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
                    maybe_emit_tested_by(
                        &mut edge_stmts,
                        &mut seen_edges,
                        &test_fns,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
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
                    maybe_emit_tested_by(
                        &mut edge_stmts,
                        &mut seen_edges,
                        &test_fns,
                        &site.caller_qualified_name,
                        &target.qualified_name,
                        &target.label,
                        &pid,
                    );
                }
            }

            // If the callee also matches a class name, emit an
            // additional USES edge for the instantiation relationship
            // regardless of which CALLS tier handled the method/function.
            if !site.is_method_call
                && let Some(class_entries) = classes_by_name.get(&site.callee_name)
            {
                for (class_qn, _sf) in class_entries.iter().take(3) {
                    let edge_key = format!("INST:{}->{}", site.caller_qualified_name, class_qn);
                    if seen_edges.insert(edge_key) {
                        push_uses_edge(
                            &mut edge_stmts,
                            "Function",
                            &site.caller_qualified_name,
                            class_qn,
                            &pid,
                            0.85,
                            "instantiation",
                        );
                        push_uses_edge(
                            &mut edge_stmts,
                            "Method",
                            &site.caller_qualified_name,
                            class_qn,
                            &pid,
                            0.85,
                            "instantiation",
                        );
                    }
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

    // Disambiguation policy for every USES tier below: a unique base
    // class match scores 0.85, multiple matches drop to 0.50 and fan
    // out to at most three candidates to keep the edge set bounded on
    // names that collide across large workspaces.
    if let Some(pf) = progress_fn {
        pf(
            0.78,
            &format!(
                "Emitting type-reference edges ({} params, {} fields)",
                param_types.len(),
                field_types.len()
            ),
            &result,
        );
    }

    for ((fn_qname, param_name), type_text) in &param_types {
        let Some(base) = base_type_name(type_text) else { continue };
        let Some(entries) = classes_by_name.get(&base) else { continue };
        let param_qname = format!("{fn_qname}::{param_name}");
        let confidence = if entries.len() == 1 { 0.85 } else { 0.50 };
        let reason = if entries.len() == 1 {
            "type-ref-unique"
        } else {
            "type-ref-multi"
        };
        for (class_qn, _sf) in entries.iter().take(3) {
            let edge_key = format!("USE:{param_qname}->{class_qn}");
            if seen_edges.insert(edge_key) {
                push_uses_edge(
                    &mut edge_stmts,
                    "Parameter",
                    &param_qname,
                    class_qn,
                    &pid,
                    confidence,
                    reason,
                );
            }
        }
    }

    for ((class_qn, field_name), type_text) in &field_types {
        let Some(base) = base_type_name(type_text) else { continue };
        let Some(entries) = classes_by_name.get(&base) else { continue };
        let field_qname = format!("{class_qn}::{field_name}");
        let confidence = if entries.len() == 1 { 0.85 } else { 0.50 };
        let reason = if entries.len() == 1 {
            "type-ref-unique"
        } else {
            "type-ref-multi"
        };
        for (target_qn, _sf) in entries.iter().take(3) {
            // Skip self-edges (linked-list nodes that hold a field of
            // their own type) — the edge carries no new information.
            if target_qn == class_qn {
                continue;
            }
            let edge_key = format!("USE:{field_qname}->{target_qn}");
            if seen_edges.insert(edge_key) {
                push_uses_edge(
                    &mut edge_stmts,
                    "Variable",
                    &field_qname,
                    target_qn,
                    &pid,
                    confidence,
                    reason,
                );
            }
        }
    }

    for (fn_qname, (type_text, src_label)) in &function_return_types {
        let Some(base) = base_type_name(type_text) else { continue };
        let Some(entries) = classes_by_name.get(&base) else { continue };
        let confidence = if entries.len() == 1 { 0.85 } else { 0.50 };
        let reason = if entries.len() == 1 {
            "return-type-unique"
        } else {
            "return-type-multi"
        };
        for (target_qn, _sf) in entries.iter().take(3) {
            let edge_key = format!("USE:{fn_qname}->{target_qn}");
            if seen_edges.insert(edge_key) {
                push_uses_edge(
                    &mut edge_stmts,
                    src_label,
                    fn_qname,
                    target_qn,
                    &pid,
                    confidence,
                    reason,
                );
            }
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

/// Resolve a receiver expression to its declared type text.
///
/// Handles three receiver shapes:
/// 1. `self.field` / `this.field` — look up `field` in the enclosing class's
///    field_types map.
/// 2. Plain identifier (e.g. `param`, `local`) — look up first in the
///    current function's param_types, then fall back to the enclosing
///    class's field_types (covers Go-style receiver-field accesses without
///    the `self.` prefix, and Java/C# `field` without `this.`).
///
/// Returns `None` for more complex receivers (method-call chains,
/// expressions with parens, etc.) — those fall through to the name-based
/// tiers unchanged.
fn resolve_receiver_type(
    receiver: &str,
    caller_qn: &str,
    classes_by_qname: &HashSet<String>,
    param_types: &HashMap<(String, String), String>,
    field_types: &HashMap<(String, String), String>,
    local_types: &HashMap<(String, String), String>,
) -> Option<String> {
    // Case 1: self.field / this.field → single-level field access.
    if let Some(field) = receiver
        .strip_prefix("self.")
        .or_else(|| receiver.strip_prefix("this."))
    {
        // Reject nested (`self.a.b`) — we'd need transitive resolution.
        if field.contains('.') || field.contains('(') || field.contains('[') {
            return None;
        }
        let class_qn = find_enclosing_class(caller_qn, classes_by_qname)?;
        return field_types
            .get(&(class_qn.to_string(), field.to_string()))
            .cloned();
    }

    // Case 2: plain identifier.
    if !receiver.is_empty()
        && !receiver.contains('.')
        && !receiver.contains('(')
        && !receiver.contains('[')
        && receiver != "self"
        && receiver != "this"
        && receiver
            .chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_')
            .unwrap_or(false)
    {
        // Locals shadow params and fields in real scope resolution, so
        // consult them first. Walk the enclosing-function chain the same
        // way as params: a nested closure's body sees the outer
        // function's locals.
        let mut scope = caller_qn;
        loop {
            if let Some(ty) =
                local_types.get(&(scope.to_string(), receiver.to_string()))
            {
                return Some(ty.clone());
            }
            match scope.rsplit_once("::") {
                Some((parent, _)) => scope = parent,
                None => break,
            }
        }
        let mut scope = caller_qn;
        loop {
            if let Some(ty) =
                param_types.get(&(scope.to_string(), receiver.to_string()))
            {
                return Some(ty.clone());
            }
            match scope.rsplit_once("::") {
                Some((parent, _)) => scope = parent,
                None => break,
            }
        }
        // Naked field access: `s.field` inside a method on S where the
        // language has no `self.`/`this.` prefix (Go, and informal Java
        // / C# patterns).
        if let Some(class_qn) = find_enclosing_class(caller_qn, classes_by_qname)
            && let Some(ty) =
                field_types.get(&(class_qn.to_string(), receiver.to_string()))
        {
            return Some(ty.clone());
        }
    }

    None
}

/// Resolve a receiver that ends with a function call — `foo()`,
/// `self.method()`, `this.method()` — by looking up the callee's
/// return type. Handles one level of chaining so `foo().bar()` binds
/// `bar` against the class that `foo` returns.
fn resolve_chained_receiver_type(
    receiver: &str,
    caller_qn: &str,
    classes_by_qname: &HashSet<String>,
    symbols_by_name: &HashMap<String, Vec<SymbolEntry>>,
    methods_by_class: &HashMap<String, SymbolEntry>,
    function_return_types: &HashMap<String, (String, String)>,
    relative: &str,
) -> Option<String> {
    // Strip the trailing argument list: `foo()` → `foo`,
    // `foo(x, y)` → `foo`. Bail if no parens found.
    let open = receiver.rfind('(')?;
    if !receiver.ends_with(')') {
        return None;
    }
    let inner = &receiver[..open];
    if inner.is_empty() {
        return None;
    }

    // `self.method(...)` / `this.method(...)` → look up the method's
    // return type on the enclosing class.
    if let Some(method_name) = inner
        .strip_prefix("self.")
        .or_else(|| inner.strip_prefix("this."))
    {
        if method_name.contains('.') || method_name.contains('(') {
            return None;
        }
        let class_qn = find_enclosing_class(caller_qn, classes_by_qname)?;
        let method_key = format!("{class_qn}::{method_name}");
        let method_entry = methods_by_class.get(&method_key)?;
        let (return_type, _) = function_return_types.get(&method_entry.qualified_name)?;
        return Some(return_type.clone());
    }

    // `foo(...)` — bare function/method call. Resolve by name,
    // preferring the same-file definition for disambiguation.
    if !inner.contains('.') && !inner.contains('(') {
        let entries = symbols_by_name.get(inner)?;
        let best = entries
            .iter()
            .find(|e| e.source_file == relative)
            .or_else(|| entries.first())?;
        let (return_type, _) = function_return_types.get(&best.qualified_name)?;
        return Some(return_type.clone());
    }

    None
}

/// Normalize a source-level type expression to a bare class name
/// suitable for lookup in `classes_by_name`.
///
/// Handles:
/// - Leading references/pointers/qualifiers: `&`, `&mut`, `*`, `*mut`,
///   `*const`, `mut `, `const `
/// - Trailing nullable markers: `?`, `*`, `&`, `...`, `[]`
/// - Generic wrappers: `Arc<Foo>`, `Box<Foo>`, `Mutex<Foo>`, `Rc<Foo>`,
///   `Option<Foo>`, `Vec<Foo>`, `RefCell<Foo>`, `Pin<Box<Foo>>`, etc.
///   (recurses into the first type arg of known wrappers)
/// - Path scoping: `foo::Bar`, `foo.Bar`, `java.lang.String` → leaf name
///
/// Returns `None` if the result is empty or contains no class-like token.
fn base_type_name(type_text: &str) -> Option<String> {
    // Wrappers whose inner type is the "real" class we care about. When
    // the outer of a generic is one of these, recurse into the first arg.
    const WRAPPERS: &[&str] = &[
        "Arc", "Rc", "Box", "Option", "Result", "Vec", "Mutex", "RwLock",
        "RefCell", "Cell", "Pin", "Weak", "MaybeUninit", "NonNull",
        "UnsafeCell", "Cow", "Lazy", "OnceCell", "OnceLock", "Reverse",
        // Java/Kotlin/C# common wrappers
        "List", "ArrayList", "LinkedList", "Set", "HashSet", "Map",
        "HashMap", "Optional", "Iterable", "Iterator", "Stream", "Flux",
        "Mono", "Future", "CompletableFuture", "Nullable", "NonNull",
        "IEnumerable", "ICollection", "IList", "IReadOnlyList", "Task",
        "ValueTask", "Nullable",
    ];

    let mut s = type_text.trim();

    // Strip leading `&`, `&mut`, `*`, `*mut`, `*const`, `mut `, `const `.
    loop {
        let start = s;
        s = s.trim_start_matches('&').trim_start();
        s = s.trim_start_matches('*').trim_start();
        if let Some(rest) = s.strip_prefix("mut ") {
            s = rest.trim_start();
        }
        if let Some(rest) = s.strip_prefix("const ") {
            s = rest.trim_start();
        }
        if s == start {
            break;
        }
    }

    // Strip trailing `?`, `*`, `&`, `...`, `[]`.
    loop {
        let start = s;
        s = s
            .trim_end_matches('?')
            .trim_end_matches('*')
            .trim_end_matches('&')
            .trim_end_matches("...")
            .trim_end_matches("[]")
            .trim_end();
        if s == start {
            break;
        }
    }

    if s.is_empty() {
        return None;
    }

    // If the type has a generic clause, consider unwrapping.
    if let Some(lt_pos) = s.find('<') {
        let outer = s[..lt_pos].trim();
        let outer_leaf = leaf_name(outer);
        if WRAPPERS.contains(&outer_leaf)
            && let Some(gt_pos) = s.rfind('>')
            && gt_pos > lt_pos
        {
            let inner = &s[lt_pos + 1..gt_pos];
            let first_arg = split_first_type_arg(inner);
            return base_type_name(first_arg);
        }
        // Not a known wrapper — use the outer as the base.
        let leaf = leaf_name(outer);
        return if leaf.is_empty() {
            None
        } else {
            Some(leaf.to_string())
        };
    }

    // No generics — take the leaf of the path-scoped name.
    let leaf = leaf_name(s);
    if leaf.is_empty() {
        None
    } else {
        Some(leaf.to_string())
    }
}

/// Strip path scoping (`a::b::Foo`, `a.b.Foo`) to the leaf identifier.
fn leaf_name(s: &str) -> &str {
    let s = s.trim();
    let after_colons = s.rsplit("::").next().unwrap_or(s);
    after_colons.rsplit('.').next().unwrap_or(after_colons).trim()
}

/// Split a generic argument list at the first top-level comma,
/// returning the first argument.
fn split_first_type_arg(inner: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in inner.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => return inner[..i].trim(),
            _ => {}
        }
    }
    inner.trim()
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

/// Emit a `TESTED_BY` edge from a resolved callee back to the test
/// function that called it, deduped separately from the `CALLS` edge
/// set so both edges land even when the same test → target pair
/// resolves through multiple confidence tiers in different call sites.
fn maybe_emit_tested_by(
    stmts: &mut Vec<String>,
    seen: &mut HashSet<String>,
    test_fns: &HashSet<String>,
    caller_qn: &str,
    target_qn: &str,
    target_label: &str,
    pid: &str,
) {
    if !test_fns.contains(caller_qn) {
        return;
    }
    let key = format!("TB:{target_qn}->{caller_qn}");
    if !seen.insert(key) {
        return;
    }
    let src_escaped = cypher_escape(target_qn);
    let tgt_escaped = cypher_escape(caller_qn);
    for src_label in &["Function", "Method"] {
        stmts.push(format!(
            "MATCH (a:{target_label}), (b:{src_label}) \
             WHERE a.qualified_name = '{src_escaped}' AND a.project_id = '{pid}' \
             AND b.qualified_name = '{tgt_escaped}' AND b.project_id = '{pid}' \
             CREATE (a)-[:CodeRelation {{type: 'TESTED_BY', confidence: 1.0, reason: 'test-caller', step: 0}}]->(b)",
        ));
    }
}

/// Build and push a USES edge from a typed symbol to its declared class.
///
/// Unlike `push_edge`, the source label is passed in explicitly — callers
/// already know whether the source is Parameter/Variable/Function/Method
/// and fanning out across both Function and Method (as `push_edge` does)
/// would double-emit for Parameter/Variable sources. One MATCH statement
/// is produced per candidate target label so Class/Struct/Interface/Trait
/// all resolve without the caller needing to know which applies.
fn push_uses_edge(
    stmts: &mut Vec<String>,
    src_label: &str,
    src_qn: &str,
    tgt_qn: &str,
    pid: &str,
    confidence: f64,
    reason: &str,
) {
    let src_escaped = cypher_escape(src_qn);
    let tgt_escaped = cypher_escape(tgt_qn);

    for tgt_label in &["Class", "Struct", "Interface", "Trait"] {
        stmts.push(format!(
            "MATCH (a:{src_label}), (b:{tgt_label}) \
             WHERE a.qualified_name = '{src_escaped}' AND a.project_id = '{pid}' \
             AND b.qualified_name = '{tgt_escaped}' AND b.project_id = '{pid}' \
             CREATE (a)-[:CodeRelation {{type: 'USES', confidence: {confidence}, reason: '{reason}', step: 0}}]->(b)",
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
