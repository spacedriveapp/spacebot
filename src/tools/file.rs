//! File tool for reading/writing/listing files (task workers only).

use crate::sandbox::Sandbox;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tool for file operations with sandbox-aware path validation.
///
/// When sandbox mode is enabled, file access is restricted to the workspace
/// boundary. When sandbox is disabled, any path accessible to the process is
/// allowed (relative paths are still resolved against the workspace root).
#[derive(Debug, Clone)]
pub struct FileTool {
    workspace: PathBuf,
    sandbox: Arc<Sandbox>,
}

impl FileTool {
    /// Create a new file tool with sandbox-aware path validation.
    pub fn new(workspace: PathBuf, sandbox: Arc<Sandbox>) -> Self {
        Self { workspace, sandbox }
    }

    /// Resolve and validate a path.
    ///
    /// Relative paths are resolved against the workspace root. When sandbox mode
    /// is enabled, absolute paths must fall within the workspace and symlink
    /// traversal is blocked. When sandbox is disabled, any readable/writable
    /// path is accepted.
    fn resolve_path(&self, raw: &str) -> Result<PathBuf, FileError> {
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

        let workspace_canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());

        if !canonical.starts_with(&workspace_canonical) {
            return Err(FileError(format!(
                "ACCESS DENIED: Path is outside the workspace boundary. \
                 File operations are restricted to {}. \
                 You do not have access to this file and must not attempt to reproduce, \
                 guess, or fabricate its contents. Inform the user that the path is \
                 outside your workspace.",
                self.workspace.display()
            )));
        }

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
}

/// Canonicalize as much of the path as possible. For paths where the final
/// components don't exist yet (e.g. writing a new file), canonicalize the
/// deepest existing ancestor and append the rest.
pub(crate) fn best_effort_canonicalize(path: &Path) -> PathBuf {
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

/// Error type for file tool.
#[derive(Debug, thiserror::Error)]
#[error("File operation failed: {0}")]
pub struct FileError(String);

/// Arguments for file tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileArgs {
    /// The operation to perform.
    pub operation: String,
    /// The file or directory path.
    pub path: String,
    /// Content to write (required for write operation).
    pub content: Option<String>,
    /// Whether to create parent directories if they don't exist (for write operations).
    #[serde(default = "default_create_dirs")]
    pub create_dirs: bool,
}

fn default_create_dirs() -> bool {
    true
}

/// Output from file tool.
#[derive(Debug, Serialize)]
pub struct FileOutput {
    /// Whether the operation succeeded.
    pub success: bool,
    /// The operation performed.
    pub operation: String,
    /// The file/directory path.
    pub path: String,
    /// File content (for read operations).
    pub content: Option<String>,
    /// Directory entries (for list operations).
    pub entries: Option<Vec<FileEntryOutput>>,
    /// Error message if operation failed.
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

impl Tool for FileTool {
    const NAME: &'static str = "file";

    type Error = FileError;
    type Args = FileArgs;
    type Output = FileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/file").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["read", "write", "list"],
                        "description": "The file operation to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "The file or directory path. Relative paths are resolved from the workspace root."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file (required for write operation)"
                    },
                    "create_dirs": {
                        "type": "boolean",
                        "default": true,
                        "description": "For write operations: create parent directories if they don't exist"
                    }
                },
                "required": ["operation", "path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.resolve_path(&args.path)?;

        match args.operation.as_str() {
            "read" => do_file_read(&path).await,
            "write" => {
                // When sandbox is enabled, identity files must go through the
                // dedicated identity API to keep the update flow consistent.
                // When sandbox is disabled, the user has opted into full access.
                if self.sandbox.mode_enabled() {
                    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    const PROTECTED_FILES: &[&str] = &["SOUL.md", "IDENTITY.md", "USER.md"];
                    if PROTECTED_FILES
                        .iter()
                        .any(|f| file_name.eq_ignore_ascii_case(f))
                    {
                        return Err(FileError(
                            "ACCESS DENIED: Identity files are protected and cannot be modified \
                             through file operations. Use the identity management API instead."
                                .to_string(),
                        ));
                    }
                }

                let content = args.content.ok_or_else(|| {
                    FileError("Content is required for write operation".to_string())
                })?;
                do_file_write(&path, content, args.create_dirs).await
            }
            "list" => do_file_list(&path).await,
            _ => Err(FileError(format!("Unknown operation: {}", args.operation))),
        }
    }
}

async fn do_file_read(path: &Path) -> Result<FileOutput, FileError> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| FileError(format!("Failed to read file: {e}")))?;

    let content = crate::tools::truncate_output(&raw, crate::tools::MAX_TOOL_OUTPUT_BYTES);

    Ok(FileOutput {
        success: true,
        operation: "read".to_string(),
        path: path.to_string_lossy().to_string(),
        content: Some(content),
        entries: None,
        error: None,
    })
}

async fn do_file_write(
    path: &Path,
    content: String,
    create_dirs: bool,
) -> Result<FileOutput, FileError> {
    // Ensure parent directory exists if requested
    if create_dirs && let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| FileError(format!("Failed to create directory: {e}")))?;
    }

    tokio::fs::write(path, content)
        .await
        .map_err(|e| FileError(format!("Failed to write file: {e}")))?;

    Ok(FileOutput {
        success: true,
        operation: "write".to_string(),
        path: path.to_string_lossy().to_string(),
        content: None,
        entries: None,
        error: None,
    })
}

