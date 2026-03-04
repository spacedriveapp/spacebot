//! Live runtime configuration inspection for cortex chat.

use crate::config::RuntimeConfig;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Tool for inspecting the current resolved runtime config (redacted).
#[derive(Debug, Clone)]
pub struct ConfigInspectTool {
    agent_id: String,
    runtime_config: Arc<RuntimeConfig>,
}

impl ConfigInspectTool {
    pub fn new(agent_id: impl Into<String>, runtime_config: Arc<RuntimeConfig>) -> Self {
        Self {
            agent_id: agent_id.into(),
            runtime_config,
        }
    }
}

/// Error type for `config_inspect`.
#[derive(Debug, thiserror::Error)]
#[error("config_inspect failed: {0}")]
pub struct ConfigInspectError(String);

/// Arguments for `config_inspect`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigInspectArgs {
    /// Optional section selector. Defaults to "all".
    pub section: Option<String>,
}

/// Output from `config_inspect`.
#[derive(Debug, Serialize)]
pub struct ConfigInspectOutput {
    pub success: bool,
    pub agent_id: String,
    pub generated_at: String,
    pub section: String,
    pub snapshot: serde_json::Value,
}

impl Tool for ConfigInspectTool {
    const NAME: &'static str = "config_inspect";

    type Error = ConfigInspectError;
    type Args = ConfigInspectArgs;
    type Output = ConfigInspectOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/config_inspect").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "section": {
                        "type": "string",
                        "description": "Optional subsection to return. Valid values: all, paths, routing, limits, compaction, memory_persistence, coalesce, ingestion, cortex, warmup, work_readiness, browser, sandbox, opencode, mcp_servers, brave_search, timezones, bulletin, secrets, binary_version, deployment"
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let snapshot =
            crate::self_awareness::runtime_snapshot_value(&self.agent_id, &self.runtime_config);
        let section = args
            .section
            .unwrap_or_else(|| "all".to_string())
            .trim()
            .to_ascii_lowercase();

        let selected = if section == "all" {
            snapshot
        } else {
            select_section(&snapshot, &section)?
        };

        Ok(ConfigInspectOutput {
            success: true,
            agent_id: self.agent_id.clone(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            section,
            snapshot: selected,
        })
    }
}

fn select_section(
    snapshot: &serde_json::Value,
    section: &str,
) -> Result<serde_json::Value, ConfigInspectError> {
    match section {
        "binary_version" => snapshot
            .get("binary_version")
            .cloned()
            .ok_or_else(|| unknown_section_error("binary_version")),
        "deployment" => snapshot
            .get("deployment")
            .cloned()
            .ok_or_else(|| unknown_section_error("deployment")),
        "mcp" => snapshot
            .get("mcp_servers")
            .cloned()
            .ok_or_else(|| unknown_section_error("mcp")),
        other => snapshot
            .get(other)
            .cloned()
            .ok_or_else(|| unknown_section_error(other)),
    }
}

fn unknown_section_error(section: &str) -> ConfigInspectError {
    ConfigInspectError(format!(
        "unknown section '{section}'. valid sections: all, paths, routing, limits, compaction, memory_persistence, coalesce, ingestion, cortex, warmup, work_readiness, browser, sandbox, opencode, mcp_servers (or mcp), brave_search, timezones, bulletin, secrets, binary_version, deployment"
    ))
}
