//! Factory tool: update operational configuration for an existing agent.
//!
//! Covers model routing and tuning parameters. Delegates to the same config
//! update logic as the API handler, writing to config.toml and hot-reloading
//! via RuntimeConfig.

use crate::api::ApiState;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// Tool that updates operational config (routing, tuning) for an existing agent.
#[derive(Clone)]
pub struct FactoryUpdateConfigTool {
    state: Arc<ApiState>,
}

impl std::fmt::Debug for FactoryUpdateConfigTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactoryUpdateConfigTool")
            .finish_non_exhaustive()
    }
}

impl FactoryUpdateConfigTool {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }
}

/// Error type for the factory update config tool.
#[derive(Debug, thiserror::Error)]
#[error("Factory update config failed: {0}")]
pub struct FactoryUpdateConfigError(String);

/// Model routing overrides.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RoutingOverrides {
    /// Model for channel (user-facing) processes.
    #[serde(default)]
    pub channel: Option<String>,
    /// Model for branch (thinking) processes.
    #[serde(default)]
    pub branch: Option<String>,
    /// Model for worker (task execution) processes.
    #[serde(default)]
    pub worker: Option<String>,
    /// Model for compactor processes.
    #[serde(default)]
    pub compactor: Option<String>,
    /// Model for cortex (system observation) processes.
    #[serde(default)]
    pub cortex: Option<String>,
}

/// Tuning parameter overrides.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TuningOverrides {
    /// Maximum concurrent branches.
    #[serde(default)]
    pub max_concurrent_branches: Option<usize>,
    /// Maximum concurrent workers.
    #[serde(default)]
    pub max_concurrent_workers: Option<usize>,
    /// Maximum turns for channel processes.
    #[serde(default)]
    pub max_turns: Option<usize>,
    /// Maximum turns for branch processes.
    #[serde(default)]
    pub branch_max_turns: Option<usize>,
    /// Context window size in tokens.
    #[serde(default)]
    pub context_window: Option<usize>,
}

/// Arguments for factory update config tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactoryUpdateConfigArgs {
    /// The agent ID to update.
    pub agent_id: String,
    /// Model routing overrides (which models each process type uses).
    #[serde(default)]
    pub routing: Option<RoutingOverrides>,
    /// Tuning parameter overrides.
    #[serde(default)]
    pub tuning: Option<TuningOverrides>,
}

/// Output from factory update config tool.
#[derive(Debug, Serialize)]
pub struct FactoryUpdateConfigOutput {
    pub agent_id: String,
    pub sections_updated: Vec<String>,
    pub message: String,
}

impl Tool for FactoryUpdateConfigTool {
    const NAME: &'static str = "factory_update_config";

