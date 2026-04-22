//! tree-sitter AST parse, extract symbol nodes.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::phase::{Phase, PhaseCtx};
use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::lang;
use crate::codegraph::types::{CodeGraphConfig, PipelinePhase};

/// Hard ceiling on file size for tree-sitter parsing. The walker's
/// 512 KB soft cap catches the common case, but this defends against
/// anything that slips through (e.g. when `SPACEBOT_NO_GITIGNORE` is
/// set). tree-sitter allocates a buffer proportional to file size, so
/// unbounded reads risk OOM.
const MAX_PARSE_BYTES: u64 = 32 * 1024 * 1024;

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Normalize a path to always use forward slashes (cross-platform).
fn normalize_path(s: &str) -> String {
    s.replace('\\', "/")
}

/// Extract a code snippet from file content between line_start and line_end.
/// Returns an empty string if the range is invalid. Caps at 2000 chars.
fn extract_snippet(content: &str, line_start: u32, line_end: u32) -> String {
    if line_start == 0 || line_end == 0 || line_end < line_start {
        return String::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    let start = (line_start as usize).saturating_sub(1);
    let end = (line_end as usize).min(lines.len());
    if start >= lines.len() {
        return String::new();
    }
    let mut snippet = lines[start..end].join("\n");
    if snippet.len() > 2000 {
        // Find a valid UTF-8 char boundary at or before byte 2000.
        let mut boundary = 2000;
        while boundary > 0 && !snippet.is_char_boundary(boundary) {
            boundary -= 1;
        }
        snippet.truncate(boundary);
    }
    snippet
}

/// Parse all source files with tree-sitter and extract symbol nodes.
///
/// For each file, determines the language, parses the AST, and extracts
/// Class, Function, Method, Variable, Interface, Enum, etc. nodes with
/// DEFINES edges from their containing File node.
pub async fn parse_files(
    project_id: &str,
    root_path: &Path,
    files: &[PathBuf],
    db: &SharedCodeGraphDb,
    _config: &CodeGraphConfig,
    progress_fn: Option<&super::ProgressFn>,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    // Accumulate ALL nodes first, then ALL edges. This guarantees nodes
    // exist in the DB before any edge MATCH queries reference them.
    const BATCH_SIZE: usize = 100;
    let mut node_stmts: Vec<String> = Vec::new();
    let mut edge_stmts: Vec<String> = Vec::new();
    let total_files = files.len();
    let report_interval = (total_files / 20).max(1);

    // Project-root-relative path set for markdown link resolution.
    // Built once up front so every markdown file can probe
    // membership in O(1); the alternative (scan `files` per link)
    // is O(docs × links × files).
    let known_files: std::collections::HashSet<String> = files
        .iter()
        .filter_map(|p| {
            p.strip_prefix(root_path)
                .ok()
                .map(|r| normalize_path(&r.to_string_lossy()))
        })
        .collect();

    for (file_idx, file_path) in files.iter().enumerate() {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Markdown files get Section node extraction directly —
        // no tree-sitter provider needed.
        if ext == "md" || ext == "mdx" {
            let content = match tokio::fs::read_to_string(file_path).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let relative = normalize_path(
                &file_path
                    .strip_prefix(root_path)
                    .unwrap_or(file_path)
                    .to_string_lossy(),
            );
            let rel_escaped = cypher_escape(&relative);
            let file_qname = format!("{pid}::{rel_escaped}");
            extract_markdown_sections(
                &content, &relative, &pid, &file_qname, &mut node_stmts, &mut edge_stmts,
            );
            extract_markdown_link_edges(
                &content,
                &relative,
                &pid,
                &file_qname,
                &known_files,
                &mut edge_stmts,
            );
            result.files_parsed += 1;
            continue;
        }

        let provider = match lang::provider_for_extension(ext) {
            Some(p) => p,
            None => {
                result.files_skipped += 1;
                continue;
            }
        };

        // Defense-in-depth size ceiling — bail before loading the file
        // into RAM if it exceeds MAX_PARSE_BYTES.
        match tokio::fs::metadata(file_path).await {
            Ok(meta) if meta.len() > MAX_PARSE_BYTES => {
                tracing::warn!(
                    file = %file_path.display(),
                    size = meta.len(),
                    max = MAX_PARSE_BYTES,
                    "skipping file (exceeds parse size ceiling)"
                );
                result.files_skipped += 1;
                continue;
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    file = %file_path.display(),
                    %err,
                    "skipping file (stat error)"
                );
                result.errors += 1;
                continue;
            }
        }

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    file = %file_path.display(),
                    %err,
                    "skipping file (read error)"
                );
                result.errors += 1;
                continue;
            }
        };

        let relative = normalize_path(
            &file_path
                .strip_prefix(root_path)
                .unwrap_or(file_path)
                .to_string_lossy(),
        );

        let mut symbols = provider.extract_symbols(&relative, &content);
        let test_qnames: std::collections::HashSet<String> =
            provider.extract_tests(&relative, &content).into_iter().collect();

        // Link each Decorator to the nearest decoratable symbol
        // (Function, Method, or Class) that starts at or after the
        // decorator's last line. This is a line-proximity heuristic
        // that works across all languages without provider-specific
        // knowledge of the decoration AST shape.
        for i in 0..symbols.len() {
            if symbols[i].label != crate::codegraph::types::NodeLabel::Decorator
                || symbols[i].decorates.is_some()
            {
                continue;
            }
            let dec_end = symbols[i].line_end;
            let target_qn = symbols
                .iter()
                .filter(|s| {
                    matches!(
                        s.label,
                        crate::codegraph::types::NodeLabel::Function
                            | crate::codegraph::types::NodeLabel::Method
                            | crate::codegraph::types::NodeLabel::Class
                    ) && s.line_start >= dec_end
                })
                .min_by_key(|s| s.line_start)
                .map(|s| s.qualified_name.clone());
            if let Some(qn) = target_qn {
                symbols[i].decorates = Some(qn);
            }
        }

        tracing::trace!(
            file = %relative,
            lang = %provider.language(),
            symbols = symbols.len(),
            "parsed file"
        );

        let rel_escaped = cypher_escape(&relative);
        let file_qname = format!("{pid}::{rel_escaped}");

        for sym in &symbols {
            let label = sym.label.as_str();
            let name = cypher_escape(&sym.name);
            let qname = cypher_escape(&sym.qualified_name);
            // Merge `extends` and `implements` into a single comma-
            // separated field. Heritage.rs splits on commas and uses the
            // target's label to decide EXTENDS vs IMPLEMENTS, so the two
            // sources of parent names are interchangeable here.
            let mut heritage_parts: Vec<&str> = Vec::new();
            if let Some(ref ext) = sym.extends
                && !ext.is_empty()
            {
                heritage_parts.push(ext);
            }
            for imp in &sym.implements {
                if !imp.is_empty() {
                    heritage_parts.push(imp);
                }
            }
            // For Import nodes, extends_type carries the original name
            // when the import is aliased (`import { Foo as Bar }` →
            // name="Bar", extends_type="Foo") so the import resolver
            // can look up the correct symbol in the target file.
            let extends_val = if sym.label == crate::codegraph::types::NodeLabel::Import {
                cypher_escape(
                    sym.metadata
                        .get("original_name")
                        .map(String::as_str)
                        .unwrap_or(""),
                )
            } else {
                cypher_escape(&heritage_parts.join(", "))
            };
            let import_src = cypher_escape(sym.import_source.as_deref().unwrap_or(""));
            // Type text stashed in metadata by the language provider
            // (typed params / typed fields). Empty string when the
            // symbol doesn't carry a declared type.
            let declared_type = cypher_escape(
                sym.metadata
                    .get("declared_type")
                    .map(String::as_str)
                    .unwrap_or(""),
            );

            // Extract code snippet for the `content` column (capped at
            // 2000 chars to avoid bloating the DB with huge functions).
            let snippet = extract_snippet(&content, sym.line_start, sym.line_end);
            let snippet_escaped = cypher_escape(&snippet);

            let return_type_escaped = cypher_escape(sym.return_type.as_deref().unwrap_or(""));
            let visibility_escaped = cypher_escape(sym.visibility.as_deref().unwrap_or(""));
            let annotations_escaped = cypher_escape(sym.annotations.as_deref().unwrap_or(""));
            let param_count = sym.parameter_count.map(|c| c as i32).unwrap_or(-1);

            node_stmts.push(format!(
                "CREATE (:{label} {{qualified_name: '{qname}', name: '{name}', \
                 project_id: '{pid}', source_file: '{rel_escaped}', \
                 line_start: {ls}, line_end: {le}, \
                 source: 'pipeline', written_by: 'pipeline', \
                 extends_type: '{extends_val}', import_source: '{import_src}', \
                 declared_type: '{declared_type}', \
                 content: '{snippet_escaped}', description: '', \
                 is_exported: {is_exported}, return_type: '{return_type_escaped}', \
                 visibility: '{visibility_escaped}', parameter_count: {param_count}, \
                 is_static: {is_static}, is_readonly: {is_readonly}, \
                 is_abstract: {is_abstract}, is_final: {is_final}, \
                 annotations: '{annotations_escaped}'}})",
                ls = sym.line_start,
                le = sym.line_end,
                is_exported = sym.is_exported,
                is_static = sym.is_static,
                is_readonly = sym.is_readonly,
                is_abstract = sym.is_abstract,
                is_final = sym.is_final,
            ));

            edge_stmts.push(format!(
                "MATCH (f:File), (s:{label}) WHERE f.qualified_name = '{file_qname}' \
                 AND s.qualified_name = '{qname}' AND s.project_id = '{pid}' \
                 CREATE (f)-[:CodeRelation {{type: 'DEFINES', confidence: 1.0, reason: '', step: 0}}]->(s)",
            ));

            if let Some(ref parent_qn) = sym.parent {
                let parent_escaped = cypher_escape(parent_qn);
                let parent_label = symbols
                    .iter()
                    .find(|s| s.qualified_name == *parent_qn)
                    .map(|s| s.label.as_str())
                    .unwrap_or("Class");

                let edge_type = match sym.label {
                    crate::codegraph::types::NodeLabel::Method
                    | crate::codegraph::types::NodeLabel::Constructor => "HAS_METHOD",
                    crate::codegraph::types::NodeLabel::Variable
                    | crate::codegraph::types::NodeLabel::Property => "HAS_PROPERTY",
                    crate::codegraph::types::NodeLabel::Parameter => "HAS_PARAMETER",
                    _ => "CONTAINS",
                };

                edge_stmts.push(format!(
                    "MATCH (p:{parent_label}), (c:{label}) \
                     WHERE p.qualified_name = '{parent_escaped}' AND p.project_id = '{pid}' \
                     AND c.qualified_name = '{qname}' AND c.project_id = '{pid}' \
                     CREATE (p)-[:CodeRelation {{type: '{edge_type}', confidence: 1.0, reason: '', step: 0}}]->(c)",
                ));
            }

            // Decorator → decorated symbol (Function/Method/Class).
            // The target was resolved by line-proximity above.
            if let Some(ref target_qn) = sym.decorates {
                let target_escaped = cypher_escape(target_qn);
                let target_label = symbols
                    .iter()
                    .find(|s| s.qualified_name == *target_qn)
                    .map(|s| s.label.as_str())
                    .unwrap_or("Function");

                edge_stmts.push(format!(
                    "MATCH (d:Decorator), (t:{target_label}) \
                     WHERE d.qualified_name = '{qname}' AND d.project_id = '{pid}' \
                     AND t.qualified_name = '{target_escaped}' AND t.project_id = '{pid}' \
                     CREATE (d)-[:CodeRelation {{type: 'DECORATES', confidence: 1.0, reason: 'line-proximity', step: 0}}]->(t)",
                ));
            }
        }

        // Create Test nodes for functions identified as tests by the
        // language provider. Each Test mirrors the Function/Method it
        // wraps and gets a TESTED_BY edge pointing from the test back
        // to the original symbol.
        for sym in &symbols {
            let sym_qn = &sym.qualified_name;
            if !test_qnames.contains(sym_qn) {
                continue;
            }
            if !matches!(
                sym.label,
                crate::codegraph::types::NodeLabel::Function
                    | crate::codegraph::types::NodeLabel::Method
            ) {
                continue;
            }

            let test_name = cypher_escape(&sym.name);
            let test_qname = cypher_escape(&format!("{}::test", sym.qualified_name));
            let sym_label = sym.label.as_str();

            node_stmts.push(format!(
                "CREATE (:Test {{qualified_name: '{test_qname}', name: '{test_name}', \
                 project_id: '{pid}', source_file: '{rel_escaped}', \
                 line_start: {ls}, line_end: {le}, \
                 source: 'pipeline', written_by: 'pipeline', \
                 extends_type: '', import_source: '', declared_type: '', \
                 content: '', description: '', \
                 is_exported: false, return_type: '', visibility: '', \
                 parameter_count: -1, is_static: false, is_readonly: false, \
                 is_abstract: false, is_final: false, annotations: ''}})",
                ls = sym.line_start,
                le = sym.line_end,
            ));

            // File DEFINES Test.
            edge_stmts.push(format!(
                "MATCH (f:File), (t:Test) WHERE f.qualified_name = '{file_qname}' \
                 AND t.qualified_name = '{test_qname}' AND t.project_id = '{pid}' \
                 CREATE (f)-[:CodeRelation {{type: 'DEFINES', confidence: 1.0, reason: '', step: 0}}]->(t)",
            ));

            // Function/Method TESTED_BY Test.
            let sym_escaped = cypher_escape(sym_qn);
            edge_stmts.push(format!(
                "MATCH (s:{sym_label}), (t:Test) \
                 WHERE s.qualified_name = '{sym_escaped}' AND s.project_id = '{pid}' \
                 AND t.qualified_name = '{test_qname}' AND t.project_id = '{pid}' \
                 CREATE (s)-[:CodeRelation {{type: 'TESTED_BY', confidence: 1.0, reason: 'test-attribute', step: 0}}]->(t)",
            ));
        }

        result.files_parsed += 1;

        // Flush nodes periodically to keep memory bounded, but NEVER flush
        // edges until all nodes in this batch are committed.
        if node_stmts.len() >= BATCH_SIZE {
            let batch = db.execute_batch(std::mem::take(&mut node_stmts)).await?;
            result.nodes_created += batch.success;
            result.errors += batch.errors;
        }

        // Report intermediate progress so the frontend can show live stats.
        if let Some(pf) = progress_fn
            && (file_idx + 1) % report_interval == 0
        {
            let pct = (file_idx + 1) as f32 / total_files as f32;
            pf(
                pct * 0.8,
                &format!("Parsing files ({}/{})", file_idx + 1, total_files),
                &result,
            );
        }
    }

    // Flush remaining nodes FIRST, then all edges.
    if !node_stmts.is_empty() {
        let batch = db.execute_batch(node_stmts).await?;
        result.nodes_created += batch.success;
        result.errors += batch.errors;
    }

    // Report that node parsing is done, starting edges.
    if let Some(pf) = progress_fn {
        pf(0.85, &format!("Creating symbol edges ({})", edge_stmts.len()), &result);
    }

    // Now all nodes exist — safe to create edges.
    let total_edge_chunks = (edge_stmts.len() + BATCH_SIZE - 1).max(1) / BATCH_SIZE.max(1);
    for (ci, chunk) in edge_stmts.chunks(BATCH_SIZE).enumerate() {
        let batch = db.execute_batch(chunk.to_vec()).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;

        if let Some(pf) = progress_fn {
            let edge_pct = (ci + 1) as f32 / total_edge_chunks as f32;
            pf(0.85 + edge_pct * 0.15, &format!("Creating edges ({})", result.edges_created), &result);
        }
    }

    tracing::info!(
        project_id = %project_id,
        files_parsed = result.files_parsed,
        nodes = result.nodes_created,
        edges = result.edges_created,
        errors = result.errors,
        "AST parsing complete"
    );

    Ok(result)
}

