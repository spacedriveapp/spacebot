//! wiki_list tool — list all wiki pages, optionally filtered by type.

use crate::wiki::{WikiPageSummary, WikiPageType, WikiStore};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct WikiListTool {
    wiki_store: Arc<WikiStore>,
}

impl std::fmt::Debug for WikiListTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WikiListTool").finish()
    }
}

impl WikiListTool {
    pub fn new(wiki_store: Arc<WikiStore>) -> Self {
        Self { wiki_store }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wiki_list failed: {0}")]
pub struct WikiListError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WikiListArgs {
    #[serde(default)]
    pub page_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WikiListOutput {
    pub pages: Vec<WikiPageSummary>,
    pub total: usize,
}

impl Tool for WikiListTool {
    const NAME: &'static str = "wiki_list";
    type Error = WikiListError;
    type Args = WikiListArgs;
    type Output = WikiListOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/wiki_list").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "page_type": {
                        "type": "string",
                        "enum": WikiPageType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
                        "description": "Optional filter"
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let page_type = args
            .page_type
            .as_deref()
            .map(|s| {
                WikiPageType::parse(s)
                    .ok_or_else(|| WikiListError(format!("invalid page_type: {s}")))
            })
            .transpose()?;

        let pages = self
            .wiki_store
            .list(page_type)
            .await
            .map_err(|e| WikiListError(e.to_string()))?;

        let total = pages.len();
        Ok(WikiListOutput { pages, total })
    }
}
