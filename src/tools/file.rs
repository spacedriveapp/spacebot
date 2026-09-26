//! File tools for reading, writing, editing, and listing files (task workers only).
//!
//! Provides a suite of separate tools (`file_read`, `file_write`, `file_edit`,
//! `file_list`) backed by a shared `FileContext` that handles sandbox-aware path
//! validation. This mirrors the flat-tool pattern used by the browser tools.

use crate::sandbox::Sandbox;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Shared context

/// Shared context cloned into each file tool. Holds workspace root and sandbox
/// for path validation, mirroring how `BrowserContext` is shared across browser
/// tools.
#[derive(Debug, Clone)]
pub(crate) struct FileContext {
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
}

impl FileContext {
    fn new(workspace: PathBuf, sandbox: Arc<Sandbox>) -> Self {
        Self { workspace, sandbox }
    }

    fn resolve_path_inner(&self, raw: &str, require_writable: bool) -> Result<PathBuf, FileError> {
        let path = Path::new(raw);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        };

        // For writes, the target may not exist yet. Canonicalize the deepest
        // existing ancestor and append the remaining components.
        let canonical = best_effort_canonicalize(&resolved);

        // When sandbox is disabled, skip workspace boundary enforcement.
        if !self.sandbox.mode_enabled() {
            return Ok(canonical);
        }

        if !self.sandbox.is_path_allowed(&canonical) {
            return Err(FileError(format!(
                "ACCESS DENIED: Path is outside the workspace boundary. \
                 File operations are restricted to {} and allowed project paths. \
                 You do not have access to this file and must not attempt to reproduce, \
                 guess, or fabricate its contents. Inform the user that the path is \
                 outside your workspace.",
                self.workspace.display()
            )));
        }

        if require_writable && !self.sandbox.is_path_writable(&canonical) {
            return Err(FileError(
                "ACCESS DENIED: This path is mounted as read-only. You can read files here but cannot write or edit them.".to_string(),
            ));
        }

        let workspace_canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());

        // Reject paths containing symlinks to prevent TOCTOU races where a
        // path component is replaced with a symlink between resolution and I/O.
        {
            let mut check = workspace_canonical.clone();
            if let Ok(relative) = canonical.strip_prefix(&workspace_canonical) {
                for component in relative.components() {
                    check.push(component);
                    if let Ok(metadata) = std::fs::symlink_metadata(&check)
                        && metadata.file_type().is_symlink()
                    {
                        return Err(FileError(
                            "ACCESS DENIED: Symlinks are not allowed within the workspace \
                             for security reasons. Use direct paths instead."
                                .to_string(),
                        ));
                    }
                }
            }
        }

        Ok(canonical)
    }

    fn resolve_path(&self, raw: &str) -> Result<PathBuf, FileError> {
        self.resolve_path_inner(raw, false)
    }

    fn resolve_writable_path(&self, raw: &str) -> Result<PathBuf, FileError> {
        self.resolve_path_inner(raw, true)
    }

    // NOTE: Identity file protection (PROTECTED_FILES) has been removed.
    // Identity files (SOUL.md, IDENTITY.md, ROLE.md) now live in the agent
    // root directory, outside the workspace sandbox boundary. The sandbox
    // path validation in `resolve_path()` naturally prevents worker file
    // tools from accessing them — no explicit filename checks needed.
}

/// Canonicalize as much of the path as possible. For paths where the final
/// components don't exist yet (e.g. writing a new file), canonicalize the
/// deepest existing ancestor and append the rest.
fn best_effort_canonicalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }

    // Walk up until we find something that exists
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    while !existing.exists() {
        if let Some(file_name) = existing.file_name() {
            suffix.push(file_name.to_os_string());
        } else {
            break;
        }
        if !existing.pop() {
            break;
        }
    }

    let base = existing.canonicalize().unwrap_or(existing);
    let mut result = base;
    for component in suffix.into_iter().rev() {
        result.push(component);
    }
    result
}

// Error type

/// Error type for file tools.
#[derive(Debug, thiserror::Error)]
#[error("File operation failed: {0}")]
pub struct FileError(String);

// Shared output types

