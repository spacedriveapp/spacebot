//! Factory tool: list available preset archetypes.

use crate::factory::presets::{PresetMeta, PresetRegistry};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool that lists all available preset archetypes with metadata.
#[derive(Debug, Clone)]
pub struct FactoryListPresetsTool;

impl Default for FactoryListPresetsTool {
    fn default() -> Self {
        Self
    }
}

impl FactoryListPresetsTool {
    pub fn new() -> Self {
        Self
    }
}

/// Error type for the factory list presets tool.
#[derive(Debug, thiserror::Error)]
#[error("Factory list presets failed: {0}")]
pub struct FactoryListPresetsError(String);

/// Arguments for factory list presets tool (no parameters needed).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactoryListPresetsArgs {}

/// Output from factory list presets tool.
#[derive(Debug, Serialize)]
pub struct FactoryListPresetsOutput {
    pub presets: Vec<PresetSummary>,
    pub total: usize,
}

/// Summary of a preset for display.
#[derive(Debug, Serialize)]
pub struct PresetSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub tags: Vec<String>,
}

impl From<PresetMeta> for PresetSummary {
    fn from(meta: PresetMeta) -> Self {
        Self {
            id: meta.id,
            name: meta.name,
            description: meta.description,
            icon: meta.icon,
            tags: meta.tags,
        }
    }
}

impl Tool for FactoryListPresetsTool {
    const NAME: &'static str = "factory_list_presets";

    type Error = FactoryListPresetsError;
    type Args = FactoryListPresetsArgs;
    type Output = FactoryListPresetsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/factory_list_presets").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let metas = PresetRegistry::list();
        let total = metas.len();
        let presets: Vec<PresetSummary> = metas.into_iter().map(PresetSummary::from).collect();

        tracing::debug!(total, "factory_list_presets called");

        Ok(FactoryListPresetsOutput { presets, total })
    }
}
