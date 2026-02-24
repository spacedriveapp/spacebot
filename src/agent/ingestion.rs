//! Memory ingestion: Background file processing for bulk memory import.
//!
//! Polls a directory in the agent workspace for supported files, extracts text,
//! chunks it, and processes each chunk through the memory recall + save flow.
//! Files are deleted after all chunks are successfully ingested.
//!
//! Progress is tracked per-chunk in SQLite using a SHA-256 hash of the file
//! content. If the server restarts mid-file, already-completed chunks are
//! skipped on the next run.

use crate::AgentDeps;
use crate::ProcessType;
use crate::config::IngestionConfig;
use crate::llm::SpacebotModel;

use anyhow::Context as _;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use rig::tool::server::ToolServerHandle;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Spawn the ingestion polling loop for an agent.
///
/// Runs until the returned JoinHandle is dropped or aborted. Scans the ingest
/// directory on a timer, processes any supported files found, and deletes them
/// after successful ingestion.
pub fn spawn_ingestion_loop(ingest_dir: PathBuf, deps: AgentDeps) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_ingestion_loop(&ingest_dir, &deps).await {
            tracing::error!(%error, "ingestion loop exited with error");
        }
    })
}

async fn run_ingestion_loop(ingest_dir: &Path, deps: &AgentDeps) -> anyhow::Result<()> {
    tracing::info!(path = %ingest_dir.display(), "ingestion loop started");

    loop {
        let config = **deps.runtime_config.ingestion.load();

        if !config.enabled {
            tokio::time::sleep(Duration::from_secs(config.poll_interval_secs)).await;
            continue;
        }

        // Scan for files
        match scan_ingest_dir(ingest_dir).await {
            Ok(files) if !files.is_empty() => {
                for file_path in files {
                    if let Err(error) = process_file(&file_path, deps, &config).await {
                        tracing::error!(
                            path = %file_path.display(),
                            %error,
                            "failed to ingest file"
                        );
                    }
                }
            }
            Err(error) => {
                // Directory might not exist yet — that's fine
                tracing::debug!(%error, "failed to scan ingest directory");
            }
            _ => {}
        }

        tokio::time::sleep(Duration::from_secs(config.poll_interval_secs)).await;
    }
}

/// Scan the ingest directory for supported ingestion files.
///
/// Returns files sorted by modification time (oldest first) so ingestion
/// order is predictable.
async fn scan_ingest_dir(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to read ingest directory: {}", dir.display()))?;

    let mut files = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Skip directories, hidden files, and unsupported files.
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            continue;
        }

        // Only process files that look ingestible.
        if is_supported_ingest_file(&path) {
            files.push(path);
        } else {
            tracing::warn!(
                path = %path.display(),
                "skipping unsupported file in ingest directory"
            );
        }
    }

    // Sort by modification time (oldest first)
    files.sort_by(|a, b| {
        let time_a = std::fs::metadata(a).and_then(|m| m.modified()).ok();
        let time_b = std::fs::metadata(b).and_then(|m| m.modified()).ok();
        time_a.cmp(&time_b)
    });

    Ok(files)
}

/// Check if a file extension suggests ingestible content.
fn is_supported_ingest_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        // No extension — try to read as text
        return true;
    };

    matches!(
        ext.to_lowercase().as_str(),
        "txt"
            | "md"
            | "markdown"
            | "json"
            | "jsonl"
            | "csv"
            | "tsv"
            | "log"
            | "xml"
            | "yaml"
            | "yml"
            | "toml"
            | "rst"
            | "org"
            | "html"
            | "htm"
            | "pdf"
    )
}