/// Output from file tools.
#[derive(Debug, Serialize)]
pub struct FileOutput {
    /// Whether the operation succeeded.
    pub success: bool,
    /// The operation performed.
    pub operation: String,
    /// The file/directory path.
    pub path: String,
    /// File content (for read operations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Directory entries (for list operations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FileEntryOutput>>,
    /// Error message if operation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// File entry for serialization.
#[derive(Debug, Serialize)]
pub struct FileEntryOutput {
    /// Entry name.
    pub name: String,
    /// Entry type (file, directory, or other).
    pub entry_type: String,
    /// File size in bytes (0 for directories).
    pub size: u64,
}

// Tool: file_read

#[derive(Debug, Clone)]
pub struct FileReadTool {
    pub(crate) context: FileContext,
}

impl FileReadTool {
    pub fn new(workspace: std::path::PathBuf, sandbox: Arc<crate::sandbox::Sandbox>) -> Self {
        Self {
            context: FileContext::new(workspace, sandbox),
        }
    }
}

/// Arguments for file_read.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileReadArgs {
    /// The file path. Relative paths are resolved from the workspace root.
    pub path: String,
    /// Line number to start reading from (1-indexed). Omit to start from the beginning.
    pub offset: Option<usize>,
    /// Maximum number of lines to return. Omit to read the entire file.
    pub limit: Option<usize>,
}

impl Tool for FileReadTool {
    const NAME: &'static str = "file_read";

    type Error = FileError;
    type Args = FileReadArgs;
    type Output = FileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/file_read").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read. Relative paths are resolved from the workspace root."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed). Omit to start from the beginning."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return. Omit to read the entire file (up to size limit)."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_writable_path(&args.path)?;

        let raw = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| FileError(format!("Failed to read file: {error}")))?;

        // Apply line-based offset/limit if requested
        let content = if args.offset.is_some() || args.limit.is_some() {
            let lines: Vec<&str> = raw.lines().collect();
            let total_lines = lines.len();

            // offset is 1-indexed; default to line 1
            let start = args.offset.unwrap_or(1).saturating_sub(1).min(total_lines);
            let end = if let Some(limit) = args.limit {
                (start + limit).min(total_lines)
            } else {
                total_lines
            };

            let selected: Vec<String> = lines[start..end]
                .iter()
                .enumerate()
                .map(|(i, line)| format!("{}: {}", start + i + 1, line))
                .collect();

            let mut result = selected.join("\n");
            if end < total_lines {
                result.push_str(&format!(
                    "\n\n[showing lines {}-{} of {total_lines}. Use offset={} to see more]",
                    start + 1,
                    end,
                    end + 1
                ));
            }
            result
        } else {
            crate::tools::truncate_output(&raw, crate::tools::MAX_TOOL_OUTPUT_BYTES)
        };

        Ok(FileOutput {
            success: true,
            operation: "read".to_string(),
            path: path.to_string_lossy().to_string(),
            content: Some(content),
            entries: None,
            error: None,
        })
    }
}

// Tool: file_write

#[derive(Debug, Clone)]
pub struct FileWriteTool {
    context: FileContext,
}

impl FileWriteTool {
    pub fn new(workspace: std::path::PathBuf, sandbox: Arc<crate::sandbox::Sandbox>) -> Self {
        Self {
            context: FileContext::new(workspace, sandbox),
        }
    }
}

/// Arguments for file_write.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileWriteArgs {
    /// The file path to write. Relative paths are resolved from the workspace root.
    pub path: String,
    /// The content to write to the file.
    pub content: String,
    /// Whether to create parent directories if they don't exist. Defaults to true.
    #[serde(default = "default_create_dirs")]
    pub create_dirs: bool,
}

fn default_create_dirs() -> bool {
    true
}

impl Tool for FileWriteTool {
    const NAME: &'static str = "file_write";

    type Error = FileError;
    type Args = FileWriteArgs;
    type Output = FileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/file_write").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to write. Relative paths are resolved from the workspace root."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file."
                    },
                    "create_dirs": {
                        "type": "boolean",
                        "default": true,
                        "description": "Create parent directories if they don't exist."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_writable_path(&args.path)?;

        // Ensure parent directory exists if requested
        if args.create_dirs
            && let Some(parent) = path.parent()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| FileError(format!("Failed to create directory: {error}")))?;
        }

        tokio::fs::write(&path, &args.content)
            .await
            .map_err(|error| FileError(format!("Failed to write file: {error}")))?;

        Ok(FileOutput {
            success: true,
            operation: "write".to_string(),
            path: path.to_string_lossy().to_string(),
            content: None,
            entries: None,
            error: None,
        })
    }
}

