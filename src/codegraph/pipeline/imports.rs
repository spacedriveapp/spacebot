//! Phase 4: Resolve import/require/use statements.

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

    // 1. Query all Import nodes with their source file and import_source.
    let imports = db.query(&format!(
        "MATCH (i:Import) WHERE i.project_id = '{pid}' \
         RETURN i.source_file, i.import_source"
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

    // 3. For each import, resolve to a file and create an IMPORTS edge.
    let mut edge_stmts: Vec<String> = Vec::new();

    for row in &imports {
        let (source_file, import_source) = match (row.first(), row.get(1)) {
            (Some(lbug::Value::String(sf)), Some(lbug::Value::String(is))) => (sf, is),
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
