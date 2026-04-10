//! Indexing pipeline orchestrator.
//!
//! Each phase runs sequentially, reporting progress via a
//! `watch::Sender<PipelineProgress>`.

pub mod walker;
pub mod structure;
pub mod parsing;
pub mod imports;
pub mod calls;
pub mod heritage;
pub mod overrides;
pub mod communities;
pub mod processes;
pub mod enriching;
pub mod incremental;
pub mod embeddings;
pub mod routes;
pub mod fts;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::watch;

/// Callback for phases to report intermediate progress.
/// Arguments: (phase_progress 0.0–1.0, message, current phase result).
pub type ProgressFn = Arc<dyn Fn(f32, &str, &PhaseResult) + Send + Sync>;

use super::db::SharedCodeGraphDb;
use super::events::CodeGraphEvent;
use super::types::{CodeGraphConfig, PipelinePhase, PipelineProgress, PipelineStats};
use crate::llm::LlmManager;
use crate::memory::EmbeddingModel;

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
///
/// Two optional integrations:
/// - `llm_manager` unlocks LLM-driven label generation in the enriching phase.
/// - `embedding_model` unlocks semantic vector embeddings for
///   Function/Method/Class nodes.
///
/// Both are optional — when `None`, the corresponding phases no-op so
/// the pipeline still completes end-to-end.
#[allow(clippy::too_many_arguments)]
pub fn start_full_pipeline(
    project_id: String,
    root_path: PathBuf,
    db: SharedCodeGraphDb,
    config: Arc<CodeGraphConfig>,
    event_tx: tokio::sync::broadcast::Sender<CodeGraphEvent>,
    llm_manager: Option<Arc<LlmManager>>,
    embedding_model: Option<Arc<EmbeddingModel>>,
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
        llm_manager,
        embedding_model,
    ));

    PipelineHandle {
        progress_rx,
        cancel_tx,
        join_handle,
    }
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
    llm_manager: Option<Arc<LlmManager>>,
    embedding_model: Option<Arc<EmbeddingModel>>,
) -> Result<PipelineStats> {
    let pipeline_start = Instant::now();
    let mut stats = PipelineStats::default();
    let mut phase_timings = std::collections::HashMap::new();

    db.ensure_schema().await?;

    // Note: graph data purge for re-index scenarios happens in
    // manager.rs::remove_project() — not here — so indexing starts instantly.

    // Cancellation is checked between phases, never inside one. A
    // long-running phase like parsing or community detection will run
    // to completion even if cancel is signalled mid-phase — phases must
    // not block on this channel themselves.
    macro_rules! check_cancel {
        () => {
            if *cancel_rx.borrow() {
                tracing::info!(project_id = %project_id, "pipeline cancelled");
                return Ok(stats);
            }
        };
    }

    let update_progress = |phase: PipelinePhase, progress: f32, msg: &str, stats: &PipelineStats| {
        let _ = progress_tx.send(PipelineProgress {
            phase,
            phase_progress: progress,
            message: msg.to_string(),
            stats: stats.clone(),
        });
        let _ = event_tx.send(CodeGraphEvent::IndexProgress {
            project_id: project_id.clone(),
            phase,
            phase_progress: progress,
            message: msg.to_string(),
        });
    };

    // ── Extracting ───────────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Extracting, 0.0, "Walking filesystem", &stats);

    // Walker callback fires every WALK_PROGRESS_INTERVAL files. Total
    // file count is unknown until the walk finishes, so the walker
    // reports phase_progress at a fixed midway value (~0.5) and the
    // final 1.0 tick is sent below after walk_project returns.
    let walk_progress: ProgressFn = {
        let tx = Arc::clone(&progress_tx);
        let etx = event_tx.clone();
        let pid = project_id.clone();
        let base = stats.clone();
        Arc::new(move |pct: f32, msg: &str, _pr: &PhaseResult| {
            let _ = tx.send(PipelineProgress {
                phase: PipelinePhase::Extracting,
                phase_progress: pct,
                message: msg.to_string(),
                stats: base.clone(),
            });
            let _ = etx.send(CodeGraphEvent::IndexProgress {
                project_id: pid.clone(),
                phase: PipelinePhase::Extracting,
                phase_progress: pct,
                message: msg.to_string(),
            });
        })
    };

    let walk_outcome =
        walker::walk_project(&root_path, &config, Some(&walk_progress)).await?;
    let files = walk_outcome.files;
    stats.files_found = files.len() as u64;

    // Surface ignore-rule state in the progress message so users can see
    // whether their .spacebotignore / SPACEBOT_NO_GITIGNORE took effect.
    let mut walk_suffix_parts: Vec<String> = Vec::new();
    if walk_outcome.spacebotignore_loaded.is_some() {
        walk_suffix_parts.push(".spacebotignore applied".to_string());
    }
    if walk_outcome.gitignore_bypassed {
        walk_suffix_parts.push(".gitignore bypassed".to_string());
    }
    if walk_outcome.oversized_skipped > 0 {
        walk_suffix_parts.push(format!(
            "{} oversized skipped",
            walk_outcome.oversized_skipped
        ));
    }
    let walk_message = if walk_suffix_parts.is_empty() {
        format!("Found {} files", files.len())
    } else {
        format!(
            "Found {} files ({})",
            files.len(),
            walk_suffix_parts.join(", ")
        )
    };

    update_progress(PipelinePhase::Extracting, 1.0, &walk_message, &stats);
    phase_timings.insert("extracting".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Structure ────────────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Structure, 0.0, "Building structural nodes", &stats);

    let structure_result = structure::build_structure(&project_id, &root_path, &files, &db).await?;
    stats.nodes_created += structure_result.nodes_created;
    stats.edges_created += structure_result.edges_created;

    update_progress(PipelinePhase::Structure, 1.0, "Structure complete", &stats);
    phase_timings.insert("structure".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Parsing ──────────────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Parsing, 0.0, "Parsing source files", &stats);

    // The parse callback is invoked many times during a single phase
    // run. `base` snapshots the cumulative stats from prior phases so
    // each intermediate update reports a coherent total instead of
    // double-counting parser progress on top of itself.
    let parse_progress: ProgressFn = {
        let tx = Arc::clone(&progress_tx);
        let etx = event_tx.clone();
        let pid = project_id.clone();
        let base = stats.clone();
        Arc::new(move |pct: f32, msg: &str, pr: &PhaseResult| {
            let mut merged = base.clone();
            merged.files_parsed += pr.files_parsed;
            merged.files_skipped += pr.files_skipped;
            merged.nodes_created += pr.nodes_created;
            merged.edges_created += pr.edges_created;
            merged.errors += pr.errors;
            let _ = tx.send(PipelineProgress {
                phase: PipelinePhase::Parsing,
                phase_progress: pct,
                message: msg.to_string(),
                stats: merged,
            });
            let _ = etx.send(CodeGraphEvent::IndexProgress {
                project_id: pid.clone(),
                phase: PipelinePhase::Parsing,
                phase_progress: pct,
                message: msg.to_string(),
            });
        })
    };

    let parse_result = parsing::parse_files(&project_id, &root_path, &files, &db, &config, Some(&parse_progress)).await?;
    stats.files_parsed = parse_result.files_parsed;
    stats.files_skipped = parse_result.files_skipped;
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

    // ── Imports ──────────────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Imports, 0.0, "Resolving imports", &stats);

    let import_result = imports::resolve_imports(&project_id, &db).await?;
    stats.nodes_created += import_result.phase.nodes_created;
    stats.edges_created += import_result.phase.edges_created;
    let import_map = import_result.import_map;

    update_progress(PipelinePhase::Imports, 1.0, "Imports resolved", &stats);
    phase_timings.insert("imports".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Calls ────────────────────────────────────────────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Calls, 0.0, "Resolving call-sites", &stats);

    let calls_progress: ProgressFn = {
        let tx = Arc::clone(&progress_tx);
        let etx = event_tx.clone();
        let pid = project_id.clone();
        let base = stats.clone();
        Arc::new(move |pct: f32, msg: &str, pr: &PhaseResult| {
            let mut merged = base.clone();
            merged.edges_created += pr.edges_created;
            merged.errors += pr.errors;
            let _ = tx.send(PipelineProgress {
                phase: PipelinePhase::Calls,
                phase_progress: pct,
                message: msg.to_string(),
                stats: merged,
            });
            let _ = etx.send(CodeGraphEvent::IndexProgress {
                project_id: pid.clone(),
                phase: PipelinePhase::Calls,
                phase_progress: pct,
                message: msg.to_string(),
            });
        })
    };

    let call_result = calls::resolve_calls(&project_id, &db, &root_path, &files, &import_map, Some(&calls_progress)).await?;
    stats.edges_created += call_result.edges_created;

    update_progress(PipelinePhase::Calls, 1.0, "Calls resolved", &stats);
    phase_timings.insert("calls".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Heritage (extends/implements + overrides) ───────────────────────
    let phase_start = Instant::now();
    update_progress(PipelinePhase::Heritage, 0.0, "Resolving inheritance", &stats);

    let heritage_result = heritage::resolve_heritage(&project_id, &db).await?;
    stats.edges_created += heritage_result.edges_created;

    update_progress(
        PipelinePhase::Heritage,
        0.5,
        "Inheritance resolved, computing overrides",
        &stats,
    );

    let overrides_result = overrides::resolve_overrides(&project_id, &db).await?;
    stats.edges_created += overrides_result.edges_created;

    update_progress(PipelinePhase::Heritage, 1.0, "Heritage resolved", &stats);
    phase_timings.insert("heritage".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Routes ──────────────────────────────────────────────────────────
    let phase_start = Instant::now();
    let route_result = routes::detect_routes(&project_id, &root_path, &files, &db).await?;
    stats.nodes_created += route_result.nodes_created;
    stats.edges_created += route_result.edges_created;
    phase_timings.insert("routes".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Communities ──────────────────────────────────────────────────────
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

    // ── Processes ────────────────────────────────────────────────────────
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

    // ── Enriching ────────────────────────────────────────────────────────
    // Two orthogonal optional passes: LLM labels for Community nodes and
    // vector embeddings for Function/Method/Class nodes. Both gated on
    // service availability and the node-count threshold.
    let phase_start = Instant::now();
    // Both passes share the same node-count ceiling because their cost
    // (token spend for the LLM, CPU + memory for embeddings) scales
    // with graph size. Any project bigger than the threshold gets
    // skipped on both fronts to keep enrichment from blowing up the
    // budget on monorepos.
    let enrichment_eligible =
        config.llm_enrichment && stats.nodes_created <= config.node_embedding_skip_threshold;

    if enrichment_eligible {
        if let Some(ref llm) = llm_manager {
            update_progress(PipelinePhase::Enriching, 0.0, "Enriching with LLM labels", &stats);
            if let Err(err) = enriching::enrich(&project_id, &db, llm).await {
                tracing::warn!(%err, "LLM enrichment failed, continuing without labels");
            }
            update_progress(PipelinePhase::Enriching, 0.5, "Label enrichment complete", &stats);
        } else {
            update_progress(
                PipelinePhase::Enriching,
                0.5,
                "LLM enrichment skipped — no llm_manager wired",
                &stats,
            );
        }
    } else {
        let reason = if stats.nodes_created > config.node_embedding_skip_threshold {
            format!(
                "Skipping LLM enrichment ({} nodes exceeds {} threshold)",
                stats.nodes_created, config.node_embedding_skip_threshold
            )
        } else {
            "LLM enrichment disabled".to_string()
        };
        update_progress(PipelinePhase::Enriching, 0.5, &reason, &stats);
    }

    // Vector embeddings — shares the node-count threshold with the LLM
    // pass so large projects skip both and avoid runaway cost.
    if let Some(ref embedder) = embedding_model {
        if stats.nodes_created <= config.node_embedding_skip_threshold {
            update_progress(
                PipelinePhase::Enriching,
                0.55,
                "Generating code embeddings",
                &stats,
            );
            match embeddings::generate_embeddings(&project_id, &root_path, &db, embedder).await {
                Ok(embed_stats) => {
                    tracing::info!(
                        project_id = %project_id,
                        symbols = embed_stats.embedded,
                        "code embeddings generated"
                    );
                }
                Err(err) => {
                    tracing::warn!(%err, "code embeddings phase failed, continuing");
                }
            }
        } else {
            tracing::info!(
                project_id = %project_id,
                nodes = stats.nodes_created,
                threshold = config.node_embedding_skip_threshold,
                "skipping embeddings (node count exceeds threshold)"
            );
        }
    }

    update_progress(PipelinePhase::Enriching, 1.0, "Enrichment complete", &stats);
    phase_timings.insert("enriching".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── FTS ─────────────────────────────────────────────────────────────
    let phase_start = Instant::now();
    match fts::build_fts_index(&project_id, &db).await {
        Ok(fts_result) => {
            tracing::info!(
                project_id = %project_id,
                indexed = fts_result.nodes_created,
                "FTS index ready"
            );
        }
        Err(err) => {
            tracing::warn!(%err, "FTS indexing failed, continuing without search index");
        }
    }
    phase_timings.insert("fts".to_string(), phase_start.elapsed().as_secs_f64());
    check_cancel!();

    // ── Finalize ─────────────────────────────────────────────────────────
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

    // Write meta.json with the current git commit so staleness can be
    // detected on next startup without re-parsing the repo.
    let head_commit = read_git_head(&root_path).await;
    let meta = super::types::ProjectMeta {
        project_id: project_id.clone(),
        schema_version: super::schema::SCHEMA_VERSION,
        status: super::types::IndexStatus::Indexed,
        last_commit: head_commit.clone(),
        phase_timings: phase_timings.clone(),
        stats: Some(stats.clone()),
        last_indexed_at: Some(chrono::Utc::now()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let meta_dir = db.db_path.parent().unwrap_or(std::path::Path::new("."));
    let meta_path = meta_dir.join("meta.json");
    if let Ok(json) = serde_json::to_string_pretty(&meta)
        && let Err(err) = tokio::fs::write(&meta_path, json).await
    {
        tracing::warn!(%err, path = %meta_path.display(), "failed to write meta.json");
    }

    let _ = event_tx.send(CodeGraphEvent::GraphIndexed {
        project_id: project_id.clone(),
        stats: stats.clone(),
    });

    update_progress(PipelinePhase::Complete, 1.0, "Index complete", &stats);
    phase_timings.insert("complete".to_string(), phase_start.elapsed().as_secs_f64());

    Ok(stats)
}

/// Read the current HEAD commit hash from a git repository.
async fn read_git_head(root_path: &std::path::Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root_path)
        .output()
        .await
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Check whether a project's index is stale by comparing the stored
/// commit hash in meta.json against the current HEAD.
pub async fn check_staleness(
    root_path: &std::path::Path,
    meta_path: &std::path::Path,
) -> (bool, Option<String>, Option<String>) {
    let current_head = read_git_head(root_path).await;
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
