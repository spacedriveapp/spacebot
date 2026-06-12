//! wiki_create tool — create a new wiki page.

use crate::wiki::{CreateWikiPageInput, WikiPageType, WikiStore};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct WikiCreateTool {
    wiki_store: Arc<WikiStore>,
    author_type: String,
    author_id: String,
}

impl std::fmt::Debug for WikiCreateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WikiCreateTool")
            .field("author_id", &self.author_id)
            .finish()
    }
}

impl WikiCreateTool {
    pub fn new(
        wiki_store: Arc<WikiStore>,
        author_type: impl Into<String>,
        author_id: impl Into<String>,
    ) -> Self {
        Self {
            wiki_store,
            author_type: author_type.into(),
            author_id: author_id.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wiki_create failed: {0}")]
pub struct WikiCreateError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WikiCreateArgs {
    pub title: String,
    pub page_type: String,
    pub content: String,
    #[serde(default)]
    pub related: Vec<String>,
    #[serde(default)]
    pub edit_summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WikiCreateOutput {
    pub slug: String,
    pub version: i64,
    pub message: String,
}

impl Tool for WikiCreateTool {
    const NAME: &'static str = "wiki_create";
    type Error = WikiCreateError;
    type Args = WikiCreateArgs;
    type Output = WikiCreateOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/wiki_create").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "page_type": {
                        "type": "string",
                        "enum": WikiPageType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>()
                    },
                    "content": { "type": "string", "description": "Full markdown body" },
                    "related": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Slugs of related pages"
                    },
                    "edit_summary": { "type": "string" }
                },
                "required": ["title", "page_type", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let page_type = WikiPageType::parse(&args.page_type)
            .ok_or_else(|| WikiCreateError(format!("invalid page_type: {}", args.page_type)))?;

        let page = self
            .wiki_store
            .create(CreateWikiPageInput {
                title: args.title,
                page_type,
                content: args.content,
                related: args.related,
                author_type: self.author_type.clone(),
                author_id: self.author_id.clone(),
                edit_summary: args.edit_summary,
            })
            .await
            .map_err(|e| WikiCreateError(e.to_string()))?;

        Ok(WikiCreateOutput {
            message: format!("Created wiki page '{}' (slug: {})", page.title, page.slug),
            slug: page.slug,
            version: page.version,
        })
    }
}
