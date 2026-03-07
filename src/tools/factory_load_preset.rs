//! Factory tool: load a specific preset archetype by ID.

use crate::factory::presets::{PresetDefaults, PresetRegistry};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tool that loads a full preset including identity file content.
#[derive(Debug, Clone)]
pub struct FactoryLoadPresetTool;

impl Default for FactoryLoadPresetTool {
    fn default() -> Self {
        Self
    }
}

impl FactoryLoadPresetTool {
    pub fn new() -> Self {
        Self
    }
}

/// Error type for the factory load preset tool.
#[derive(Debug, thiserror::Error)]
#[error("Factory load preset failed: {0}")]
pub struct FactoryLoadPresetError(String);

/// Arguments for factory load preset tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactoryLoadPresetArgs {
    /// The preset ID to load (e.g. "community-manager", "engineering-assistant").
    pub preset_id: String,
}

/// Output from factory load preset tool.
#[derive(Debug, Serialize)]
pub struct FactoryLoadPresetOutput {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub tags: Vec<String>,
    pub defaults: PresetDefaults,
    pub soul: String,
    pub identity: String,
    pub role: String,
}

impl Tool for FactoryLoadPresetTool {
    const NAME: &'static str = "factory_load_preset";

    type Error = FactoryLoadPresetError;
    type Args = FactoryLoadPresetArgs;
    type Output = FactoryLoadPresetOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/factory_load_preset").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["preset_id"],
                "properties": {
                    "preset_id": {
                        "type": "string",
                        "description": "The preset archetype ID to load (e.g. \"community-manager\", \"engineering-assistant\")."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let preset_id = args.preset_id.trim();
        if preset_id.is_empty() {
            return Err(FactoryLoadPresetError("preset_id cannot be empty".into()));
        }

        let preset = PresetRegistry::load(preset_id).ok_or_else(|| {
            FactoryLoadPresetError(format!(
                "preset \"{preset_id}\" not found. Use factory_list_presets to see available presets."
            ))
        })?;

        tracing::debug!(preset_id, "factory_load_preset called");

        Ok(FactoryLoadPresetOutput {
            id: preset.meta.id,
            name: preset.meta.name,
            description: preset.meta.description,
            icon: preset.meta.icon,
            tags: preset.meta.tags,
            defaults: preset.meta.defaults,
            soul: preset.soul,
            identity: preset.identity,
            role: preset.role,
        })
    }
}
