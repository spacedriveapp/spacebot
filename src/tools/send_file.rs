//! Send file tool for delivering file attachments to users (channel only).

use crate::OutboundResponse;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Tool for sending files to users.
///
/// Reads a file from the local filesystem and sends it as an attachment
/// in the conversation. The channel process creates a response sender per
/// conversation turn and this tool routes file responses through it.
/// File access is restricted to the agent's workspace boundary.
#[derive(Debug, Clone)]
pub struct SendFileTool {
    response_tx: mpsc::Sender<OutboundResponse>,
    workspace: PathBuf,
}

impl SendFileTool {
    pub fn new(response_tx: mpsc::Sender<OutboundResponse>, workspace: PathBuf) -> Self {
        Self {
            response_tx,
            workspace,
        }
    }

    /// Validate that a path falls within the workspace boundary.
    ///
    /// Checks both the canonicalized path and individual path components for
    /// symlinks to prevent TOCTOU races where a symlink is swapped between
    /// validation and the actual file read.
    fn validate_workspace_path(&self, path: &std::path::Path) -> Result<PathBuf, SendFileError> {
        let workspace = &self.workspace;

        let canonical = path.canonicalize().map_err(|error| {
            SendFileError(format!("can't resolve path '{}': {error}", path.display()))
        })?;
        let workspace_canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        if !canonical.starts_with(&workspace_canonical) {
            return Err(SendFileError(format!(
                "ACCESS DENIED: Path is outside the workspace boundary. \
                 File operations are restricted to {}.",
                workspace.display()
            )));
        }

        // Reject paths containing symlinks within the workspace to prevent
        // TOCTOU races where a path component is replaced with a symlink
        // between this check and the file read.
        let relative_original = path
            .strip_prefix(workspace)
            .or_else(|_| path.strip_prefix(&workspace_canonical))
            .unwrap_or(path);
        let mut walk = workspace_canonical.clone();
        for component in relative_original.components() {
            walk.push(component);
            match walk.symlink_metadata() {
                Ok(meta) if meta.is_symlink() => {
                    return Err(SendFileError(
                        "ACCESS DENIED: Symlinks are not allowed within the workspace.".into(),
                    ));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(SendFileError(format!(
                        "can't verify path component '{}': {error}",
                        walk.display()
                    )));
                }
            }
        }

        Ok(canonical)
    }
}

/// Error type for send_file tool.
#[derive(Debug, thiserror::Error)]
#[error("Send file failed: {0}")]
pub struct SendFileError(String);

/// Arguments for send_file tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendFileArgs {
    /// The absolute path to the file to send.
    pub file_path: String,
    /// Optional caption/message to accompany the file.
    #[serde(default)]
    pub caption: Option<String>,
}

/// Output from send_file tool.
#[derive(Debug, Serialize)]
pub struct SendFileOutput {
    pub success: bool,
    pub filename: String,
    pub size_bytes: u64,
}

/// Maximum file size: 25 MB (Discord's limit for non-boosted servers).
const MAX_FILE_SIZE_BYTES: u64 = 25 * 1024 * 1024;

impl Tool for SendFileTool {
    const NAME: &'static str = "send_file";

    type Error = SendFileError;
    type Args = SendFileArgs;
    type Output = SendFileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/send_file").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The absolute path to the file to send."
                    },
                    "caption": {
                        "type": "string",
                        "description": "Optional caption or message to accompany the file."
                    }
                },
                "required": ["file_path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let raw_path = PathBuf::from(&args.file_path);

        if !raw_path.is_absolute() {
            return Err(SendFileError("file_path must be an absolute path".into()));
        }

        let path = self.validate_workspace_path(&raw_path)?;

        let metadata = tokio::fs::metadata(&path).await.map_err(|error| {
            SendFileError(format!("can't read file '{}': {error}", path.display()))
        })?;

        if !metadata.is_file() {
            return Err(SendFileError(format!("'{}' is not a file", path.display())));
        }

        if metadata.len() > MAX_FILE_SIZE_BYTES {
            return Err(SendFileError(format!(
                "file is too large ({} bytes, max {} bytes)",
                metadata.len(),
                MAX_FILE_SIZE_BYTES,
            )));
        }

        let data = tokio::fs::read(&path).await.map_err(|error| {
            SendFileError(format!("failed to read '{}': {error}", path.display()))
        })?;

        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());

        let mime_type = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();

        let size_bytes = data.len() as u64;

        tracing::info!(
            file_path = %path.display(),
            filename = %filename,
            mime_type = %mime_type,
            size_bytes,
            "send_file tool called"
        );

        let response = OutboundResponse::File {
            filename: filename.clone(),
            data,
            mime_type,
            caption: args.caption,
        };

        self.response_tx
            .send(response)
            .await
            .map_err(|error| SendFileError(format!("failed to send file: {error}")))?;

        Ok(SendFileOutput {
            success: true,
            filename,
            size_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn create_tool(workspace: PathBuf) -> SendFileTool {
        let (response_tx, _response_rx) = mpsc::channel(1);
        SendFileTool::new(response_tx, workspace)
    }

    #[test]
    fn validate_workspace_path_accepts_regular_file() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace");

        let path = workspace.join("report.txt");
        fs::write(&path, "ok").expect("failed to write test file");

        let tool = create_tool(workspace.clone());
        let validated = tool
            .validate_workspace_path(&path)
            .expect("path should be accepted");

        assert_eq!(
            validated,
            path.canonicalize().expect("failed to canonicalize")
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_workspace_path_rejects_symlink_components() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let workspace = temp_dir.path().join("workspace");
        let real_dir = workspace.join("real");
        let real_file = real_dir.join("file.txt");
        let link_dir = workspace.join("link");

        fs::create_dir_all(&real_dir).expect("failed to create real dir");
        fs::write(&real_file, "secret").expect("failed to write test file");
        symlink(&real_dir, &link_dir).expect("failed to create symlink");

        let tool = create_tool(workspace.clone());
        let result = tool.validate_workspace_path(&link_dir.join("file.txt"));

        assert!(result.is_err(), "symlink traversal should be rejected");
        let error = result.expect_err("missing expected error").to_string();
        assert!(
            error.contains("Symlinks are not allowed"),
            "unexpected error: {error}"
        );
    }
}
