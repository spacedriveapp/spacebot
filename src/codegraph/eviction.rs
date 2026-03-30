//! Automatic stale memory eviction.
//!
//! After `graph_changed` or `graph_indexed` events, the eviction system
//! evaluates project memories for relevance and removes memories that are
//! no longer accurate.

use anyhow::Result;

use super::events::CodeGraphEvent;
use super::types::{StalenessCheck, StalenessCheckType};

/// Run a staleness evaluation pass triggered by a code graph event.
///
/// Checks all project memories that reference changed symbols and determines
/// whether they are still current, need updating, or should be removed.
pub async fn run_eviction_pass(
    project_id: &str,
    event: &CodeGraphEvent,
) -> Result<Vec<StalenessCheck>> {
    let checks = Vec::new();

    let check_type = match event {
        CodeGraphEvent::GraphChanged { .. } => StalenessCheckType::CodeChange,
        CodeGraphEvent::GraphIndexed { .. } => StalenessCheckType::Scheduled,
        _ => return Ok(checks),
    };

    tracing::debug!(
        project_id = %project_id,
        check_type = ?check_type,
        "running stale memory eviction pass (stub)"
    );

    // Will be implemented when the centralized project memory store is ready.
    // The flow:
    // 1. Get changed/removed/added symbols from the event
    // 2. Query project memories that reference any of these symbols
    // 3. For each affected memory, re-evaluate against current graph state
    // 4. Mark stale memories for removal (with 24h grace period)
    // 5. Log all eviction decisions to memory_eviction.log

    Ok(checks)
}

/// Run a scheduled staleness check (e.g., every 24 hours).
pub async fn run_scheduled_eviction(project_id: &str) -> Result<Vec<StalenessCheck>> {
    tracing::debug!(
        project_id = %project_id,
        "running scheduled stale memory eviction (stub)"
    );

    // Will query all project memories where last_verified_at > 30 days
    // or relevance_score < threshold.

    Ok(Vec::new())
}
