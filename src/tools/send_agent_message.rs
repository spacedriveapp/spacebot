//! Send message to another agent through the communication graph.

use crate::conversation::ChannelStore;
use crate::links::AgentLink;
use crate::messaging::MessagingManager;
use crate::tools::SkipFlag;
use crate::{AgentId, InboundMessage, MessageContent, ProcessEvent};

use arc_swap::ArcSwap;
use chrono::Utc;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
    channel_id: crate::ChannelId,
    links: Arc<ArcSwap<Vec<AgentLink>>>,
    messaging_manager: Arc<MessagingManager>,
    channel_store: ChannelStore,
    event_tx: broadcast::Sender<ProcessEvent>,
    /// Map of known agent IDs to display names, for resolving targets.
    agent_names: Arc<HashMap<String, String>>,
    /// Per-turn skip flag. When set after sending, the channel turn ends immediately
    /// instead of looping back to the LLM (which would burn depth calling skip).
    skip_flag: Option<SkipFlag>,
    /// Source of the originating channel's inbound message (e.g. "webchat", "discord").
    /// Propagated through metadata so conclusion routing uses the correct adapter.
    originating_source: Option<String>,
    /// Channel that originated the current link conversation (if any).
    /// Used for guardrails to prevent re-delegating to the upstream counterparty
    /// instead of concluding back to them.
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: AgentId,
        agent_name: String,
        channel_id: crate::ChannelId,
        links: Arc<ArcSwap<Vec<AgentLink>>>,
        messaging_manager: Arc<MessagingManager>,
        channel_store: ChannelStore,
        event_tx: broadcast::Sender<ProcessEvent>,
        agent_names: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            agent_id,
            agent_name,
            channel_id,
            links,
            messaging_manager,
            channel_store,
            event_tx,
            agent_names,
            skip_flag: None,
            originating_source: None,
            originating_channel: None,
        }
    }
}

impl SendAgentMessageTool {
    /// Set the per-turn skip flag so the channel turn ends after sending.
    pub fn with_skip_flag(mut self, flag: SkipFlag) -> Self {
        self.skip_flag = Some(flag);
        self
    }

    /// Set the originating source (adapter name) for conclusion routing.
    pub fn with_originating_source(mut self, source: String) -> Self {
        self.originating_source = Some(source);
        self
    }