/// Extract Markdown `[text](./link.md)` references and emit IMPORTS
/// edges between the source File and the target File when the target
/// exists in the project.
///
/// Only relative, in-project links become edges. External URLs,
/// mailto targets, and links to files we don't index fall on the
/// floor — they're documentation references, not structural code
/// dependencies.
pub(crate) fn extract_markdown_link_edges(
    content: &str,
    relative: &str,
    pid: &str,
    file_qname: &str,
    known_files: &std::collections::HashSet<String>,
    edge_stmts: &mut Vec<String>,
) {
    let src_dir = relative
        .rfind('/')
        .map(|i| &relative[..i])
        .unwrap_or("");
    let src_escaped = cypher_escape(file_qname);

    for link in crate::codegraph::semantic::markdown_links::extract(content) {
        let target = &link.target;
        let resolved = if target.starts_with('/') {
            // Root-anchored in-project link.
            target.trim_start_matches('/').to_string()
        } else if src_dir.is_empty() {
            target.trim_start_matches("./").to_string()
        } else {
            let cleaned = target.trim_start_matches("./");
            format!("{src_dir}/{cleaned}")
        };
        let normalized = resolved.replace("//", "/");
        // Collapse `..` segments so `docs/../index.md` resolves to
        // `index.md`. We do this one pass because markdown links
        // rarely chain more than two `..` levels in practice.
        let mut segments: Vec<&str> = Vec::new();
        for seg in normalized.split('/') {
            if seg == ".." {
                segments.pop();
            } else if !seg.is_empty() && seg != "." {
                segments.push(seg);
            }
        }
        let final_path = segments.join("/");
        if !known_files.contains(&final_path) {
            continue;
        }
        let tgt_qname = format!("{pid}::{esc}", esc = cypher_escape(&final_path));
        let tgt_escaped = cypher_escape(&tgt_qname);
        edge_stmts.push(format!(
            "MATCH (s:File), (t:File) WHERE s.qualified_name = '{src_escaped}' \
             AND s.project_id = '{pid}' AND t.qualified_name = '{tgt_escaped}' \
             AND t.project_id = '{pid}' \
             CREATE (s)-[:CodeRelation {{type: 'IMPORTS', confidence: 0.85, reason: 'markdown link', step: 0}}]->(t)"
        ));
    }
}

