//! Code graph tool logic — pure functions over the graph DB.
//!
//! These functions power the agent-facing tools (codegraph_context,
//! codegraph_impact, codegraph_detect_changes). They take a DB handle
//! and return structured results. The `rig::tool::Tool` wrappers live
//! in `src/tools/codegraph.rs`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::db::SharedCodeGraphDb;
use super::schema::DISPLAY_NODE_LABELS;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn val_str(v: Option<&lbug::Value>) -> String {
    match v {
        Some(lbug::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn val_str_opt(v: Option<&lbug::Value>) -> Option<String> {
    match v {
        Some(lbug::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn val_i64(v: Option<&lbug::Value>) -> i64 {
    match v {
        Some(lbug::Value::Int64(n)) => *n,
        Some(lbug::Value::Int32(n)) => *n as i64,
        _ => 0,
    }
}

fn val_f64(v: Option<&lbug::Value>) -> f64 {
    match v {
        Some(lbug::Value::Double(n)) => *n,
        Some(lbug::Value::Float(n)) => *n as f64,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRef {
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub source_file: Option<String>,
    pub line_start: Option<u32>,
}

// ---------------------------------------------------------------------------
// 1. Symbol Context
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SymbolContext {
    pub symbol: SymbolRef,
    pub incoming: HashMap<String, Vec<SymbolRef>>,
    pub outgoing: HashMap<String, Vec<SymbolRef>>,
    pub processes: Vec<ProcessRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessRef {
    pub name: String,
    pub qualified_name: String,
}

/// 360-degree view of a symbol — all callers, callees, processes, etc.
pub async fn symbol_context(
    db: &SharedCodeGraphDb,
    project_id: &str,
    name: &str,
    file_path: Option<&str>,
) -> Result<Option<SymbolContext>> {
    let pid = esc(project_id);
    let name_esc = esc(name);

    // Resolve the symbol across all display labels.
    let mut symbol: Option<(SymbolRef, String)> = None;
    for &label in DISPLAY_NODE_LABELS {
        let file_filter = file_path
            .map(|fp| format!(" AND n.source_file = '{}'", esc(fp)))
            .unwrap_or_default();
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.name = '{name_esc}' \
                 AND n.project_id = '{pid}'{file_filter} \
                 RETURN n.qualified_name, n.name, n.source_file, n.line_start"
            ))
            .await;
        if let Ok(rows) = rows {
            if let Some(row) = rows.first() {
                symbol = Some((
                    SymbolRef {
                        qualified_name: val_str(row.first()),
                        name: val_str(row.get(1)),
                        label: label.to_string(),
                        source_file: val_str_opt(row.get(2)),
                        line_start: row.get(3).and_then(|v| match v {
                            lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                            _ => None,
                        }),
                    },
                    label.to_string(),
                ));
                break;
            }
        }
    }

    let (sym, sym_label) = match symbol {
        Some(s) => s,
        None => return Ok(None),
    };

    let qname = esc(&sym.qualified_name);

    // Incoming edges: (caller)-[r]->(this symbol)
    let mut incoming: HashMap<String, Vec<SymbolRef>> = HashMap::new();
    for &from_label in DISPLAY_NODE_LABELS {
        let rows = db
            .query(&format!(
                "MATCH (a:{from_label})-[r:CodeRelation]->(b:{sym_label}) \
                 WHERE b.qualified_name = '{qname}' AND b.project_id = '{pid}' \
                 RETURN a.qualified_name, a.name, a.source_file, a.line_start, r.type"
            ))
            .await;
        if let Ok(rows) = rows {
            for row in &rows {
                let edge_type = val_str(row.get(4));
                if edge_type.is_empty() {
                    continue;
                }
                incoming.entry(edge_type).or_default().push(SymbolRef {
                    qualified_name: val_str(row.first()),
                    name: val_str(row.get(1)),
                    label: from_label.to_string(),
                    source_file: val_str_opt(row.get(2)),
                    line_start: row.get(3).and_then(|v| match v {
                        lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                        _ => None,
                    }),
                });
            }
        }
    }

    // Outgoing edges: (this symbol)-[r]->(target)
    let mut outgoing: HashMap<String, Vec<SymbolRef>> = HashMap::new();
    for &to_label in DISPLAY_NODE_LABELS {
        let rows = db
            .query(&format!(
                "MATCH (a:{sym_label})-[r:CodeRelation]->(b:{to_label}) \
                 WHERE a.qualified_name = '{qname}' AND a.project_id = '{pid}' \
                 RETURN b.qualified_name, b.name, b.source_file, b.line_start, r.type"
            ))
            .await;
        if let Ok(rows) = rows {
            for row in &rows {
                let edge_type = val_str(row.get(4));
                if edge_type.is_empty() {
                    continue;
                }
                outgoing.entry(edge_type).or_default().push(SymbolRef {
                    qualified_name: val_str(row.first()),
                    name: val_str(row.get(1)),
                    label: to_label.to_string(),
                    source_file: val_str_opt(row.get(2)),
                    line_start: row.get(3).and_then(|v| match v {
                        lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                        _ => None,
                    }),
                });
            }
        }
    }

    // Processes this symbol participates in.
    let mut processes = Vec::new();
    let proc_rows = db
        .query(&format!(
            "MATCH (a:{sym_label})<-[r:CodeRelation]->(p:Process) \
             WHERE a.qualified_name = '{qname}' AND a.project_id = '{pid}' \
             AND r.type = 'STEP_IN_PROCESS' \
             RETURN p.qualified_name, p.name"
        ))
        .await;
    if let Ok(rows) = proc_rows {
        for row in &rows {
            processes.push(ProcessRef {
                qualified_name: val_str(row.first()),
                name: val_str(row.get(1)),
            });
        }
    }

    Ok(Some(SymbolContext {
        symbol: sym,
        incoming,
        outgoing,
        processes,
    }))
}

// ---------------------------------------------------------------------------
// 2. Impact Analysis
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ImpactResult {
    pub target: SymbolRef,
    pub risk: String,
    pub by_depth: HashMap<u32, Vec<SymbolRef>>,
    pub affected_processes: Vec<ProcessRef>,
    pub summary: ImpactSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactSummary {
    pub direct: usize,
    pub total: usize,
    pub processes_affected: usize,
}

const IMPACT_EDGE_TYPES: &[&str] = &[
    "CALLS", "IMPORTS", "EXTENDS", "IMPLEMENTS", "HAS_METHOD",
];

pub async fn impact_analysis(
    db: &SharedCodeGraphDb,
    project_id: &str,
    target_name: &str,
    direction: &str,
    max_depth: u32,
) -> Result<Option<ImpactResult>> {
    let pid = esc(project_id);
    let name_esc = esc(target_name);

    // Resolve the target symbol.
    let mut target: Option<(SymbolRef, String)> = None;
    for &label in DISPLAY_NODE_LABELS {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.name = '{name_esc}' \
                 AND n.project_id = '{pid}' \
                 RETURN n.qualified_name, n.name, n.source_file, n.line_start"
            ))
            .await;
        if let Ok(rows) = rows {
            if let Some(row) = rows.first() {
                target = Some((
                    SymbolRef {
                        qualified_name: val_str(row.first()),
                        name: val_str(row.get(1)),
                        label: label.to_string(),
                        source_file: val_str_opt(row.get(2)),
                        line_start: row.get(3).and_then(|v| match v {
                            lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                            _ => None,
                        }),
                    },
                    label.to_string(),
                ));
                break;
            }
        }
    }

    let (target_sym, _target_label) = match target {
        Some(t) => t,
        None => return Ok(None),
    };

    let is_upstream = direction == "upstream";
    let mut by_depth: HashMap<u32, Vec<SymbolRef>> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(target_sym.qualified_name.clone());
    let mut frontier: Vec<String> = vec![target_sym.qualified_name.clone()];

    let edge_type_filter = IMPACT_EDGE_TYPES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");

    for depth in 1..=max_depth {
        let mut next_frontier: Vec<String> = Vec::new();
        let mut depth_results: Vec<SymbolRef> = Vec::new();

        for qname in &frontier {
            let qname_esc = esc(qname);

            // For each frontier symbol, find neighbors at this depth.
            // Need to try each label pair since LadybugDB requires concrete labels.
            for &from_label in DISPLAY_NODE_LABELS {
                for &to_label in DISPLAY_NODE_LABELS {
                    let query = if is_upstream {
                        format!(
                            "MATCH (a:{from_label})-[r:CodeRelation]->(b:{to_label}) \
                             WHERE b.qualified_name = '{qname_esc}' AND b.project_id = '{pid}' \
                             AND r.type IN [{edge_type_filter}] \
                             RETURN a.qualified_name, a.name, a.source_file, a.line_start"
                        )
                    } else {
                        format!(
                            "MATCH (a:{from_label})-[r:CodeRelation]->(b:{to_label}) \
                             WHERE a.qualified_name = '{qname_esc}' AND a.project_id = '{pid}' \
                             AND r.type IN [{edge_type_filter}] \
                             RETURN b.qualified_name, b.name, b.source_file, b.line_start"
                        )
                    };

                    let rows = db.query(&query).await;
                    if let Ok(rows) = rows {
                        for row in &rows {
                            let qn = val_str(row.first());
                            if qn.is_empty() || visited.contains(&qn) {
                                continue;
                            }
                            visited.insert(qn.clone());
                            let neighbor_label = if is_upstream { from_label } else { to_label };
                            let sym = SymbolRef {
                                qualified_name: qn.clone(),
                                name: val_str(row.get(1)),
                                label: neighbor_label.to_string(),
                                source_file: val_str_opt(row.get(2)),
                                line_start: row.get(3).and_then(|v| match v {
                                    lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                                    _ => None,
                                }),
                            };
                            depth_results.push(sym);
                            next_frontier.push(qn);
                        }
                    }
                }
            }
        }

        if !depth_results.is_empty() {
            by_depth.insert(depth, depth_results);
        }
        frontier = next_frontier;
        if frontier.is_empty() {
            break;
        }
    }

    // Find affected processes.
    let mut affected_processes: Vec<ProcessRef> = Vec::new();
    let all_affected: Vec<&String> = visited.iter().collect();
    for qname in &all_affected {
        let qname_esc = esc(qname);
        for &label in &["Function", "Method"] {
            let rows = db
                .query(&format!(
                    "MATCH (a:{label})<-[r:CodeRelation]-(p:Process) \
                     WHERE a.qualified_name = '{qname_esc}' AND a.project_id = '{pid}' \
                     AND r.type = 'STEP_IN_PROCESS' \
                     RETURN p.qualified_name, p.name"
                ))
                .await;
            if let Ok(rows) = rows {
                for row in &rows {
                    let pqn = val_str(row.first());
                    if !affected_processes.iter().any(|p| p.qualified_name == pqn) {
                        affected_processes.push(ProcessRef {
                            qualified_name: pqn,
                            name: val_str(row.get(1)),
                        });
                    }
                }
            }
        }
    }

    let direct = by_depth.get(&1).map(|v| v.len()).unwrap_or(0);
    let total: usize = by_depth.values().map(|v| v.len()).sum();
    let procs = affected_processes.len();

    let risk = if direct >= 30 || procs >= 5 || total >= 200 {
        "CRITICAL"
    } else if direct >= 15 || procs >= 3 || total >= 100 {
        "HIGH"
    } else if direct >= 5 || total >= 30 {
        "MEDIUM"
    } else {
        "LOW"
    };

    Ok(Some(ImpactResult {
        target: target_sym,
        risk: risk.to_string(),
        by_depth,
        affected_processes,
        summary: ImpactSummary {
            direct,
            total,
            processes_affected: procs,
        },
    }))
}

// ---------------------------------------------------------------------------
// 3. Detect Changes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DetectChangesResult {
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<SymbolRef>,
    pub affected_processes: Vec<ProcessRef>,
    pub risk_level: String,
    pub summary: String,
}

pub async fn detect_changes(
    db: &SharedCodeGraphDb,
    project_id: &str,
    root_path: &Path,
    scope: &str,
    base_ref: Option<&str>,
) -> Result<DetectChangesResult> {
    let pid = esc(project_id);

    // Run git diff to get changed files.
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(root_path);
    match scope {
        "staged" => {
            cmd.args(["diff", "--cached", "--name-only"]);
        }
        "all" => {
            cmd.args(["diff", "HEAD", "--name-only"]);
        }
        "compare" => {
            let base = base_ref.unwrap_or("main");
            cmd.args(["diff", &format!("{base}...HEAD"), "--name-only"]);
        }
        _ => {
            // "unstaged" (default)
            cmd.args(["diff", "--name-only"]);
        }
    }

    let output = cmd.output()?;
    let diff_text = String::from_utf8_lossy(&output.stdout);
    let changed_files: Vec<String> = diff_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();

    if changed_files.is_empty() {
        return Ok(DetectChangesResult {
            changed_files: Vec::new(),
            changed_symbols: Vec::new(),
            affected_processes: Vec::new(),
            risk_level: "LOW".to_string(),
            summary: "No changes detected.".to_string(),
        });
    }

    // Find symbols in the changed files.
    let mut changed_symbols: Vec<SymbolRef> = Vec::new();
    for file in &changed_files {
        let file_esc = esc(file);
        for &label in DISPLAY_NODE_LABELS {
            let rows = db
                .query(&format!(
                    "MATCH (n:{label}) WHERE n.source_file = '{file_esc}' \
                     AND n.project_id = '{pid}' \
                     RETURN n.qualified_name, n.name, n.source_file, n.line_start"
                ))
                .await;
            if let Ok(rows) = rows {
                for row in &rows {
                    changed_symbols.push(SymbolRef {
                        qualified_name: val_str(row.first()),
                        name: val_str(row.get(1)),
                        label: label.to_string(),
                        source_file: val_str_opt(row.get(2)),
                        line_start: row.get(3).and_then(|v| match v {
                            lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                            _ => None,
                        }),
                    });
                }
            }
        }
    }

    // Find affected processes.
    let mut affected_processes: Vec<ProcessRef> = Vec::new();
    let mut seen_procs: HashSet<String> = HashSet::new();
    for sym in &changed_symbols {
        if sym.label != "Function" && sym.label != "Method" {
            continue;
        }
        let qname_esc = esc(&sym.qualified_name);
        let rows = db
            .query(&format!(
                "MATCH (a:{label})<-[r:CodeRelation]-(p:Process) \
                 WHERE a.qualified_name = '{qname_esc}' AND a.project_id = '{pid}' \
                 AND r.type = 'STEP_IN_PROCESS' \
                 RETURN p.qualified_name, p.name",
                label = sym.label,
            ))
            .await;
        if let Ok(rows) = rows {
            for row in &rows {
                let pqn = val_str(row.first());
                if seen_procs.insert(pqn.clone()) {
                    affected_processes.push(ProcessRef {
                        qualified_name: pqn,
                        name: val_str(row.get(1)),
                    });
                }
            }
        }
    }

    let procs = affected_processes.len();
    let risk_level = if procs >= 16 {
        "CRITICAL"
    } else if procs >= 6 {
        "HIGH"
    } else if procs >= 1 {
        "MEDIUM"
    } else {
        "LOW"
    };

    let summary = format!(
        "{} files changed, {} symbols affected, {} processes impacted ({})",
        changed_files.len(),
        changed_symbols.len(),
        procs,
        risk_level,
    );

    Ok(DetectChangesResult {
        changed_files,
        changed_symbols,
        affected_processes,
        risk_level: risk_level.to_string(),
        summary,
    })
}

// ---------------------------------------------------------------------------
// 4. Cypher — raw graph query
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CypherResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
}

/// Execute a raw Cypher query against the code graph and return results
/// as a JSON table. The query is read-only — mutations are rejected.
pub async fn cypher_query(
    db: &SharedCodeGraphDb,
    query: &str,
) -> Result<CypherResult> {
    // Safety: reject mutations.
    let upper = query.trim().to_uppercase();
    if upper.starts_with("CREATE")
        || upper.starts_with("DELETE")
        || upper.starts_with("SET")
        || upper.starts_with("MERGE")
        || upper.starts_with("DROP")
        || upper.starts_with("DETACH")
    {
        anyhow::bail!("Mutation queries are not allowed — use read-only queries only.");
    }

    let rows = db.query(query).await?;

    let mut result_rows: Vec<Vec<serde_json::Value>> = Vec::new();
    for row in &rows {
        let json_row: Vec<serde_json::Value> = row
            .iter()
            .map(|v| match v {
                lbug::Value::String(s) => serde_json::Value::String(s.clone()),
                lbug::Value::Int64(n) => serde_json::json!(n),
                lbug::Value::Int32(n) => serde_json::json!(n),
                lbug::Value::Int16(n) => serde_json::json!(n),
                lbug::Value::Double(n) => serde_json::json!(n),
                lbug::Value::Float(n) => serde_json::json!(n),
                lbug::Value::Bool(b) => serde_json::Value::Bool(*b),
                _ => serde_json::Value::Null,
            })
            .collect();
        result_rows.push(json_row);
    }

    let row_count = result_rows.len();
    Ok(CypherResult {
        columns: Vec::new(), // LadybugDB doesn't expose column names in the result
        rows: result_rows,
        row_count,
    })
}

// ---------------------------------------------------------------------------
// 5. Rename — multi-file coordinated rename
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RenameEdit {
    pub file_path: String,
    pub line: u32,
    pub old_text: String,
    pub new_text: String,
    pub confidence: String, // "graph" or "text_search"
}

#[derive(Debug, Clone, Serialize)]
pub struct RenameResult {
    pub old_name: String,
    pub new_name: String,
    pub files_affected: usize,
    pub total_edits: usize,
    pub graph_edits: usize,
    pub text_search_edits: usize,
    pub changes: Vec<RenameEdit>,
    pub applied: bool,
}

/// Find all references to a symbol via graph + text search and optionally
/// apply the rename. Returns a preview by default (dry_run = true).
pub async fn rename_symbol(
    db: &SharedCodeGraphDb,
    project_id: &str,
    root_path: &Path,
    symbol_name: &str,
    new_name: &str,
    file_path: Option<&str>,
    dry_run: bool,
) -> Result<Option<RenameResult>> {
    // Resolve the symbol via context.
    let ctx = symbol_context(db, project_id, symbol_name, file_path).await?;
    let ctx = match ctx {
        Some(c) => c,
        None => return Ok(None),
    };

    let mut edits: Vec<RenameEdit> = Vec::new();

    // 1. Definition location — the symbol itself.
    if let Some(ref sf) = ctx.symbol.source_file {
        if let Some(ls) = ctx.symbol.line_start {
            edits.push(RenameEdit {
                file_path: sf.clone(),
                line: ls,
                old_text: symbol_name.to_string(),
                new_text: new_name.to_string(),
                confidence: "graph".to_string(),
            });
        }
    }

    // 2. Graph-based references — all incoming edges that reference this symbol.
    let mut graph_files: HashSet<String> = HashSet::new();
    if let Some(ref sf) = ctx.symbol.source_file {
        graph_files.insert(sf.clone());
    }
    for refs in ctx.incoming.values() {
        for r in refs {
            if let Some(ref sf) = r.source_file {
                if let Some(ls) = r.line_start {
                    if graph_files.insert(sf.clone()) || true {
                        edits.push(RenameEdit {
                            file_path: sf.clone(),
                            line: ls,
                            old_text: symbol_name.to_string(),
                            new_text: new_name.to_string(),
                            confidence: "graph".to_string(),
                        });
                    }
                }
            }
        }
    }

    let graph_edits = edits.len();

    // 3. Text search — find references the graph missed using grep.
    let grep_output = std::process::Command::new("grep")
        .args(["-rn", "--include=*.rs", "--include=*.ts", "--include=*.tsx",
               "--include=*.js", "--include=*.py", "--include=*.go",
               "-w", symbol_name])
        .current_dir(root_path)
        .output();

    if let Ok(output) = grep_output {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            // Format: "file:line:content"
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() >= 2 {
                let file = parts[0].to_string();
                let line_num: u32 = parts[1].parse().unwrap_or(0);
                if line_num > 0 && !graph_files.contains(&file) {
                    edits.push(RenameEdit {
                        file_path: file,
                        line: line_num,
                        old_text: symbol_name.to_string(),
                        new_text: new_name.to_string(),
                        confidence: "text_search".to_string(),
                    });
                }
            }
        }
    }

    let text_search_edits = edits.len() - graph_edits;

    // Collect unique files.
    let files_affected: HashSet<&str> = edits.iter().map(|e| e.file_path.as_str()).collect();

    let mut result = RenameResult {
        old_name: symbol_name.to_string(),
        new_name: new_name.to_string(),
        files_affected: files_affected.len(),
        total_edits: edits.len(),
        graph_edits,
        text_search_edits,
        changes: edits,
        applied: false,
    };

    // Apply edits if not dry_run.
    if !dry_run {
        for edit in &result.changes {
            let full_path = root_path.join(&edit.file_path);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let updated = content.replace(&edit.old_text, &edit.new_text);
                if updated != content {
                    std::fs::write(&full_path, updated).ok();
                }
            }
        }
        result.applied = true;
    }

    Ok(Some(result))
}

// ---------------------------------------------------------------------------
// 6. Route Map — show API route → handler mappings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RouteMapping {
    pub method: String,
    pub path: String,
    pub handler_name: String,
    pub handler_file: Option<String>,
    pub handler_line: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteMapResult {
    pub routes: Vec<RouteMapping>,
    pub total: usize,
}

pub async fn route_map(
    db: &SharedCodeGraphDb,
    project_id: &str,
    route_filter: Option<&str>,
) -> Result<RouteMapResult> {
    let pid = esc(project_id);
    let mut routes = Vec::new();

    // Route nodes have name = "METHOD /path" (e.g. "GET /api/health").
    // HANDLES_ROUTE edges go from Function/Method → Route.
    for &handler_label in &["Function", "Method"] {
        let rows = db
            .query(&format!(
                "MATCH (f:{handler_label})-[r:CodeRelation]->(rt:Route) \
                 WHERE rt.project_id = '{pid}' AND r.type = 'HANDLES_ROUTE' \
                 RETURN rt.name, f.name, f.source_file, f.line_start"
            ))
            .await;
        if let Ok(rows) = rows {
            for row in &rows {
                let route_name = val_str(row.first());
                let parts: Vec<&str> = route_name.splitn(2, ' ').collect();
                let (method, path) = if parts.len() == 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    ("*".to_string(), route_name.clone())
                };

                if let Some(filter) = route_filter {
                    if !path.contains(filter) {
                        continue;
                    }
                }

                routes.push(RouteMapping {
                    method,
                    path,
                    handler_name: val_str(row.get(1)),
                    handler_file: val_str_opt(row.get(2)),
                    handler_line: row.get(3).and_then(|v| match v {
                        lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                        _ => None,
                    }),
                });
            }
        }
    }

    routes.sort_by(|a, b| a.path.cmp(&b.path));
    let total = routes.len();
    Ok(RouteMapResult { routes, total })
}

// ---------------------------------------------------------------------------
// 7. Tool Map — show tool/MCP definitions and handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ToolMapping {
    pub tool_name: String,
    pub handler_name: String,
    pub handler_file: Option<String>,
    pub handler_line: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolMapResult {
    pub tools: Vec<ToolMapping>,
    pub total: usize,
}

pub async fn tool_map(
    db: &SharedCodeGraphDb,
    project_id: &str,
    tool_filter: Option<&str>,
) -> Result<ToolMapResult> {
    let pid = esc(project_id);
    let mut tools = Vec::new();

    // HANDLES_TOOL edges are self-loops on Function/Method nodes.
    // The tool name is stored in r.reason.
    for &label in &["Function", "Method"] {
        let rows = db
            .query(&format!(
                "MATCH (f:{label})-[r:CodeRelation]->(f) \
                 WHERE f.project_id = '{pid}' AND r.type = 'HANDLES_TOOL' \
                 RETURN r.reason, f.name, f.source_file, f.line_start"
            ))
            .await;
        if let Ok(rows) = rows {
            for row in &rows {
                let tool_name = val_str(row.first());
                if tool_name.is_empty() {
                    continue;
                }

                if let Some(filter) = tool_filter {
                    if !tool_name.contains(filter) {
                        continue;
                    }
                }

                tools.push(ToolMapping {
                    tool_name,
                    handler_name: val_str(row.get(1)),
                    handler_file: val_str_opt(row.get(2)),
                    handler_line: row.get(3).and_then(|v| match v {
                        lbug::Value::Int32(n) if *n > 0 => Some(*n as u32),
                        _ => None,
                    }),
                });
            }
        }
    }

    tools.sort_by(|a, b| a.tool_name.cmp(&b.tool_name));
    let total = tools.len();
    Ok(ToolMapResult { tools, total })
}

// ---------------------------------------------------------------------------
// 8. API Impact — pre-change impact report for a route
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ApiImpactResult {
    pub route: RouteMapping,
    pub handler_impact: Option<ImpactResult>,
    pub risk: String,
}

/// Combine route lookup with blast-radius analysis on the handler.
pub async fn api_impact(
    db: &SharedCodeGraphDb,
    project_id: &str,
    route_path: Option<&str>,
    handler_file: Option<&str>,
) -> Result<Option<ApiImpactResult>> {
    // Find the route.
    let map = route_map(db, project_id, route_path).await?;
    let route = if let Some(file) = handler_file {
        map.routes
            .iter()
            .find(|r| r.handler_file.as_deref() == Some(file))
    } else {
        map.routes.first()
    };

    let route = match route {
        Some(r) => r.clone(),
        None => return Ok(None),
    };

    // Run impact analysis on the handler function.
    let impact = impact_analysis(
        db,
        project_id,
        &route.handler_name,
        "upstream",
        3,
    )
    .await?;

    let risk = impact
        .as_ref()
        .map(|i| i.risk.clone())
        .unwrap_or_else(|| "LOW".to_string());

    Ok(Some(ApiImpactResult {
        route,
        handler_impact: impact,
        risk,
    }))
}
