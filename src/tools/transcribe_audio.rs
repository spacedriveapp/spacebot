//! Transcribe audio tool for workers.
//!
//! Allows workers to transcribe audio files using whatever STT backend is
//! configured in `routing.voice` — local Whisper or an HTTP provider.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::llm::manager::LlmManager;

/// Tool for transcribing audio files to text.
#[derive(Clone)]
pub struct TranscribeAudioTool {
    /// The configured voice model spec (full `routing.voice` value).
    voice_model: String,
    llm_manager: Arc<LlmManager>,
    http: reqwest::Client,
}

impl TranscribeAudioTool {
    /// Create a new transcribe audio tool.
    pub fn new(
        voice_model: impl Into<String>,
        llm_manager: Arc<LlmManager>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            voice_model: voice_model.into(),
            llm_manager,
            http,
        }
    }
}

/// Error type for transcribe audio tool.
#[derive(Debug, thiserror::Error)]
#[error("Audio transcription failed: {0}")]
pub struct TranscribeAudioError(String);

/// Arguments for transcribe audio tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TranscribeAudioArgs {
    /// Path to the audio file to transcribe (absolute or relative to the workspace).
    /// Supports ogg, opus, mp3, flac, wav, m4a.
    pub path: String,
}

/// Output from transcribe audio tool.
#[derive(Debug, Serialize)]
pub struct TranscribeAudioOutput {
    /// The transcribed text.
    pub transcript: String,
}

impl Tool for TranscribeAudioTool {
    const NAME: &'static str = "transcribe_audio";

    type Error = TranscribeAudioError;
    type Args = TranscribeAudioArgs;
    type Output = TranscribeAudioOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/transcribe_audio").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the audio file to transcribe (absolute or relative to the workspace). Supports ogg, opus, mp3, flac, wav, m4a."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let audio = tokio::fs::read(&args.path)
            .await
            .map_err(|e| TranscribeAudioError(format!("failed to read {}: {}", args.path, e)))?;

        // Infer mime type from file extension for the HTTP provider path.
        let mime_type = mime_from_path(&args.path);

        let transcript =
            crate::stt::transcribe_bytes(&self.voice_model, &audio, mime_type, &self.llm_manager, &self.http)
                .await
                .map_err(|e| TranscribeAudioError(e.to_string()))?;

        Ok(TranscribeAudioOutput { transcript })
    }
}

/// Infer a MIME type string from a file path extension.
fn mime_from_path(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "m4a" | "mp4" => "audio/mp4",
        "opus" => "audio/opus",
        _ => "audio/ogg",
    }
}
