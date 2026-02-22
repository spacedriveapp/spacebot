//! Send message to another agent through the communication graph.

use crate::links::LinkStore;
use crate::messaging::MessagingManager;
use crate::{AgentId, InboundMessage, MessageContent, ProcessEvent};

use chrono::Utc;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Tool for sending messages to other agents through the agent communication graph.
///
/// Resolves the target agent by ID or name, validates the link exists and permits
/// messaging in this direction, constructs an `InboundMessage` with source "internal",
/// and delivers via `MessagingManager::inject_message()`.
#[derive(Clone)]
pub struct SendAgentMessageTool {
    agent_id: AgentId,
    agent_name: String,
    link_store: Arc<LinkStore>,
    messaging_manager: Arc<MessagingManager>,
    event_tx: broadcast::Sender<ProcessEvent>,
    /// Map of known agent IDs to display names, for resolving targets.
    agent_names: Arc<HashMap<String, String>>,
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
        agent_id: AgentId,
        agent_name: String,
        link_store: Arc<LinkStore>,
        messaging_manager: Arc<MessagingManager>,
        event_tx: broadcast::Sender<ProcessEvent>,
        agent_names: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            agent_id,
            agent_name,
            link_store,
            messaging_manager,
            event_tx,
            agent_names,
        }
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
    pub link_id: String,
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
        let target_agent_id = self
            .resolve_agent_id(&args.target)
            .ok_or_else(|| {
                SendAgentMessageError(format!(
                    "unknown agent '{}'. Check your organization context for available agents.",
                    args.target
                ))
            })?;

        // Look up the link between sending agent and target
        let link = self
            .link_store
            .get_between(&self.agent_id, &target_agent_id)
            .await
            .map_err(|error| {
                SendAgentMessageError(format!("failed to look up link: {error}"))
            })?
            .ok_or_else(|| {
                SendAgentMessageError(format!(
                    "no communication link exists between you and agent '{}'.",
                    args.target
                ))
            })?;

        if !link.enabled {
            return Err(SendAgentMessageError(format!(
                "the link to agent '{}' is currently disabled.",
                args.target
            )));
        }

        // Check direction: if the link is one_way, only from_agent can initiate
        let sending_agent_id = self.agent_id.as_ref();
        let is_from_agent = link.from_agent_id == sending_agent_id;
        let is_to_agent = link.to_agent_id == sending_agent_id;

        if link.direction == crate::links::LinkDirection::OneWay && is_to_agent {
            return Err(SendAgentMessageError(format!(
                "the link to agent '{}' is one-way and you cannot initiate messages.",
                args.target
            )));
        }

        // Determine the receiving agent and the relationship from sender's perspective
        let receiving_agent_id = if is_from_agent {
            &link.to_agent_id
        } else {
            &link.from_agent_id
        };

        let relationship = if is_from_agent {
            link.relationship
        } else {
            link.relationship.inverse()
        };

        let target_agent_arc: AgentId = Arc::from(receiving_agent_id.as_str());
        let conversation_id = format!("link:{}", link.id);

        // Construct the internal message
        let message = InboundMessage {
            id: uuid::Uuid::new_v4().to_string(),
            source: "internal".into(),
            conversation_id: conversation_id.clone(),
            sender_id: sending_agent_id.to_string(),
            agent_id: Some(target_agent_arc),
            content: MessageContent::Text(args.message),
            timestamp: Utc::now(),
            metadata: HashMap::from([
                ("link_id".into(), serde_json::json!(link.id)),
                ("from_agent_id".into(), serde_json::json!(sending_agent_id)),
                (
                    "relationship".into(),
                    serde_json::json!(relationship.as_str()),
                ),
            ]),
            formatted_author: Some(format!("[{}]", self.agent_name)),
        };

        // Inject into the messaging pipeline
        self.messaging_manager
            .inject_message(message)
            .await
            .map_err(|error| {
                SendAgentMessageError(format!("failed to deliver message: {error}"))
            })?;

        // Emit process event for dashboard visibility
        self.event_tx
            .send(ProcessEvent::AgentMessageSent {
                from_agent_id: self.agent_id.clone(),
                to_agent_id: Arc::from(receiving_agent_id.as_str()),
                link_id: link.id.clone(),
                channel_id: Arc::from(conversation_id.as_str()),
            })
            .ok();

        let target_display = self
            .agent_names
            .get(receiving_agent_id)
            .cloned()
            .unwrap_or_else(|| receiving_agent_id.to_string());

        tracing::info!(
            from = %self.agent_id,
            to = %receiving_agent_id,
            link_id = %link.id,
            "agent message sent"
        );

        Ok(SendAgentMessageOutput {
            success: true,
            target_agent: target_display,
            link_id: link.id,
        })
    }
}

impl SendAgentMessageTool {
    /// Resolve an agent target string to an agent ID.
    /// Checks both IDs and display names.
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
