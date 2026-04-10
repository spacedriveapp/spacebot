//! Resolve import/require/use statements.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Result from import resolution including the import map for downstream phases.
pub struct ImportPhaseResult {
    pub phase: PhaseResult,
    /// Map of source_file → set of imported file paths (for call resolution).
    pub import_map: HashMap<String, HashSet<String>>,
}

/// Resolve import statements and create IMPORTS edges between files.
///
/// Queries all Import nodes, resolves their `import_source` to target File
/// nodes, and creates CodeRelation edges of type IMPORTS.
pub async fn resolve_imports(
    project_id: &str,
    db: &SharedCodeGraphDb,
) -> Result<ImportPhaseResult> {
    resolve_imports_scoped(project_id, db, None).await
}

/// Scoped variant of `resolve_imports`: if `scope_files` is `Some`, only
/// Import nodes whose `source_file` is in the set are processed. Used by
/// the incremental pipeline to avoid duplicating IMPORTS edges for
/// unchanged files.
pub async fn resolve_imports_scoped(
    project_id: &str,
    db: &SharedCodeGraphDb,
    scope_files: Option<&HashSet<String>>,
) -> Result<ImportPhaseResult> {
    let mut result = PhaseResult::default();
    let mut import_map: HashMap<String, HashSet<String>> = HashMap::new();
    let pid = cypher_escape(project_id);

    tracing::debug!(
        project_id = %project_id,
        scoped = scope_files.is_some(),
        "resolving imports"
    );

    // 1. Query all Import nodes with source file, import_source, name,
    //    and extends_type (which carries the original name for aliased
    //    imports like `import { Foo as Bar }`).
    let imports = db.query(&format!(
        "MATCH (i:Import) WHERE i.project_id = '{pid}' \
         RETURN i.source_file, i.import_source, i.name, i.extends_type"
    )).await?;

    // 2. Query all File nodes to build a lookup map.
    let files = db.query(&format!(
        "MATCH (f:File) WHERE f.project_id = '{pid}' \
         RETURN f.source_file, f.qualified_name"
    )).await?;

    let mut file_by_path: HashMap<String, String> = HashMap::new();
    for row in &files {
        if let (Some(lbug::Value::String(path)), Some(lbug::Value::String(qname))) =
            (row.first(), row.get(1))
        {
            file_by_path.insert(path.clone(), qname.clone());
            // Also index without extension for fuzzy matching
            if let Some(stem) = path.strip_suffix(".ts")
                .or_else(|| path.strip_suffix(".tsx"))
                .or_else(|| path.strip_suffix(".js"))
                .or_else(|| path.strip_suffix(".jsx"))
                .or_else(|| path.strip_suffix(".rs"))
                .or_else(|| path.strip_suffix(".py"))
            {
                file_by_path.entry(stem.to_string()).or_insert_with(|| qname.clone());
                // index.ts convention
                let index_path = format!("{stem}/index.ts");
                file_by_path.entry(index_path).or_insert_with(|| qname.clone());
            }
        }
    }

    // 2b. Build a (source_file, symbol_name) → (symbol_qname, symbol_label)
    //     lookup across every symbol kind that an import can target.
    //     Keyed by source_file because the resolver first narrows to the
    //     target file via import_source, then looks up the name inside.
    let mut symbols_by_file_name: HashMap<(String, String), (String, String)> = HashMap::new();
    for label in &["Function", "Method", "Class", "Interface", "Struct", "Trait", "Variable", "Enum", "TypeAlias", "Const"] {
        let rows = db.query(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
             RETURN n.source_file, n.name, n.qualified_name"
        )).await?;
        for row in &rows {
            if let (
                Some(lbug::Value::String(sf)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(qname)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                symbols_by_file_name
                    .entry((sf.clone(), name.clone()))
                    .or_insert_with(|| (qname.clone(), label.to_string()));
            }
        }
    }

    // 3. For each import, resolve to a file and create an IMPORTS edge.
    let mut edge_stmts: Vec<String> = Vec::new();

    for row in &imports {
        let (source_file, import_source, name, original_name) = match (row.first(), row.get(1), row.get(2), row.get(3)) {
            (
                Some(lbug::Value::String(sf)),
                Some(lbug::Value::String(is)),
                Some(lbug::Value::String(n)),
                orig,
            ) => {
                let orig_str = match orig {
                    Some(lbug::Value::String(o)) if !o.is_empty() => o.as_str(),
                    _ => "",
                };
                (sf, is, n.as_str(), orig_str)
            }
            _ => continue,
        };

        if import_source.is_empty() {
            continue;
        }

        // Scoped runs only process imports originating in the scope set.
        if let Some(scope) = scope_files
            && !scope.contains(source_file)
        {
            continue;
        }

        // Clean up import source: strip quotes, normalize
        let cleaned = import_source
            .trim_matches(|c| c == '\'' || c == '"')
            .replace("use ", "")
            .trim()
            .to_string();

        // Try to resolve relative to source file's directory
        let source_dir = source_file
            .rfind(['/', '\\'])
            .map(|i| &source_file[..i])
            .unwrap_or("");

        let candidates = [
            // Direct path
            cleaned.clone(),
            // Relative to source dir
            format!("{source_dir}/{cleaned}"),
            // With common extensions
            format!("{source_dir}/{cleaned}.ts"),
            format!("{source_dir}/{cleaned}.tsx"),
            format!("{source_dir}/{cleaned}.rs"),
            format!("{source_dir}/{cleaned}.py"),
            format!("{cleaned}.ts"),
            format!("{cleaned}.tsx"),
            format!("{cleaned}.rs"),
            format!("{cleaned}.py"),
            // Rust-style mod resolution
            format!("{cleaned}/mod.rs"),
        ];

        // Normalize path separators and strip leading ./
        for candidate in &candidates {
            let normalized = candidate
                .replace('\\', "/")
                .trim_start_matches("./")
                .to_string();

            if let Some(target_qname) = file_by_path.get(&normalized) {
                let src_qname = match file_by_path.get(source_file.as_str()) {
                    Some(q) => q,
                    None => continue,
                };

                let src_escaped = cypher_escape(src_qname);
                let tgt_escaped = cypher_escape(target_qname);

                edge_stmts.push(format!(
                    "MATCH (s:File), (t:File) WHERE s.qualified_name = '{src_escaped}' \
                     AND t.qualified_name = '{tgt_escaped}' \
                     CREATE (s)-[:CodeRelation {{type: 'IMPORTS', confidence: 1.0, reason: 'import statement', step: 0}}]->(t)",
                ));

                if !name.is_empty() && name != "*" {
                    // Named import — try the local name first, then the
                    // original name for aliased imports. `Bar` from
                    // `import { Foo as Bar }` won't match any symbol
                    // in the target file, but `Foo` will.
                    let lookup_name = if let Some((sq, sl)) =
                        symbols_by_file_name.get(&(normalized.clone(), name.to_string()))
                    {
                        Some((sq.clone(), sl.clone()))
                    } else if !original_name.is_empty() {
                        symbols_by_file_name
                            .get(&(normalized.clone(), original_name.to_string()))
                            .map(|(sq, sl)| (sq.clone(), sl.clone()))
                    } else {
                        None
                    };
                    if let Some((sym_qname, sym_label)) = lookup_name.as_ref()
                    {
                        let sym_escaped = cypher_escape(sym_qname);
                        edge_stmts.push(format!(
                            "MATCH (s:File), (t:{sym_label}) WHERE s.qualified_name = '{src_escaped}' \
                             AND t.qualified_name = '{sym_escaped}' \
                             CREATE (s)-[:CodeRelation {{type: 'IMPORTS', confidence: 0.95, reason: 'symbol import', step: 0}}]->(t)",
                        ));
                    }
                } else if name == "*" {
                    // Wildcard import — synthesize per-symbol edges for
                    // every exported symbol in the target file. This
                    // expands Go whole-module imports, Python `from x
                    // import *`, and C++ `using namespace` into the same
                    // File→Symbol edges that named imports produce.
                    for ((sf, sym_name), (sym_qname, sym_label)) in &symbols_by_file_name {
                        if sf == &normalized {
                            let sym_escaped = cypher_escape(sym_qname);
                            edge_stmts.push(format!(
                                "MATCH (s:File), (t:{sym_label}) WHERE s.qualified_name = '{src_escaped}' \
                                 AND t.qualified_name = '{sym_escaped}' \
                                 CREATE (s)-[:CodeRelation {{type: 'IMPORTS', confidence: 0.80, reason: 'wildcard import', step: 0}}]->(t)",
                            ));
                            let _ = sym_name; // suppress unused warning
                        }
                    }
                }

                // Record in import map for call resolution
                import_map
                    .entry(source_file.clone())
                    .or_default()
                    .insert(normalized);

                break;
            }
        }
    }

    // 4. Execute edge batch.
    if !edge_stmts.is_empty() {
        let batch = db.execute_batch(edge_stmts).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;
    }

    tracing::info!(
        project_id = %project_id,
        edges = result.edges_created,
        import_entries = import_map.len(),
        "import resolution complete"
    );

    Ok(ImportPhaseResult {
        phase: result,
        import_map,
    })
}
