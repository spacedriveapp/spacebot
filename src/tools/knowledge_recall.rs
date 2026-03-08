//! Knowledge recall tool for branches.

use crate::knowledge::{
    KnowledgeHit, KnowledgeQuery, KnowledgeRetrievalService, KnowledgeServiceError,
    KnowledgeSourceDescriptor, KnowledgeSourceStatus,
};
use crate::memory::MemorySearch;
use crate::tools::truncate_utf8_ellipsis;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for normalized knowledge retrieval across native and external sources.
#[derive(Debug, Clone)]
pub struct KnowledgeRecallTool {
    service: KnowledgeRetrievalService,
}

impl KnowledgeRecallTool {
    pub fn new(
        memory_search: Arc<MemorySearch>,
        runtime_config: Arc<crate::config::RuntimeConfig>,
    ) -> Self {
        Self {
            service: KnowledgeRetrievalService::new(memory_search, runtime_config),
        }
    }
}

/// Error type for knowledge_recall tool.
#[derive(Debug, thiserror::Error)]
#[error("knowledge_recall failed: {0}")]
pub struct KnowledgeRecallError(String);

impl From<KnowledgeServiceError> for KnowledgeRecallError {
    fn from(error: KnowledgeServiceError) -> Self {
        Self(error.to_string())
    }
}

/// Arguments for knowledge_recall.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct KnowledgeRecallArgs {
    /// The search query. Required.
    pub query: String,
    /// Maximum number of normalized hits to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Optional source list. Defaults to enabled native sources.
    #[serde(default)]
    pub source_ids: Vec<String>,
}

fn default_max_results() -> usize {
    10
}

/// Output for knowledge_recall.
#[derive(Debug, Serialize)]
pub struct KnowledgeRecallOutput {
    pub query: String,
    pub hits: Vec<KnowledgeHit>,
    pub source_statuses: Vec<KnowledgeSourceStatus>,
    pub available_sources: Vec<KnowledgeSourceDescriptor>,
    pub summary: String,
}

impl Tool for KnowledgeRecallTool {
    const NAME: &'static str = "knowledge_recall";

    type Error = KnowledgeRecallError;
    type Args = KnowledgeRecallArgs;
    type Output = KnowledgeRecallOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/knowledge_recall").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to search for across native memory and future external knowledge sources."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "default": 10,
                        "description": "Maximum number of normalized hits to return (1-50)."
                    },
                    "source_ids": {
                        "type": "array",
                        "description": "Optional source IDs to constrain retrieval. Examples: native_memory, qmd, google_workspace_drive.",
                        "items": { "type": "string" }
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(KnowledgeRecallError(
                "query must be a non-empty string".to_string(),
            ));
        }

        let max_results = args.max_results.clamp(1, 50);
        let result = self
            .service
            .retrieve(KnowledgeQuery {
                query: query.clone(),
                max_results,
                source_ids: args.source_ids,
            })
            .await?;
        let available_sources = self.service.registry().sources().to_vec();
        let summary = format_summary(&result.hits, &result.source_statuses);

        Ok(KnowledgeRecallOutput {
            query,
            hits: result.hits,
            source_statuses: result.source_statuses,
            available_sources,
            summary,
        })
    }
}

fn format_summary(hits: &[KnowledgeHit], source_statuses: &[KnowledgeSourceStatus]) -> String {
    let hit_summary = if hits.is_empty() {
        "No knowledge hits found.".to_string()
    } else {
        let labels = hits
            .iter()
            .map(|hit| format!("{} ({})", hit.title, hit.provenance.source_id))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "Found {} knowledge hit(s): {}",
            hits.len(),
            truncate_utf8_ellipsis(&labels, 400)
        )
    };

    if source_statuses.is_empty() {
        return hit_summary;
    }

    let source_summary = source_statuses
        .iter()
        .map(|status| match &status.message {
            Some(message) => format!(
                "{}={} ({} hit(s); {})",
                status.source_id,
                serde_json::to_string(&status.state)
                    .unwrap_or_else(|_| "\"unknown\"".to_string())
                    .trim_matches('"'),
                status.hit_count,
                message
            ),
            None => format!(
                "{}={} ({} hit(s))",
                status.source_id,
                serde_json::to_string(&status.state)
                    .unwrap_or_else(|_| "\"unknown\"".to_string())
                    .trim_matches('"'),
                status.hit_count
            ),
        })
        .collect::<Vec<_>>()
        .join("; ");

    format!("{hit_summary}\nSources: {source_summary}")
}
