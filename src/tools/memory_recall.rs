//! Memory recall tool for branches.

use crate::error::Result;
use crate::memory::MemorySearch;
use crate::memory::search::{SearchConfig, SearchMode, SearchSort, curate_results};
use crate::memory::types::Memory;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// Tool for recalling memories using hybrid search.
#[derive(Debug, Clone)]
pub struct MemoryRecallTool {
    memory_search: Arc<MemorySearch>,
}

impl MemoryRecallTool {
    /// Create a new memory recall tool.
    pub fn new(memory_search: Arc<MemorySearch>) -> Self {
        Self { memory_search }
    }
}

/// Error type for memory recall tool.
#[derive(Debug, thiserror::Error)]
#[error("Memory recall failed: {0}")]
pub struct MemoryRecallError(String);

impl From<crate::error::Error> for MemoryRecallError {
    fn from(e: crate::error::Error) -> Self {
        MemoryRecallError(format!("{e}"))
    }
}

/// Arguments for memory recall tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryRecallArgs {
    /// The search query. Required for hybrid mode, ignored for recent/important/typed modes.
    #[serde(default)]
    pub query: Option<String>,
    /// Maximum number of results to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Optional memory type filter. Required for "typed" mode.
    pub memory_type: Option<String>,
    /// Search mode: "hybrid" (default), "recent", "important", "typed".
    #[serde(default)]
    pub mode: Option<String>,
    /// Sort order for non-hybrid modes: "recent" (default), "importance", "most_accessed".
    #[serde(default)]
    pub sort_by: Option<String>,
}

fn default_max_results() -> usize {
    10
}

fn parse_search_mode(s: &str) -> std::result::Result<SearchMode, MemoryRecallError> {
    match s {
        "hybrid" => Ok(SearchMode::Hybrid),
        "recent" => Ok(SearchMode::Recent),
        "important" => Ok(SearchMode::Important),
        "typed" => Ok(SearchMode::Typed),
        other => Err(MemoryRecallError(format!(
            "unknown mode \"{other}\". Valid modes: hybrid, recent, important, typed"
        ))),
    }
}

fn parse_search_sort(s: &str) -> std::result::Result<SearchSort, MemoryRecallError> {
    match s {
        "recent" => Ok(SearchSort::Recent),
        "importance" => Ok(SearchSort::Importance),
        "most_accessed" => Ok(SearchSort::MostAccessed),
        other => Err(MemoryRecallError(format!(
            "unknown sort \"{other}\". Valid sorts: recent, importance, most_accessed"
        ))),
    }
}

