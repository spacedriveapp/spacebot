//! Memory delete tool for branches.
//!
//! Soft-deletes a memory by setting its `forgotten` flag. The memory stays in
//! the database but is excluded from all search and recall operations.

use crate::memory::MemorySearch;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for soft-deleting memories.
#[derive(Debug, Clone)]
pub struct MemoryDeleteTool {
    memory_search: Arc<MemorySearch>,
}

impl MemoryDeleteTool {
    /// Create a new memory delete tool.
    pub fn new(memory_search: Arc<MemorySearch>) -> Self {
        Self { memory_search }
    }
}

/// Error type for memory delete tool.
#[derive(Debug, thiserror::Error)]
#[error("Memory delete failed: {0}")]
pub struct MemoryDeleteError(String);

/// Arguments for memory delete tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryDeleteArgs {
    /// The ID of the memory to forget.
    pub memory_id: String,
    /// Brief reason for forgetting this memory (for audit purposes).
    pub reason: Option<String>,
}

/// Output from memory delete tool.
#[derive(Debug, Serialize)]
pub struct MemoryDeleteOutput {
    /// Whether the memory was found and forgotten.
    pub forgotten: bool,
    /// Description of what happened.
    pub message: String,
}

impl Tool for MemoryDeleteTool {
    const NAME: &'static str = "memory_delete";

    type Error = MemoryDeleteError;
    type Args = MemoryDeleteArgs;
    type Output = MemoryDeleteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/memory_delete").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "memory_id": {
                        "type": "string",
                        "description": "The ID of the memory to forget (from memory_recall results)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional reason for forgetting this memory"
                    }
                },
                "required": ["memory_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let store = self.memory_search.store();

        // Verify the memory exists first
        let memory = store
            .load(&args.memory_id)
            .await
            .map_err(|e| MemoryDeleteError(format!("Failed to look up memory: {e}")))?;

        let Some(memory) = memory else {
            return Ok(MemoryDeleteOutput {
                forgotten: false,
                message: format!("No memory found with ID: {}", args.memory_id),
            });
        };

        if memory.forgotten {
            return Ok(MemoryDeleteOutput {
                forgotten: false,
                message: format!("Memory {} is already forgotten.", args.memory_id),
            });
        }

        let was_forgotten = store
            .forget(&args.memory_id)
            .await
            .map_err(|e| MemoryDeleteError(format!("Failed to forget memory: {e}")))?;

        let reason_suffix = args
            .reason
            .as_deref()
            .map(|r| format!(" Reason: {r}"))
            .unwrap_or_default();

        if was_forgotten {
            #[cfg(feature = "metrics")]
            crate::telemetry::Metrics::global()
                .memory_updates_total
                .with_label_values(&["unknown", "forget"])
                .inc();

            tracing::info!(
                memory_id = %args.memory_id,
                memory_type = %memory.memory_type,
                reason = ?args.reason,
                "memory forgotten"
            );

            let preview = memory.content.lines().next().unwrap_or("(empty)");
            Ok(MemoryDeleteOutput {
                forgotten: true,
                message: format!(
                    "Forgotten [{type}] memory: \"{preview}\".{reason_suffix}",
                    type = memory.memory_type,
                    preview = truncate(preview, 80),
                ),
            })
        } else {
            Ok(MemoryDeleteOutput {
                forgotten: false,
                message: format!("Failed to forget memory {}.", args.memory_id),
            })
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}
