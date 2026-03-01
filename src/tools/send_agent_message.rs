//! Assign a task to another agent through the communication graph.
//!
//! When called, creates a task in the target agent's task store (skipping
//! `pending_approval` for agent-delegated tasks) and logs a system message
//! in the link channel between the two agents. The calling agent's turn ends
//! immediately — the result will be delivered when the target agent's cortex
//! picks up and completes the task.

use crate::conversation::history::ConversationLogger;
use crate::links::AgentLink;
use crate::tasks::TaskStore;
use crate::tools::SkipFlag;

use arc_swap::ArcSwap;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Tool for delegating tasks to other agents through the agent communication graph.
///
/// Resolves the target agent by ID or name, validates the link exists and permits
/// this direction, creates a task in the target agent's task store, and logs the
/// delegation in the link channel. The calling agent's turn ends after delegation.
#[derive(Clone)]
pub struct SendAgentMessageTool {
    agent_id: crate::AgentId,
    links: Arc<ArcSwap<Vec<AgentLink>>>,
    /// Map of known agent IDs to display names, for resolving targets.
    agent_names: Arc<HashMap<String, String>>,
    /// Cross-agent task store registry for creating tasks on target agents.
    task_store_registry: Arc<ArcSwap<HashMap<String, Arc<TaskStore>>>>,
    /// Per-agent conversation logger for writing link channel audit records.
    conversation_logger: ConversationLogger,
    /// Per-turn skip flag. When set after delegation, the channel turn ends immediately.
    skip_flag: Option<SkipFlag>,
    /// The originating channel (conversation_id) where the user request came from.
    /// Set per-turn so task completion notifications route back to the right place.
    originating_channel: Option<String>,
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
        task_store_registry: Arc<ArcSwap<HashMap<String, Arc<TaskStore>>>>,
        conversation_logger: ConversationLogger,
    ) -> Self {
        Self {
            agent_id,
            links,
            agent_names,
            task_store_registry,
            conversation_logger,
            skip_flag: None,
            originating_channel: None,
        }
    }

    /// Set the per-turn skip flag so the channel turn ends after delegation.
    pub fn with_skip_flag(mut self, flag: SkipFlag) -> Self {
        self.skip_flag = Some(flag);
        self
    }

    /// Set the originating channel for this turn so task completion notifications
    /// route back to the conversation where the user asked for the work.
    pub fn with_originating_channel(mut self, channel_id: String) -> Self {
        self.originating_channel = Some(channel_id);
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
    /// The task to assign. First sentence is used as the task title;
    /// full content becomes the task description.
    pub message: String,
}

/// Output from send_agent_message tool.
#[derive(Debug, Serialize)]
pub struct SendAgentMessageOutput {
    pub success: bool,
    pub target_agent: String,
    pub task_number: Option<i64>,
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
                        "description": "The task to assign. First sentence becomes the title; full content is the description."
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

        let target_display = self
            .agent_names
            .get(receiving_agent_id)
            .cloned()
            .unwrap_or_else(|| receiving_agent_id.to_string());

        // Look up the target agent's task store from the cross-agent registry.
        let registry = self.task_store_registry.load();
        let target_task_store = registry.get(receiving_agent_id).ok_or_else(|| {
            SendAgentMessageError(format!(
                "target agent '{}' has no task store available. It may not be initialized.",
                target_display
            ))
        })?;

        // Extract title from the message: first sentence or first 120 chars.
        let title = extract_task_title(&args.message);

        // Build task metadata with delegation context.
        let metadata = serde_json::json!({
            "delegated_by": sending_agent_id,
            "delegating_agent_id": sending_agent_id,
            "originating_channel": self.originating_channel,
        });

        // Create the task on the target agent's store.
        // Agent-delegated tasks skip pending_approval and go straight to ready.
        let task = target_task_store
            .create(crate::tasks::CreateTaskInput {
                agent_id: receiving_agent_id.to_string(),
                title: title.clone(),
                description: Some(args.message.clone()),
                status: crate::tasks::TaskStatus::Ready,
                priority: crate::tasks::TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata,
                source_memory_id: None,
                created_by: format!("agent:{}", sending_agent_id),
            })
            .await
            .map_err(|error| {
                SendAgentMessageError(format!(
                    "failed to create task on agent '{}': {error}",
                    target_display
                ))
            })?;

        let task_number = task.task_number;

        // Log delegation record in the link channel (system message).
        let sender_display = self
            .agent_names
            .get(sending_agent_id)
            .cloned()
            .unwrap_or_else(|| sending_agent_id.to_string());
        let link_channel_id = link.channel_id_for(sending_agent_id);

        self.conversation_logger.log_system_message(
            &link_channel_id,
            &format!(
                "{sender_display} assigned task #{task_number} to {target_display}: \"{title}\""
            ),
        );

        // Also log to the receiver's side of the link channel.
        let receiver_link_channel_id = link.channel_id_for(receiving_agent_id);
        self.conversation_logger.log_system_message(
            &receiver_link_channel_id,
            &format!(
                "{sender_display} assigned task #{task_number} to {target_display}: \"{title}\""
            ),
        );

        // End the current turn immediately after delegation.
        if let Some(ref flag) = self.skip_flag {
            flag.store(true, Ordering::Relaxed);
        }

        tracing::info!(
            from = %self.agent_id,
            to = %receiving_agent_id,
            task_number,
            "task delegated to target agent"
        );

        Ok(SendAgentMessageOutput {
            success: true,
            target_agent: target_display,
            task_number: Some(task_number),
            message: format!(
                "Task #{task_number} assigned. The target agent's cortex will pick it up and execute it autonomously. \
                 You will be notified when it completes."
            ),
        })
    }
}

/// Extract a task title from the message content.
/// Uses the first sentence (up to first `.`, `!`, or `?`) or truncates at 120 chars.
fn extract_task_title(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or(message);

    // Find the first sentence-ending punctuation
    if let Some(position) = first_line.find(['.', '!', '?']) {
        let title = &first_line[..=position];
        if title.len() <= 120 {
            return title.trim().to_string();
        }
    }

    // Fall back to first 120 chars
    if first_line.len() <= 120 {
        first_line.trim().to_string()
    } else {
        let boundary = first_line.floor_char_boundary(120);
        format!("{}...", first_line[..boundary].trim())
    }
}