    /// Set the direct originating channel for this turn.
    pub fn with_originating_channel(mut self, channel_id: String) -> Self {
        self.originating_channel = Some(channel_id);
        self
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
    pub channel_id: String,
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

        // In link channels, responding to the current counterparty should use the reply tool
        // so metadata and conclusion routing stay on the same conversation chain.
        if self
            .current_link_counterparty_id()
            .as_deref()
            .is_some_and(|counterparty| counterparty == target_agent_id)
        {
            return Err(SendAgentMessageError(
                "you are already in a direct link conversation with this agent. Use reply to respond in the current link channel. Use send_agent_message to contact a different agent.".to_string(),
            ));
        }

        // In nested link flows, if the target is the upstream counterparty
        // from the parent link channel, the correct action is conclude_link.
        // Re-sending to that agent creates parallel link threads with incorrect
        // originating metadata.
        if self
            .upstream_counterparty_id()
            .as_deref()
            .is_some_and(|counterparty| counterparty == target_agent_id)
        {
            return Err(SendAgentMessageError(
                "this target is the upstream counterparty for this link conversation. Use conclude_link to route the result back up the chain instead of send_agent_message.".to_string(),
            ));
        }

        // Look up the link between sending agent and target
        let links = self.links.load();
        let link = crate::links::find_link_between(&links, &self.agent_id, &target_agent_id)
            .ok_or_else(|| {
                SendAgentMessageError(format!(
                    "no communication link exists between you and agent '{}'.",
                    args.target
                ))
            })?
            .clone();

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

        let target_agent_arc: AgentId = Arc::from(receiving_agent_id.as_str());
        // Each agent gets its own side of the link channel
        let receiver_channel = link.channel_id_for(receiving_agent_id);
        let sender_channel = link.channel_id_for(sending_agent_id);

        // Materialize the sender-side link channel immediately so both sides
        // are visible in their channel lists even before the first reply.
        let sender_channel_metadata: HashMap<String, serde_json::Value> = HashMap::new();
        self.channel_store
            .upsert(&sender_channel, &sender_channel_metadata);

        // Construct the internal message targeting the receiver's link channel.
        // originating_channel is always the current channel — the direct parent
        // of this link conversation. Conclusions route one hop back, not to the root.
        let mut metadata = HashMap::from([
            ("from_agent_id".into(), serde_json::json!(sending_agent_id)),
            ("link_kind".into(), serde_json::json!(link.kind.as_str())),
            ("reply_to_agent".into(), serde_json::json!(sending_agent_id)),
            (
                "reply_to_channel".into(),
                serde_json::json!(&sender_channel),
            ),
            (
                "originating_channel".into(),
                serde_json::json!(self.channel_id.as_ref()),
            ),
            (
                "original_sent_message".into(),
                serde_json::json!(&args.message),
            ),
        ]);
        // Propagate the adapter name from the originating channel so conclusion
        // routing can look up the correct messaging adapter (e.g. "webchat").
        if let Some(source) = &self.originating_source {
            metadata.insert("originating_source".into(), serde_json::json!(source));
        }

        let message = InboundMessage {
            id: uuid::Uuid::new_v4().to_string(),
            source: "internal".into(),
            conversation_id: receiver_channel.clone(),
            sender_id: sending_agent_id.to_string(),
            agent_id: Some(target_agent_arc),
            content: MessageContent::Text(args.message),
            timestamp: Utc::now(),
            metadata,
            formatted_author: Some(format!("[{}]", self.agent_name)),
        };

        // Inject into the messaging pipeline
        self.messaging_manager
            .inject_message(message)
            .await
            .map_err(|error| {
                SendAgentMessageError(format!("failed to deliver message: {error}"))
            })?;

        // End the current turn immediately. Delegating to another agent means
        // this agent is done — without this, the LLM loops calling skip and
        // burns through its depth budget while waiting for a response that
        // arrives asynchronously.
        if let Some(ref flag) = self.skip_flag {
            flag.store(true, Ordering::Relaxed);
        }

        // Emit process event for dashboard visibility
        self.event_tx
            .send(ProcessEvent::AgentMessageSent {
                from_agent_id: self.agent_id.clone(),
                to_agent_id: Arc::from(receiving_agent_id.as_str()),
                link_id: receiver_channel.clone(),
                channel_id: Arc::from(receiver_channel.as_str()),
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
            receiver_channel = %receiver_channel,
            sender_channel = %sender_channel,
            "agent message sent"
        );

        Ok(SendAgentMessageOutput {
            success: true,
            target_agent: target_display,
            channel_id: sender_channel,
        })
    }
}

impl SendAgentMessageTool {
    /// If this tool is running in a link channel, return the peer agent ID.
    fn current_link_counterparty_id(&self) -> Option<String> {
        self.channel_id
            .as_ref()
            .strip_prefix("link:")
            .and_then(|rest| {
                let (self_id, peer_id) = rest.split_once(':')?;
                if self_id == self.agent_id.as_ref() {
                    Some(peer_id.to_string())
                } else {
                    None
                }
            })
    }

    /// If this link conversation was initiated from another link channel,
    /// return that upstream link's counterparty agent ID.
    fn upstream_counterparty_id(&self) -> Option<String> {
        let originating = self.originating_channel.as_deref()?;
        let rest = originating.strip_prefix("link:")?;
        let (self_id, counterparty_id) = rest.split_once(':')?;
        if self_id == self.agent_id.as_ref() {
            Some(counterparty_id.to_string())
        } else {
            None
        }
    }

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
