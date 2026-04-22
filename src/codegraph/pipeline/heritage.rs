//! Resolve extends/implements/inherits relationships.

use std::collections::HashMap;

use anyhow::Result;

use super::phase::{Phase, PhaseCtx};
use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::types::PipelinePhase;

fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

struct HeritageNode {
    qualified_name: String,
    extends_type: String,
    label: String,
}

/// Resolve inheritance relationships and create heritage edges.
pub async fn resolve_heritage(
    project_id: &str,
    db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let mut result = PhaseResult::default();
    let pid = cypher_escape(project_id);

    tracing::debug!(project_id = %project_id, "resolving heritage");

    let inheritable_labels = &["Class", "Interface", "Struct", "Trait"];

    // 1. Query all inheritable nodes with extends_type, per label.
    let mut sources: Vec<HeritageNode> = Vec::new();
    let mut targets_by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut targets_by_qname: HashMap<String, (String, String)> = HashMap::new();

    for &label in inheritable_labels {
        let rows = db.query(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' AND n.extends_type <> '' \
             RETURN n.qualified_name, n.extends_type"
        )).await?;

        for row in &rows {
            if let (
                Some(lbug::Value::String(qn)),
                Some(lbug::Value::String(ext)),
            ) = (row.first(), row.get(1))
            {
                sources.push(HeritageNode {
                    qualified_name: qn.clone(),
                    extends_type: ext.clone(),
                    label: label.to_string(),
                });
            }
        }

        // Also build target lookup (all nodes, not just ones with extends)
        let all = db.query(&format!(
            "MATCH (n:{label}) WHERE n.project_id = '{pid}' \
             RETURN n.qualified_name, n.name"
        )).await?;

        for row in &all {
            if let (
                Some(lbug::Value::String(qname)),
                Some(lbug::Value::String(name)),
            ) = (row.first(), row.get(1))
            {
                targets_by_name
                    .entry(name.clone())
                    .or_default()
                    .push((qname.clone(), label.to_string()));
                targets_by_qname
                    .insert(qname.clone(), (qname.clone(), label.to_string()));
            }
        }
    }

    if sources.is_empty() {
        tracing::info!(project_id = %project_id, "no heritage relationships found");
        return Ok(result);
    }

    // 2. For each source, resolve extends_type to a target and create edge.
    let mut edge_stmts: Vec<String> = Vec::new();

    for src in &sources {
        for parent_name in src.extends_type.split(',').map(|s| s.trim()) {
            if parent_name.is_empty() {
                continue;
            }

            // Tier 1: Try exact qualified name match (highest confidence).
            let (tgt_qname, tgt_label, confidence) =
                if let Some((qn, lbl)) = targets_by_qname.get(parent_name) {
                    (qn.clone(), lbl.clone(), 0.95)
                }
                // Tier 2: Simple name match.
                else if let Some(targets) = targets_by_name.get(parent_name) {
                    if targets.len() > 1 {
                        tracing::debug!(
                            source = %src.qualified_name,
                            parent = %parent_name,
                            candidates = targets.len(),
                            "ambiguous heritage target, using first match"
                        );
                    }
                    match targets.first() {
                        Some((qn, lbl)) => (qn.clone(), lbl.clone(), 0.80),
                        None => continue,
                    }
                } else {
                    tracing::trace!(
                        source = %src.qualified_name,
                        parent = %parent_name,
                        "heritage target not found, skipping"
                    );
                    continue;
                };

            // If target is Interface/Trait → IMPLEMENTS, else EXTENDS
            let edge_type = if tgt_label == "Interface" || tgt_label == "Trait" {
                "IMPLEMENTS"
            } else {
                "EXTENDS"
            };

            let src_escaped = cypher_escape(&src.qualified_name);
            let tgt_escaped = cypher_escape(&tgt_qname);
            let src_label = &src.label;

            edge_stmts.push(format!(
                "MATCH (a:{src_label}), (b:{tgt_label}) \
                 WHERE a.qualified_name = '{src_escaped}' AND a.project_id = '{pid}' \
                 AND b.qualified_name = '{tgt_escaped}' AND b.project_id = '{pid}' \
                 CREATE (a)-[:CodeRelation {{type: '{edge_type}', confidence: {confidence}, reason: 'heritage', step: 0}}]->(b)",
            ));
        }
    }

    if !edge_stmts.is_empty() {
        let batch = db.execute_batch(edge_stmts).await?;
        result.edges_created += batch.success;
        result.errors += batch.errors;
    }

    tracing::info!(
        project_id = %project_id,
        edges = result.edges_created,
        errors = result.errors,
        "heritage resolution complete"
    );

    Ok(result)
}

/// Heritage phase: resolves `extends` / `implements` edges, then runs the
/// overrides pass (C3-linearized MRO) in the same phase because they
/// share state and report combined progress to the UI.
pub struct HeritagePhase;

#[async_trait::async_trait]
impl Phase for HeritagePhase {
    fn label(&self) -> &'static str {
        "heritage"
    }

    fn phase(&self) -> Option<PipelinePhase> {
        Some(PipelinePhase::Heritage)
    }

    async fn run(&self, ctx: &mut PhaseCtx) -> Result<()> {
        ctx.emit_progress(PipelinePhase::Heritage, 0.0, "Resolving inheritance");

        let heritage_result = resolve_heritage(&ctx.project_id, &ctx.db).await?;
        ctx.stats.edges_created += heritage_result.edges_created;

        ctx.emit_progress(
            PipelinePhase::Heritage,
            0.5,
            "Inheritance resolved, computing overrides",
        );

        let overrides_result =
            super::overrides::resolve_overrides(&ctx.project_id, &ctx.db).await?;
        ctx.stats.edges_created += overrides_result.edges_created;

        ctx.emit_progress(PipelinePhase::Heritage, 1.0, "Heritage resolved");
        Ok(())
    }
}
