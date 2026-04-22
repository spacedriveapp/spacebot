//! Weighted Louvain community detection.
//!
//! Single-level Louvain modularity-optimization pass. Edges are weighted
//! by relationship type (CALLS stronger than IMPORTS, etc.) which drives
//! clusters toward functional cohesion rather than pure reachability.
//!
//! For each surviving community we compute and persist:
//! - `density`  — the fraction of possible internal edges that exist.
//! - `modularity` (per-community contribution Q_c) — packed into
//!   `description` as `density=0.67; mod=0.042` because adding a schema
//!   column mid-development would force an index wipe.
//! - A label built from the shared file-path prefix + dominant symbol
//!   types (e.g. `src/codegraph/ — 8 functions, 3 structs`).

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::phase::{Phase, PhaseCtx};
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::types::{CodeGraphConfig, PipelinePhase};

/// Escape a string for use in a Cypher string literal.
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Result with community-specific stats.
pub struct CommunityResult {
    pub communities_detected: u64,
    pub nodes_created: u64,
    pub edges_created: u64,
}

/// Edge type weights used by the Louvain objective. Higher = stronger
/// semantic coupling → those edges pull their endpoints into the same
/// community more aggressively.
fn edge_weight(edge_type: &str) -> f64 {
    match edge_type {
        "CALLS" => 1.0,
        "EXTENDS" | "IMPLEMENTS" => 0.8,
        "CONTAINS" | "HAS_METHOD" => 0.5,
        "IMPORTS" => 0.3,
        _ => 0.0,
    }
}

/// Metadata for a code symbol, used for labeling a community after the
/// clustering pass is done.
#[derive(Clone, Debug)]
struct SymbolMeta {
    label: String,
    source_file: String,
}