// Tool: file_edit

#[derive(Debug, Clone)]
pub struct FileEditTool {
    context: FileContext,
}

impl FileEditTool {
    pub fn new(workspace: std::path::PathBuf, sandbox: Arc<crate::sandbox::Sandbox>) -> Self {
        Self {
            context: FileContext::new(workspace, sandbox),
        }
    }
}

/// Arguments for file_edit.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileEditArgs {
    /// The file path to edit. Relative paths are resolved from the workspace root.
    pub path: String,
    /// The exact text to find in the file.
    pub old_string: String,
    /// The replacement text.
    pub new_string: String,
    /// Replace all occurrences instead of just the first. Defaults to false.
    #[serde(default)]
    pub replace_all: bool,
}

impl Tool for FileEditTool {
    const NAME: &'static str = "file_edit";

    type Error = FileError;
    type Args = FileEditArgs;
    type Output = FileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/file_edit").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to edit. Relative paths are resolved from the workspace root."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find in the file."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "default": false,
                        "description": "Replace all occurrences instead of just the first."
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_writable_path(&args.path)?;

        let original = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| FileError(format!("Failed to read file: {error}")))?;

        // Count occurrences to provide useful feedback
        let match_count = original.matches(&args.old_string).count();

        if match_count == 0 {
            return Err(FileError(format!(
                "old_string not found in {}. Make sure the text matches exactly, \
                 including whitespace and indentation. Use file_read to verify \
                 the current file contents.",
                args.path
            )));
        }

        if !args.replace_all && match_count > 1 {
            return Err(FileError(format!(
                "Found {match_count} matches for old_string in {}. Provide more \
                 surrounding context in old_string to identify a unique match, \
                 or set replace_all to true to replace every occurrence.",
                args.path
            )));
        }

        let updated = if args.replace_all {
            original.replace(&args.old_string, &args.new_string)
        } else {
            original.replacen(&args.old_string, &args.new_string, 1)
        };

        tokio::fs::write(&path, &updated)
            .await
            .map_err(|error| FileError(format!("Failed to write file: {error}")))?;

        let replacements = if args.replace_all { match_count } else { 1 };

        Ok(FileOutput {
            success: true,
            operation: "edit".to_string(),
            path: path.to_string_lossy().to_string(),
            content: Some(format!(
                "Replaced {replacements} occurrence{} in {}",
                if replacements != 1 { "s" } else { "" },
                args.path
            )),
            entries: None,
            error: None,
        })
    }
}

// Tool: file_list

#[derive(Debug, Clone)]
pub struct FileListTool {
    context: FileContext,
}

impl FileListTool {
    pub fn new(workspace: std::path::PathBuf, sandbox: Arc<crate::sandbox::Sandbox>) -> Self {
        Self {
            context: FileContext::new(workspace, sandbox),
        }
    }
}

/// Arguments for file_list.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileListArgs {
    /// The directory path to list. Relative paths are resolved from the workspace root.
    pub path: String,
}

impl Tool for FileListTool {
    const NAME: &'static str = "file_list";

    type Error = FileError;
    type Args = FileListArgs;
    type Output = FileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/file_list").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The directory path to list. Relative paths are resolved from the workspace root."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_path(&args.path)?;
        do_file_list(&path).await
    }
}

// Internal helpers

async fn do_file_list(path: &Path) -> Result<FileOutput, FileError> {
    let mut entries = Vec::new();

    let mut reader = tokio::fs::read_dir(path)
        .await
        .map_err(|error| FileError(format!("Failed to read directory: {error}")))?;

    let max_entries = crate::tools::MAX_DIR_ENTRIES;
    let mut total_count = 0usize;

    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| FileError(format!("Failed to read entry: {error}")))?
    {
        total_count += 1;

        if entries.len() < max_entries {
            let metadata = entry
                .metadata()
                .await
                .map_err(|error| FileError(format!("Failed to read metadata: {error}")))?;

            let entry_type = if metadata.is_file() {
                "file".to_string()
            } else if metadata.is_dir() {
                "directory".to_string()
            } else {
                "other".to_string()
            };

            entries.push(FileEntryOutput {
                name: entry.file_name().to_string_lossy().to_string(),
                entry_type,
                size: metadata.len(),
            });
        }
    }

    // Sort entries: directories first, then files, both alphabetically
    entries.sort_by(|a, b| {
        let a_is_dir = a.entry_type == "directory";
        let b_is_dir = b.entry_type == "directory";
        match (a_is_dir, b_is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });

    if total_count > max_entries {
        entries.push(FileEntryOutput {
            name: format!(
                "... and {} more entries (listing capped at {max_entries})",
                total_count - max_entries
            ),
            entry_type: "notice".to_string(),
            size: 0,
        });
    }

    Ok(FileOutput {
        success: true,
        operation: "list".to_string(),
        path: path.to_string_lossy().to_string(),
        content: None,
        entries: Some(entries),
        error: None,
    })
}

