//! Phase 8: Entry point scoring and call chain tracing.

use anyhow::Result;

use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::types::CodeGraphConfig;

/// Result with process-specific stats.
pub struct ProcessResult {
    pub processes_traced: u64,
    pub nodes_created: u64,
    pub edges_created: u64,
}

/// Score entry-point likelihood and trace call chains to create Process nodes.
///
/// Identifies functions that are likely entry points (main, handlers, exports)
/// and traces their call chains up to `max_process_depth`.
pub async fn trace_processes(
    project_id: &str,
    _db: &SharedCodeGraphDb,
    config: &CodeGraphConfig,
) -> Result<ProcessResult> {
    tracing::debug!(
        project_id = %project_id,
        max_depth = config.max_process_depth,
        "tracing processes (stub)"
    );

    // Will be implemented with actual graph traversal.
    // The flow:
    // 1. Score all Function/Method nodes for entry-point likelihood
    //    (high score: main(), handler functions, exported functions, CLI entry)
    // 2. For each entry point above threshold, trace call chain via CALLS edges
    // 3. Create Process nodes with entry_function, call_depth
    // 4. Create STEP_IN_PROCESS edges for each step in the chain

    Ok(ProcessResult {
        processes_traced: 0,
        nodes_created: 0,
        edges_created: 0,
    })
}