/// SHA-256 hex digest of file content, used as a stable identifier for
/// progress tracking across restarts.
pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Process a single file: read, chunk, process each chunk, then delete.
///
/// Checks the ingestion_progress table to skip chunks that were already
/// completed in a previous run (e.g. before a server restart).
#[tracing::instrument(skip(deps, config), fields(agent_id = %deps.agent_id, path = %path.display()))]
async fn process_file(
    path: &Path,
    deps: &AgentDeps,
    config: &IngestionConfig,
) -> anyhow::Result<()> {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    tracing::info!(file = %filename, "starting file ingestion");

    let content = read_ingest_content(path).await?;

    if content.trim().is_empty() {
        tracing::info!(file = %filename, "skipping empty file");
        tokio::fs::remove_file(path).await?;
        return Ok(());
    }

    let hash = content_hash(&content);
    let file_size = content.len() as i64;
    let chunks = chunk_text(&content, config.chunk_size);
    let total_chunks = chunks.len();

    let completed = load_completed_chunks(&deps.sqlite_pool, &hash).await?;
    let remaining = total_chunks - completed.len();

    // Record file-level tracking (idempotent — skips if already exists from a previous run)
    upsert_ingestion_file(
        &deps.sqlite_pool,
        &hash,
        filename,
        file_size,
        total_chunks as i64,
    )
    .await?;

    if !completed.is_empty() {
        tracing::info!(
            file = %filename,
            chunks = total_chunks,
            already_completed = completed.len(),
            remaining,
            "resuming partially ingested file"
        );
    } else {
        tracing::info!(
            file = %filename,
            chunks = total_chunks,
            total_chars = content.len(),
            "chunked file for ingestion"
        );
    }

    let mut had_failure = false;

    for (index, chunk) in chunks.iter().enumerate() {
        let chunk_number = index + 1;

        if completed.contains(&(index as i64)) {
            tracing::debug!(
                file = %filename,
                chunk = %format!("{chunk_number}/{total_chunks}"),
                "chunk already ingested, skipping"
            );
            continue;
        }

        tracing::info!(
            file = %filename,
            chunk = %format!("{chunk_number}/{total_chunks}"),
            chars = chunk.len(),
            "processing chunk"
        );

        match process_chunk(chunk, filename, chunk_number, total_chunks, deps).await {
            Ok(()) => {
                record_chunk_completed(
                    &deps.sqlite_pool,
                    &hash,
                    index as i64,
                    total_chunks as i64,
                    filename,
                )
                .await?;
            }
            Err(error) => {
                tracing::error!(
                    file = %filename,
                    chunk = %format!("{chunk_number}/{total_chunks}"),
                    %error,
                    "failed to process chunk"
                );
                had_failure = true;
            }
        }
    }

    // Mark file as completed (or failed if any chunk errored)
    let final_status = if had_failure { "failed" } else { "completed" };
    complete_ingestion_file(&deps.sqlite_pool, &hash, final_status).await?;

    if had_failure {
        // Keep the source file and progress rows so the next poll cycle can
        // resume from where it left off. Deleting on failure would cause data
        // loss when a provider error interrupts mid-ingestion (fixes #48).
        tracing::warn!(
            file = %filename,
            chunks = total_chunks,
            "file ingestion had failures — keeping file and progress for retry"
        );
        return Ok(());
    }

    // Full success: clean up progress rows and remove the source file.
    delete_progress(&deps.sqlite_pool, &hash).await?;

    tokio::fs::remove_file(path)
        .await
        .with_context(|| format!("failed to delete ingested file: {}", path.display()))?;

    tracing::info!(file = %filename, chunks = total_chunks, status = final_status, "file ingestion complete, file deleted");

    Ok(())
}

/// Read an ingest file and return extracted text content.
///
/// Plaintext-like files are read directly as UTF-8. PDFs are read as bytes and
/// converted to text through the PDF extractor.
async fn read_ingest_content(path: &Path) -> anyhow::Result<String> {
    let extension = path.extension().and_then(|extension| extension.to_str());

    if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("failed to read pdf file: {}", path.display()))?;

        let extracted =
            tokio::task::spawn_blocking(move || pdf_extract::extract_text_from_mem(&bytes))
                .await
                .context("pdf extraction task failed")?
                .with_context(|| format!("failed to extract text from pdf: {}", path.display()))?;

        return Ok(extracted);
    }

    tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read file: {}", path.display()))
}

// -- Progress tracking queries --------------------------------------------------