// Tool registration helper

/// Register all file tools on a `ToolServer`. The tools share a single
/// `FileContext` for path validation and sandbox enforcement.
pub fn register_file_tools(
    server: rig::tool::server::ToolServer,
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
) -> rig::tool::server::ToolServer {
    let context = FileContext::new(workspace, sandbox);

    server
        .tool(FileReadTool {
            context: context.clone(),
        })
        .tool(FileWriteTool {
            context: context.clone(),
        })
        .tool(FileEditTool {
            context: context.clone(),
        })
        .tool(FileListTool { context })
}

// Legacy types (used by system-internal callers)

/// File entry metadata (legacy).
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub file_type: FileType,
    pub size: u64,
}

/// File type classification (legacy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Other,
}

/// System-internal file read that bypasses workspace containment.
/// Used by the system itself (not LLM-facing) and operates on arbitrary paths.
pub async fn file_read(path: impl AsRef<Path>) -> crate::error::Result<String> {
    let raw = tokio::fs::read_to_string(path.as_ref())
        .await
        .map_err(|error| {
            crate::error::AgentError::Other(anyhow::anyhow!("Failed to read file: {error}"))
        })?;

    let content = crate::tools::truncate_output(&raw, crate::tools::MAX_TOOL_OUTPUT_BYTES);
    Ok(content)
}

/// System-internal file write that bypasses workspace containment.
pub async fn file_write(
    path: impl AsRef<Path>,
    content: impl AsRef<[u8]>,
) -> crate::error::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            crate::error::AgentError::Other(anyhow::anyhow!("Failed to create directory: {error}"))
        })?;
    }
    tokio::fs::write(path, content).await.map_err(|error| {
        crate::error::AgentError::Other(anyhow::anyhow!("Failed to write file: {error}"))
    })?;
    Ok(())
}