/// Detect communities using weighted Louvain and persist labeled
/// Community nodes with MEMBER_OF edges.
pub async fn detect_communities(
    project_id: &str,
    db: &SharedCodeGraphDb,
    config: &CodeGraphConfig,
) -> Result<CommunityResult> {
    let pid = cypher_escape(project_id);
    let min_size = config.community_min_size as usize;

    tracing::debug!(
        project_id = %project_id,
        min_size = min_size,
        "detecting communities (weighted Louvain)"
    );

    // 1. Pull all candidate edges with their type so we can weight them.
    let edges = db
        .query(&format!(
            "MATCH (a)-[r:CodeRelation]->(b) \
             WHERE r.type IN ['CALLS', 'EXTENDS', 'IMPLEMENTS', \
             'CONTAINS', 'HAS_METHOD', 'IMPORTS'] \
             AND a.project_id = '{pid}' \
             RETURN a.qualified_name, b.qualified_name, r.type"
        ))
        .await?;

    if edges.is_empty() {
        tracing::info!(project_id = %project_id, "no edges for community detection");
        return Ok(CommunityResult {
            communities_detected: 0,
            nodes_created: 0,
            edges_created: 0,
        });
    }

    // 2. Query symbol metadata (name, label, source_file) for all potential
    //    members. We join this back when labeling communities later.
    let mut symbol_meta: HashMap<String, SymbolMeta> = HashMap::new();
    for label in &[
        "Function",
        "Method",
        "Class",
        "Interface",
        "Struct",
        "Trait",
        "Enum",
        "TypeAlias",
        "Const",
    ] {
        let rows = db
            .query(&format!(
                "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
                 RETURN n.qualified_name, n.name, n.source_file"
            ))
            .await?;
        for row in &rows {
            if let (Some(lbug::Value::String(qname)), Some(lbug::Value::String(sf))) =
                (row.first(), row.get(2))
            {
                symbol_meta.insert(
                    qname.clone(),
                    SymbolMeta {
                        label: (*label).to_string(),
                        source_file: sf.clone(),
                    },
                );
            }
        }
    }

    // 3. Build weighted adjacency.
    let mut node_to_idx: HashMap<String, usize> = HashMap::new();
    let mut idx_to_node: Vec<String> = Vec::new();
    let mut adj: Vec<HashMap<usize, f64>> = Vec::new();

    let intern = |name: &str,
                  nti: &mut HashMap<String, usize>,
                  itn: &mut Vec<String>,
                  a: &mut Vec<HashMap<usize, f64>>|
     -> usize {
        if let Some(&i) = nti.get(name) {
            return i;
        }
        let i = itn.len();
        nti.insert(name.to_string(), i);
        itn.push(name.to_string());
        a.push(HashMap::new());
        i
    };

    for row in &edges {
        let (a_qn, b_qn, etype) = match (row.first(), row.get(1), row.get(2)) {
            (
                Some(lbug::Value::String(a)),
                Some(lbug::Value::String(b)),
                Some(lbug::Value::String(t)),
            ) => (a, b, t),
            _ => continue,
        };
        let w = edge_weight(etype);
        if w <= 0.0 || a_qn == b_qn {
            continue;
        }

        let ai = intern(a_qn, &mut node_to_idx, &mut idx_to_node, &mut adj);
        let bi = intern(b_qn, &mut node_to_idx, &mut idx_to_node, &mut adj);

        // Undirected weighted accumulation — multiple edges of different
        // types between the same pair add up.
        *adj[ai].entry(bi).or_insert(0.0) += w;
        *adj[bi].entry(ai).or_insert(0.0) += w;
    }

    let n = idx_to_node.len();
    if n == 0 {
        return Ok(CommunityResult {
            communities_detected: 0,
            nodes_created: 0,
            edges_created: 0,
        });
    }

    // 4. Run Leiden community detection (Louvain + refinement + aggregation).
    let (community, modularity_q, m_total) = leiden(&adj);

    tracing::info!(
        project_id = %project_id,
        n,
        modularity = modularity_q,
        "louvain pass complete"
    );

    // 5. Group nodes by community id, filter by min_size.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (idx, &c) in community.iter().enumerate() {
        groups.entry(c).or_default().push(idx);
    }

    let mut node_stmts: Vec<String> = Vec::new();
    let mut edge_stmts: Vec<String> = Vec::new();
    let mut communities_detected = 0u64;
    let mut surviving_cid = 0usize;

    for members in groups.values() {
        if members.len() < min_size {
            continue;
        }

        // --- 5a. compute internal weight, density, per-community Q --- //
        let member_set: HashSet<usize> = members.iter().copied().collect();
        let mut internal_w = 0.0;
        let mut incident_w = 0.0;
        for &i in members {
            for (&j, &w) in &adj[i] {
                incident_w += w;
                if member_set.contains(&j) {
                    internal_w += w;
                }
            }
        }
        // incident_w counts each internal edge twice (from both endpoints);
        // likewise internal_w. Convert to undirected edge-weight sums.
        let internal_edges = internal_w / 2.0;
        let sum_tot = incident_w; // sum of degrees for the community
        let sum_in = internal_w;

        // Density = internal_edges / possible_edges (undirected simple).
        let k = members.len();
        let possible = (k * (k - 1) / 2).max(1) as f64;
        let density = internal_edges / possible;

        // Per-community modularity contribution.
        let community_q = if m_total > 0.0 {
            (sum_in / (2.0 * m_total)) - (sum_tot / (2.0 * m_total)).powi(2)
        } else {
            0.0
        };

        // --- 5b. label + counts + keywords --- //
        let member_metas: Vec<&SymbolMeta> = members
            .iter()
            .filter_map(|&idx| symbol_meta.get(&idx_to_node[idx]))
            .collect();

        let label = build_community_label(&member_metas, surviving_cid);
        let (file_count, function_count) = count_files_and_functions(&member_metas);
        let keywords = extract_keywords(&member_metas);

        // Cohesion: ratio of actual internal connectivity to max possible.
        let cohesion = density.min(1.0);

        let description = format!("density={density:.3}; mod={community_q:.4}");

        // --- 5c. build INSERT statement --- //
        let comm_qname = format!("{pid}::comm_{surviving_cid}");
        let comm_qname_escaped = cypher_escape(&comm_qname);
        let label_escaped = cypher_escape(&label);
        let description_escaped = cypher_escape(&description);
        let keywords_escaped = cypher_escape(&keywords.join(","));

        node_stmts.push(format!(
            "CREATE (:Community {{qualified_name: '{comm_qname_escaped}', name: '{label_escaped}', \
             project_id: '{pid}', description: '{description_escaped}', node_count: {nc}, \
             file_count: {fc}, function_count: {func_c}, density: {density}, \
             cohesion: {cohesion}, keywords: '{keywords_escaped}', source: 'pipeline'}})",
            nc = members.len(),
            fc = file_count,
            func_c = function_count,
        ));

        // MEMBER_OF edges — emit for every possible member label so
        // LadybugDB's type system can match at least one.
        for &idx in members {
            let member_qname = cypher_escape(&idx_to_node[idx]);
            for label in &[
                "Function",
                "Method",
                "Class",
                "Interface",
                "Struct",
                "Trait",
                "Enum",
                "TypeAlias",
                "Const",
                "Constructor",
                "Property",
                "UnionType",
                "Typedef",
                "Static",
                "Delegate",
                "CodeElement",
            ] {
                edge_stmts.push(format!(
                    "MATCH (n:{label}), (c:Community) WHERE n.qualified_name = '{member_qname}' \
                     AND n.project_id = '{pid}' AND c.qualified_name = '{comm_qname_escaped}' \
                     CREATE (n)-[:CodeRelation {{type: 'MEMBER_OF', confidence: 1.0, reason: 'louvain', step: 0}}]->(c)",
                ));
            }
        }

        communities_detected += 1;
        surviving_cid += 1;
    }

    // 6. Execute node and edge batches.
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
        communities = communities_detected,
        nodes = nodes_created,
        edges = edges_created,
        modularity_q = modularity_q,
        "community detection complete"
    );

    Ok(CommunityResult {
        communities_detected,
        nodes_created,
        edges_created,
    })
}

