//! wiki_edit tool — partial edit on an existing wiki page with tolerant matching.

use crate::wiki::{EditWikiPageInput, WikiStore};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct WikiEditTool {
    wiki_store: Arc<WikiStore>,
    author_type: String,
    author_id: String,
}

impl std::fmt::Debug for WikiEditTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WikiEditTool")
            .field("author_id", &self.author_id)
            .finish()
    }
}

impl WikiEditTool {
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
#[error("wiki_edit failed: {0}")]
pub struct WikiEditError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WikiEditArgs {
    pub slug: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
    #[serde(default)]
    pub edit_summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WikiEditOutput {
    pub slug: String,
    pub version: i64,
    pub message: String,
}

impl Tool for WikiEditTool {
    const NAME: &'static str = "wiki_edit";
    type Error = WikiEditError;
    type Args = WikiEditArgs;
    type Output = WikiEditOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/wiki_edit").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "old_string": { "type": "string", "description": "Text to find and replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" },
                    "edit_summary": { "type": "string" }
                },
                "required": ["slug", "old_string", "new_string"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let page = self
            .wiki_store
            .edit(EditWikiPageInput {
                slug: args.slug,
                old_string: args.old_string,
                new_string: args.new_string,
                replace_all: args.replace_all,
                edit_summary: args.edit_summary,
                author_type: self.author_type.clone(),
                author_id: self.author_id.clone(),
            })
            .await
            .map_err(|e| WikiEditError(e.to_string()))?;

        Ok(WikiEditOutput {
            message: format!("Updated '{}' — now at version {}", page.title, page.version),
            slug: page.slug,
            version: page.version,
        })
    }
}
