//! Indexing pipeline orchestrator.
//!
//! Each phase runs sequentially, reporting progress via a
//! `watch::Sender<PipelineProgress>`. Phases are `Phase` impls (see
//! [`phase::Phase`]) living in their own modules. `run_pipeline` walks a
//! static `Vec<Box<dyn Phase>>`, checks cancellation between phases,
//! records per-phase timings, and returns the final [`PipelineStats`].

pub mod calls;
pub mod communities;
pub mod complete;
pub mod embeddings;
pub mod enriching;
pub mod fts;
pub mod heritage;
pub mod imports;
pub mod incremental;
pub mod named_bindings;
pub mod overrides;
pub mod parsing;
pub mod phase;
pub mod processes;
pub mod routes;
pub mod structure;
pub mod walker;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::watch;

pub use phase::{Phase, PhaseCtx};

/// Callback for phases to report intermediate progress.
/// Arguments: (phase_progress 0.0–1.0, message, current phase result).
pub type ProgressFn = Arc<dyn Fn(f32, &str, &PhaseResult) + Send + Sync>;

use super::db::SharedCodeGraphDb;
use super::events::CodeGraphEvent;
use super::types::{CodeGraphConfig, PipelinePhase, PipelineProgress, PipelineStats};

/// A handle to a running pipeline, allowing progress monitoring and cancellation.
pub struct PipelineHandle {
    /// Watch receiver for live progress updates.
    pub progress_rx: watch::Receiver<PipelineProgress>,
    /// Set to `true` to request cancellation.
    cancel_tx: watch::Sender<bool>,
    /// Join handle for the pipeline task.
    join_handle: tokio::task::JoinHandle<Result<PipelineStats>>,
}

impl PipelineHandle {
    /// Cancel the pipeline. The pipeline will stop after completing the current phase.
    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }

    /// Wait for the pipeline to complete, returning final stats.
    pub async fn wait(self) -> Result<PipelineStats> {
        self.join_handle
            .await
            .context("pipeline task panicked")?
    }
}

/// Start a full indexing pipeline for a project.
///
/// Returns a `PipelineHandle` for monitoring progress and cancellation.
/// The pipeline is fully deterministic — every phase runs against
/// LadybugDB only, no model is invoked, and no network call is made.
#[allow(clippy::too_many_arguments)]
pub fn start_full_pipeline(
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    config: Arc<CodeGraphConfig>,
    event_tx: tokio::sync::broadcast::Sender<CodeGraphEvent>,
) -> PipelineHandle {
    let initial_progress = PipelineProgress {
        phase: PipelinePhase::Extracting,
        phase_progress: 0.0,
        message: "Starting indexing pipeline".to_string(),
        stats: PipelineStats::default(),
    };

    let (progress_tx, progress_rx) = watch::channel(initial_progress);
    let progress_tx = Arc::new(progress_tx);
    let (cancel_tx, cancel_rx) = watch::channel(false);

    let join_handle = tokio::spawn(run_pipeline(
        project_id,
        root_path,
        db,
        config,
        progress_tx,
        cancel_rx,
        event_tx,
    ));

    PipelineHandle {
        progress_rx,
        cancel_tx,
        join_handle,
    }
}

/// Build the static phase list executed by [`run_pipeline`].
///
/// Ordering is load-bearing: walker produces `files`, imports produces
/// `import_map`, enriching's Parameter cleanup assumes calls has already
/// emitted CALLS edges. Changing the order requires auditing every
/// downstream phase's assumptions about `PhaseCtx` state.
fn default_phases() -> Vec<Box<dyn Phase>> {
    vec![
        Box::new(walker::ExtractingPhase),
        Box::new(structure::StructurePhase),
        Box::new(parsing::ParsingPhase),
        Box::new(imports::ImportsPhase),
        Box::new(calls::CallsPhase),
        Box::new(heritage::HeritagePhase),
        Box::new(routes::RoutesPhase),
        // NamedBindings runs after symbol nodes exist (parsing) and
        // after routes create framework-aware structures, so every
        // consumer/provider lookup has the full graph to resolve
        // against. No UI phase — runs silently between routes and
        // communities.
        Box::new(named_bindings::NamedBindingsPhase),
        Box::new(communities::CommunitiesPhase),
        Box::new(processes::ProcessesPhase),
        Box::new(enriching::EnrichingPhase),
        Box::new(complete::CompletePhase),
    ]
}

#[allow(clippy::too_many_arguments)]
async fn run_pipeline(
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    config: Arc<CodeGraphConfig>,
    progress_tx: Arc<watch::Sender<PipelineProgress>>,
    cancel_rx: watch::Receiver<bool>,
    event_tx: tokio::sync::broadcast::Sender<CodeGraphEvent>,
) -> Result<PipelineStats> {
    let pipeline_start = Instant::now();

    db.ensure_schema().await?;

    // Note: graph data purge for re-index scenarios happens in
    // manager.rs::remove_project() — not here — so indexing starts instantly.

    let mut ctx = PhaseCtx::new(
        project_id.clone(),
        root_path,
        db,
        config,
        event_tx.clone(),
        progress_tx,
    );

    // Cancellation is checked between phases, never inside one. A
    // long-running phase like parsing or community detection will run
    // to completion even if cancel is signalled mid-phase — phases must
    // not block on this channel themselves.
    for phase in default_phases() {
        if *cancel_rx.borrow() {
            tracing::info!(project_id = %project_id, "pipeline cancelled");
            return Ok(ctx.stats);
        }

        let phase_start = Instant::now();
        phase.run(&mut ctx).await?;
        ctx.phase_timings
            .insert(phase.label().to_string(), phase_start.elapsed().as_secs_f64());
    }

    tracing::info!(
        project_id = %project_id,
        files = ctx.stats.files_found,
        nodes = ctx.stats.nodes_created,
        edges = ctx.stats.edges_created,
        communities = ctx.stats.communities_detected,
        processes = ctx.stats.processes_traced,
        duration_secs = pipeline_start.elapsed().as_secs_f64(),
        "indexing pipeline complete"
    );

    Ok(ctx.stats)
}

/// Check whether a project's index is stale by comparing the stored
/// commit hash in meta.json against the current HEAD.
pub async fn check_staleness(
    root_path: &std::path::Path,
    meta_path: &std::path::Path,
) -> (bool, Option<String>, Option<String>) {
    let current_head = complete::read_git_head(root_path).await;
    let stored_commit = tokio::fs::read_to_string(meta_path)
        .await
        .ok()
        .and_then(|json| {
            serde_json::from_str::<super::types::ProjectMeta>(&json)
                .ok()
                .and_then(|m| m.last_commit)
        });
    let is_stale = match (&current_head, &stored_commit) {
        (Some(head), Some(stored)) => head != stored,
        (Some(_), None) => true,
        _ => false,
    };
    (is_stale, current_head, stored_commit)
}

/// Result from a single pipeline phase.
#[derive(Debug, Default)]
pub struct PhaseResult {
    pub nodes_created: u64,
    pub edges_created: u64,
    pub files_parsed: u64,
    pub files_skipped: u64,
    pub communities_detected: u64,
    pub processes_traced: u64,
    pub errors: u64,
}
