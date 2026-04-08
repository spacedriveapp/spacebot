//! Phase 8: Entry point scoring and call chain tracing.
//!
//! Wave 4.3 improvements:
//! - Entry points are ranked by a weighted score that now incorporates
//!   transitive call-chain **reach** and the number of distinct
//!   **communities** the chain spans. A function that fans out across
//!   multiple modules is much more likely to be a meaningful entry point
//!   than one with high out-degree but a single-community blast radius.
//! - BFS tracing already tracks a `visited` set (cycles can't reappear
//!   as steps).
//! - The old `truncate(50)` hardcap is replaced by `config.max_processes`
//!   from Wave 1.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;

use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::types::CodeGraphConfig;

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Result with process-specific stats.
pub struct ProcessResult {
    pub processes_traced: u64,
    pub nodes_created: u64,
    pub edges_created: u64,
}

/// Entry-point name patterns.
const ENTRY_PATTERNS: &[&str] = &[
    "main", "run", "start", "init", "setup", "bootstrap",
    "handle", "handler", "on_", "listen", "serve", "execute",
    "process", "dispatch", "route", "controller",
];

/// Score a function/method for entry-point likelihood.
///
/// The score is a weighted blend of:
/// - Name pattern match (+2.0) or exact `main` (+10.0)
/// - Degree ratio `out / (in + 1)` capped at 5.0 (stay cheap for hubs)
/// - `ln(1 + reach)` × 1.5 — rewards functions whose transitive call
///   chains touch many symbols (log so we don't let one massive hub
///   dominate every slot)
/// - `community_span` × 1.5 — rewards functions whose chain crosses
///   module boundaries, which is the strongest signal of "this is a
///   real entry into a feature"
fn entry_score(
    name: &str,
    out_degree: usize,
    in_degree: usize,
    reach: usize,
    community_span: usize,
) -> f64 {
    let name_lower = name.to_lowercase();
    let mut score = 0.0;

    // Name pattern bonus
    for pattern in ENTRY_PATTERNS {
        if name_lower.contains(pattern) {
            score += 2.0;
            break;
        }
    }

    // Call ratio: high out-degree + low in-degree → likely entry point
    let ratio = (out_degree as f64 + 1.0) / (in_degree as f64 + 1.0);
    score += ratio.min(5.0);

    // Reach: log-damped so a single hub doesn't fill the top-N slots.
    score += ((reach as f64) + 1.0).ln() * 1.5;

    // Community span: every extra module the chain touches is worth a
    // full ranking unit.
    score += community_span as f64 * 1.5;

    // Bonus for "main" specifically
    if name_lower == "main" {
        score += 10.0;
    }

    score
}

/// BFS outward from `entry` over the forward CALLS graph, bounded by
/// `max_depth`, and return both:
/// - the total number of reachable nodes (including `entry`)
/// - the number of distinct communities those nodes belong to
///
/// `max_depth` matches the depth bound used by the main tracing pass so
/// scoring and tracing observe the same blast radius.
fn reach_and_span(
    entry: &str,
    forward: &HashMap<String, Vec<String>>,
    community_of: &HashMap<String, String>,
    max_depth: usize,
) -> (usize, usize) {
    let mut visited: HashSet<String> = HashSet::new();
    let mut communities: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    queue.push_back((entry.to_string(), 0));
    visited.insert(entry.to_string());
    if let Some(c) = community_of.get(entry) {
        communities.insert(c.clone());
    }

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callees) = forward.get(&node) {
            for callee in callees {
                if visited.insert(callee.clone()) {
                    if let Some(c) = community_of.get(callee) {
                        communities.insert(c.clone());
                    }
                    queue.push_back((callee.clone(), depth + 1));
                }
            }
        }
    }

    (visited.len(), communities.len())
}