async fn do_file_list(path: &Path) -> Result<FileOutput, FileError> {
    let mut entries = Vec::new();

    let mut reader = tokio::fs::read_dir(path)
        .await
        .map_err(|e| FileError(format!("Failed to read directory: {e}")))?;

    let max_entries = crate::tools::MAX_DIR_ENTRIES;
    let mut total_count = 0usize;

    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|e| FileError(format!("Failed to read entry: {e}")))?
    {
        total_count += 1;

        if entries.len() < max_entries {
            let metadata = entry
                .metadata()
                .await
                .map_err(|e| FileError(format!("Failed to read metadata: {e}")))?;

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

/// System-internal file operations that bypass workspace containment.
/// These are used by the system itself (not LLM-facing) and operate on
/// arbitrary paths.
pub async fn file_read(path: impl AsRef<Path>) -> crate::error::Result<String> {
    do_file_read(path.as_ref())
        .await
        .map(|output| output.content.unwrap_or_default())
        .map_err(|e| crate::error::AgentError::Other(e.into()).into())
}

pub async fn file_write(
    path: impl AsRef<Path>,
    content: impl AsRef<[u8]>,
) -> crate::error::Result<()> {
    do_file_write(
        path.as_ref(),
        String::from_utf8_lossy(content.as_ref()).to_string(),
        true,
    )
    .await
    .map(|_| ())
    .map_err(|e| crate::error::AgentError::Other(e.into()).into())
}

pub async fn file_list(path: impl AsRef<Path>) -> crate::error::Result<Vec<FileEntry>> {
    let output = do_file_list(path.as_ref())
        .await
        .map_err(|e| crate::error::AgentError::Other(e.into()))?;

    let entries = output.entries.ok_or_else(|| {
        crate::error::AgentError::Other(anyhow::anyhow!("No entries in list result"))
    })?;

    Ok(entries
        .into_iter()
        .map(|e| FileEntry {
            name: e.name,
            file_type: match e.entry_type.as_str() {
                "file" => FileType::File,
                "directory" => FileType::Directory,
                _ => FileType::Other,
            },
            size: e.size,
        })
        .collect())
}

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

    #[tokio::test]
    async fn sandbox_enabled_rejects_read_outside_workspace() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let outside = temp_dir.path().join("outside");
        fs::create_dir_all(&workspace).expect("failed to create workspace");
        fs::create_dir_all(&outside).expect("failed to create outside dir");

        let file = outside.join("secret.txt");
        fs::write(&file, "secret data").expect("failed to write file");

        let sandbox = create_sandbox(SandboxMode::Enabled, &workspace);
        let tool = FileTool::new(workspace, sandbox);

        let result = tool
            .call(FileArgs {
                operation: "read".to_string(),
                path: file.to_string_lossy().into_owned(),
                content: None,
                create_dirs: false,
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

        let sandbox = create_sandbox(SandboxMode::Disabled, &workspace);
        let tool = FileTool::new(workspace, sandbox);

        let result = tool
            .call(FileArgs {
                operation: "read".to_string(),
                path: file.to_string_lossy().into_owned(),
                content: None,
                create_dirs: false,
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

        let sandbox = create_sandbox(SandboxMode::Disabled, &workspace);
        let tool = FileTool::new(workspace, sandbox);

        let result = tool
            .call(FileArgs {
                operation: "write".to_string(),
                path: file.to_string_lossy().into_owned(),
                content: Some("written outside workspace".to_string()),
                create_dirs: false,
            })
            .await
            .expect("should succeed when sandbox is disabled");

        assert!(result.success);
        let written = fs::read_to_string(&file).expect("failed to read written file");
        assert_eq!(written, "written outside workspace");
    }

    #[tokio::test]
    async fn sandbox_enabled_blocks_identity_file_write() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let sandbox = create_sandbox(SandboxMode::Enabled, &workspace);
        let tool = FileTool::new(workspace.clone(), sandbox);

        let result = tool
            .call(FileArgs {
                operation: "write".to_string(),
                path: workspace.join("SOUL.md").to_string_lossy().into_owned(),
                content: Some("overwritten".to_string()),
                create_dirs: false,
            })
            .await;

        let error = result
            .expect_err("should block identity file write")
            .to_string();
        assert!(
            error.contains("Identity files are protected"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn sandbox_disabled_allows_identity_file_write() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let sandbox = create_sandbox(SandboxMode::Disabled, &workspace);
        let tool = FileTool::new(workspace.clone(), sandbox);

        let result = tool
            .call(FileArgs {
                operation: "write".to_string(),
                path: workspace.join("IDENTITY.md").to_string_lossy().into_owned(),
                content: Some("new identity".to_string()),
                create_dirs: false,
            })
            .await
            .expect("should allow identity file write when sandbox is disabled");

        assert!(result.success);
        let written =
            fs::read_to_string(workspace.join("IDENTITY.md")).expect("failed to read file");
        assert_eq!(written, "new identity");
    }
}
