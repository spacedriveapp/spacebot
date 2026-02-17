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
#[derive(Debug, Clone)]
pub struct SendFileTool {
    response_tx: mpsc::Sender<OutboundResponse>,
}

impl SendFileTool {
    pub fn new(response_tx: mpsc::Sender<OutboundResponse>) -> Self {
        Self { response_tx }
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
        let path = PathBuf::from(&args.file_path);

        if !path.is_absolute() {
            return Err(SendFileError("file_path must be an absolute path".into()));
        }

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
