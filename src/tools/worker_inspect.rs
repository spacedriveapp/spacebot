//! Worker transcript inspection tool for branches.
//!
//! Allows a branch to retrieve the full transcript of a completed worker run,
//! or list recent worker runs to find the right one.

use crate::conversation::history::ProcessRunLogger;
use crate::conversation::worker_transcript;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool for inspecting worker run transcripts.
#[derive(Debug, Clone)]
pub struct WorkerInspectTool {
    run_logger: ProcessRunLogger,
    agent_id: String,
}

impl WorkerInspectTool {
    pub fn new(run_logger: ProcessRunLogger, agent_id: String) -> Self {
        Self {
            run_logger,
            agent_id,
        }
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let boundary = text.floor_char_boundary(max_bytes);
    format!("{}...", &text[..boundary])
}

#[derive(Debug, thiserror::Error)]
#[error("Worker inspect failed: {0}")]
pub struct WorkerInspectError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkerInspectArgs {
    /// The worker ID to inspect. Omit to list recent worker runs.
    #[serde(default)]
    pub worker_id: Option<String>,
    /// Maximum number of worker runs to list (default 10, max 50). Only used when listing.
    #[serde(default = "default_list_limit")]
    pub limit: i64,
}

fn default_list_limit() -> i64 {
    10
}

#[derive(Debug, Serialize)]
pub struct WorkerInspectOutput {
    pub action: String,
    pub summary: String,
}

impl Tool for WorkerInspectTool {
    const NAME: &'static str = "worker_inspect";

    type Error = WorkerInspectError;
    type Args = WorkerInspectArgs;
    type Output = WorkerInspectOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/worker_inspect").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "worker_id": {
                        "type": "string",
                        "description": "UUID of the worker run to inspect. Omit to list recent workers."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 10,
                        "description": "Number of recent workers to list (1-50). Only used when worker_id is omitted."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let Some(worker_id) = args.worker_id else {
            return self.list_workers(args.limit).await;
        };

        let detail = self
            .run_logger
            .get_worker_detail(&self.agent_id, &worker_id)
            .await
            .map_err(|e| WorkerInspectError(format!("Failed to query worker: {e}")))?
            .ok_or_else(|| WorkerInspectError(format!("No worker found with ID {worker_id}")))?;

        let mut summary = format!(
            "## Worker {}\n\n**Task:** {}\n**Status:** {}\n**Started:** {}\n",
            detail.id, detail.task, detail.status, detail.started_at,
        );

        if let Some(completed_at) = &detail.completed_at {
            summary.push_str(&format!("**Completed:** {completed_at}\n"));
        }

        if let Some(result) = &detail.result {
            summary.push_str(&format!("\n### Result\n\n{result}\n"));
        }

        let events = self
            .run_logger
            .list_worker_events(&worker_id, 50)
            .await
            .map_err(|e| WorkerInspectError(format!("Failed to load worker events: {e}")))?;
        if !events.is_empty() {
            summary.push_str("\n### Event Timeline\n\n");
            for event in events {
                let payload = event
                    .payload_json
                    .as_deref()
                    .map(|json| truncate_utf8(json, 200))
                    .unwrap_or_default();
                if payload.is_empty() {
                    summary.push_str(&format!("- [{}] {}\n", event.created_at, event.event_type));
                } else {
                    summary.push_str(&format!(
                        "- [{}] {} — {}\n",
                        event.created_at, event.event_type, payload
                    ));
                }
            }
            summary.push('\n');
        }

        if let Some(blob) = &detail.transcript_blob {
            match worker_transcript::deserialize_transcript(blob) {
                Ok(steps) => {
                    summary.push_str(&format!("\n### Transcript ({} steps)\n\n", steps.len()));
                    for step in &steps {
                        match step {
                            worker_transcript::TranscriptStep::Action { content } => {
                                for item in content {
                                    match item {
                                        worker_transcript::ActionContent::Text { text } => {
                                            summary.push_str(&format!("**Agent:** {text}\n\n"));
                                        }
                                        worker_transcript::ActionContent::ToolCall {
                                            name,
                                            args,
                                            ..
                                        } => {
                                            summary.push_str(&format!(
                                                "**Tool call:** `{name}`\n```\n{args}\n```\n\n"
                                            ));
                                        }
                                    }
                                }
                            }
                            worker_transcript::TranscriptStep::ToolResult {
                                name, text, ..
                            } => {
                                let label = if name.is_empty() { "tool" } else { name };
                                let display = if text.len() > 500 {
                                    format!(
                                        "{}\n[truncated, {} bytes total]",
                                        truncate_utf8(text, 500),
                                        text.len()
                                    )
                                } else {
                                    text.clone()
                                };
                                summary.push_str(&format!(
                                    "**Result ({label}):**\n```\n{display}\n```\n\n"
                                ));
                            }
                        }
                    }
                }
                Err(error) => {
                    summary.push_str(&format!(
                        "\n*Transcript could not be decompressed: {error}*\n"
                    ));
                }
            }
        } else {
            summary.push_str("\n*No transcript available for this worker.*\n");
        }

        Ok(WorkerInspectOutput {
            action: "inspect".to_string(),
            summary,
        })
    }
}

impl WorkerInspectTool {
    async fn list_workers(&self, limit: i64) -> Result<WorkerInspectOutput, WorkerInspectError> {
        let limit = limit.clamp(1, 50);
        let (rows, total) = self
            .run_logger
            .list_worker_runs(&self.agent_id, limit, 0, None)
            .await
            .map_err(|e| WorkerInspectError(format!("Failed to list workers: {e}")))?;

        if rows.is_empty() {
            return Ok(WorkerInspectOutput {
                action: "list".to_string(),
                summary: "No worker runs found.".to_string(),
            });
        }

        let mut summary = format!("## Recent Workers ({} of {total})\n\n", rows.len());

        for row in &rows {
            let status_marker = match row.status.as_str() {
                "running" => "[running]",
                "done" => "[done]",
                "failed" => "[failed]",
                _ => "[-]",
            };
            summary.push_str(&format!(
                "{status_marker} `{}` — {} ({})\n",
                row.id, row.task, row.status,
            ));
            if let Some(channel) = &row.channel_name {
                summary.push_str(&format!("  Channel: {channel}\n"));
            }
            summary.push_str(&format!(
                "  Started: {} | {} tool calls{}\n\n",
                row.started_at,
                row.tool_calls,
                if row.has_transcript {
                    " | transcript available"
                } else {
                    ""
                },
            ));
        }

        summary.push_str("Use `worker_inspect` with a `worker_id` to view the full transcript.");

        Ok(WorkerInspectOutput {
            action: "list".to_string(),
            summary,
        })
    }
}