fn parse_memory_type(s: &str) -> std::result::Result<crate::memory::MemoryType, MemoryRecallError> {
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
        other => Err(MemoryRecallError(format!(
            "unknown memory_type \"{other}\". Valid types: {}",
            crate::memory::MemoryType::ALL
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Output from memory recall tool.
#[derive(Debug, Serialize)]
pub struct MemoryRecallOutput {
    /// The memories found by the search.
    pub memories: Vec<MemoryOutput>,
    /// Total number of results found before curation.
    pub total_found: usize,
    /// Formatted summary of the memories.
    pub summary: String,
}

/// Simplified memory output for serialization.
#[derive(Debug, Serialize)]
pub struct MemoryOutput {
    /// The memory ID.
    pub id: String,
    /// The memory content.
    pub content: String,
    /// The memory type.
    pub memory_type: String,
    /// The importance score.
    pub importance: f32,
    /// When the memory was created.
    pub created_at: String,
    /// The relevance score from the search.
    pub relevance_score: f32,
}

impl Tool for MemoryRecallTool {
    const NAME: &'static str = "memory_recall";

    type Error = MemoryRecallError;
    type Args = MemoryRecallArgs;
    type Output = MemoryRecallOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/memory_recall").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for hybrid mode. Required for hybrid, ignored for other modes."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 10,
                        "description": "Maximum number of memories to return (1-50)"
                    },
                    "memory_type": {
                        "type": "string",
                        "enum": crate::memory::types::MemoryType::ALL
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>(),
                        "description": "Filter to a specific memory type. Required for \"typed\" mode, optional filter for other modes."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["hybrid", "recent", "important", "typed"],
                        "default": "hybrid",
                        "description": "Search mode. \"hybrid\": semantic + keyword + graph (needs query). \"recent\": most recent by time. \"important\": highest importance. \"typed\": filter by memory_type."
                    },
                    "sort_by": {
                        "type": "string",
                        "enum": ["recent", "importance", "most_accessed"],
                        "default": "recent",
                        "description": "Sort order for non-hybrid modes. Default: recent."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let mode = match args.mode.as_deref() {
            Some(m) => parse_search_mode(m)?,
            None => SearchMode::Hybrid,
        };

        let sort_by = match args.sort_by.as_deref() {
            Some(s) => parse_search_sort(s)?,
            None => SearchSort::Recent,
        };

        let memory_type = args
            .memory_type
            .as_deref()
            .map(parse_memory_type)
            .transpose()?;

        // Validate mode-specific requirements
        if mode == SearchMode::Hybrid && args.query.as_ref().is_none_or(|q| q.is_empty()) {
            return Err(MemoryRecallError(
                "hybrid mode requires a non-empty query".to_string(),
            ));
        }
        if mode == SearchMode::Typed && memory_type.is_none() {
            return Err(MemoryRecallError(
                "typed mode requires a memory_type filter".to_string(),
            ));
        }

        let config = SearchConfig {
            mode,
            memory_type,
            sort_by,
            max_results: args.max_results,
            max_results_per_source: args.max_results * 2,
            ..Default::default()
        };

        let query = args.query.as_deref().unwrap_or("");
        let search_results = self
            .memory_search
            .search(query, &config)
            .await
            .map_err(|e| MemoryRecallError(format!("Search failed: {e}")))?;

        let curated = curate_results(&search_results, args.max_results);

        let store = self.memory_search.store();
        let mut memories = Vec::new();

        for result in &curated {
            if let Err(error) = store.record_access(&result.memory.id).await {
                tracing::warn!(
                    memory_id = %result.memory.id,
                    %error,
                    "failed to record memory access"
                );
            }

            memories.push(MemoryOutput {
                id: result.memory.id.clone(),
                content: result.memory.content.clone(),
                memory_type: result.memory.memory_type.to_string(),
                importance: result.memory.importance,
                created_at: result.memory.created_at.to_rfc3339(),
                relevance_score: result.score,
            });
        }

        let total_found = search_results.len();
        let summary = format_memories(&memories);

        Ok(MemoryRecallOutput {
            memories,
            total_found,
            summary,
        })
    }
}

/// Format memories for display to an agent.
pub fn format_memories(memories: &[MemoryOutput]) -> String {
    if memories.is_empty() {
        return "No relevant memories found.".to_string();
    }

    let mut output = String::from("## Relevant Memories\n\n");

    for (i, memory) in memories.iter().enumerate() {
        let preview = memory.content.lines().next().unwrap_or(&memory.content);
        output.push_str(&format!(
            "{}. [{}] (importance: {:.2}, relevance: {:.2})\n   {}\n\n",
            i + 1,
            memory.memory_type,
            memory.importance,
            memory.relevance_score,
            preview
        ));
    }

    output
}

/// Legacy convenience function for direct memory recall.
pub async fn memory_recall(
    memory_search: Arc<MemorySearch>,
    query: &str,
    max_results: usize,
) -> Result<Vec<Memory>> {
    let tool = MemoryRecallTool::new(Arc::clone(&memory_search));
    let args = MemoryRecallArgs {
        query: Some(query.to_string()),
        max_results,
        memory_type: None,
        mode: None,
        sort_by: None,
    };

    let output = tool
        .call(args)
        .await
        .map_err(|e| crate::error::AgentError::Other(anyhow::anyhow!(e)))?;

    // Convert back to Memory type for backward compatibility
    let store = memory_search.store();
    let mut memories = Vec::new();

    for mem_out in output.memories {
        if let Ok(Some(memory)) = store.load(&mem_out.id).await {
            memories.push(memory);
        }
    }

    Ok(memories)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_mode_valid() {
        assert_eq!(parse_search_mode("hybrid").unwrap(), SearchMode::Hybrid);
        assert_eq!(parse_search_mode("recent").unwrap(), SearchMode::Recent);
        assert_eq!(
            parse_search_mode("important").unwrap(),
            SearchMode::Important
        );
        assert_eq!(parse_search_mode("typed").unwrap(), SearchMode::Typed);
    }

    #[test]
    fn test_parse_search_mode_invalid() {
        assert!(parse_search_mode("invalid").is_err());
        assert!(parse_search_mode("").is_err());
    }

    #[test]
    fn test_parse_search_sort_valid() {
        assert_eq!(parse_search_sort("recent").unwrap(), SearchSort::Recent);
        assert_eq!(
            parse_search_sort("importance").unwrap(),
            SearchSort::Importance
        );
        assert_eq!(
            parse_search_sort("most_accessed").unwrap(),
            SearchSort::MostAccessed
        );
    }

    #[test]
    fn test_parse_search_sort_invalid() {
        assert!(parse_search_sort("invalid").is_err());
    }

    #[test]
    fn test_parse_memory_type_valid() {
        use crate::memory::MemoryType;
        assert_eq!(parse_memory_type("fact").unwrap(), MemoryType::Fact);
        assert_eq!(parse_memory_type("identity").unwrap(), MemoryType::Identity);
        assert_eq!(parse_memory_type("decision").unwrap(), MemoryType::Decision);
        assert_eq!(parse_memory_type("goal").unwrap(), MemoryType::Goal);
        assert_eq!(parse_memory_type("todo").unwrap(), MemoryType::Todo);
    }

    #[test]
    fn test_parse_memory_type_invalid() {
        assert!(parse_memory_type("invalid").is_err());
    }
}
