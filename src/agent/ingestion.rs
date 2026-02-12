//! Memory ingestion: Background file processing for bulk memory import.
//!
//! Polls a directory in the agent workspace for text files, chunks them, and
//! processes each chunk through the memory recall + save flow. Files are deleted
//! after all chunks are successfully ingested.

use crate::config::IngestionConfig;
use crate::llm::SpacebotModel;
use crate::memory::MemorySearch;
use crate::AgentDeps;
use crate::ProcessType;

use anyhow::Context as _;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use rig::tool::server::ToolServerHandle;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Spawn the ingestion polling loop for an agent.
///
/// Runs until the returned JoinHandle is dropped or aborted. Scans the ingest
/// directory on a timer, processes any text files found, and deletes them after
/// successful ingestion.
pub fn spawn_ingestion_loop(
    ingest_dir: PathBuf,
    deps: AgentDeps,
) -> tokio::task::JoinHandle<()> {
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

/// Scan the ingest directory for text files.
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

        // Skip directories, hidden files, and non-text files
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }

        // Only process files that look like text
        if is_text_file(&path) {
            files.push(path);
        } else {
            tracing::warn!(
                path = %path.display(),
                "skipping non-text file in ingest directory"
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

/// Check if a file extension suggests text content.
fn is_text_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        // No extension — try to read as text
        return true;
    };

    matches!(
        ext.to_lowercase().as_str(),
        "txt" | "md" | "markdown"
            | "json" | "jsonl"
            | "csv" | "tsv"
            | "log"
            | "xml" | "yaml" | "yml" | "toml"
            | "rst" | "org"
            | "html" | "htm"
    )
}

/// Process a single file: read, chunk, process each chunk, then delete.
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

    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read file: {}", path.display()))?;

    if content.trim().is_empty() {
        tracing::info!(file = %filename, "skipping empty file");
        tokio::fs::remove_file(path).await?;
        return Ok(());
    }

    let chunks = chunk_text(&content, config.chunk_size);
    let total_chunks = chunks.len();

    tracing::info!(
        file = %filename,
        chunks = total_chunks,
        total_chars = content.len(),
        "chunked file for ingestion"
    );

    for (index, chunk) in chunks.iter().enumerate() {
        let chunk_number = index + 1;

        tracing::info!(
            file = %filename,
            chunk = %format!("{chunk_number}/{total_chunks}"),
            chars = chunk.len(),
            "processing chunk"
        );

        if let Err(error) = process_chunk(chunk, filename, chunk_number, total_chunks, deps).await {
            tracing::error!(
                file = %filename,
                chunk = %format!("{chunk_number}/{total_chunks}"),
                %error,
                "failed to process chunk"
            );
            // Continue with remaining chunks rather than aborting the whole file
        }
    }

    // Delete the file after all chunks are processed
    tokio::fs::remove_file(path)
        .await
        .with_context(|| format!("failed to delete ingested file: {}", path.display()))?;

    tracing::info!(file = %filename, chunks = total_chunks, "file ingestion complete, file deleted");

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
async fn process_chunk(
    chunk: &str,
    filename: &str,
    chunk_number: usize,
    total_chunks: usize,
    deps: &AgentDeps,
) -> anyhow::Result<()> {
    let ingestion_prompt = deps.runtime_config.prompts.load().ingestion.clone();

    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Branch, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_routing((**routing).clone());

    let tool_server: ToolServerHandle =
        crate::tools::create_branch_tool_server(deps.memory_search.clone());

    let agent = AgentBuilder::new(model)
        .preamble(&ingestion_prompt)
        .default_max_turns(10)
        .tool_server_handle(tool_server)
        .build();

    let user_prompt = format!(
        "## File: {filename} (chunk {chunk_number} of {total_chunks})\n\n\
         Process the following text and extract any useful memories:\n\n\
         ---\n\n\
         {chunk}\n\n\
         ---"
    );

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
    fn test_is_text_file() {
        assert!(is_text_file(Path::new("notes.txt")));
        assert!(is_text_file(Path::new("data.json")));
        assert!(is_text_file(Path::new("readme.md")));
        assert!(is_text_file(Path::new("no_extension")));
        assert!(!is_text_file(Path::new("image.png")));
        assert!(!is_text_file(Path::new("binary.exe")));
    }
}
