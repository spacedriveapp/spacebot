//! Finalize phase: write `meta.json` and fire the `GraphIndexed` event.
//!
//! The Complete phase runs last. It snapshots `stats` + `phase_timings`
//! accumulated across the pipeline into a `ProjectMeta` record that
//! persists next to the LadybugDB files, so the next startup can detect
//! staleness and skip re-indexing if the HEAD commit hasn't changed.

use anyhow::Result;

use super::phase::{Phase, PhaseCtx};
use crate::codegraph::events::CodeGraphEvent;
use crate::codegraph::schema;
use crate::codegraph::types::{IndexStatus, PipelinePhase, ProjectMeta};

/// Read the current HEAD commit hash from a git repository. Returns
/// `None` if the directory is not a git repo or `git rev-parse` fails.
pub async fn read_git_head(root_path: &std::path::Path) -> Option<String> {
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

/// Complete phase: writes `meta.json` and emits `GraphIndexed`.
pub struct CompletePhase;

#[async_trait::async_trait]
impl Phase for CompletePhase {
    fn label(&self) -> &'static str {
        "complete"
    }

    fn phase(&self) -> Option<PipelinePhase> {
        Some(PipelinePhase::Complete)
    }

    async fn run(&self, ctx: &mut PhaseCtx) -> Result<()> {
        ctx.emit_progress(PipelinePhase::Complete, 0.0, "Finalizing index");

        // Write meta.json with the current git commit so staleness can be
        // detected on next startup without re-parsing the repo.
        let head_commit = read_git_head(&ctx.root_path).await;
        let meta = ProjectMeta {
            project_id: ctx.project_id.clone(),
            schema_version: schema::SCHEMA_VERSION,
            status: IndexStatus::Indexed,
            last_commit: head_commit,
            phase_timings: ctx.phase_timings.clone(),
            stats: Some(ctx.stats.clone()),
            last_indexed_at: Some(chrono::Utc::now()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let meta_dir = ctx
            .db
            .db_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let meta_path = meta_dir.join("meta.json");
        if let Ok(json) = serde_json::to_string_pretty(&meta)
            && let Err(err) = tokio::fs::write(&meta_path, json).await
        {
            tracing::warn!(%err, path = %meta_path.display(), "failed to write meta.json");
        }

        let _ = ctx.event_tx.send(CodeGraphEvent::GraphIndexed {
            project_id: ctx.project_id.clone(),
            stats: ctx.stats.clone(),
        });

        ctx.emit_progress(PipelinePhase::Complete, 1.0, "Index complete");
        Ok(())
    }
}
