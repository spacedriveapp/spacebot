//! Factory tool: create a fully configured new agent.
//!
//! This tool wraps the same logic as the `POST /api/agents` handler but is
//! callable by the LLM during a factory conversation. It creates the agent
//! entry in config.toml, initializes all subsystems (databases, memory,
//! identity, cron, cortex), writes identity files, creates org links, and
//! starts the agent running.

use crate::api::ApiState;
use crate::links::{AgentLink, LinkDirection, LinkKind};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

/// Tool that creates a fully configured new agent from a complete specification.
#[derive(Clone)]
pub struct FactoryCreateAgentTool {
    state: Arc<ApiState>,
}

impl std::fmt::Debug for FactoryCreateAgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactoryCreateAgentTool")
            .finish_non_exhaustive()
    }
}

impl FactoryCreateAgentTool {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }
}

/// Error type for the factory create agent tool.
#[derive(Debug, thiserror::Error)]
#[error("Factory create agent failed: {0}")]
pub struct FactoryCreateAgentError(String);

/// A link to create alongside the new agent.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkSpec {
    /// The other end of the link (agent ID or human ID).
    pub target: String,
    /// Link direction: "two_way" or "one_way".
    #[serde(default = "default_direction")]
    pub direction: String,
    /// Link kind: "peer" or "hierarchical".
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_direction() -> String {
    "two_way".into()
}

fn default_kind() -> String {
    "peer".into()
}

/// Arguments for factory create agent tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FactoryCreateAgentArgs {
    /// Unique agent ID (lowercase, hyphens ok, no spaces).
    pub agent_id: String,
    /// Human-readable display name for the agent.
    pub display_name: String,
    /// Short role description (shown in topology and status).
    #[serde(default)]
    pub role: Option<String>,
    /// Content for SOUL.md — personality, voice, values, boundaries.
    pub soul_content: String,
    /// Content for IDENTITY.md — what the agent is, what it does, scope.
    pub identity_content: String,
    /// Content for ROLE.md — behavioral rules, delegation, escalation.
    pub role_content: String,
    /// Links to create between the new agent and existing agents/humans.
    #[serde(default)]
    pub links: Vec<LinkSpec>,
}

/// Output from factory create agent tool.
#[derive(Debug, Serialize)]
pub struct FactoryCreateAgentOutput {
    pub success: bool,
    pub agent_id: String,
    pub message: String,
    pub identity_errors: Vec<String>,
    pub links_created: usize,
    pub link_errors: Vec<String>,
}

impl Tool for FactoryCreateAgentTool {
    const NAME: &'static str = "factory_create_agent";

