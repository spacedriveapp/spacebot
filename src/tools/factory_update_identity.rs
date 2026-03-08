//! Factory tool: update identity files for an existing agent.
//!
//! Allows refinement of an agent's soul, identity, and role content after
//! creation. Only files provided in the arguments are overwritten; omitted
//! files are left unchanged.

use crate::api::ApiState;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// Tool that updates identity files for an existing agent.
#[derive(Clone)]
pub struct FactoryUpdateIdentityTool {
    state: Arc<ApiState>,
}

impl std::fmt::Debug for FactoryUpdateIdentityTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactoryUpdateIdentityTool")
            .finish_non_exhaustive()
    }
}

impl FactoryUpdateIdentityTool {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }
}

/// Error type for the factory update identity tool.
#[derive(Debug, thiserror::Error)]
#[error("Factory update identity failed: {0}")]
pub struct FactoryUpdateIdentityError(String);

/// Arguments for factory update identity tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactoryUpdateIdentityArgs {
    /// The agent ID to update.
    pub agent_id: String,
    /// New content for SOUL.md (omit to leave unchanged).
    #[serde(default)]
    pub soul_content: Option<String>,
    /// New content for IDENTITY.md (omit to leave unchanged).
    #[serde(default)]
    pub identity_content: Option<String>,
    /// New content for ROLE.md (omit to leave unchanged).
    #[serde(default)]
    pub role_content: Option<String>,
}

/// Output from factory update identity tool.
#[derive(Debug, Serialize)]
pub struct FactoryUpdateIdentityOutput {
    pub agent_id: String,
    pub files_updated: Vec<String>,
    pub message: String,
}

impl Tool for FactoryUpdateIdentityTool {
    const NAME: &'static str = "factory_update_identity";

    type Error = FactoryUpdateIdentityError;
    type Args = FactoryUpdateIdentityArgs;
    type Output = FactoryUpdateIdentityOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/factory_update_identity").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["agent_id"],
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The agent ID to update."
                    },
                    "soul_content": {
                        "type": "string",
                        "description": "New markdown content for SOUL.md. Omit to leave unchanged."
                    },
                    "identity_content": {
                        "type": "string",
                        "description": "New markdown content for IDENTITY.md. Omit to leave unchanged."
                    },
                    "role_content": {
                        "type": "string",
                        "description": "New markdown content for ROLE.md. Omit to leave unchanged."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let agent_id = args.agent_id.trim().to_string();
        if agent_id.is_empty() {
            return Err(FactoryUpdateIdentityError(
                "agent_id cannot be empty".into(),
            ));
        }

        // Validate agent exists and find identity directory (agent root)
        let identity_dirs = self.state.agent_identity_dirs.load();
        let identity_dir = identity_dirs
            .get(&agent_id)
            .ok_or_else(|| FactoryUpdateIdentityError(format!("agent '{agent_id}' not found")))?;

        if args.soul_content.is_none()
            && args.identity_content.is_none()
            && args.role_content.is_none()
        {
            return Err(FactoryUpdateIdentityError(
                "at least one of soul_content, identity_content, or role_content must be provided"
                    .into(),
            ));
        }

        // Validate provided content is non-empty
        for (label, content) in [
            ("soul_content", &args.soul_content),
            ("identity_content", &args.identity_content),
            ("role_content", &args.role_content),
        ] {
            if let Some(content) = content
                && content.trim().is_empty()
            {
                return Err(FactoryUpdateIdentityError(format!(
                    "{label} cannot be empty or whitespace-only"
                )));
            }
        }

        let mut files_updated = Vec::new();

        let updates: [(&str, &Option<String>); 3] = [
            ("SOUL.md", &args.soul_content),
            ("IDENTITY.md", &args.identity_content),
            ("ROLE.md", &args.role_content),
        ];

        for (filename, content) in &updates {
            if let Some(content) = content {
                let path = identity_dir.join(filename);
                tokio::fs::write(&path, content).await.map_err(|error| {
                    FactoryUpdateIdentityError(format!("failed to write {filename}: {error}"))
                })?;
                files_updated.push(filename.to_string());
            }
        }

        // Reload identity into runtime config so the agent picks up changes
        // immediately without requiring a restart.
        let identity = crate::identity::Identity::load(identity_dir).await;
        if let Some(runtime_config) = self.state.runtime_configs.load().get(&agent_id) {
            runtime_config.identity.store(Arc::new(identity));
        } else {
            tracing::warn!(
                agent_id = %agent_id,
                "runtime config not found for agent — identity reload skipped"
            );
        }

        let message = format!(
            "Updated {} for agent '{agent_id}'",
            files_updated.join(", ")
        );

        tracing::info!(
            agent_id = %agent_id,
            files = ?files_updated,
            "factory_update_identity completed"
        );

        Ok(FactoryUpdateIdentityOutput {
            agent_id,
            files_updated,
            message,
        })
    }
}
