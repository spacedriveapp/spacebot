//! Phase 5: Resolve call-sites with confidence scoring.

use anyhow::Result;

use super::PhaseResult;
use crate::codegraph::db::SharedCodeGraphDb;

/// Resolve function/method call-sites and create CALLS edges.
///
/// Uses three resolution strategies with different confidence scores:
/// - Same-file direct: 0.95 confidence
/// - Import-scoped: 0.90 confidence
/// - Global fuzzy: 0.50 confidence
pub async fn resolve_calls(
    project_id: &str,
    _db: &SharedCodeGraphDb,
) -> Result<PhaseResult> {
    let result = PhaseResult::default();

    tracing::debug!(
        project_id = %project_id,
        "resolving call-sites (stub)"
    );

    // Will be implemented with actual graph queries once kuzu is integrated.
    // The flow:
    // 1. Query all Function/Method nodes
    // 2. For each, scan the AST for call expressions
    // 3. Attempt resolution: same-file → import-scoped → global fuzzy
    // 4. Create CALLS edges with confidence scores

    Ok(result)
}
