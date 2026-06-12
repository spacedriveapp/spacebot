//! wiki_search tool — FTS search across all wiki pages.

use crate::wiki::{WikiPageSummary, WikiPageType, WikiStore};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct WikiSearchTool {
    wiki_store: Arc<WikiStore>,
}

impl std::fmt::Debug for WikiSearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WikiSearchTool").finish()
    }
}

impl WikiSearchTool {
    pub fn new(wiki_store: Arc<WikiStore>) -> Self {
        Self { wiki_store }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wiki_search failed: {0}")]
pub struct WikiSearchError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WikiSearchArgs {
    pub query: String,
    #[serde(default)]
    pub page_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WikiSearchOutput {
    pub results: Vec<WikiPageSummary>,
    pub total: usize,
}

impl Tool for WikiSearchTool {
    const NAME: &'static str = "wiki_search";
    type Error = WikiSearchError;
    type Args = WikiSearchArgs;
    type Output = WikiSearchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/wiki_search").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "page_type": {
                        "type": "string",
                        "enum": WikiPageType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
                        "description": "Optional filter"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let page_type = args
            .page_type
            .as_deref()
            .map(|s| {
                WikiPageType::parse(s)
                    .ok_or_else(|| WikiSearchError(format!("invalid page_type: {s}")))
            })
            .transpose()?;

        let results = self
            .wiki_store
            .search(&args.query, page_type)
            .await
            .map_err(|e| WikiSearchError(e.to_string()))?;

        let total = results.len();
        Ok(WikiSearchOutput { results, total })
    }
}
