//! 10-phase indexing pipeline orchestrator.
//!
//! Matches GitNexus's `runFullAnalysis` orchestrator. Each phase runs
//! sequentially, reporting progress via a `watch::Sender<PipelineProgress>`.

pub mod walker;
pub mod structure;
pub mod parsing;
pub mod imports;
pub mod calls;
pub mod heritage;
pub mod communities;
pub mod processes;
pub mod enriching;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::watch;

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

/// The main pipeline execution function.
async fn run_pipeline(
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    config: Arc<CodeGraphConfig>,
    progress_tx: watch::Sender<PipelineProgress>,
    cancel_rx: watch::Receiver<bool>,
    event_tx: tokio::sync::broadcast::Sender<CodeGraphEvent>,
) -> Result<PipelineStats> {
    let pipeline_start = Instant::now();
    let mut stats = PipelineStats::default();
    let mut phase_timings = std::collections::HashMap::new();

    // Ensure the database schema is initialized.
    db.ensure_schema().await?;

    // Helper macro to check cancellation between phases.
    macro_rules! check_cancel {
        () => {
            if *cancel_rx.borrow() {
                tracing::info!(project_id = %project_id, "pipeline cancelled");
                return Ok(stats);
            }
        };
    }

    // Helper to update progress.
    let update_progress = |phase: PipelinePhase, progress: f32, msg: &str, stats: &PipelineStats| {
        let _ = progress_tx.send(PipelineProgress {
            phase,
            phase_progress: progress,
            message: msg.to_string(),
            stats: stats.clone(),
        });
        // Also fire SSE event for live UI updates.
        let _ = event_tx.send(CodeGraphEvent::IndexProgress {
            project_id: project_id.clone(),
            phase,
            phase_progress: progress,
            message: msg.to_string(),
        });
    };

    // ── Phase 1: Extracting ──────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Extracting, 0.0, "Walking filesystem", &stats);

    let files = walker::walk_project(&root_path, &config).await?;
    stats.files_found = files.len() as u64;

    update_progress(
        PipelinePhase::Extracting,
        1.0,
        &format!("Found {} files", files.len()),
        &stats,
    );
    phase_timings.insert("extracting".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 2: Structure ───────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Structure, 0.0, "Building structural nodes", &stats);

    let structure_result = structure::build_structure(&project_id, &root_path, &files, &db).await?;
    stats.nodes_created += structure_result.nodes_created;
    stats.edges_created += structure_result.edges_created;

    update_progress(PipelinePhase::Structure, 1.0, "Structure complete", &stats);
    phase_timings.insert("structure".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 3: Parsing ─────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Parsing, 0.0, "Parsing source files", &stats);

    let parse_result = parsing::parse_files(&project_id, &root_path, &files, &db, &config).await?;
    stats.files_parsed = parse_result.files_parsed;
    stats.nodes_created += parse_result.nodes_created;
    stats.edges_created += parse_result.edges_created;

    update_progress(
        PipelinePhase::Parsing,
        1.0,
        &format!("Parsed {} files", parse_result.files_parsed),
        &stats,
    );
    phase_timings.insert("parsing".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 4: Imports ─────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Imports, 0.0, "Resolving imports", &stats);

    let import_result = imports::resolve_imports(&project_id, &db).await?;
    stats.nodes_created += import_result.nodes_created;
    stats.edges_created += import_result.edges_created;

    update_progress(PipelinePhase::Imports, 1.0, "Imports resolved", &stats);
    phase_timings.insert("imports".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 5: Calls ───────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Calls, 0.0, "Resolving call-sites", &stats);

    let call_result = calls::resolve_calls(&project_id, &db).await?;
    stats.edges_created += call_result.edges_created;

    update_progress(PipelinePhase::Calls, 1.0, "Calls resolved", &stats);
    phase_timings.insert("calls".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 6: Heritage ────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Heritage, 0.0, "Resolving inheritance", &stats);

    let heritage_result = heritage::resolve_heritage(&project_id, &db).await?;
    stats.edges_created += heritage_result.edges_created;

    update_progress(PipelinePhase::Heritage, 1.0, "Heritage resolved", &stats);
    phase_timings.insert("heritage".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 7: Communities ─────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Communities, 0.0, "Detecting communities", &stats);

    let community_result = communities::detect_communities(&project_id, &db, &config).await?;
    stats.communities_detected = community_result.communities_detected;
    stats.nodes_created += community_result.nodes_created;
    stats.edges_created += community_result.edges_created;

    update_progress(
        PipelinePhase::Communities,
        1.0,
        &format!("Detected {} communities", community_result.communities_detected),
        &stats,
    );
    phase_timings.insert("communities".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 8: Processes ───────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Processes, 0.0, "Tracing processes", &stats);

    let process_result = processes::trace_processes(&project_id, &db, &config).await?;
    stats.processes_traced = process_result.processes_traced;
    stats.nodes_created += process_result.nodes_created;
    stats.edges_created += process_result.edges_created;

    update_progress(
        PipelinePhase::Processes,
        1.0,
        &format!("Traced {} processes", process_result.processes_traced),
        &stats,
    );
    phase_timings.insert("processes".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 9: Enriching ───────────────────────────────────────────────
    let phase_start = Instant::now();
    if config.llm_enrichment && stats.nodes_created <= config.node_embedding_skip_threshold {
        update_progress(PipelinePhase::Enriching, 0.0, "Enriching with LLM labels", &stats);
        enriching::enrich(&project_id, &db).await?;
        update_progress(PipelinePhase::Enriching, 1.0, "Enrichment complete", &stats);
    } else {
        let reason = if stats.nodes_created > config.node_embedding_skip_threshold {
            format!(
                "Skipping LLM enrichment ({} nodes exceeds {} threshold)",
                stats.nodes_created, config.node_embedding_skip_threshold
            )
        } else {
            "LLM enrichment disabled".to_string()
        };
        update_progress(PipelinePhase::Enriching, 1.0, &reason, &stats);
    }
    phase_timings.insert("enriching".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Phase 10: Complete ───────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Complete, 0.0, "Finalizing index", &stats);

    let total_duration = pipeline_start.elapsed().as_secs_f64();
    tracing::info!(
        project_id = %project_id,
        files = stats.files_found,
        nodes = stats.nodes_created,
        edges = stats.edges_created,
        communities = stats.communities_detected,
        processes = stats.processes_traced,
        duration_secs = total_duration,
        "indexing pipeline complete"
    );

    // Fire the graph_indexed event.
    let _ = event_tx.send(CodeGraphEvent::GraphIndexed {
        project_id: project_id.clone(),
        stats: stats.clone(),
    });

    update_progress(PipelinePhase::Complete, 1.0, "Index complete", &stats);
    phase_timings.insert("complete".to_string(), phase_start.elapsed().as_secs_f64());

    Ok(stats)
}

/// Result from a single pipeline phase.
#[derive(Debug, Default)]
pub struct PhaseResult {
    pub nodes_created: u64,
    pub edges_created: u64,
    pub files_parsed: u64,
    pub communities_detected: u64,
    pub processes_traced: u64,
    pub errors: u64,
}