    type Error = FactoryCreateAgentError;
    type Args = FactoryCreateAgentArgs;
    type Output = FactoryCreateAgentOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/factory_create_agent").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["agent_id", "display_name", "soul_content", "identity_content", "role_content"],
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "Unique agent ID (lowercase, hyphens ok, no spaces)."
                    },
                    "display_name": {
                        "type": "string",
                        "description": "Human-readable display name."
                    },
                    "role": {
                        "type": "string",
                        "description": "Short role description for topology/status display."
                    },
                    "soul_content": {
                        "type": "string",
                        "description": "Full markdown content for SOUL.md."
                    },
                    "identity_content": {
                        "type": "string",
                        "description": "Full markdown content for IDENTITY.md."
                    },
                    "role_content": {
                        "type": "string",
                        "description": "Full markdown content for ROLE.md."
                    },
                    "links": {
                        "type": "array",
                        "description": "Links to create between the new agent and existing agents/humans.",
                        "items": {
                            "type": "object",
                            "required": ["target"],
                            "properties": {
                                "target": {
                                    "type": "string",
                                    "description": "Agent ID or human ID to link to."
                                },
                                "direction": {
                                    "type": "string",
                                    "enum": ["two_way", "one_way"],
                                    "default": "two_way",
                                    "description": "Link direction: two_way (both see each other) or one_way (from new agent to target)."
                                },
                                "kind": {
                                    "type": "string",
                                    "enum": ["peer", "hierarchical"],
                                    "default": "peer",
                                    "description": "Relationship type: peer (equals) or hierarchical (one manages the other)."
                                }
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
            return Err(FactoryCreateAgentError("agent_id cannot be empty".into()));
        }
        if let Err(reason) = validate_agent_id(&agent_id) {
            return Err(FactoryCreateAgentError(reason));
        }

        // Validate identity content is non-empty
        for (label, content) in [
            ("soul_content", &args.soul_content),
            ("identity_content", &args.identity_content),
            ("role_content", &args.role_content),
        ] {
            if content.trim().is_empty() {
                return Err(FactoryCreateAgentError(format!(
                    "{label} cannot be empty or whitespace-only"
                )));
            }
        }

        // Validate agent doesn't already exist
        {
            let existing = self.state.agent_configs.load();
            if existing.iter().any(|a| a.id == agent_id) {
                return Err(FactoryCreateAgentError(format!(
                    "agent '{agent_id}' already exists"
                )));
            }
        }

        // Step 1: Create the agent via the same path as the API handler.
        // We build a CreateAgentRequest and call the internal creation logic.
        let create_request = crate::api::agents::CreateAgentRequest {
            agent_id: agent_id.clone(),
            display_name: Some(args.display_name.clone()),
            role: args.role.clone(),
        };

        let create_result = crate::api::agents::create_agent_internal(&self.state, create_request)
            .await
            .map_err(|error| FactoryCreateAgentError(format!("agent creation failed: {error}")))?;

        if !create_result.success {
            return Err(FactoryCreateAgentError(format!(
                "agent creation failed: {}",
                create_result.message
            )));
        }

        // Step 2: Write identity files to the agent's workspace.
        // Files are written best-effort: partial failures are reported but don't
        // abort the entire creation (the agent is already running with scaffold
        // identity files from create_agent_internal).
        let workspaces = self.state.agent_workspaces.load();
        let workspace = workspaces.get(&agent_id).ok_or_else(|| {
            FactoryCreateAgentError(format!(
                "agent '{agent_id}' created but workspace not found in state"
            ))
        })?;

        let mut identity_errors = Vec::new();
        for (filename, content) in [
            ("SOUL.md", args.soul_content.as_str()),
            ("IDENTITY.md", args.identity_content.as_str()),
            ("ROLE.md", args.role_content.as_str()),
        ] {
            let path = workspace.join(filename);
            if let Err(error) = tokio::fs::write(&path, content).await {
                let message = format!("failed to write {filename}: {error}");
                tracing::error!(agent_id = %agent_id, %error, filename, "identity file write failed");
                identity_errors.push(message);
            }
        }

        // Step 3: Reload identity into the runtime config so the agent picks
        // up the custom content immediately.
        let identity = crate::identity::Identity::load(workspace).await;
        if let Some(runtime_config) = self.state.runtime_configs.load().get(&agent_id) {
            runtime_config.identity.store(Arc::new(identity));
        } else {
            tracing::warn!(
                agent_id = %agent_id,
                "runtime config not found for newly created agent — identity reload skipped"
            );
        }

        // Step 4: Create requested links.
        let mut links_created = 0usize;
        let mut link_errors = Vec::new();

        for link_spec in &args.links {
            let direction: LinkDirection = match link_spec.direction.parse() {
                Ok(d) => d,
                Err(_) => {
                    link_errors.push(format!(
                        "invalid direction \"{}\" for link to {}",
                        link_spec.direction, link_spec.target
                    ));
                    continue;
                }
            };
            let kind: LinkKind = match link_spec.kind.parse() {
                Ok(k) => k,
                Err(_) => {
                    link_errors.push(format!(
                        "invalid kind \"{}\" for link to {}",
                        link_spec.kind, link_spec.target
                    ));
                    continue;
                }
            };

            // Validate target exists
            let agent_configs = self.state.agent_configs.load();
            let human_configs = self.state.agent_humans.load();
            let target_exists = agent_configs.iter().any(|a| a.id == link_spec.target)
                || human_configs.iter().any(|h| h.id == link_spec.target);

            if !target_exists {
                link_errors.push(format!(
                    "target \"{}\" not found (not an existing agent or human)",
                    link_spec.target
                ));
                continue;
            }

            // Check for duplicate link
            let existing_links = self.state.agent_links.load();
            let duplicate = existing_links.iter().any(|link| {
                (link.from_agent_id == agent_id && link.to_agent_id == link_spec.target)
                    || (link.from_agent_id == link_spec.target && link.to_agent_id == agent_id)
            });
            if duplicate {
                link_errors.push(format!(
                    "link between '{agent_id}' and '{}' already exists",
                    link_spec.target
                ));
                continue;
            }

            // Write link to config.toml
            if let Err(error) =
                write_link_to_config(&self.state, &agent_id, &link_spec.target, &direction, &kind)
                    .await
            {
                link_errors.push(format!(
                    "failed to write link to {}: {error}",
                    link_spec.target
                ));
                continue;
            }

            // Update in-memory state
            let new_link = AgentLink {
                from_agent_id: agent_id.clone(),
                to_agent_id: link_spec.target.clone(),
                direction,
                kind,
            };
            let mut links = (**self.state.agent_links.load()).clone();
            links.push(new_link);
            self.state.set_agent_links(links);

            links_created += 1;
        }

        tracing::info!(
            agent_id = %agent_id,
            identity_errors = identity_errors.len(),
            links_created,
            link_errors = link_errors.len(),
            "factory_create_agent completed"
        );

        let mut message = create_result.message;
        if !identity_errors.is_empty() {
            message.push_str(&format!(
                " WARNING: {} identity file(s) failed to write — the agent is running with scaffold defaults.",
                identity_errors.len()
            ));
        }

        Ok(FactoryCreateAgentOutput {
            success: true,
            agent_id,
            message,
            identity_errors,
            links_created,
            link_errors,
        })
    }
}

