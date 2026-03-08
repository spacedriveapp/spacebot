//! Factory tool: search the main agent's memories for organizational context.

use crate::memory::MemorySearch;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort, curate_results};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// Tool that searches the main agent's memories for context relevant to agent creation.
///
/// This grounds new agents in real organizational knowledge rather than relying
/// solely on generic preset content.
#[derive(Debug, Clone)]
pub struct FactorySearchContextTool {
    memory_search: Arc<MemorySearch>,
}

impl FactorySearchContextTool {
    pub fn new(memory_search: Arc<MemorySearch>) -> Self {
        Self { memory_search }
    }
}

/// Error type for the factory search context tool.
#[derive(Debug, thiserror::Error)]
#[error("Factory search context failed: {0}")]
pub struct FactorySearchContextError(String);

/// Arguments for factory search context tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactorySearchContextArgs {
    /// Search query describing the kind of organizational context needed.
    pub query: String,
    /// Maximum number of results to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Optional memory type filter: "fact", "preference", "decision", "identity", "event", "observation".
    #[serde(default)]
    pub memory_type: Option<String>,
}

fn default_max_results() -> usize {
    10
}

/// Output from factory search context tool.
#[derive(Debug, Serialize)]
pub struct FactorySearchContextOutput {
    pub results: Vec<ContextResult>,
    pub total_found: usize,
    pub summary: String,
}

/// A single memory result relevant to agent creation.
#[derive(Debug, Serialize)]
pub struct ContextResult {
    pub content: String,
    pub memory_type: String,
    pub importance: f32,
    pub relevance_score: f32,
}

fn parse_memory_type(s: &str) -> Result<crate::memory::MemoryType, FactorySearchContextError> {
    use crate::memory::MemoryType;
    match s {
        "fact" => Ok(MemoryType::Fact),
        "preference" => Ok(MemoryType::Preference),
        "decision" => Ok(MemoryType::Decision),
        "identity" => Ok(MemoryType::Identity),
        "event" => Ok(MemoryType::Event),
        "observation" => Ok(MemoryType::Observation),
        "goal" => Ok(MemoryType::Goal),
        "todo" => Ok(MemoryType::Todo),
        other => Err(FactorySearchContextError(format!(
            "unknown memory_type \"{other}\". Valid types: {}",
            crate::memory::MemoryType::ALL
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

impl Tool for FactorySearchContextTool {
    const NAME: &'static str = "factory_search_context";

    type Error = FactorySearchContextError;
    type Args = FactorySearchContextArgs;
    type Output = FactorySearchContextOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/factory_search_context").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for organizational context relevant to the agent being created."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 25,
                        "default": 10,
                        "description": "Maximum number of memory results to return (1-25)."
                    },
                    "memory_type": {
                        "type": "string",
                        "enum": crate::memory::types::MemoryType::ALL
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>(),
                        "description": "Optional filter to a specific memory type."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let query = args.query.trim();
        if query.is_empty() {
            return Err(FactorySearchContextError("query cannot be empty".into()));
        }

        let memory_type = args
            .memory_type
            .as_deref()
            .map(parse_memory_type)
            .transpose()?;

        // Enforce the 1-25 range declared in the JSON schema
        let max_results = args.max_results.clamp(1, 25);

        let config = SearchConfig {
            mode: SearchMode::Hybrid,
            memory_type,
            sort_by: SearchSort::Recent,
            max_results,
            max_results_per_source: max_results * 2,
            ..Default::default()
        };

        let search_results = self
            .memory_search
            .search(query, &config)
            .await
            .map_err(|error| FactorySearchContextError(format!("search failed: {error}")))?;

        let curated = curate_results(&search_results, max_results);
        let total_found = search_results.len();

        let results: Vec<ContextResult> = curated
            .iter()
            .map(|result| ContextResult {
                content: result.memory.content.clone(),
                memory_type: result.memory.memory_type.to_string(),
                importance: result.memory.importance,
                relevance_score: result.score,
            })
            .collect();

        let summary = if results.is_empty() {
            "No relevant organizational context found.".to_string()
        } else {
            let mut output = String::from("## Organizational Context\n\n");
            for (i, result) in results.iter().enumerate() {
                let preview = result.content.lines().next().unwrap_or(&result.content);
                output.push_str(&format!(
                    "{}. [{}] (importance: {:.2})\n   {}\n\n",
                    i + 1,
                    result.memory_type,
                    result.importance,
                    preview
                ));
            }
            output
        };

        tracing::debug!(
            query,
            total_found,
            returned = results.len(),
            "factory_search_context called"
        );

        Ok(FactorySearchContextOutput {
            results,
            total_found,
            summary,
        })
    }
}