/// Score entry-point likelihood and trace call chains to create Process nodes.
pub async fn trace_processes(
    project_id: &str,
    db: &SharedCodeGraphDb,
    config: &CodeGraphConfig,
) -> Result<ProcessResult> {
    let pid = cypher_escape(project_id);
    let max_depth = config.max_process_depth as usize;

    tracing::debug!(
        project_id = %project_id,
        max_depth = max_depth,
        "tracing processes"
    );

    // 1. Query CALLS edges to build directed adjacency list.
    let edges = db.query(&format!(
        "MATCH (a)-[r:CodeRelation]->(b) \
         WHERE r.type = 'CALLS' AND a.project_id = '{pid}' \
         RETURN a.qualified_name, b.qualified_name"
    )).await?;

    if edges.is_empty() {
        tracing::info!(project_id = %project_id, "no CALLS edges for process tracing");
        return Ok(ProcessResult { processes_traced: 0, nodes_created: 0, edges_created: 0 });
    }

    let mut forward: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut out_degree: HashMap<String, usize> = HashMap::new();

    for row in &edges {
        if let (Some(lbug::Value::String(a)), Some(lbug::Value::String(b))) =
            (row.first(), row.get(1))
        {
            forward.entry(a.clone()).or_default().push(b.clone());
            *out_degree.entry(a.clone()).or_default() += 1;
            *in_degree.entry(b.clone()).or_default() += 1;
            // Ensure all nodes appear in both maps
            in_degree.entry(a.clone()).or_default();
            out_degree.entry(b.clone()).or_default();
        }
    }

    // 2. Query Function and Method nodes separately for scoring.
    let mut name_map: HashMap<String, (String, String)> = HashMap::new();

    for label in &["Function", "Method"] {
        let nodes = db.query(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name, n.source_file"
        )).await?;

        for row in &nodes {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
                Some(lbug::Value::String(sf)),
            ) = (row.first(), row.get(1), row.get(2))
            {
                name_map.insert(qname.clone(), (name.clone(), sf.clone()));
            }
        }
    }

    // 2b. Query MEMBER_OF edges so we can measure the community span
    //     reachable from each candidate entry point. If community
    //     detection hasn't run (or produced nothing), this map stays
    //     empty and community_span contributes 0 to the score —
    //     equivalent to the pre-Wave-4 behaviour.
    let mut community_of: HashMap<String, String> = HashMap::new();
    let member_rows = db
        .query(&format!(
            "MATCH (n)-[r:CodeRelation]->(c:Community) \
             WHERE r.type = 'MEMBER_OF' AND c.project_id = '{pid}' \
             RETURN n.qualified_name, c.qualified_name"
        ))
        .await?;
    for row in &member_rows {
        if let (Some(lbug::Value::String(qname)), Some(lbug::Value::String(cname))) =
            (row.first(), row.get(1))
        {
            community_of.insert(qname.clone(), cname.clone());
        }
    }

    // 3. Score all nodes for entry-point likelihood.
    let mut scored: Vec<(String, f64)> = Vec::new();
    for (qname, (name, _)) in &name_map {
        let od = out_degree.get(qname).copied().unwrap_or(0);
        let id = in_degree.get(qname).copied().unwrap_or(0);
        if od == 0 {
            continue; // Must call something to be an entry point
        }
        let (reach, span) = reach_and_span(qname, &forward, &community_of, max_depth);
        let score = entry_score(name, od, id, reach, span);
        scored.push((qname.clone(), score));
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(config.max_processes as usize);

    // 4. BFS trace from each entry point.
    let mut node_stmts: Vec<String> = Vec::new();
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut processes_traced = 0u64;

    for (idx, (entry_qname, _score)) in scored.iter().enumerate() {
        let (entry_name, entry_sf) = match name_map.get(entry_qname) {
            Some(v) => v,
            None => continue,
        };

        // BFS with max depth and max branching
        let mut trace: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        queue.push_back((entry_qname.clone(), 0));
        visited.insert(entry_qname.clone());

        while let Some((node, depth)) = queue.pop_front() {
            trace.push(node.clone());

            if depth >= max_depth {
                continue;
            }

            if let Some(callees) = forward.get(&node) {
                for callee in callees.iter().take(config.max_process_branching as usize) {
                    if visited.insert(callee.clone()) {
                        queue.push_back((callee.clone(), depth + 1));
                    }
                }
            }
        }

        // Require minimum steps for a meaningful process
        if trace.len() < config.min_process_steps as usize {
            continue;
        }

        let proc_qname = format!("{pid}::proc_{idx}_{}", cypher_escape(entry_name));
        let proc_qname_escaped = cypher_escape(&proc_qname);
        let entry_name_escaped = cypher_escape(entry_name);
        let entry_sf_escaped = cypher_escape(entry_sf);

        // Get terminal node name
        let terminal_name = trace.last()
            .and_then(|qn| name_map.get(qn))
            .map(|(n, _)| n.as_str())
            .unwrap_or("?");
        let proc_label = format!("{entry_name} -> {terminal_name}");
        let proc_label_escaped = cypher_escape(&proc_label);

        node_stmts.push(format!(
            "CREATE (:Process {{qualified_name: '{proc_qname_escaped}', \
             name: '{proc_label_escaped}', project_id: '{pid}', \
             entry_function: '{entry_name_escaped}', source_file: '{entry_sf_escaped}', \
             call_depth: {depth}, source: 'pipeline'}})",
            depth = trace.len(),
        ));

        // STEP_IN_PROCESS edges — try Function first, then Method
        for (step, step_qname) in trace.iter().enumerate() {
            let step_escaped = cypher_escape(step_qname);
            for label in &["Function", "Method"] {
                edge_stmts.push(format!(
                    "MATCH (p:Process), (n:{label}) WHERE p.qualified_name = '{proc_qname_escaped}' \
                     AND n.qualified_name = '{step_escaped}' AND n.project_id = '{pid}' \
                     CREATE (p)-[:CodeRelation {{type: 'STEP_IN_PROCESS', confidence: 1.0, reason: '', step: {s}}}]->(n)",
                    s = step + 1,
                ));
            }
        }

        processes_traced += 1;
    }

    // 5. Execute node and edge batches.
    let mut nodes_created = 0u64;
    let mut edges_created = 0u64;

    if !node_stmts.is_empty() {
        let batch = db.execute_batch(node_stmts).await?;
        nodes_created += batch.success;
    }
    if !edge_stmts.is_empty() {
        for chunk in edge_stmts.chunks(100) {
            let batch = db.execute_batch(chunk.to_vec()).await?;
            edges_created += batch.success;
        }
    }

    tracing::info!(
        project_id = %project_id,
        processes = processes_traced,
        nodes = nodes_created,
        edges = edges_created,
        "process tracing complete"
    );

    Ok(ProcessResult {
        processes_traced,
        nodes_created,
        edges_created,
    })
}