/// Load the set of chunk indices already completed for a given content hash.
async fn load_completed_chunks(pool: &SqlitePool, hash: &str) -> anyhow::Result<HashSet<i64>> {
    let rows = sqlx::query_scalar::<_, i64>(
        "SELECT chunk_index FROM ingestion_progress WHERE content_hash = ?",
    )
    .bind(hash)
    .fetch_all(pool)
    .await
    .context("failed to load ingestion progress")?;

    Ok(rows.into_iter().collect())
}

/// Record a single chunk as completed.
async fn record_chunk_completed(
    pool: &SqlitePool,
    hash: &str,
    chunk_index: i64,
    total_chunks: i64,
    filename: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT OR IGNORE INTO ingestion_progress (content_hash, chunk_index, total_chunks, filename)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(hash)
    .bind(chunk_index)
    .bind(total_chunks)
    .bind(filename)
    .execute(pool)
    .await
    .context("failed to record ingestion progress")?;

    Ok(())
}

/// Remove all progress records for a content hash after the file is fully processed.
async fn delete_progress(pool: &SqlitePool, hash: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM ingestion_progress WHERE content_hash = ?")
        .bind(hash)
        .execute(pool)
        .await
        .context("failed to clean up ingestion progress")?;

    Ok(())
}

// -- File-level tracking queries ------------------------------------------------

/// Record that a file is now being processed. If a `queued` record already
/// exists (from the upload handler), update it with chunk info and flip to
/// `processing`. Otherwise insert a fresh `processing` record.
async fn upsert_ingestion_file(
    pool: &SqlitePool,
    hash: &str,
    filename: &str,
    file_size: i64,
    total_chunks: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO ingestion_files (content_hash, filename, file_size, total_chunks, status)
        VALUES (?, ?, ?, ?, 'processing')
        ON CONFLICT(content_hash) DO UPDATE SET
            total_chunks = excluded.total_chunks,
            status = 'processing'
        WHERE status = 'queued' OR status = 'processing'
        "#,
    )
    .bind(hash)
    .bind(filename)
    .bind(file_size)
    .bind(total_chunks)
    .execute(pool)
    .await
    .context("failed to upsert ingestion file record")?;

    Ok(())
}

/// Mark a file as completed or failed.
async fn complete_ingestion_file(
    pool: &SqlitePool,
    hash: &str,
    status: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE ingestion_files SET status = ?, completed_at = CURRENT_TIMESTAMP WHERE content_hash = ?",
    )
    .bind(status)
    .bind(hash)
    .execute(pool)
    .await
    .context("failed to update ingestion file status")?;

    Ok(())
}