/// Leiden community detection: Louvain local-moving + refinement +
/// aggregation loop. Matches GitNexus's algorithm with resolution
/// parameter and timeout protection.
fn leiden(adj: &[HashMap<usize, f64>]) -> (Vec<usize>, f64, f64) {
    let n = adj.len();
    if n == 0 {
        return (Vec::new(), 0.0, 0.0);
    }

    let large = n > 10_000;
    let resolution = if large { 2.0 } else { 1.0 };
    let max_levels: usize = if large { 3 } else { 10 };
    let timeout = std::time::Duration::from_secs(60);
    let start = std::time::Instant::now();

    let weighted_deg: Vec<f64> = adj.iter().map(|m| m.values().sum::<f64>()).collect();
    let m_total = weighted_deg.iter().sum::<f64>() / 2.0;
    if m_total <= 0.0 {
        return ((0..n).collect(), 0.0, 0.0);
    }

    // Phase 1: Local moving (Louvain)
    let (mut community, mut best_q) = louvain_local_moving(adj, &weighted_deg, m_total, resolution);

    // Iterative refinement + aggregation
    let mut current_adj: Vec<HashMap<usize, f64>> = adj.to_vec();
    let mut current_deg = weighted_deg.clone();
    let mut node_map: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();

    for _level in 0..max_levels {
        if start.elapsed() > timeout {
            tracing::warn!("leiden timeout — returning current partition");
            break;
        }

        // Phase 2: Refinement — try improving within each community
        let refined = leiden_refine(&current_adj, &community, &current_deg, m_total, resolution);

        // Phase 3: Aggregate into super-nodes
        let num_comms = *refined.iter().max().unwrap_or(&0) + 1;
        if num_comms >= current_adj.len() {
            break;
        }

        let (new_adj, new_deg, new_map) = aggregate(&current_adj, &refined, &node_map);
        if new_adj.len() >= current_adj.len() {
            break;
        }

        current_adj = new_adj;
        current_deg = new_deg;
        node_map = new_map;

        let m_agg = current_deg.iter().sum::<f64>() / 2.0;
        if m_agg <= 0.0 { break; }

        let (new_comm, q) = louvain_local_moving(&current_adj, &current_deg, m_agg, resolution);
        if q <= best_q { break; }
        best_q = q;
        community = new_comm;
    }

    // Unpack super-nodes back to original node assignments
    let mut final_comm = vec![0usize; n];
    for (super_id, members) in node_map.iter().enumerate() {
        let c = community.get(super_id).copied().unwrap_or(super_id);
        for &orig in members {
            if orig < n { final_comm[orig] = c; }
        }
    }

    // Renumber communities to contiguous 0..k
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0;
    for c in &mut final_comm {
        let new_id = *remap.entry(*c).or_insert_with(|| { let id = next_id; next_id += 1; id });
        *c = new_id;
    }

    (final_comm, best_q, m_total)
}