    type Error = FactoryUpdateConfigError;
    type Args = FactoryUpdateConfigArgs;
    type Output = FactoryUpdateConfigOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/factory_update_config").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["agent_id"],
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The agent ID to update."
                    },
                    "routing": {
                        "type": "object",
                        "description": "Model routing overrides.",
                        "properties": {
                            "channel": {
                                "type": "string",
                                "description": "Model for channel (user-facing) processes."
                            },
                            "branch": {
                                "type": "string",
                                "description": "Model for branch (thinking) processes."
                            },
                            "worker": {
                                "type": "string",
                                "description": "Model for worker (task execution) processes."
                            },
                            "compactor": {
                                "type": "string",
                                "description": "Model for compactor processes."
                            },
                            "cortex": {
                                "type": "string",
                                "description": "Model for cortex (system observation) processes."
                            }
                        }
                    },
                    "tuning": {
                        "type": "object",
                        "description": "Tuning parameter overrides.",
                        "properties": {
                            "max_concurrent_branches": {
                                "type": "integer",
                                "description": "Maximum concurrent branches."
                            },
                            "max_concurrent_workers": {
                                "type": "integer",
                                "description": "Maximum concurrent workers."
                            },
                            "max_turns": {
                                "type": "integer",
                                "description": "Maximum turns for channel processes."
                            },
                            "branch_max_turns": {
                                "type": "integer",
                                "description": "Maximum turns for branch processes."
                            },
                            "context_window": {
                                "type": "integer",
                                "description": "Context window size in tokens."
                            }
                        }
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let agent_id = args.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return Err(FactoryUpdateConfigError("agent_id cannot be empty".into()));
        }

        // Validate agent exists
        {
            let existing = self.state.agent_configs.load();
            if !existing.iter().any(|a| a.id == agent_id) {
                return Err(FactoryUpdateConfigError(format!(
                    "agent '{agent_id}' not found"
                )));
            }
        }

        if args.routing.is_none() && args.tuning.is_none() {
            return Err(FactoryUpdateConfigError(
                "at least one of routing or tuning must be provided".into(),
            ));
        }

        // Validate routing model names: check that the provider prefix is known
        if let Some(routing) = &args.routing
            && let Some(ref llm_manager) = *self.state.llm_manager.read().await
        {
            for (label, model) in [
                ("channel", &routing.channel),
                ("branch", &routing.branch),
                ("worker", &routing.worker),
                ("compactor", &routing.compactor),
                ("cortex", &routing.cortex),
            ] {
                if let Some(model_name) = model {
                    let (provider_id, _) =
                        llm_manager.resolve_model(model_name).map_err(|error| {
                            FactoryUpdateConfigError(format!("invalid model for {label}: {error}"))
                        })?;
                    if llm_manager.get_provider(&provider_id).is_err() {
                        return Err(FactoryUpdateConfigError(format!(
                            "provider '{provider_id}' (from model '{model_name}' for {label}) is not configured"
                        )));
                    }
                }
            }
        }

        // Validate tuning value ranges
        if let Some(tuning) = &args.tuning {
            if let Some(value) = tuning.max_concurrent_branches
                && (value == 0 || value > 32)
            {
                return Err(FactoryUpdateConfigError(format!(
                    "max_concurrent_branches must be between 1 and 32 (got {value})"
                )));
            }
            if let Some(value) = tuning.max_concurrent_workers
                && (value == 0 || value > 64)
            {
                return Err(FactoryUpdateConfigError(format!(
                    "max_concurrent_workers must be between 1 and 64 (got {value})"
                )));
            }
            if let Some(value) = tuning.max_turns
                && (value == 0 || value > 100)
            {
                return Err(FactoryUpdateConfigError(format!(
                    "max_turns must be between 1 and 100 (got {value})"
                )));
            }
            if let Some(value) = tuning.branch_max_turns
                && (value == 0 || value > 100)
            {
                return Err(FactoryUpdateConfigError(format!(
                    "branch_max_turns must be between 1 and 100 (got {value})"
                )));
            }
            if let Some(value) = tuning.context_window
                && !(1000..=2_000_000).contains(&value)
            {
                return Err(FactoryUpdateConfigError(format!(
                    "context_window must be between 1000 and 2000000 (got {value})"
                )));
            }
        }

        // Acquire the config write mutex to prevent concurrent read-modify-write races
        let _guard = self.state.config_write_mutex.lock().await;

        // Build the config update using toml_edit (same approach as the API handler)
        let config_path = self.state.config_path.read().await.clone();
        let content = tokio::fs::read_to_string(&config_path)
            .await
            .map_err(|error| {
                FactoryUpdateConfigError(format!("failed to read config.toml: {error}"))
            })?;

        let mut doc: toml_edit::DocumentMut = content.parse().map_err(|error| {
            FactoryUpdateConfigError(format!("failed to parse config.toml: {error}"))
        })?;

        let agent_idx = find_agent_table_index(&doc, &agent_id).ok_or_else(|| {
            FactoryUpdateConfigError(format!("agent '{agent_id}' not found in config.toml"))
        })?;

        let mut sections_updated = Vec::new();

        if let Some(routing) = &args.routing {
            write_routing_to_toml(&mut doc, agent_idx, routing).map_err(|error| {
                FactoryUpdateConfigError(format!("failed to update routing: {error}"))
            })?;
            sections_updated.push("routing".to_string());
        }

        if let Some(tuning) = &args.tuning {
            write_tuning_to_toml(&mut doc, agent_idx, tuning).map_err(|error| {
                FactoryUpdateConfigError(format!("failed to update tuning: {error}"))
            })?;
            sections_updated.push("tuning".to_string());
        }

        // Write the updated config
        tokio::fs::write(&config_path, doc.to_string())
            .await
            .map_err(|error| {
                FactoryUpdateConfigError(format!("failed to write config.toml: {error}"))
            })?;

        // Hot-reload the config into RuntimeConfig
        match crate::config::Config::load_from_path(&config_path) {
            Ok(new_config) => {
                self.state
                    .set_defaults_config(new_config.defaults.clone())
                    .await;

                let runtime_configs = self.state.runtime_configs.load();
                let mcp_managers = self.state.mcp_managers.load();
                if let (Some(runtime_config), Some(mcp_manager)) = (
                    runtime_configs.get(&agent_id).cloned(),
                    mcp_managers.get(&agent_id).cloned(),
                ) {
                    runtime_config
                        .reload_config(&new_config, &agent_id, &mcp_manager)
                        .await;
                }
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "config.toml written but failed to reload immediately"
                );
            }
        }

        let message = format!(
            "Updated {} for agent '{agent_id}'",
            sections_updated.join(", ")
        );

        tracing::info!(
            agent_id = %agent_id,
            sections = ?sections_updated,
            "factory_update_config completed"
        );

        Ok(FactoryUpdateConfigOutput {
            agent_id,
            sections_updated,
            message,
        })
    }
}