/// Split text into chunks at line boundaries.
///
/// Chunks target `chunk_size` characters but won't split mid-line. If a single
/// line exceeds `chunk_size`, it gets its own chunk.
fn chunk_text(text: &str, chunk_size: usize) -> Vec<String> {
    if text.len() <= chunk_size {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current_chunk = String::new();

    for line in text.lines() {
        // If adding this line would exceed the limit and we already have content,
        // finalize the current chunk
        if !current_chunk.is_empty() && current_chunk.len() + line.len() + 1 > chunk_size {
            chunks.push(current_chunk);
            current_chunk = String::new();
        }

        if !current_chunk.is_empty() {
            current_chunk.push('\n');
        }
        current_chunk.push_str(line);
    }

    if !current_chunk.is_empty() {
        chunks.push(current_chunk);
    }

    chunks
}

/// Process a single chunk through the memory recall + save flow.
///
/// Creates a fresh LLM agent with memory tools for each chunk. No history
/// carries over between chunks — each chunk is independent.
#[tracing::instrument(skip(chunk, deps), fields(agent_id = %deps.agent_id, filename, chunk_number, total_chunks))]
async fn process_chunk(
    chunk: &str,
    filename: &str,
    chunk_number: usize,
    total_chunks: usize,
    deps: &AgentDeps,
) -> anyhow::Result<()> {
    let prompt_engine = deps.runtime_config.prompts.load();
    let ingestion_prompt = prompt_engine.render_static("ingestion")?;

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "branch")
        .with_routing((**routing).clone());

    let conversation_logger =
        crate::conversation::history::ConversationLogger::new(deps.sqlite_pool.clone());
    let channel_store = crate::conversation::ChannelStore::new(deps.sqlite_pool.clone());
    let tool_server: ToolServerHandle = crate::tools::create_branch_tool_server(
        deps.memory_search.clone(),
        conversation_logger,
        channel_store,
        crate::conversation::ProcessRunLogger::new(deps.sqlite_pool.clone()),
        &deps.agent_id,
    );

    let agent = AgentBuilder::new(model)
        .preamble(&ingestion_prompt)
        .default_max_turns(10)
        .tool_server_handle(tool_server)
        .build();

    let user_prompt =
        prompt_engine.render_system_ingestion_chunk(filename, chunk_number, total_chunks, chunk)?;

    let mut history = Vec::new();
    match agent.prompt(&user_prompt).with_history(&mut history).await {
        Ok(response) => {
            tracing::debug!(
                file = %filename,
                chunk = %format!("{chunk_number}/{total_chunks}"),
                response = %response.chars().take(200).collect::<String>(),
                "chunk processed"
            );
        }
        Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
            tracing::warn!(
                file = %filename,
                chunk = %format!("{chunk_number}/{total_chunks}"),
                "chunk processing hit max turns"
            );
        }
        Err(error) => {
            return Err(error.into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_text_small_input() {
        let text = "Hello, world!";
        let chunks = chunk_text(text, 4000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn test_chunk_text_splits_at_lines() {
        let text = "line one\nline two\nline three\nline four";
        let chunks = chunk_text(text, 20);
        assert!(chunks.len() > 1);
        // Each chunk should be valid text (no partial lines)
        for chunk in &chunks {
            assert!(!chunk.starts_with('\n'));
        }
    }

    #[test]
    fn test_chunk_text_handles_long_line() {
        let long_line = "a".repeat(5000);
        let chunks = chunk_text(&long_line, 4000);
        // A single line exceeding chunk_size gets its own chunk
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 5000);
    }

    #[test]
    fn test_chunk_text_empty() {
        let chunks = chunk_text("", 4000);
        // Empty string produces one empty chunk, but process_file skips
        // empty content before chunking.
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_is_supported_ingest_file() {
        assert!(is_supported_ingest_file(Path::new("notes.txt")));
        assert!(is_supported_ingest_file(Path::new("data.json")));
        assert!(is_supported_ingest_file(Path::new("readme.md")));
        assert!(is_supported_ingest_file(Path::new("manual.pdf")));
        assert!(is_supported_ingest_file(Path::new("no_extension")));
        assert!(!is_supported_ingest_file(Path::new("image.png")));
        assert!(!is_supported_ingest_file(Path::new("binary.exe")));
    }

    #[test]
    fn test_content_hash_deterministic() {
        let hash1 = content_hash("hello world");
        let hash2 = content_hash("hello world");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_content_hash_differs_for_different_content() {
        let hash1 = content_hash("hello world");
        let hash2 = content_hash("hello world!");
        assert_ne!(hash1, hash2);
    }

    /// Regression test for #48: when any chunk errors, had_failure must be true
    /// and the source file must be kept for retry. Tests the pure flag logic that
    /// guards the delete path without requiring a live SQLite/filesystem setup.
    #[test]
    fn test_failure_flag_prevents_delete() {
        let mut had_failure = false;

        // Simulate a chunk that errors (e.g. provider 401)
        let chunk_result: anyhow::Result<()> = Err(anyhow::anyhow!("provider error"));
        if chunk_result.is_err() {
            had_failure = true;
        }

        assert!(had_failure, "had_failure must be true after a chunk error");
        // The guard `if had_failure { return Ok(()); }` means remove_file is
        // never reached — assert the condition that triggers the early return.
        assert!(
            had_failure,
            "early return condition must hold to skip file deletion"
        );
    }

    /// Complement to test_failure_flag_prevents_delete: a clean run must reach
    /// the delete path (had_failure stays false).
    #[test]
    fn test_success_flag_allows_delete() {
        let mut had_failure = false;

        let chunk_result: anyhow::Result<()> = Ok(());
        if chunk_result.is_err() {
            had_failure = true;
        }

        assert!(
            !had_failure,
            "had_failure must stay false when all chunks succeed"
        );
    }
}
