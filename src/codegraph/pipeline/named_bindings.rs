//! Named-bindings / DI extraction phase.
//!
//! Walks source files per language, dispatches to the per-language
//! DI extractors in [`crate::codegraph::semantic::named_bindings`],
//! and emits USES edges from consumer to provider with the binding
//! lifecycle recorded on the edge's `reason` field.
//!
//! Matching is **best-effort**: extractors return bare identifier
//! names (e.g. `UserService`) and we resolve them against Class /
//! Function / Interface / Trait / Struct nodes via simple-name
//! lookup. Unresolved consumers and providers are silently dropped —
//! DI registrations for symbols we didn't index (framework
//! internals, third-party services) shouldn't produce dangling
//! edges.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::phase::{Phase, PhaseCtx};
use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::lang::{self, SupportedLanguage};
use crate::codegraph::semantic::named_bindings;
use crate::codegraph::types::PipelinePhase;

fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Run the named-bindings pass for a project: scan per-language
/// files, resolve consumer/provider pairs, emit USES edges.
pub async fn extract_named_bindings(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    // Index Class / Function / Interface / Trait / Struct by simple
    // name so extractor outputs can resolve without a per-call DB
    // round-trip. Returns a map `simple_name → (qname, label)` — the
    // first qname wins on collisions, matching our usual "pick one
    // deterministically" policy.
    let mut symbol_by_name: HashMap<String, (String, &'static str)> = HashMap::new();
    for label in &["Class", "Function", "Method", "Interface", "Trait", "Struct"] {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN n.qualified_name, n.name"
            ))
            .await?;
        for row in &rows {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
            ) = (row.first(), row.get(1))
                && !name.is_empty()
            {
                symbol_by_name
                    .entry(name.clone())
                    .or_insert_with(|| (qname.clone(), leak_label(label)));
            }
        }
    }

    if symbol_by_name.is_empty() {
        tracing::debug!(%project_id, "no symbols indexed; skipping named-bindings phase");
        return Ok(result);
    }

    let mut edge_stmts: Vec<String> = Vec::new();
    let mut seen_edges: HashSet<(String, String)> = HashSet::new();

    for file_path in files {
        let Some(lang) = detect_language(file_path) else {
            continue;
        };

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        let bindings = match lang {
            SupportedLanguage::TypeScript | SupportedLanguage::JavaScript => {
                named_bindings::typescript::extract(&content)
            }
            SupportedLanguage::Python => named_bindings::python::extract(&content),
            SupportedLanguage::CSharp => named_bindings::csharp::extract(&content),
            SupportedLanguage::Java | SupportedLanguage::Kotlin => {
                named_bindings::java_kotlin::extract(&content)
            }
            SupportedLanguage::Php => named_bindings::php::extract(&content),
            SupportedLanguage::Rust => named_bindings::rust::extract(&content),
            _ => continue,
        };

        if bindings.is_empty() {
            continue;
        }
        let relative = file_path
            .strip_prefix(root_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .replace('\\', "/");

        for binding in bindings {
            let Some((provider_qname, provider_label)) =
                symbol_by_name.get(&binding.provider).cloned()
            else {
                continue;
            };

            // Consumer may be an interface/class name, a container
            // identifier like "container" / "module", or a
            // free-standing name that isn't in the graph. When we
            // can't resolve it, emit the edge from the file itself
            // — it still tells the graph that *something* in this
            // file binds the provider, which is enough for search.
            let (consumer_qname, consumer_label) = match symbol_by_name.get(&binding.consumer) {
                Some((q, l)) => (q.clone(), *l),
                None => {
                    let file_qname = format!(
                        "{}::{}",
                        project_id,
                        relative.replace('\'', "\\'")
                    );
                    (file_qname, "File")
                }
            };

            let key = (consumer_qname.clone(), provider_qname.clone());
            if !seen_edges.insert(key) {
                continue;
            }

            let c_escaped = cypher_escape(&consumer_qname);
            let p_escaped = cypher_escape(&provider_qname);
            let kind = binding.kind.as_str();
            edge_stmts.push(format!(
                "MATCH (c:{consumer_label}), (p:{provider_label}) \
                 WHERE c.qualified_name = '{c_escaped}' AND c.project_id = '{pid}' \
                 AND p.qualified_name = '{p_escaped}' AND p.project_id = '{pid}' \
                 CREATE (c)-[:CodeRelation {{type: 'USES', confidence: 0.80, \
                 reason: 'di:{kind}', step: 0}}]->(p)"
            ));
        }
    }

    if !edge_stmts.is_empty() {
        for chunk in edge_stmts.chunks(100) {
            let batch = db.execute_batch(chunk.to_vec()).await?;
            result.edges_created += batch.success;
            result.errors += batch.errors;
        }
    }

    tracing::info!(
        project_id = %project_id,
        edges = result.edges_created,
        "named-bindings extraction complete"
    );
    Ok(result)
}

/// Map a file's extension to its SupportedLanguage, if we have one.
fn detect_language(path: &Path) -> Option<SupportedLanguage> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    lang::language_for_extension(ext)
}

/// Give `&str` from a temporary &&str lifetime that outlives the
/// loop. The labels are always static strings so this is safe.
fn leak_label(s: &str) -> &'static str {
    match s {
        "Class" => "Class",
        "Function" => "Function",
        "Method" => "Method",
        "Interface" => "Interface",
        "Trait" => "Trait",
        "Struct" => "Struct",
        _ => "Class",
    }
}

/// NamedBindings phase: DI container detection across 6 languages.
pub struct NamedBindingsPhase;

#[async_trait::async_trait]
impl Phase for NamedBindingsPhase {
    fn label(&self) -> &'static str {
        "named_bindings"
    }

    fn phase(&self) -> Option<PipelinePhase> {
        None // no dedicated UI phase; runs silently
    }

    async fn run(&self, ctx: &mut PhaseCtx) -> Result<()> {
        let result = extract_named_bindings(
            &ctx.project_id,
            &ctx.root_path,
            &ctx.files,
            &ctx.db,
        )
        .await?;
        ctx.stats.edges_created += result.edges_created;
        Ok(())
    }
}
