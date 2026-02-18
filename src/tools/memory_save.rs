//! Memory save tool for channels and branches.

use crate::error::Result;
use crate::memory::types::{Association, CreateAssociationInput};
use crate::memory::{Memory, MemorySearch, MemoryType};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for saving memories to the store.
#[derive(Debug, Clone)]
pub struct MemorySaveTool {
    memory_search: Arc<MemorySearch>,
}

impl MemorySaveTool {
    /// Create a new memory save tool.
    pub fn new(memory_search: Arc<MemorySearch>) -> Self {
        Self { memory_search }
    }
}

/// Error type for memory save tool.
#[derive(Debug, thiserror::Error)]
#[error("Memory save failed: {0}")]
pub struct MemorySaveError(String);

impl From<crate::error::Error> for MemorySaveError {
    fn from(e: crate::error::Error) -> Self {
        MemorySaveError(format!("{e}"))
    }
}

/// Arguments for memory save tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemorySaveArgs {
    /// The content to save as a memory.
    pub content: String,
    /// The type of memory (fact, preference, decision, identity, event, observation).
    #[serde(default = "default_memory_type")]
    pub memory_type: String,
    /// Optional importance score (0.0-1.0). If not provided, uses type default.
    pub importance: Option<f32>,
    /// Optional source information (e.g., "user", "system", "inferred").
    pub source: Option<String>,
    /// Optional channel ID to associate this memory with the conversation it came from.
    pub channel_id: Option<String>,
    /// Optional associations to create with other memories.
    #[serde(default)]
    pub associations: Vec<AssociationInput>,
}

fn default_memory_type() -> String {
    "fact".to_string()
}

/// Input for creating an association.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssociationInput {
    /// The ID of the target memory to associate with.
    pub target_id: String,
    /// The type of relation (related_to, updates, contradicts, caused_by, result_of, part_of).
    #[serde(default = "default_relation_type")]
    pub relation_type: String,
    /// The weight of the association (0.0-1.0).
    #[serde(default = "default_weight")]
    pub weight: f32,
}

fn default_relation_type() -> String {
    "related_to".to_string()
}

fn default_weight() -> f32 {
    0.5
}

/// Output from memory save tool.
#[derive(Debug, Serialize)]
pub struct MemorySaveOutput {
    /// The ID of the saved memory.
    pub memory_id: String,
    /// Whether the save was successful.
    pub success: bool,
    /// Optional message about the result.
    pub message: String,
}

impl Tool for MemorySaveTool {
    const NAME: &'static str = "memory_save";

    type Error = MemorySaveError;
    type Args = MemorySaveArgs;
    type Output = MemorySaveOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/memory_save").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The content to save as a memory. Be concise but complete."
                    },
                    "memory_type": {
                        "type": "string",
                        "enum": crate::memory::types::MemoryType::ALL
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>(),
                        "description": "The type of memory being saved. Choose the most appropriate type."
                    },
                    "importance": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 1.0,
                        "description": "Optional importance score from 0.0 to 1.0. Higher values are recalled more easily."
                    },
                    "source": {
                        "type": "string",
                        "description": "Optional source of the information (e.g., 'user stated', 'inferred', 'system')"
                    },
                    "channel_id": {
                        "type": "string",
                        "description": "Optional channel ID to associate this memory with the conversation it came from"
                    },
                    "associations": {
                        "type": "array",
                        "description": "Optional associations to link this memory to other memories",
                        "items": {
                            "type": "object",
                            "properties": {
                                "target_id": {
                                    "type": "string",
                                    "description": "The ID of the memory to associate with"
                                },
                                "relation_type": {
                                    "type": "string",
                                    "enum": ["related_to", "updates", "contradicts", "caused_by", "result_of", "part_of"],
                                    "description": "The type of relationship"
                                },
                                "weight": {
                                    "type": "number",
                                    "minimum": 0.0,
                                    "maximum": 1.0,
                                    "description": "Strength of the association (0.0-1.0)"
                                }
                            },
                            "required": ["target_id"]
                        }
                    }
                },
                "required": ["content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        // Parse memory type
        let memory_type = match args.memory_type.as_str() {
            "fact" => MemoryType::Fact,
            "preference" => MemoryType::Preference,
            "decision" => MemoryType::Decision,
            "identity" => MemoryType::Identity,
            "event" => MemoryType::Event,
            "observation" => MemoryType::Observation,
            "goal" => MemoryType::Goal,
            "todo" => MemoryType::Todo,
            _ => MemoryType::Fact,
        };

        let mut memory = Memory::new(&args.content, memory_type);

        if let Some(importance) = args.importance {
            memory = memory.with_importance(importance);
        }

        if let Some(source) = args.source {
            memory = memory.with_source(source);
        }

        if let Some(channel_id) = args.channel_id {
            memory = memory.with_channel_id(Arc::from(channel_id.as_str()));
        }

        // Save to SQLite database
        let store = self.memory_search.store();
        store
            .save(&memory)
            .await
            .map_err(|e| MemorySaveError(format!("Failed to save memory: {e}")))?;

        // Create associations
        for assoc in args.associations {
            let relation_type = match assoc.relation_type.as_str() {
                "related_to" => crate::memory::types::RelationType::RelatedTo,
                "updates" => crate::memory::types::RelationType::Updates,
                "contradicts" => crate::memory::types::RelationType::Contradicts,
                "caused_by" => crate::memory::types::RelationType::CausedBy,
                "result_of" => crate::memory::types::RelationType::ResultOf,
                "part_of" => crate::memory::types::RelationType::PartOf,
                _ => crate::memory::types::RelationType::RelatedTo,
            };

            let association = Association::new(&memory.id, &assoc.target_id, relation_type)
                .with_weight(assoc.weight);

            if let Err(error) = store.create_association(&association).await {
                tracing::warn!(
                    memory_id = %memory.id,
                    target_id = %assoc.target_id,
                    %error,
                    "failed to create memory association"
                );
            }
        }

        // Generate and store embedding (async to avoid blocking the tokio runtime)
        let embedding = self
            .memory_search
            .embedding_model_arc()
            .embed_one(&args.content)
            .await
            .map_err(|e| MemorySaveError(format!("Failed to generate embedding: {e}")))?;

        self.memory_search
            .embedding_table()
            .store(&memory.id, &args.content, &embedding)
            .await
            .map_err(|e| MemorySaveError(format!("Failed to store embedding: {e}")))?;

        // Ensure the FTS index exists so full_text_search queries work.
        // Safe to call repeatedly — no-ops if the index already exists.
        if let Err(error) = self
            .memory_search
            .embedding_table()
            .ensure_fts_index()
            .await
        {
            tracing::warn!(%error, "failed to ensure FTS index after memory save");
        }

        Ok(MemorySaveOutput {
            memory_id: memory.id,
            success: true,
            message: "Memory saved successfully".to_string(),
        })
    }
}

/// Convenience function for simple fact saving.
pub async fn save_fact(
    memory_search: Arc<MemorySearch>,
    content: impl Into<String>,
    channel_id: Option<crate::ChannelId>,
) -> Result<String> {
    let tool = MemorySaveTool::new(memory_search);
    let args = MemorySaveArgs {
        content: content.into(),
        memory_type: "fact".to_string(),
        importance: None,
        source: None,
        channel_id: channel_id.map(|id| id.to_string()),
        associations: vec![],
    };

    let output = tool
        .call(args)
        .await
        .map_err(|e| crate::error::AgentError::Other(anyhow::anyhow!(e)))?;
    Ok(output.memory_id)
}
