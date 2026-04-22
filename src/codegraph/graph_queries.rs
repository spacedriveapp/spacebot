//! Read-side Cypher queries for the code graph.
//!
//! All functions take a `SharedCodeGraphDb` handle and a project ID,
//! run Cypher queries, and return plain Rust structs. The API handlers
//! in `api::codegraph` map these into their response types.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use futures::{Stream, StreamExt};

use super::db::SharedCodeGraphDb;
use super::schema::ALL_NODE_LABELS;

/// Labels that don't carry `project_id` and should be skipped in
/// project-scoped queries (stats, node browse, edge queries).
const SKIP_PROJECT_LABELS: &[&str] = &["CodeEmbedding"];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string for use inside a Cypher single-quoted literal.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Extract a `String` from a `lbug::Value`, returning an empty string for
/// null / non-string values.
fn val_str(v: Option<&lbug::Value>) -> String {
    match v {
        Some(lbug::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Extract an optional `String` (returns `None` for empty or missing).
fn val_str_opt(v: Option<&lbug::Value>) -> Option<String> {
    match v {
        Some(lbug::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Extract an `i64` from a `lbug::Value`.
fn val_i64(v: Option<&lbug::Value>) -> i64 {
    match v {
        Some(lbug::Value::Int64(n)) => *n,
        Some(lbug::Value::Int32(n)) => *n as i64,
        Some(lbug::Value::Int16(n)) => *n as i64,
        _ => 0,
    }
}

/// Extract an `Option<u32>`.
fn val_u32_opt(v: Option<&lbug::Value>) -> Option<u32> {
    match v {
        Some(lbug::Value::Int32(n)) if *n > 0 => Some(*n as u32),
        Some(lbug::Value::Int64(n)) if *n > 0 => Some(*n as u32),
        _ => None,
    }
}

/// Extract an `f64`.
fn val_f64(v: Option<&lbug::Value>) -> f64 {
    match v {
        Some(lbug::Value::Double(n)) => *n,
        Some(lbug::Value::Float(n)) => *n as f64,
        Some(lbug::Value::Int64(n)) => *n as f64,
        Some(lbug::Value::Int32(n)) => *n as f64,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// A node summary returned by list/browse queries.
#[derive(Debug, Clone)]
pub struct QueriedNode {
    pub id: i64,
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub source_file: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub source: Option<String>,
    pub written_by: Option<String>,
    pub properties: HashMap<String, serde_json::Value>,
}

/// An edge summary returned by edge queries.
#[derive(Debug, Clone)]
pub struct QueriedEdge {
    pub from_id: i64,
    pub from_name: String,
    pub from_label: String,
    pub to_id: i64,
    pub to_name: String,
    pub to_label: String,
    pub edge_type: String,
    pub confidence: f64,
}

/// Graph statistics.
#[derive(Debug, Clone)]
pub struct GraphStatsResult {
    pub total_nodes: u64,
    pub total_edges: u64,
    pub nodes_by_label: Vec<(String, u64)>,
    pub edges_by_type: Vec<(String, u64)>,
}

// ---------------------------------------------------------------------------
// Community queries
// ---------------------------------------------------------------------------

/// Fetch all Community nodes for a project with their key_symbols.
pub async fn query_communities(
    db: &SharedCodeGraphDb,
    project_id: &str,
) -> Result<Vec<super::types::CommunityInfo>> {
    let pid = esc(project_id);

    // 1. Get all Community nodes.
    let rows = db
        .query(&format!(
            "MATCH (c:Community) WHERE c.project_id = '{pid}' \
             RETURN c.qualified_name, c.name, c.description, \
             c.node_count, c.file_count, c.function_count, \
             c.cohesion, c.keywords"
        ))
        .await?;

    let mut communities: Vec<super::types::CommunityInfo> = Vec::new();
    let mut qname_to_idx: HashMap<String, usize> = HashMap::new();

    for row in &rows {
        let qname = val_str(row.first());
        let cohesion_val = match row.get(6) {
            Some(lbug::Value::Double(d)) => Some(*d),
            _ => None,
        };
        let keywords_str = val_str(row.get(7));
        let keywords: Vec<String> = if keywords_str.is_empty() {
            Vec::new()
        } else {
            keywords_str.split(',').map(|s| s.trim().to_string()).collect()
        };
        let info = super::types::CommunityInfo {
            id: qname.clone(),
            name: val_str(row.get(1)),
            description: val_str_opt(row.get(2)),
            node_count: val_i64(row.get(3)) as u64,
            file_count: val_i64(row.get(4)) as u64,
            function_count: val_i64(row.get(5)) as u64,
            key_symbols: Vec::new(),
            cohesion: cohesion_val,
            keywords,
        };
        qname_to_idx.insert(qname, communities.len());
        communities.push(info);
    }

    if communities.is_empty() {
        return Ok(communities);
    }

    // 2. Bulk-fetch MEMBER_OF edges to get key_symbols for each community.
    // LadybugDB requires a concrete FROM label. Query for each symbol label
    // that can be a member.
    let member_labels = &[
        "Function", "Method", "Class", "Interface", "Struct", "Trait",
        "Variable", "Enum", "Import", "Module",
    ];
    for &label in member_labels {
        let member_rows = db
            .query(&format!(
                "MATCH (n:{label})-[r:CodeRelation]->(c:Community) \
                 WHERE r.type = 'MEMBER_OF' AND c.project_id = '{pid}' \
                 RETURN c.qualified_name, n.name"
            ))
            .await?;

        for row in &member_rows {
            let comm_qname = val_str(row.first());
            let sym_name = val_str(row.get(1));
            if let Some(&idx) = qname_to_idx.get(&comm_qname) {
                let info = &mut communities[idx];
                if info.key_symbols.len() < 8 {
                    info.key_symbols.push(sym_name);
                }
            }
        }
    }

    Ok(communities)
}

// ---------------------------------------------------------------------------
// Process queries
// ---------------------------------------------------------------------------

/// Fetch all Process nodes for a project with their ordered steps.
pub async fn query_processes(
    db: &SharedCodeGraphDb,
    project_id: &str,
) -> Result<Vec<super::types::ProcessInfo>> {
    let pid = esc(project_id);

    let rows = db
        .query(&format!(
            "MATCH (p:Process) WHERE p.project_id = '{pid}' \
             RETURN p.qualified_name, p.name, p.entry_function, \
             p.source_file, p.call_depth, p.process_type, \
             p.communities, p.entry_point_score, p.terminal_id"
        ))
        .await?;

    let mut processes: Vec<super::types::ProcessInfo> = Vec::new();
    let mut qname_to_idx: HashMap<String, usize> = HashMap::new();

    for row in &rows {
        let qname = val_str(row.first());
        let communities_str = val_str(row.get(6));
        let communities: Vec<String> = if communities_str.is_empty() {
            Vec::new()
        } else {
            communities_str.split(',').map(|s| s.trim().to_string()).collect()
        };
        let entry_point_score = match row.get(7) {
            Some(lbug::Value::Double(d)) => Some(*d),
            _ => None,
        };
        let info = super::types::ProcessInfo {
            id: qname.clone(),
            entry_function: val_str(row.get(2)),
            source_file: val_str(row.get(3)),
            call_depth: val_i64(row.get(4)) as u32,
            community: None,
            steps: Vec::new(),
            process_type: val_str_opt(row.get(5)),
            communities,
            entry_point_score,
            terminal_id: val_str_opt(row.get(8)),
        };
        qname_to_idx.insert(qname, processes.len());
        processes.push(info);
    }

    if processes.is_empty() {
        return Ok(processes);
    }

    // Collect steps into a temporary map: proc_qname -> Vec<(order, name)>
    let mut step_map: HashMap<String, Vec<(i32, String)>> = HashMap::new();

    for &callable_label in &["Function", "Method"] {
        let step_rows = db
            .query(&format!(
                "MATCH (p:Process)-[r:CodeRelation]->(n:{callable_label}) \
                 WHERE r.type = 'STEP_IN_PROCESS' AND p.project_id = '{pid}' \
                 RETURN p.qualified_name, n.name, r.step"
            ))
            .await?;

        for row in &step_rows {
            let proc_qname = val_str(row.first());
            let step_name = val_str(row.get(1));
            let step_order = val_i64(row.get(2)) as i32;
            step_map
                .entry(proc_qname)
                .or_default()
                .push((step_order, step_name));
        }
    }

    // Sort each process's steps by order, assign to processes.
    for (qname, mut steps) in step_map {
        steps.sort_by_key(|(order, _)| *order);
        if let Some(&idx) = qname_to_idx.get(&qname) {
            processes[idx].steps = steps.into_iter().map(|(_, name)| name).collect();
        }
    }

    // Get community membership for entry functions.
    for &label in &["Function", "Method"] {
        let mem_rows = db
            .query(&format!(
                "MATCH (n:{label})-[r:CodeRelation]->(c:Community) \
                 WHERE r.type = 'MEMBER_OF' AND n.project_id = '{pid}' \
                 RETURN n.qualified_name, c.name"
            ))
            .await?;

        let community_by_qname: HashMap<String, String> = mem_rows
            .iter()
            .map(|row| (val_str(row.first()), val_str(row.get(1))))
            .collect();

        for proc in &mut processes {
            if proc.community.is_none()
                && let Some(comm) = community_by_qname.get(&proc.entry_function)
            {
                proc.community = Some(comm.clone());
            }
        }
    }

    Ok(processes)
}

// ---------------------------------------------------------------------------
// Node queries
// ---------------------------------------------------------------------------

/// List nodes with optional label filter and pagination.
///
/// Returns `(nodes, total_count)`.
pub async fn query_nodes(
    db: &SharedCodeGraphDb,
    project_id: &str,
    label_filter: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<(Vec<QueriedNode>, usize)> {
    let pid = esc(project_id);
    let limit = limit.min(500);

    if let Some(label) = label_filter {
        // Validate the label exists in schema.
        if !ALL_NODE_LABELS.contains(&label) {
            return Ok((Vec::new(), 0));
        }
        return query_nodes_single_label(db, &pid, label, offset, limit).await;
    }

    // No label filter: query across all labels, collect and paginate in Rust.
    let mut all_nodes: Vec<QueriedNode> = Vec::new();

    for &label in ALL_NODE_LABELS {
        if SKIP_PROJECT_LABELS.contains(&label) {
            continue;
        }

        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN id(n), n.qualified_name, n.name, n.source_file, \
                 n.line_start, n.line_end"
            ))
            .await?;

        for row in &rows {
            all_nodes.push(QueriedNode {
                id: val_i64(row.first()),
                qualified_name: val_str(row.get(1)),
                name: val_str(row.get(2)),
                label: label.to_string(),
                source_file: val_str_opt(row.get(3)),
                line_start: val_u32_opt(row.get(4)),
                line_end: val_u32_opt(row.get(5)),
                source: None,
                written_by: None,
                properties: HashMap::new(),
            });
        }
    }

    // Sort by qualified_name for deterministic pagination.
    all_nodes.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    let total = all_nodes.len();
    let page = all_nodes
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();

    Ok((page, total))
}

/// Query nodes for a single label with pagination.
async fn query_nodes_single_label(
    db: &SharedCodeGraphDb,
    pid: &str,
    label: &str,
    offset: usize,
    limit: usize,
) -> Result<(Vec<QueriedNode>, usize)> {
    // Get total count.
    let count = db
        .query_scalar_i64(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' RETURN count(n)"
        ))
        .await?
        .unwrap_or(0) as usize;

    // Get the page.
    let rows = db
        .query(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
             RETURN id(n), n.qualified_name, n.name, n.source_file, \
             n.line_start, n.line_end \
             SKIP {offset} LIMIT {limit}"
        ))
        .await?;

    let nodes: Vec<QueriedNode> = rows
        .iter()
        .map(|row| QueriedNode {
            id: val_i64(row.first()),
            qualified_name: val_str(row.get(1)),
            name: val_str(row.get(2)),
            label: label.to_string(),
            source_file: val_str_opt(row.get(3)),
            line_start: val_u32_opt(row.get(4)),
            line_end: val_u32_opt(row.get(5)),
            source: None,
            written_by: None,
            properties: HashMap::new(),
        })
        .collect();

    Ok((nodes, count))
}

// ---------------------------------------------------------------------------
// Node detail
// ---------------------------------------------------------------------------

/// Get a single node by ID with full properties.
///
/// If `label_hint` is provided, only that label's table is checked.
/// Otherwise all label tables are scanned (bounded at ~29 queries).
pub async fn query_node_by_id(
    db: &SharedCodeGraphDb,
    project_id: &str,
    node_id: i64,
    label_hint: Option<&str>,
) -> Result<Option<QueriedNode>> {
    let pid = esc(project_id);

    let labels: Vec<&str> = if let Some(hint) = label_hint {
        if ALL_NODE_LABELS.contains(&hint) {
            vec![hint]
        } else {
            return Ok(None);
        }
    } else {
        ALL_NODE_LABELS.to_vec()
    };

    for &label in &labels {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE id(n) = {node_id} AND n.project_id = '{pid}' \
                 RETURN id(n), n.qualified_name, n.name, n.source_file, \
                 n.line_start, n.line_end, n.source, n.written_by"
            ))
            .await?;

        if let Some(row) = rows.first() {
            let mut props = HashMap::new();

            // For Community and Process nodes, fetch extra properties.
            if label == "Community" {
                let extra = db
                    .query(&format!(
                        "MATCH (n:Community) WHERE id(n) = {node_id} \
                         RETURN n.description, n.density, n.node_count, \
                         n.file_count, n.function_count"
                    ))
                    .await?;
                if let Some(erow) = extra.first() {
                    if let Some(s) = val_str_opt(erow.first()) {
                        props.insert("description".into(), serde_json::Value::String(s));
                    }
                    let density = val_f64(erow.get(1));
                    if density > 0.0 {
                        props.insert("density".into(), serde_json::json!(density));
                    }
                    props.insert("node_count".into(), serde_json::json!(val_i64(erow.get(2))));
                    props.insert("file_count".into(), serde_json::json!(val_i64(erow.get(3))));
                    props.insert("function_count".into(), serde_json::json!(val_i64(erow.get(4))));
                }
            } else if label == "Process" {
                let extra = db
                    .query(&format!(
                        "MATCH (n:Process) WHERE id(n) = {node_id} \
                         RETURN n.entry_function, n.call_depth"
                    ))
                    .await?;
                if let Some(erow) = extra.first() {
                    if let Some(s) = val_str_opt(erow.first()) {
                        props.insert("entry_function".into(), serde_json::Value::String(s));
                    }
                    props.insert("call_depth".into(), serde_json::json!(val_i64(erow.get(1))));
                }
            } else {
                // Standard code nodes: include declared_type, extends_type, import_source.
                let extra = db
                    .query(&format!(
                        "MATCH (n:{label}) WHERE id(n) = {node_id} \
                         RETURN n.declared_type, n.extends_type, n.import_source"
                    ))
                    .await?;
                if let Some(erow) = extra.first() {
                    if let Some(s) = val_str_opt(erow.first()) {
                        props.insert("declared_type".into(), serde_json::Value::String(s));
                    }
                    if let Some(s) = val_str_opt(erow.get(1)) {
                        props.insert("extends_type".into(), serde_json::Value::String(s));
                    }
                    if let Some(s) = val_str_opt(erow.get(2)) {
                        props.insert("import_source".into(), serde_json::Value::String(s));
                    }
                }
            }

            return Ok(Some(QueriedNode {
                id: val_i64(row.first()),
                qualified_name: val_str(row.get(1)),
                name: val_str(row.get(2)),
                label: label.to_string(),
                source_file: val_str_opt(row.get(3)),
                line_start: val_u32_opt(row.get(4)),
                line_end: val_u32_opt(row.get(5)),
                source: val_str_opt(row.get(6)),
                written_by: val_str_opt(row.get(7)),
                properties: props,
            }));
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Edge queries
// ---------------------------------------------------------------------------

/// Get edges for a node, with directional and type filtering.
///
/// `direction` should be `"outgoing"`, `"incoming"`, or `"both"` (default).
///
/// Returns `(edges, total_count)`.
#[allow(clippy::too_many_arguments)]
pub async fn query_node_edges(
    db: &SharedCodeGraphDb,
    project_id: &str,
    node_id: i64,
    node_label: &str,
    direction: &str,
    edge_type_filter: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<(Vec<QueriedEdge>, usize)> {
    let pid = esc(project_id);
    let limit = limit.min(200);
    let mut all_edges: Vec<QueriedEdge> = Vec::new();

    let type_filter = edge_type_filter
        .map(|t| format!(" AND r.type = '{}'", esc(t)))
        .unwrap_or_default();

    // Outgoing edges: (node)-[r]->(b)
    if direction != "incoming" {
        for &target_label in ALL_NODE_LABELS {
            let rows = db
                .query(&format!(
                    "MATCH (a:{node_label})-[r:CodeRelation]->(b:{target_label}) \
                     WHERE id(a) = {node_id} AND a.project_id = '{pid}'{type_filter} \
                     RETURN id(a), a.name, id(b), b.name, r.type, r.confidence"
                ))
                .await?;

            for row in &rows {
                all_edges.push(QueriedEdge {
                    from_id: val_i64(row.first()),
                    from_name: val_str(row.get(1)),
                    from_label: node_label.to_string(),
                    to_id: val_i64(row.get(2)),
                    to_name: val_str(row.get(3)),
                    to_label: target_label.to_string(),
                    edge_type: val_str(row.get(4)),
                    confidence: val_f64(row.get(5)),
                });
            }
        }
    }

    // Incoming edges: (a)-[r]->(node)
    if direction != "outgoing" {
        for &source_label in ALL_NODE_LABELS {
            let rows = db
                .query(&format!(
                    "MATCH (a:{source_label})-[r:CodeRelation]->(b:{node_label}) \
                     WHERE id(b) = {node_id} AND b.project_id = '{pid}'{type_filter} \
                     RETURN id(a), a.name, id(b), b.name, r.type, r.confidence"
                ))
                .await?;

            for row in &rows {
                all_edges.push(QueriedEdge {
                    from_id: val_i64(row.first()),
                    from_name: val_str(row.get(1)),
                    from_label: source_label.to_string(),
                    to_id: val_i64(row.get(2)),
                    to_name: val_str(row.get(3)),
                    to_label: node_label.to_string(),
                    edge_type: val_str(row.get(4)),
                    confidence: val_f64(row.get(5)),
                });
            }
        }
    }

    let total = all_edges.len();
    let page: Vec<QueriedEdge> = all_edges
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();

    Ok((page, total))
}

// ---------------------------------------------------------------------------
// Graph stats
// ---------------------------------------------------------------------------

/// Compute aggregate statistics: node counts per label, edge counts per type.
pub async fn query_graph_stats(
    db: &SharedCodeGraphDb,
    project_id: &str,
) -> Result<GraphStatsResult> {
    let pid = esc(project_id);

    // Node counts per label.
    let mut nodes_by_label: Vec<(String, u64)> = Vec::new();
    let mut total_nodes: u64 = 0;

    for &label in super::schema::DISPLAY_NODE_LABELS {
        if SKIP_PROJECT_LABELS.contains(&label) {
            continue;
        }

        let count = db
            .query_scalar_i64(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' RETURN count(n)"
            ))
            .await?
            .unwrap_or(0) as u64;

        if count > 0 {
            nodes_by_label.push((label.to_string(), count));
            total_nodes += count;
        }
    }

    nodes_by_label.sort_by(|a, b| b.1.cmp(&a.1));

    // Edge counts per type. We need to iterate FROM labels since
    // LadybugDB requires a concrete label in MATCH.
    let mut edge_type_counts: HashMap<String, u64> = HashMap::new();
    let mut total_edges: u64 = 0;

    for &from_label in super::schema::DISPLAY_NODE_LABELS {
        if SKIP_PROJECT_LABELS.contains(&from_label) {
            continue;
        }

        let rows = db
            .query(&format!(
                "MATCH (a:{from_label})-[r:CodeRelation]->() \
                 WHERE a.project_id = '{pid}' \
                 RETURN r.type, count(r)"
            ))
            .await;

        // Some FROM labels may not have outgoing CodeRelation edges
        // (e.g. if the label has no FROM entry in the schema). Silently
        // skip errors.
        let rows = match rows {
            Ok(r) => r,
            Err(_) => continue,
        };

        for row in &rows {
            let etype = val_str(row.first());
            let count = val_i64(row.get(1)) as u64;
            if !etype.is_empty() {
                *edge_type_counts.entry(etype).or_default() += count;
                total_edges += count;
            }
        }
    }

    let mut edges_by_type: Vec<(String, u64)> = edge_type_counts.into_iter().collect();
    edges_by_type.sort_by(|a, b| b.1.cmp(&a.1));

    Ok(GraphStatsResult {
        total_nodes,
        total_edges,
        nodes_by_label,
        edges_by_type,
    })
}

// ---------------------------------------------------------------------------
// Bulk graph queries — for the interactive graph canvas view
// ---------------------------------------------------------------------------

/// Page size for SKIP/LIMIT on bulk node and edge queries. LadybugDB's
/// native result collector crashes the process on very large single-query
/// result sets (no Rust panic, no error — just process death), so we
/// always page. 5k rows × ~6 string columns is well inside the safe zone
/// and keeps the round-trip cost negligible.
const BULK_PAGE_SIZE: usize = 5_000;

/// Fetch every node in the project, for the bulk graph endpoint.
///
/// Pages each per-label query with SKIP/LIMIT so LadybugDB never has to
/// materialize a giant result set in a single call — large single queries
/// have been observed to segfault the native library and tear down the
/// whole backend process (no Rust panic catches this).
pub async fn query_bulk_nodes(
    db: &SharedCodeGraphDb,
    project_id: &str,
) -> Result<Vec<QueriedNode>> {
    let pid = esc(project_id);
    let mut all_nodes: Vec<QueriedNode> = Vec::new();

    // Use DISPLAY_NODE_LABELS — Parameter (the one pipeline-only label)
    // has already been deleted by the cleanup step.
    for &label in super::schema::DISPLAY_NODE_LABELS {
        if SKIP_PROJECT_LABELS.contains(&label) {
            continue;
        }

        // First page decides whether this label supports the full column
        // set. Project / Community / Process lack source_file / line_start
        // / line_end — fall back to the name-only projection.
        let uses_full_cols = match fetch_node_page(db, label, &pid, 0, BULK_PAGE_SIZE, true).await {
            Ok(rows) => {
                all_nodes.extend(rows_to_nodes(&rows, label, true));
                if rows.len() < BULK_PAGE_SIZE {
                    continue;
                }
                true
            }
            Err(_) => {
                // Retry first page with narrow projection.
                match fetch_node_page(db, label, &pid, 0, BULK_PAGE_SIZE, false).await {
                    Ok(rows) => {
                        all_nodes.extend(rows_to_nodes(&rows, label, false));
                        if rows.len() < BULK_PAGE_SIZE {
                            continue;
                        }
                        false
                    }
                    Err(e) => {
                        tracing::warn!(label, %e, "bulk-nodes: skipping label after query error");
                        continue;
                    }
                }
            }
        };

        // Continue paging with whichever projection the first page accepted.
        let mut skip = BULK_PAGE_SIZE;
        loop {
            let rows = match fetch_node_page(db, label, &pid, skip, BULK_PAGE_SIZE, uses_full_cols)
                .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(label, skip, %e, "bulk-nodes: page fetch failed, stopping label");
                    break;
                }
            };
            let fetched = rows.len();
            all_nodes.extend(rows_to_nodes(&rows, label, uses_full_cols));
            if fetched < BULK_PAGE_SIZE {
                break;
            }
            skip += BULK_PAGE_SIZE;
        }
    }

    Ok(all_nodes)
}

async fn fetch_node_page(
    db: &SharedCodeGraphDb,
    label: &str,
    pid: &str,
    skip: usize,
    limit: usize,
    full_cols: bool,
) -> Result<Vec<Vec<lbug::Value>>> {
    let projection = if full_cols {
        "id(n), n.qualified_name, n.name, n.source_file, n.line_start, n.line_end"
    } else {
        "id(n), n.qualified_name, n.name"
    };
    db.query(&format!(
        "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
         RETURN {projection} SKIP {skip} LIMIT {limit}"
    ))
    .await
}

fn rows_to_nodes(rows: &[Vec<lbug::Value>], label: &str, full_cols: bool) -> Vec<QueriedNode> {
    rows.iter()
        .map(|row| QueriedNode {
            id: val_i64(row.first()),
            qualified_name: val_str(row.get(1)),
            name: val_str(row.get(2)),
            label: label.to_string(),
            source_file: if full_cols { val_str_opt(row.get(3)) } else { None },
            line_start: if full_cols { val_u32_opt(row.get(4)) } else { None },
            line_end: if full_cols { val_u32_opt(row.get(5)) } else { None },
            source: None,
            written_by: None,
            properties: HashMap::new(),
        })
        .collect()
}

/// Fetch every edge whose both endpoints are present in `node_qnames`, for
/// the bulk graph endpoint. Uses `qualified_name` (not `id(n)`, which
/// LadybugDB always returns as 0) as the join key. Iterates over label-pair
/// permutations because LadybugDB requires concrete labels on both sides.
pub async fn query_bulk_edges(
    db: &SharedCodeGraphDb,
    project_id: &str,
    node_qnames: &HashSet<String>,
) -> Result<Vec<QueriedEdge>> {
    let pid = esc(project_id);
    let mut all_edges: Vec<QueriedEdge> = Vec::new();

    for &from_label in super::schema::DISPLAY_NODE_LABELS {
        if SKIP_PROJECT_LABELS.contains(&from_label) {
            continue;
        }
        for &to_label in super::schema::DISPLAY_NODE_LABELS {
            if SKIP_PROJECT_LABELS.contains(&to_label) {
                continue;
            }

            let mut skip = 0usize;
            loop {
                let rows = db
                    .query(&format!(
                        "MATCH (a:{from_label})-[r:CodeRelation]->(b:{to_label}) \
                         WHERE a.project_id = '{pid}' \
                         RETURN a.qualified_name, a.name, b.qualified_name, b.name, r.type, r.confidence \
                         SKIP {skip} LIMIT {BULK_PAGE_SIZE}"
                    ))
                    .await;

                let rows = match rows {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let fetched = rows.len();

                for row in &rows {
                    let from_qname = val_str(row.first());
                    let to_qname = val_str(row.get(2));
                    if !node_qnames.contains(&from_qname) || !node_qnames.contains(&to_qname) {
                        continue;
                    }
                    all_edges.push(QueriedEdge {
                        from_id: 0,
                        from_name: from_qname,
                        from_label: from_label.to_string(),
                        to_id: 0,
                        to_name: to_qname,
                        to_label: to_label.to_string(),
                        edge_type: val_str(row.get(4)),
                        confidence: val_f64(row.get(5)),
                    });
                }

                if fetched < BULK_PAGE_SIZE {
                    break;
                }
                skip += BULK_PAGE_SIZE;
            }
        }
    }

    Ok(all_edges)
}

// ---------------------------------------------------------------------------
// Streaming bulk queries — cursor-backed, never materialize a full result set
// ---------------------------------------------------------------------------

/// Stream every node in the project row-by-row from the native Kuzu cursor.
///
/// Ports GitNexus's `streamGraphNdjson` per-label loop
/// (`gitnexus/server/api.ts:300-317`). For each label in
/// `DISPLAY_NODE_LABELS` we open a fresh `query_stream`; the flattened
/// output stream yields `QueriedNode`s as the cursor advances. A single
/// row is ever in memory per stream step.
pub fn stream_bulk_nodes(
    db: SharedCodeGraphDb,
    project_id: String,
) -> impl Stream<Item = Result<QueriedNode>> + Send + 'static {
    async_stream::try_stream! {
        let pid = esc(&project_id);
        for &label in super::schema::DISPLAY_NODE_LABELS {
            if SKIP_PROJECT_LABELS.contains(&label) {
                continue;
            }

            // Probe with the wide projection first; on failure (labels like
            // Community/Process/Project that lack file/line columns) fall
            // back to the narrow one. Same shape as the non-streaming path
            // in `query_bulk_nodes`, just cursor-driven.
            let wide = format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN id(n), n.qualified_name, n.name, n.source_file, \
                 n.line_start, n.line_end"
            );
            let narrow = format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN id(n), n.qualified_name, n.name"
            );

            let mut stream = Box::pin(db.query_stream(wide.clone()));
            let mut first = stream.next().await;
            let full_cols = match &first {
                Some(Err(_)) => {
                    // Wide projection rejected — swap to narrow.
                    first = None;
                    stream = Box::pin(db.query_stream(narrow));
                    false
                }
                _ => true,
            };

            // Drain any buffered first row + the rest of the stream.
            loop {
                let next = if first.is_some() { first.take().unwrap() } else {
                    match stream.next().await {
                        Some(v) => v,
                        None => break,
                    }
                };
                match next {
                    Ok(row) => yield row_to_queried_node(&row, label, full_cols),
                    Err(e) => {
                        tracing::warn!(label, %e, "stream_bulk_nodes: row error, stopping label");
                        break;
                    }
                }
            }
        }
    }
}

/// Stream every edge in the project row-by-row. Iterates the label-pair
/// permutations (same as `query_bulk_edges`) but drives each pair's query
/// through the cursor — invalid pairs return zero rows and cost nothing.
///
/// Unlike the paged version, this does NOT filter by a qname HashSet.
/// GitNexus's `GRAPH_RELATIONSHIP_QUERY` (`server/api.ts:258`) runs
/// unfiltered too; dangling edges are a pipeline-cleanup concern, not a
/// query-time concern. Dropping the filter also eliminates the full
/// extra node pass the old `get_bulk_edges` handler used to make.
pub fn stream_bulk_edges(
    db: SharedCodeGraphDb,
    project_id: String,
) -> impl Stream<Item = Result<QueriedEdge>> + Send + 'static {
    async_stream::try_stream! {
        let pid = esc(&project_id);
        for &from_label in super::schema::DISPLAY_NODE_LABELS {
            if SKIP_PROJECT_LABELS.contains(&from_label) {
                continue;
            }
            for &to_label in super::schema::DISPLAY_NODE_LABELS {
                if SKIP_PROJECT_LABELS.contains(&to_label) {
                    continue;
                }
                let cypher = format!(
                    "MATCH (a:{from_label})-[r:CodeRelation]->(b:{to_label}) \
                     WHERE a.project_id = '{pid}' \
                     RETURN a.qualified_name, a.name, b.qualified_name, b.name, \
                     r.type, r.confidence"
                );
                let mut stream = Box::pin(db.query_stream(cypher));
                while let Some(row) = stream.next().await {
                    match row {
                        Ok(row) => yield QueriedEdge {
                            from_id: 0,
                            from_name: val_str(row.first()),
                            from_label: from_label.to_string(),
                            to_id: 0,
                            to_name: val_str(row.get(2)),
                            to_label: to_label.to_string(),
                            edge_type: val_str(row.get(4)),
                            confidence: val_f64(row.get(5)),
                        },
                        Err(_) => break, // invalid pair or transient — skip
                    }
                }
            }
        }
    }
}

fn row_to_queried_node(row: &[lbug::Value], label: &str, full_cols: bool) -> QueriedNode {
    QueriedNode {
        id: val_i64(row.first()),
        qualified_name: val_str(row.get(1)),
        name: val_str(row.get(2)),
        label: label.to_string(),
        source_file: if full_cols { val_str_opt(row.get(3)) } else { None },
        line_start: if full_cols { val_u32_opt(row.get(4)) } else { None },
        line_end: if full_cols { val_u32_opt(row.get(5)) } else { None },
        source: None,
        written_by: None,
        properties: HashMap::new(),
    }
}
