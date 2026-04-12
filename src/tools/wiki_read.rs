//! wiki_read tool — read a wiki page and resolve inline wiki: links.

use crate::wiki::{WikiStore, extract_wiki_links};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct WikiReadTool {
    wiki_store: Arc<WikiStore>,
}

impl std::fmt::Debug for WikiReadTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WikiReadTool").finish()
    }
}

impl WikiReadTool {
    pub fn new(wiki_store: Arc<WikiStore>) -> Self {
        Self { wiki_store }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wiki_read failed: {0}")]
pub struct WikiReadError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WikiReadArgs {
    pub slug: String,
    #[serde(default)]
    pub version: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ResolvedLink {
    pub slug: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct WikiReadOutput {
    pub slug: String,
    pub title: String,
    pub page_type: String,
    pub content: String,
    pub related: Vec<String>,
    pub version: i64,
    pub updated_at: String,
    pub updated_by: String,
    /// Inline wiki: links resolved to their current titles (one hop, no content).
    pub links: Vec<ResolvedLink>,
}

impl Tool for WikiReadTool {
    const NAME: &'static str = "wiki_read";
    type Error = WikiReadError;
    type Args = WikiReadArgs;
    type Output = WikiReadOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/wiki_read").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "version": { "type": "integer", "description": "Historical version number. Omit for current." }
                },
                "required": ["slug"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let page = self
            .wiki_store
            .read(&args.slug, args.version)
            .await
            .map_err(|e| WikiReadError(e.to_string()))?
            .ok_or_else(|| WikiReadError(format!("wiki page '{}' not found", args.slug)))?;

        // Resolve inline wiki: links (titles only, no content fetch)
        let linked_slugs = extract_wiki_links(&page.content);
        let mut links = Vec::new();
        for slug in linked_slugs {
            if let Ok(Some(linked)) = self.wiki_store.load_by_slug(&slug).await {
                links.push(ResolvedLink {
                    slug: linked.slug,
                    title: linked.title,
                });
            } else {
                // Broken link — include with a marker so the agent notices
                links.push(ResolvedLink {
                    slug: slug.clone(),
                    title: format!("[broken: {slug}]"),
                });
            }
        }

        Ok(WikiReadOutput {
            slug: page.slug,
            title: page.title,
            page_type: page.page_type,
            content: page.content,
            related: page.related,
            version: page.version,
            updated_at: page.updated_at,
            updated_by: page.updated_by,
            links,
        })
    }
}
