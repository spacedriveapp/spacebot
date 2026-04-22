//! Pipeline phase abstraction.
//!
//! Each indexing phase (walker, structure, parsing, imports, calls,
//! heritage, routes, communities, processes, enriching, complete) is a
//! `Phase` impl. The orchestrator in `mod.rs` walks a static `Vec<Box<dyn
//! Phase>>`, checks cancellation between phases, times each phase, and
//! delegates the actual work to `Phase::run`.
//!
//! Phases share mutable state through [`PhaseCtx`]: inputs
//! (`project_id`, `root_path`, `db`, `config`, event/progress channels)
//! stay constant for a run, while `stats`, `phase_timings`, and
//! inter-phase data like `files` and `import_map` accumulate as phases
//! complete.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, watch};

use super::walker::WalkOutcome;
use super::{PhaseResult, ProgressFn};
use crate::codegraph::config::ConfigContext;
use crate::codegraph::db::SharedCodeGraphDb;
use crate::codegraph::events::CodeGraphEvent;
use crate::codegraph::types::{CodeGraphConfig, PipelinePhase, PipelineProgress, PipelineStats};

/// Shared state threaded through every pipeline phase.
///
/// Fields fall into three groups:
/// - **Inputs** (constant): identity, paths, db handle, config, event buses.
/// - **Accumulators**: `stats` and `phase_timings` grow as phases run.
/// - **Inter-phase data**: `walk_outcome` / `files` (produced by the walker,
///   consumed by every downstream phase) and `import_map` (produced by
///   imports, consumed by calls).
pub struct PhaseCtx {
    pub project_id: String,
    pub root_path: PathBuf,
    pub db: SharedCodeGraphDb,
    pub config: Arc<CodeGraphConfig>,
    pub event_tx: broadcast::Sender<CodeGraphEvent>,
    pub progress_tx: Arc<watch::Sender<PipelineProgress>>,

    pub stats: PipelineStats,
    pub phase_timings: HashMap<String, f64>,

    pub walk_outcome: Option<WalkOutcome>,
    pub files: Vec<PathBuf>,
    pub import_map: HashMap<String, HashSet<String>>,
    /// Build-system configuration discovered during the structure
    /// phase. The imports phase consumes this to resolve path aliases
    /// (tsconfig), module prefixes (go.mod), PSR-4 namespaces, etc.
    pub config_context: ConfigContext,
}

impl PhaseCtx {
    /// Build a fresh context for a pipeline run.
    pub fn new(
        project_id: String,
        root_path: PathBuf,
        db: SharedCodeGraphDb,
        config: Arc<CodeGraphConfig>,
        event_tx: broadcast::Sender<CodeGraphEvent>,
        progress_tx: Arc<watch::Sender<PipelineProgress>>,
    ) -> Self {
        Self {
            project_id,
            root_path,
            db,
            config,
            event_tx,
            progress_tx,
            stats: PipelineStats::default(),
            phase_timings: HashMap::new(),
            walk_outcome: None,
            files: Vec::new(),
            import_map: HashMap::new(),
            config_context: ConfigContext::default(),
        }
    }

    /// Send a progress update to both the watch channel and the event bus.
    /// The current `stats` snapshot is attached.
    pub fn emit_progress(&self, phase: PipelinePhase, progress: f32, msg: &str) {
        let _ = self.progress_tx.send(PipelineProgress {
            phase,
            phase_progress: progress,
            message: msg.to_string(),
            stats: self.stats.clone(),
        });
        let _ = self.event_tx.send(CodeGraphEvent::IndexProgress {
            project_id: self.project_id.clone(),
            phase,
            phase_progress: progress,
            message: msg.to_string(),
        });
    }

    /// Build a [`ProgressFn`] callback for phases that invoke it many
    /// times (walker, parsing, calls).
    ///
    /// `base` is a snapshot of the current `stats` so each intermediate
    /// update reports a coherent total without double-counting per-phase
    /// progress on top of itself. `merge` is applied to combine the
    /// phase's in-flight `PhaseResult` into the snapshot before emission.
    pub fn make_progress_fn<F>(&self, phase: PipelinePhase, merge: F) -> ProgressFn
    where
        F: Fn(&mut PipelineStats, &PhaseResult) + Send + Sync + 'static,
    {
        let tx = Arc::clone(&self.progress_tx);
        let etx = self.event_tx.clone();
        let pid = self.project_id.clone();
        let base = self.stats.clone();
        Arc::new(move |pct: f32, msg: &str, pr: &PhaseResult| {
            let mut merged = base.clone();
            merge(&mut merged, pr);
            let _ = tx.send(PipelineProgress {
                phase,
                phase_progress: pct,
                message: msg.to_string(),
                stats: merged,
            });
            let _ = etx.send(CodeGraphEvent::IndexProgress {
                project_id: pid.clone(),
                phase,
                phase_progress: pct,
                message: msg.to_string(),
            });
        })
    }
}

/// An indexing pipeline phase.
///
/// The orchestrator composes phases into a static list and runs them
/// sequentially, checking cancellation between phases. Implementations
/// should mutate `ctx` as needed — `stats`, inter-phase data, and
/// optional additional `phase_timings` entries — and may call
/// `ctx.emit_progress` to report user-visible progress.
#[async_trait::async_trait]
pub trait Phase: Send + Sync {
    /// Unique stable label used as the key in `ctx.phase_timings` and in
    /// diagnostic logging. Must be unique across all registered phases.
    fn label(&self) -> &'static str;

    /// The user-facing pipeline phase this impl emits progress for, if
    /// any. `None` means the phase runs silently — no progress events.
    fn phase(&self) -> Option<PipelinePhase> {
        None
    }

    /// Run this phase against the shared context.
    async fn run(&self, ctx: &mut PhaseCtx) -> Result<()>;

    /// Relative weight for overall progress-bar allocation. Reserved for
    /// future use; today the UI renders fixed per-phase segments.
    fn progress_weight(&self) -> u32 {
        1
    }

    /// Whether this phase is safe to run in the incremental re-index
    /// pipeline. Today only the full pipeline uses this flag; the
    /// incremental path in `incremental.rs` calls phase functions
    /// directly.
    fn is_incremental_safe(&self) -> bool {
        true
    }
}
