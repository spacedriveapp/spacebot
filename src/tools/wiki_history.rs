//! wiki_history tool — list version history for a wiki page.

use crate::wiki::{WikiPageVersion, WikiStore};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct WikiHistoryTool {
    wiki_store: Arc<WikiStore>,
}

impl std::fmt::Debug for WikiHistoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WikiHistoryTool").finish()
    }
}

impl WikiHistoryTool {
    pub fn new(wiki_store: Arc<WikiStore>) -> Self {
        Self { wiki_store }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wiki_history failed: {0}")]
pub struct WikiHistoryError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WikiHistoryArgs {
    pub slug: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct WikiHistoryOutput {
    pub slug: String,
    pub versions: Vec<WikiPageVersion>,
}

impl Tool for WikiHistoryTool {
    const NAME: &'static str = "wiki_history";
    type Error = WikiHistoryError;
    type Args = WikiHistoryArgs;
    type Output = WikiHistoryOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/wiki_history").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "limit": { "type": "integer", "description": "Max versions to return (default 20)" }
                },
                "required": ["slug"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let limit = args.limit.unwrap_or(20);
        let versions = self
            .wiki_store
            .history(&args.slug, limit)
            .await
            .map_err(|e| WikiHistoryError(e.to_string()))?;

        Ok(WikiHistoryOutput {
            slug: args.slug,
            versions,
        })
    }
}