/// Louvain local-moving phase with resolution parameter.
fn louvain_local_moving(
    adj: &[HashMap<usize, f64>],
    weighted_deg: &[f64],
    m_total: f64,
    resolution: f64,
) -> (Vec<usize>, f64) {
    let n = adj.len();
    let mut community: Vec<usize> = (0..n).collect();
    let mut sum_tot: Vec<f64> = weighted_deg.to_vec();
    let mut sum_in: Vec<f64> = vec![0.0; n];

    const MAX_PASSES: usize = 10;

    for _pass in 0..MAX_PASSES {
        let mut moved = false;
        for i in 0..n {
            let ki = weighted_deg[i];
            let current = community[i];
            let mut k_i_to: HashMap<usize, f64> = HashMap::new();
            for (&j, &w) in &adj[i] {
                if j == i { continue; }
                *k_i_to.entry(community[j]).or_insert(0.0) += w;
            }

            let ki_to_current = k_i_to.get(&current).copied().unwrap_or(0.0);
            sum_tot[current] -= ki;
            sum_in[current] -= 2.0 * ki_to_current;

            let mut best_c = current;
            let mut best_gain = 0.0;
            for (&c, &k_i_c) in &k_i_to {
                let gain = k_i_c / m_total
                    - resolution * sum_tot[c] * ki / (2.0 * m_total * m_total);
                if gain > best_gain {
                    best_gain = gain;
                    best_c = c;
                }
            }

            let ki_to_best = k_i_to.get(&best_c).copied().unwrap_or(0.0);
            sum_tot[best_c] += ki;
            sum_in[best_c] += 2.0 * ki_to_best;
            community[i] = best_c;
            if best_c != current { moved = true; }
        }
        if !moved { break; }
    }

    let two_m = 2.0 * m_total;
    let mut seen: HashSet<usize> = HashSet::new();
    let mut q = 0.0;
    for &c in &community {
        if seen.insert(c) {
            q += (sum_in[c] / two_m) - (sum_tot[c] / two_m).powi(2);
        }
    }

    (community, q)
}

/// Leiden refinement: within each Louvain community, try moving nodes
/// between refined sub-communities for modularity improvement.
fn leiden_refine(
    adj: &[HashMap<usize, f64>],
    partition: &[usize],
    weighted_deg: &[f64],
    m_total: f64,
    resolution: f64,
) -> Vec<usize> {
    let n = adj.len();
    let mut refined: Vec<usize> = (0..n).collect();

    let mut comm_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in partition.iter().enumerate() {
        comm_members.entry(c).or_default().push(i);
    }

    for members in comm_members.values() {
        if members.len() <= 1 { continue; }
        for &node in members {
            let mut best_target = refined[node];
            let mut best_gain = 0.0_f64;
            for (&nbr, &w) in &adj[node] {
                if partition[nbr] != partition[node] { continue; }
                let target = refined[nbr];
                if target == refined[node] { continue; }
                let ki = weighted_deg[node];
                let gain = w / m_total - resolution * ki * weighted_deg[nbr] / (2.0 * m_total * m_total);
                if gain > best_gain {
                    best_gain = gain;
                    best_target = target;
                }
            }
            refined[node] = best_target;
        }
    }

    refined
}

type AggregateResult = (Vec<HashMap<usize, f64>>, Vec<f64>, Vec<Vec<usize>>);

/// Collapse communities into super-nodes for the next Leiden level.
fn aggregate(
    adj: &[HashMap<usize, f64>],
    partition: &[usize],
    node_map: &[Vec<usize>],
) -> AggregateResult {
    let mut id_map: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    for &c in partition {
        id_map.entry(c).or_insert_with(|| { let id = next_id; next_id += 1; id });
    }
    let k = next_id;

    let mut new_adj: Vec<HashMap<usize, f64>> = vec![HashMap::new(); k];
    for (i, neighbors) in adj.iter().enumerate() {
        let ci = id_map[&partition[i]];
        for (&j, &w) in neighbors {
            let cj = id_map[&partition[j]];
            if ci != cj {
                *new_adj[ci].entry(cj).or_insert(0.0) += w;
            }
        }
    }

    let new_deg: Vec<f64> = new_adj.iter().map(|m| m.values().sum::<f64>()).collect();

    let mut new_map: Vec<Vec<usize>> = vec![Vec::new(); k];
    for (i, members) in node_map.iter().enumerate() {
        let ci = id_map[&partition[i]];
        new_map[ci].extend_from_slice(members);
    }

    (new_adj, new_deg, new_map)
}