/// Extract `#`-prefixed headings from Markdown as Section nodes with
/// hierarchical CONTAINS edges. Each heading becomes a Section node
/// whose parent is the most recent heading at a shallower depth.
fn extract_markdown_sections(
    content: &str,
    relative: &str,
    pid: &str,
    file_qname: &str,
    node_stmts: &mut Vec<String>,
    edge_stmts: &mut Vec<String>,
) {
    // Stack tracks (depth, qname) of the most recently seen heading at
    // each depth level, so we can parent deeper headings under shallower
    // ones.
    let mut stack: Vec<(usize, String)> = Vec::new();
    let rel_escaped = cypher_escape(relative);

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let depth = trimmed.chars().take_while(|c| *c == '#').count();
        let title = trimmed[depth..].trim().trim_start_matches(' ');
        if title.is_empty() || depth > 6 {
            continue;
        }
        let line_num = line_idx as u32 + 1;
        let name_escaped = cypher_escape(title);
        let sec_qname = format!(
            "{pid}::{rel_escaped}::section_{line_num}",
        );
        let sec_qname_escaped = cypher_escape(&sec_qname);

        node_stmts.push(format!(
            "CREATE (:Section {{qualified_name: '{sec_qname_escaped}', \
             name: '{name_escaped}', project_id: '{pid}', \
             source_file: '{rel_escaped}', line_start: {line_num}, line_end: {line_num}, \
             source: 'pipeline', written_by: 'pipeline', \
             extends_type: '', import_source: '', declared_type: ''}})"
        ));

        // Pop stack entries at the same or deeper depth
        while stack.last().map(|(d, _)| *d >= depth).unwrap_or(false) {
            stack.pop();
        }

        // Parent is either the most recent shallower heading or the file
        let parent_qname = stack
            .last()
            .map(|(_, qn)| qn.as_str())
            .unwrap_or(file_qname);
        let parent_escaped = cypher_escape(parent_qname);
        let parent_label = if stack.is_empty() { "File" } else { "Section" };

        edge_stmts.push(format!(
            "MATCH (p:{parent_label}), (s:Section) \
             WHERE p.qualified_name = '{parent_escaped}' AND p.project_id = '{pid}' \
             AND s.qualified_name = '{sec_qname_escaped}' AND s.project_id = '{pid}' \
             CREATE (p)-[:CodeRelation {{type: 'CONTAINS', confidence: 1.0, reason: 'heading', step: 0}}]->(s)"
        ));

        stack.push((depth, sec_qname));
    }
}

