//! Worker transcript serialization and compression.
//!
//! Converts a Rig `Vec<Message>` history into a flat `Vec<TranscriptStep>`,
//! then serializes to gzipped JSON for compact storage on the `worker_runs` row.

use crate::tools::{MAX_TOOL_OUTPUT_BYTES, truncate_output};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// Maximum byte length for tool call arguments in transcripts.
const MAX_TOOL_ARGS_BYTES: usize = 2_000;

/// A single step in a worker transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptStep {
    /// Agent reasoning and/or tool calls.
    Action { content: Vec<ActionContent> },
    /// Tool execution result.
    ToolResult {
        call_id: String,
        name: String,
        text: String,
    },
}

/// Content within an action step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionContent {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        args: String,
    },
}

/// Convert a Rig message history to transcript steps, serialize as JSON, and gzip compress.
pub fn serialize_transcript(history: &[rig::message::Message]) -> Vec<u8> {
    let steps = convert_history(history);
    let json = serde_json::to_vec(&steps).unwrap_or_default();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json).ok();
    encoder.finish().unwrap_or_default()
}

/// Decompress and deserialize a gzipped transcript blob.
pub fn deserialize_transcript(blob: &[u8]) -> anyhow::Result<Vec<TranscriptStep>> {
    let mut decoder = GzDecoder::new(blob);
    let mut json = Vec::new();
    decoder.read_to_end(&mut json)?;
    let steps: Vec<TranscriptStep> = serde_json::from_slice(&json)?;
    Ok(steps)
}

/// Convert Rig `Vec<Message>` to `Vec<TranscriptStep>`.
fn convert_history(history: &[rig::message::Message]) -> Vec<TranscriptStep> {
    let mut steps = Vec::new();

    for message in history {
        match message {
            rig::message::Message::Assistant { content, .. } => {
                let mut parts = Vec::new();
                for item in content.iter() {
                    match item {
                        rig::message::AssistantContent::Text(text) => {
                            if !text.text.is_empty() {
                                parts.push(ActionContent::Text {
                                    text: text.text.clone(),
                                });
                            }
                        }
                        rig::message::AssistantContent::ToolCall(tool_call) => {
                            let args_str = tool_call.function.arguments.to_string();
                            let args = if args_str.len() > MAX_TOOL_ARGS_BYTES {
                                truncate_output(&args_str, MAX_TOOL_ARGS_BYTES)
                            } else {
                                args_str
                            };
                            parts.push(ActionContent::ToolCall {
                                id: tool_call.id.clone(),
                                name: tool_call.function.name.clone(),
                                args,
                            });
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    steps.push(TranscriptStep::Action { content: parts });
                }
            }
            rig::message::Message::User { content } => {
                for item in content.iter() {
                    match item {
                        rig::message::UserContent::ToolResult(tool_result) => {
                            let call_id = tool_result
                                .call_id
                                .clone()
                                .unwrap_or_else(|| tool_result.id.clone());

                            let text = tool_result
                                .content
                                .iter()
                                .filter_map(|c| {
                                    if let rig::message::ToolResultContent::Text(t) = c {
                                        Some(t.text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            let truncated = truncate_output(&text, MAX_TOOL_OUTPUT_BYTES);

                            steps.push(TranscriptStep::ToolResult {
                                call_id,
                                name: String::new(),
                                text: truncated,
                            });
                        }
                        rig::message::UserContent::Text(text) => {
                            // Skip compaction markers and system-injected messages
                            if !text.text.is_empty() && !text.text.starts_with("[System:") {
                                steps.push(TranscriptStep::Action {
                                    content: vec![ActionContent::Text {
                                        text: text.text.clone(),
                                    }],
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    steps
}