/// Build a human-readable label for a community from its member metadata.
///
/// Strategy:
/// 1. Find the longest shared directory prefix across member source files.
/// 2. Count member kinds (Function, Method, Class, Struct, …).
/// 3. Format as `prefix/ — N kind1, M kind2` with the top two kinds.
fn build_community_label(members: &[&SymbolMeta], fallback_idx: usize) -> String {
    if members.is_empty() {
        return format!("cluster_{fallback_idx}");
    }

    // Longest common directory prefix.
    let files: Vec<&str> = members.iter().map(|m| m.source_file.as_str()).collect();
    let prefix = longest_common_dir_prefix(&files);

    // Count kinds. Collapse Method under functions for readability.
    let mut kind_counts: HashMap<&str, usize> = HashMap::new();
    for m in members {
        let kind = match m.label.as_str() {
            "Function" | "Method" => "function",
            "Class" => "class",
            "Interface" => "interface",
            "Struct" => "struct",
            "Trait" => "trait",
            "Enum" => "enum",
            "TypeAlias" => "type alias",
            "Const" => "const",
            other => other,
        };
        *kind_counts.entry(kind).or_insert(0) += 1;
    }

    let mut sorted: Vec<(&str, usize)> = kind_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    let kinds_str = sorted
        .iter()
        .take(2)
        .map(|(k, n)| format!("{n} {k}{}", if *n != 1 { "s" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ");

    let prefix_label = if prefix.is_empty() {
        format!("cluster_{fallback_idx}")
    } else {
        format!("{prefix}/")
    };

    if kinds_str.is_empty() {
        prefix_label
    } else {
        format!("{prefix_label} — {kinds_str}")
    }
}

/// Count distinct source files and function/method members in a community.
fn count_files_and_functions(members: &[&SymbolMeta]) -> (u64, u64) {
    let mut files: HashSet<&str> = HashSet::new();
    let mut functions = 0u64;
    for m in members {
        files.insert(m.source_file.as_str());
        if matches!(m.label.as_str(), "Function" | "Method") {
            functions += 1;
        }
    }
    (files.len() as u64, functions)
}

/// Extract dominant keywords from community member names via term
/// frequency. Splits camelCase/snake_case identifiers into words,
/// counts, and returns the top 5 most frequent terms (>= 2 chars).
fn extract_keywords(members: &[&SymbolMeta]) -> Vec<String> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for m in members {
        let name = &m.label;
        // Use source_file basename + the label type as a proxy, but
        // really we want the symbol names. We get the qname's last segment.
        let _ = name; // label is the type, not the symbol name
    }
    // Split qualified names into word tokens from the symbol metadata
    // source_file paths to extract directory/module-level keywords.
    for m in members {
        for segment in m.source_file.split('/') {
            let segment = segment.split('.').next().unwrap_or(segment);
            for word in split_identifier(segment) {
                let lower = word.to_lowercase();
                if lower.len() >= 2 && !is_stop_word(&lower) {
                    *freq.entry(lower).or_insert(0) += 1;
                }
            }
        }
    }

    let mut sorted: Vec<(String, usize)> = freq.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted.into_iter().take(5).map(|(w, _)| w).collect()
}

fn split_identifier(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if ch == '_' || ch == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if ch.is_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
            current.push(ch.to_ascii_lowercase());
        } else {
            current.push(ch.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_stop_word(w: &str) -> bool {
    matches!(
        w,
        "src" | "lib" | "mod" | "rs" | "ts" | "js" | "py" | "go" | "java"
            | "index" | "main" | "test" | "spec" | "utils" | "util" | "the"
            | "and" | "for" | "with"
    )
}

/// Find the longest directory-level common prefix across a set of
/// forward-slash-normalized file paths. Returns an empty string if the
/// files share no common directory.
fn longest_common_dir_prefix(paths: &[&str]) -> String {
    if paths.is_empty() {
        return String::new();
    }

    // Split each path into directory components (strip the file name).
    let split: Vec<Vec<&str>> = paths
        .iter()
        .map(|p| {
            let no_file = p.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
            no_file.split('/').filter(|s| !s.is_empty()).collect()
        })
        .collect();

    if split.iter().any(|c| c.is_empty()) {
        return String::new();
    }

    let mut prefix: Vec<&str> = Vec::new();
    let first = &split[0];
    'outer: for (i, seg) in first.iter().enumerate() {
        for other in &split[1..] {
            if other.get(i) != Some(seg) {
                break 'outer;
            }
        }
        prefix.push(seg);
    }

    prefix.join("/")
}

/// Communities phase: Louvain clustering over the call/heritage graph
/// with Community nodes + MEMBER_OF edges persisted to LadybugDB.
pub struct CommunitiesPhase;

#[async_trait::async_trait]
impl Phase for CommunitiesPhase {
    fn label(&self) -> &'static str {
        "communities"
    }

    fn phase(&self) -> Option<PipelinePhase> {
        Some(PipelinePhase::Communities)
    }

    async fn run(&self, ctx: &mut PhaseCtx) -> Result<()> {
        ctx.emit_progress(PipelinePhase::Communities, 0.0, "Detecting communities");
        let result = detect_communities(&ctx.project_id, &ctx.db, &ctx.config).await?;
        ctx.stats.communities_detected = result.communities_detected;
        ctx.stats.nodes_created += result.nodes_created;
        ctx.stats.edges_created += result.edges_created;
        ctx.emit_progress(
            PipelinePhase::Communities,
            1.0,
            &format!("Detected {} communities", result.communities_detected),
        );
        Ok(())
    }
}