/// System-internal directory list that bypasses workspace containment.
pub async fn file_list(path: impl AsRef<Path>) -> crate::error::Result<Vec<FileEntry>> {
    let output = do_file_list(path.as_ref())
        .await
        .map_err(|error| crate::error::AgentError::Other(error.into()))?;

    let entries = output.entries.ok_or_else(|| {
        crate::error::AgentError::Other(anyhow::anyhow!("No entries in list result"))
    })?;

    Ok(entries
        .into_iter()
        .map(|entry| FileEntry {
            name: entry.name,
            file_type: match entry.entry_type.as_str() {
                "file" => FileType::File,
                "directory" => FileType::Directory,
                _ => FileType::Other,
            },
            size: entry.size,
        })
        .collect())
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{Sandbox, SandboxConfig, SandboxMode};
    use std::fs;

    fn create_sandbox(mode: SandboxMode, workspace: &Path) -> Arc<Sandbox> {
        let config = SandboxConfig {
            mode,
            ..Default::default()
        };
        let config = Arc::new(arc_swap::ArcSwap::from_pointee(config));
        Arc::new(Sandbox::new_for_test(config, workspace.to_path_buf()))
    }

    fn make_context(mode: SandboxMode, workspace: &Path) -> FileContext {
        let sandbox = create_sandbox(mode, workspace);
        FileContext::new(workspace.to_path_buf(), sandbox)
    }

    #[tokio::test]
    async fn sandbox_enabled_rejects_read_outside_workspace() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let outside = temp_dir.path().join("outside");
        fs::create_dir_all(&workspace).expect("failed to create workspace");
        fs::create_dir_all(&outside).expect("failed to create outside dir");

        let file = outside.join("secret.txt");
        fs::write(&file, "secret data").expect("failed to write file");

        let context = make_context(SandboxMode::Enabled, &workspace);
        let tool = FileReadTool {
            context: context.clone(),
        };

        let result = tool
            .call(FileReadArgs {
                path: file.to_string_lossy().into_owned(),
                offset: None,
                limit: None,
            })
            .await;

        let error = result
            .expect_err("should reject path outside workspace")
            .to_string();
        assert!(error.contains("ACCESS DENIED"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn sandbox_disabled_allows_read_outside_workspace() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let outside = temp_dir.path().join("outside");
        fs::create_dir_all(&workspace).expect("failed to create workspace");
        fs::create_dir_all(&outside).expect("failed to create outside dir");

        let file = outside.join("report.txt");
        fs::write(&file, "public data").expect("failed to write file");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileReadTool { context };

        let result = tool
            .call(FileReadArgs {
                path: file.to_string_lossy().into_owned(),
                offset: None,
                limit: None,
            })
            .await
            .expect("should succeed when sandbox is disabled");

        assert!(result.success);
        assert_eq!(result.content.as_deref(), Some("public data"));
    }

    #[tokio::test]
    async fn sandbox_disabled_allows_write_outside_workspace() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let outside = temp_dir.path().join("outside");
        fs::create_dir_all(&workspace).expect("failed to create workspace");
        fs::create_dir_all(&outside).expect("failed to create outside dir");

        let file = outside.join("output.txt");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileWriteTool { context };

        let result = tool
            .call(FileWriteArgs {
                path: file.to_string_lossy().into_owned(),
                content: "written outside workspace".to_string(),
                create_dirs: false,
            })
            .await
            .expect("should succeed when sandbox is disabled");

        assert!(result.success);
        let written = fs::read_to_string(&file).expect("failed to read written file");
        assert_eq!(written, "written outside workspace");
    }

    #[tokio::test]
    async fn sandbox_blocks_write_outside_workspace() {
        // Identity files now live outside the workspace (in the agent root).
        // The sandbox path validation prevents any file access outside the
        // workspace — no explicit identity filename checks needed.
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let agent_root = temp_dir.path().join("agent");
        let workspace = agent_root.join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let context = make_context(SandboxMode::Enabled, &workspace);
        let tool = FileWriteTool {
            context: context.clone(),
        };

        // Try writing to the agent root (where identity files live) — should
        // be blocked by sandbox path validation.
        let result = tool
            .call(FileWriteArgs {
                path: agent_root.join("SOUL.md").to_string_lossy().into_owned(),
                content: "overwritten".to_string(),
                create_dirs: false,
            })
            .await;

        let error = result
            .expect_err("should block write outside workspace")
            .to_string();
        assert!(error.contains("ACCESS DENIED"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn sandbox_disabled_allows_write_in_workspace() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let agent_root = temp_dir.path().join("agent");
        let workspace = agent_root.join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileWriteTool { context };

        let result = tool
            .call(FileWriteArgs {
                path: workspace.join("test.md").to_string_lossy().into_owned(),
                content: "new content".to_string(),
                create_dirs: false,
            })
            .await
            .expect("should allow write when sandbox is disabled");

        assert!(result.success);
        let written = fs::read_to_string(workspace.join("test.md")).expect("failed to read file");
        assert_eq!(written, "new content");
    }

    #[tokio::test]
    async fn file_edit_replaces_single_occurrence() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let file = workspace.join("test.txt");
        fs::write(&file, "hello world").expect("failed to write file");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileEditTool { context };

        let result = tool
            .call(FileEditArgs {
                path: file.to_string_lossy().into_owned(),
                old_string: "hello".to_string(),
                new_string: "goodbye".to_string(),
                replace_all: false,
            })
            .await
            .expect("edit should succeed");

        assert!(result.success);
        let content = fs::read_to_string(&file).expect("failed to read file");
        assert_eq!(content, "goodbye world");
    }

    #[tokio::test]
    async fn file_edit_rejects_ambiguous_match() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let file = workspace.join("test.txt");
        fs::write(&file, "aaa bbb aaa").expect("failed to write file");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileEditTool { context };

        let result = tool
            .call(FileEditArgs {
                path: file.to_string_lossy().into_owned(),
                old_string: "aaa".to_string(),
                new_string: "ccc".to_string(),
                replace_all: false,
            })
            .await;

        let error = result
            .expect_err("should reject ambiguous match")
            .to_string();
        assert!(
            error.contains("Found 2 matches"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn file_edit_replace_all() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let file = workspace.join("test.txt");
        fs::write(&file, "aaa bbb aaa").expect("failed to write file");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileEditTool { context };

        let result = tool
            .call(FileEditArgs {
                path: file.to_string_lossy().into_owned(),
                old_string: "aaa".to_string(),
                new_string: "ccc".to_string(),
                replace_all: true,
            })
            .await
            .expect("replace_all should succeed");

        assert!(result.success);
        let content = fs::read_to_string(&file).expect("failed to read file");
        assert_eq!(content, "ccc bbb ccc");
    }

    #[tokio::test]
    async fn file_edit_not_found() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let file = workspace.join("test.txt");
        fs::write(&file, "hello world").expect("failed to write file");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileEditTool { context };

        let result = tool
            .call(FileEditArgs {
                path: file.to_string_lossy().into_owned(),
                old_string: "nonexistent".to_string(),
                new_string: "replacement".to_string(),
                replace_all: false,
            })
            .await;

        let error = result.expect_err("should fail when not found").to_string();
        assert!(
            error.contains("old_string not found"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn file_edit_blocks_outside_workspace() {
        // Identity files now live outside the workspace. Sandbox path
        // validation prevents any edit to files outside the workspace.
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let agent_root = temp_dir.path().join("agent");
        let workspace = agent_root.join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");
        fs::write(agent_root.join("SOUL.md"), "original").expect("failed to write file");

        let context = make_context(SandboxMode::Enabled, &workspace);
        let tool = FileEditTool { context };

        let result = tool
            .call(FileEditArgs {
                path: agent_root.join("SOUL.md").to_string_lossy().into_owned(),
                old_string: "original".to_string(),
                new_string: "hacked".to_string(),
                replace_all: false,
            })
            .await;

        let error = result
            .expect_err("should block edit outside workspace")
            .to_string();
        assert!(error.contains("ACCESS DENIED"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn file_read_with_offset_and_limit() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let file = workspace.join("lines.txt");
        fs::write(&file, "line1\nline2\nline3\nline4\nline5").expect("failed to write file");

        let context = make_context(SandboxMode::Disabled, &workspace);
        let tool = FileReadTool { context };

        let result = tool
            .call(FileReadArgs {
                path: file.to_string_lossy().into_owned(),
                offset: Some(2),
                limit: Some(2),
            })
            .await
            .expect("read with offset should succeed");

        assert!(result.success);
        let content = result.content.unwrap();
        assert!(content.contains("2: line2"), "should contain line 2");
        assert!(content.contains("3: line3"), "should contain line 3");
        assert!(!content.contains("line1"), "should not contain line 1");
        assert!(!content.contains("line4"), "should not contain line 4");
        assert!(
            content.contains("showing lines 2-3 of 5"),
            "should have continuation notice"
        );
    }

    #[tokio::test]
    async fn sandbox_blocks_file_edit_on_readable_path() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let readable = temp_dir.path().join("readonly");
        fs::create_dir_all(&workspace).expect("failed to create workspace");
        fs::create_dir_all(&readable).expect("failed to create readable dir");

        let file = readable.join("note.txt");
        fs::write(&file, "original text").expect("failed to write readable file");

        let config = SandboxConfig {
            mode: SandboxMode::Enabled,
            readable_paths: vec![readable.clone()],
            ..Default::default()
        };
        let config = Arc::new(arc_swap::ArcSwap::from_pointee(config));
        let sandbox = Arc::new(Sandbox::new_for_test(config, workspace.clone()));
        let context = FileContext::new(workspace, sandbox);
        let tool = FileEditTool { context };

        let result = tool
            .call(FileEditArgs {
                path: file.to_string_lossy().into_owned(),
                old_string: "original".to_string(),
                new_string: "updated".to_string(),
                replace_all: false,
            })
            .await;

        let error = result
            .expect_err("file_edit should reject readable_paths")
            .to_string();
        assert!(
            error.contains("mounted as read-only") || error.contains("outside the workspace boundary"),
            "unexpected error: {error}"
        );
    }
}