/// Parsing phase: tree-sitter AST parse per file, extracts Class /
/// Function / Method / Variable / Import / etc. nodes plus the per-file
/// DEFINES edges. Emits incremental progress via the shared callback.
pub struct ParsingPhase;

#[async_trait::async_trait]
impl Phase for ParsingPhase {
    fn label(&self) -> &'static str {
        "parsing"
    }

    fn phase(&self) -> Option<PipelinePhase> {
        Some(PipelinePhase::Parsing)
    }

    async fn run(&self, ctx: &mut PhaseCtx) -> Result<()> {
        ctx.emit_progress(PipelinePhase::Parsing, 0.0, "Parsing source files");

        let progress = ctx.make_progress_fn(PipelinePhase::Parsing, |merged, pr| {
            merged.files_parsed += pr.files_parsed;
            merged.files_skipped += pr.files_skipped;
            merged.nodes_created += pr.nodes_created;
            merged.edges_created += pr.edges_created;
            merged.errors += pr.errors;
        });

        let result = parse_files(
            &ctx.project_id,
            &ctx.root_path,
            &ctx.files,
            &ctx.db,
            &ctx.config,
            Some(&progress),
        )
        .await?;
        ctx.stats.files_parsed = result.files_parsed;
        ctx.stats.files_skipped = result.files_skipped;
        ctx.stats.nodes_created += result.nodes_created;
        ctx.stats.edges_created += result.edges_created;

        ctx.emit_progress(
            PipelinePhase::Parsing,
            1.0,
            &format!("Parsed {} files", result.files_parsed),
        );
        Ok(())
    }
}