/// Find the index of an agent's table in the `[[agents]]` array.
fn find_agent_table_index(doc: &toml_edit::DocumentMut, agent_id: &str) -> Option<usize> {
    let agents = doc.get("agents")?.as_array_of_tables()?;
    agents.iter().position(|table| {
        table
            .get("id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| id == agent_id)
    })
}

/// Write routing overrides into the agent's table in the TOML document.
fn write_routing_to_toml(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    routing: &RoutingOverrides,
) -> Result<(), String> {
    let agents = doc["agents"]
        .as_array_of_tables_mut()
        .ok_or("agents is not an array of tables")?;
    let agent_table = agents
        .get_mut(agent_idx)
        .ok_or("agent index out of bounds")?;

    if agent_table.get("routing").is_none() {
        agent_table["routing"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let routing_table = agent_table["routing"]
        .as_table_mut()
        .ok_or("routing is not a table")?;

    if let Some(channel) = &routing.channel {
        routing_table["channel"] = toml_edit::value(channel.as_str());
    }
    if let Some(branch) = &routing.branch {
        routing_table["branch"] = toml_edit::value(branch.as_str());
    }
    if let Some(worker) = &routing.worker {
        routing_table["worker"] = toml_edit::value(worker.as_str());
    }
    if let Some(compactor) = &routing.compactor {
        routing_table["compactor"] = toml_edit::value(compactor.as_str());
    }
    if let Some(cortex) = &routing.cortex {
        routing_table["cortex"] = toml_edit::value(cortex.as_str());
    }

    Ok(())
}

/// Write tuning overrides into the agent's table in the TOML document.
fn write_tuning_to_toml(
    doc: &mut toml_edit::DocumentMut,
    agent_idx: usize,
    tuning: &TuningOverrides,
) -> Result<(), String> {
    let agents = doc["agents"]
        .as_array_of_tables_mut()
        .ok_or("agents is not an array of tables")?;
    let agent_table = agents
        .get_mut(agent_idx)
        .ok_or("agent index out of bounds")?;

    // Tuning fields are top-level on the agent table, not nested
    if let Some(value) = tuning.max_concurrent_branches {
        agent_table["max_concurrent_branches"] = toml_edit::value(value as i64);
    }
    if let Some(value) = tuning.max_concurrent_workers {
        agent_table["max_concurrent_workers"] = toml_edit::value(value as i64);
    }
    if let Some(value) = tuning.max_turns {
        agent_table["max_turns"] = toml_edit::value(value as i64);
    }
    if let Some(value) = tuning.branch_max_turns {
        agent_table["branch_max_turns"] = toml_edit::value(value as i64);
    }
    if let Some(value) = tuning.context_window {
        agent_table["context_window"] = toml_edit::value(value as i64);
    }

    Ok(())
}
