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
    /// User-originated text (task prompt, follow-up input).
    ///
    /// Distinct from `Action` so that `transcript_to_history()` can reconstruct
    /// the correct `Message::User` role instead of treating everything as
    /// `Message::Assistant`.
    UserText { text: String },
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
    serialize_steps(&steps)
}

/// Serialize pre-built transcript steps as gzipped JSON.
///
/// Used by OpenCode workers that build transcript steps directly from SSE
/// events rather than from a Rig message history.
pub fn serialize_steps(steps: &[TranscriptStep]) -> Vec<u8> {
    let json = serde_json::to_vec(steps).unwrap_or_default();

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

/// Convert OpenCode messages (from `GET /session/:id/message`) into transcript
/// steps and extract all assistant text as the result string.
///
/// The input is the raw JSON array returned by the OpenCode server. Each element
/// has shape `{ info: Message, parts: Part[] }`. Messages are chronological.
pub fn convert_opencode_messages(messages: &[serde_json::Value]) -> (Vec<TranscriptStep>, String) {
    let mut steps = Vec::new();
    let mut all_text = String::new();

    for message_wrapper in messages {
        let role = message_wrapper
            .pointer("/info/role")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let parts = match message_wrapper.get("parts").and_then(|p| p.as_array()) {
            Some(parts) => parts,
            None => continue,
        };

        for part_value in parts {
            let part_type = part_value
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            match part_type {
                "text" => {
                    let text = part_value
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    if !text.is_empty() {
                        if role == "assistant" {
                            if !all_text.is_empty() {
                                all_text.push_str("\n\n");
                            }
                            all_text.push_str(text);
                            steps.push(TranscriptStep::Action {
                                content: vec![ActionContent::Text {
                                    text: text.to_string(),
                                }],
                            });
                        } else {
                            steps.push(TranscriptStep::UserText {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                "tool" => {
                    let tool_name = part_value
                        .get("tool")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown");
                    let call_id = part_value
                        .get("callID")
                        .and_then(|c| c.as_str())
                        .or_else(|| part_value.get("id").and_then(|i| i.as_str()))
                        .unwrap_or("")
                        .to_string();

                    let state = part_value.get("state");
                    let status = state
                        .and_then(|s| s.get("status"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");

                    // Extract tool input
                    let input = state
                        .and_then(|s| s.get("input"))
                        .map(|v| {
                            let s = v.to_string();
                            if s.len() > MAX_TOOL_ARGS_BYTES {
                                truncate_output(&s, MAX_TOOL_ARGS_BYTES)
                            } else {
                                s
                            }
                        })
                        .unwrap_or_default();

                    // Always add the tool call
                    steps.push(TranscriptStep::Action {
                        content: vec![ActionContent::ToolCall {
                            id: call_id.clone(),
                            name: tool_name.to_string(),
                            args: input,
                        }],
                    });

                    // Add result if completed or errored
                    match status {
                        "completed" => {
                            let output = state
                                .and_then(|s| s.get("output"))
                                .and_then(|o| o.as_str())
                                .unwrap_or("");
                            let truncated = truncate_output(output, MAX_TOOL_OUTPUT_BYTES);
                            steps.push(TranscriptStep::ToolResult {
                                call_id,
                                name: tool_name.to_string(),
                                text: truncated,
                            });
                        }
                        "error" => {
                            let error_text = state
                                .and_then(|s| s.get("error"))
                                .and_then(|e| e.as_str())
                                .unwrap_or("unknown error");
                            steps.push(TranscriptStep::ToolResult {
                                call_id,
                                name: tool_name.to_string(),
                                text: format!("Error: {error_text}"),
                            });
                        }
                        _ => {}
                    }
                }
                _ => {
                    // step-start, step-finish, reasoning, file, etc. — skip for transcript
                }
            }
        }
    }

    (steps, all_text)
}

/// Convert SSE-accumulated `OpenCodePart`s into transcript steps.
///
/// This is the fallback path used when the post-completion `get_messages()` API
/// call to the OpenCode server fails (e.g. because the server was recycled).
/// The parts were collected during SSE streaming and contain the same data,
/// just in a different structure.
///
/// Parts are upserted by ID during SSE streaming (a tool part transitions
/// through Pending → Running → Completed with the same ID), so we deduplicate
/// by keeping only the latest version of each part before converting.
pub fn convert_opencode_parts(
    parts: &[crate::opencode::types::OpenCodePart],
) -> Vec<TranscriptStep> {
    use crate::opencode::types::{OpenCodePart, OpenCodeToolState};

    // Deduplicate: keep insertion order, but replace earlier versions with later ones.
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut deduped: Vec<&OpenCodePart> = Vec::new();
    for part in parts {
        let id = part.id().to_string();
        if let Some(&index) = seen.get(&id) {
            deduped[index] = part;
        } else {
            seen.insert(id, deduped.len());
            deduped.push(part);
        }
    }

    let mut steps = Vec::new();
    for part in deduped {
        match part {
            OpenCodePart::Text { text, .. } => {
                if !text.is_empty() {
                    steps.push(TranscriptStep::Action {
                        content: vec![ActionContent::Text { text: text.clone() }],
                    });
                }
            }
            OpenCodePart::Tool { id, tool, state } => {
                match state {
                    OpenCodeToolState::Running { input, .. }
                    | OpenCodeToolState::Completed { input, .. } => {
                        let args = input.clone().unwrap_or_default();
                        let args = if args.len() > MAX_TOOL_ARGS_BYTES {
                            truncate_output(&args, MAX_TOOL_ARGS_BYTES)
                        } else {
                            args
                        };
                        steps.push(TranscriptStep::Action {
                            content: vec![ActionContent::ToolCall {
                                id: id.clone(),
                                name: tool.clone(),
                                args,
                            }],
                        });
                    }
                    OpenCodeToolState::Pending => {
                        steps.push(TranscriptStep::Action {
                            content: vec![ActionContent::ToolCall {
                                id: id.clone(),
                                name: tool.clone(),
                                args: String::new(),
                            }],
                        });
                    }
                    OpenCodeToolState::Error { .. } => {
                        steps.push(TranscriptStep::Action {
                            content: vec![ActionContent::ToolCall {
                                id: id.clone(),
                                name: tool.clone(),
                                args: String::new(),
                            }],
                        });
                    }
                }

                // Add result for completed/error states
                match state {
                    OpenCodeToolState::Completed { output, .. } => {
                        let text = output.as_deref().unwrap_or("");
                        let truncated = truncate_output(text, MAX_TOOL_OUTPUT_BYTES);
                        steps.push(TranscriptStep::ToolResult {
                            call_id: id.clone(),
                            name: tool.clone(),
                            text: truncated,
                        });
                    }
                    OpenCodeToolState::Error { error } => {
                        let error_text = error.as_deref().unwrap_or("unknown error");
                        steps.push(TranscriptStep::ToolResult {
                            call_id: id.clone(),
                            name: tool.clone(),
                            text: format!("Error: {error_text}"),
                        });
                    }
                    _ => {}
                }
            }
            // step_start/step_finish are visual separators, skip for transcript
            OpenCodePart::StepStart { .. } | OpenCodePart::StepFinish { .. } => {}
        }
    }
    steps
}

/// Convert a persisted `Vec<TranscriptStep>` back into a Rig `Vec<Message>` history.
///
/// Used when resuming an idle interactive worker after restart: the transcript
/// blob is the only surviving record of the worker's conversation, so we
/// reconstruct the Rig message history from it.
///
/// The mapping is lossy (reasoning, images, etc. are not round-tripped), but
/// it preserves all text and tool call/result pairs which is sufficient for
/// the LLM to pick up the conversation.
pub fn transcript_to_history(steps: &[TranscriptStep]) -> Vec<rig::message::Message> {
    use rig::message::{
        AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
        UserContent,
    };
    use rig::one_or_many::OneOrMany;

    let mut messages: Vec<Message> = Vec::new();

    for step in steps {
        match step {
            TranscriptStep::Action { content } => {
                let mut parts: Vec<AssistantContent> = Vec::new();
                for item in content {
                    match item {
                        ActionContent::Text { text } => {
                            parts.push(AssistantContent::Text(Text { text: text.clone() }));
                        }
                        ActionContent::ToolCall { id, name, args } => {
                            let arguments = serde_json::from_str(args)
                                .unwrap_or_else(|_| serde_json::Value::String(args.clone()));
                            parts.push(AssistantContent::ToolCall(ToolCall {
                                id: id.clone(),
                                call_id: None,
                                function: ToolFunction {
                                    name: name.clone(),
                                    arguments,
                                },
                                signature: None,
                                additional_params: None,
                            }));
                        }
                    }
                }
                if !parts.is_empty() {
                    // OneOrMany::many returns Err only for empty vecs; we checked above.
                    let content = OneOrMany::many(parts).expect("parts is non-empty");
                    messages.push(Message::Assistant { id: None, content });
                }
            }
            TranscriptStep::UserText { text } => {
                if !text.is_empty() {
                    messages.push(Message::User {
                        content: OneOrMany::one(UserContent::Text(Text { text: text.clone() })),
                    });
                }
            }
            TranscriptStep::ToolResult {
                call_id,
                name: _,
                text,
            } => {
                let result = ToolResult {
                    id: call_id.clone(),
                    call_id: Some(call_id.clone()),
                    content: OneOrMany::one(ToolResultContent::Text(Text { text: text.clone() })),
                };
                messages.push(Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(result)),
                });
            }
        }
    }

    messages
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
                                steps.push(TranscriptStep::UserText {
                                    text: text.text.clone(),
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