/// Validate that an agent ID is well-formed.
///
/// Rules: 2-64 characters, starts with a letter, lowercase alphanumeric + hyphens
/// only, no leading/trailing/consecutive hyphens.
fn validate_agent_id(id: &str) -> Result<(), String> {
    if id.len() < 2 {
        return Err("agent_id must be at least 2 characters".into());
    }
    if id.len() > 64 {
        return Err("agent_id must be at most 64 characters".into());
    }
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err("agent_id must start with a lowercase letter (a-z)".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("agent_id may only contain lowercase letters, digits, and hyphens".into());
    }
    if id.ends_with('-') {
        return Err("agent_id must not end with a hyphen".into());
    }
    if id.contains("--") {
        return Err("agent_id must not contain consecutive hyphens".into());
    }
    Ok(())
}

/// Write a link entry to config.toml.
///
/// Acquires `config_write_mutex` to prevent concurrent read-modify-write races.
async fn write_link_to_config(
    state: &ApiState,
    from: &str,
    to: &str,
    direction: &LinkDirection,
    kind: &LinkKind,
) -> Result<(), String> {
    let _guard = state.config_write_mutex.lock().await;

    let config_path = state.config_path.read().await.clone();
    let content = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|error| format!("failed to read config.toml: {error}"))?;

    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|error| format!("failed to parse config.toml: {error}"))?;

    if doc.get("links").is_none() {
        doc["links"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let links_array = doc["links"]
        .as_array_of_tables_mut()
        .ok_or_else(|| "links is not an array of tables".to_string())?;

    let mut link_table = toml_edit::Table::new();
    link_table["from"] = toml_edit::value(from);
    link_table["to"] = toml_edit::value(to);
    link_table["direction"] = toml_edit::value(direction.as_str());
    link_table["kind"] = toml_edit::value(kind.as_str());
    links_array.push(link_table);

    tokio::fs::write(&config_path, doc.to_string())
        .await
        .map_err(|error| format!("failed to write config.toml: {error}"))?;

    Ok(())
}
