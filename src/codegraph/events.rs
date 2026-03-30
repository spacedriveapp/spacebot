//! Code graph event types for bidirectional cortex ↔ code graph communication.

use serde::{Deserialize, Serialize};

use super::types::{IndexStatus, PipelinePhase, PipelineStats};

// ---------------------------------------------------------------------------
// Code Graph → Cortex events
// ---------------------------------------------------------------------------

/// Events fired by the code graph system, consumed by the cortex and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum CodeGraphEvent {
    /// Full pipeline completed successfully.
    GraphIndexed {
        project_id: String,
        stats: PipelineStats,
    },
    /// Incremental update completed (file watcher triggered).
    GraphChanged {
        project_id: String,
        changed_files: Vec<String>,
        added_symbols: Vec<String>,
        removed_symbols: Vec<String>,
        changed_symbols: Vec<String>,
    },
    /// File watcher detected changes but re-index hasn't started yet.
    GraphStale {
        project_id: String,
        stale_files: Vec<String>,
    },
    /// Pipeline phase failed.
    GraphError {
        project_id: String,
        phase: Option<PipelinePhase>,
        error: String,
    },
    /// Project was cascade-deleted.
    ProjectRemoved {
        project_id: String,
    },
    /// Pipeline progress update (for live UI updates).
    IndexProgress {
        project_id: String,
        phase: PipelinePhase,
        phase_progress: f32,
        message: String,
    },
}

impl CodeGraphEvent {
    pub fn project_id(&self) -> &str {
        match self {
            Self::GraphIndexed { project_id, .. }
            | Self::GraphChanged { project_id, .. }
            | Self::GraphStale { project_id, .. }
            | Self::GraphError { project_id, .. }
            | Self::ProjectRemoved { project_id }
            | Self::IndexProgress { project_id, .. } => project_id,
        }
    }

    /// Derive the new project index status from this event.
    pub fn implied_status(&self) -> Option<IndexStatus> {
        match self {
            Self::GraphIndexed { .. } => Some(IndexStatus::Indexed),
            Self::GraphChanged { .. } => Some(IndexStatus::Indexed),
            Self::GraphStale { .. } => Some(IndexStatus::Stale),
            Self::GraphError { .. } => Some(IndexStatus::Error),
            Self::IndexProgress { .. } => Some(IndexStatus::Indexing),
            Self::ProjectRemoved { .. } => None,
        }
    }
}
