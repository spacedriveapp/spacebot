//! Send a message to another agent through the communication graph.
//!
//! Currently a stub — validates the link exists and ends the turn.
//! Will be wired into the task system for cross-agent task delegation.

use crate::links::AgentLink;
use crate::tools::SkipFlag;

use arc_swap::ArcSwap;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Tool for sending messages to other agents through the agent communication graph.
///
/// Resolves the target agent by ID or name, validates the link exists and permits
/// messaging in this direction. Currently a stub — will create tasks on the
/// target agent once cross-agent task delegation is implemented.
#[derive(Clone)]
pub struct SendAgentMessageTool {
    agent_id: crate::AgentId,
    links: Arc<ArcSwap<Vec<AgentLink>>>,
    /// Map of known agent IDs to display names, for resolving targets.
    agent_names: Arc<HashMap<String, String>>,
    /// Per-turn skip flag. When set after sending, the channel turn ends immediately.
    skip_flag: Option<SkipFlag>,
}

impl std::fmt::Debug for SendAgentMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendAgentMessageTool")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

impl SendAgentMessageTool {
    pub fn new(
        agent_id: crate::AgentId,
        links: Arc<ArcSwap<Vec<AgentLink>>>,
        agent_names: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            agent_id,
            links,
            agent_names,
            skip_flag: None,
        }
    }

    /// Set the per-turn skip flag so the channel turn ends after sending.
    pub fn with_skip_flag(mut self, flag: SkipFlag) -> Self {
        self.skip_flag = Some(flag);
        self
    }

    /// Resolve an agent target string to an agent ID.
    /// Checks both IDs and display names (case-insensitive).
    fn resolve_agent_id(&self, target: &str) -> Option<String> {
        // Direct ID match
        if self.agent_names.contains_key(target) {
            return Some(target.to_string());
        }

        // Name match (case-insensitive)
        let target_lower = target.to_lowercase();
        for (agent_id, name) in self.agent_names.iter() {
            if name.to_lowercase() == target_lower {
                return Some(agent_id.clone());
            }
        }

        None
    }
}

/// Error type for send_agent_message tool.
#[derive(Debug, thiserror::Error)]
#[error("SendAgentMessage failed: {0}")]
pub struct SendAgentMessageError(String);

/// Arguments for send_agent_message tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendAgentMessageArgs {
    /// Target agent ID or name.
    pub target: String,
    /// The message content to send.
    pub message: String,
}

/// Output from send_agent_message tool.
#[derive(Debug, Serialize)]
pub struct SendAgentMessageOutput {
    pub success: bool,
    pub target_agent: String,
    pub message: String,
}

impl Tool for SendAgentMessageTool {
    const NAME: &'static str = "send_agent_message";

    type Error = SendAgentMessageError;
    type Args = SendAgentMessageArgs;
    type Output = SendAgentMessageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/send_agent_message").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "The target agent's ID or name."
                    },
                    "message": {
                        "type": "string",
                        "description": "The message content to send to the target agent."
                    }
                },
                "required": ["target", "message"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            from = %self.agent_id,
            target = %args.target,
            message_len = args.message.len(),
            "send_agent_message tool called"
        );

        // Resolve target agent ID (could be name or ID)
        let target_agent_id = self.resolve_agent_id(&args.target).ok_or_else(|| {
            SendAgentMessageError(format!(
                "unknown agent '{}'. Check your organization context for available agents.",
                args.target
            ))
        })?;

        // Look up the link between sending agent and target
        let links = self.links.load();
        let link = crate::links::find_link_between(&links, &self.agent_id, &target_agent_id)
            .ok_or_else(|| {
                SendAgentMessageError(format!(
                    "no communication link exists between you and agent '{}'.",
                    args.target
                ))
            })?;

        // Check direction: if the link is one_way, only from_agent can initiate
        let sending_agent_id = self.agent_id.as_ref();
        let is_to_agent = link.to_agent_id == sending_agent_id;

        if link.direction == crate::links::LinkDirection::OneWay && is_to_agent {
            return Err(SendAgentMessageError(format!(
                "the link to agent '{}' is one-way and you cannot initiate messages.",
                args.target
            )));
        }

        let receiving_agent_id = if link.from_agent_id == sending_agent_id {
            &link.to_agent_id
        } else {
            &link.from_agent_id
        };

        // End the current turn immediately after delegation.
        if let Some(ref flag) = self.skip_flag {
            flag.store(true, Ordering::Relaxed);
        }

        let target_display = self
            .agent_names
            .get(receiving_agent_id)
            .cloned()
            .unwrap_or_else(|| receiving_agent_id.to_string());

        tracing::info!(
            from = %self.agent_id,
            to = %receiving_agent_id,
            "agent message validated (task delegation not yet wired)"
        );

        // TODO: Create a task in the target agent's task store instead of
        // injecting a message. This is a stub — the tool validates the link
        // and ends the turn, but doesn't actually deliver anything yet.

        Ok(SendAgentMessageOutput {
            success: true,
            target_agent: target_display,
            message: "Message validated. Cross-agent task delegation not yet implemented."
                .to_string(),
        })
    }
}
