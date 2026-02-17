//! SpacebotHook: Prompt hook for channels, branches, and workers.

use crate::{AgentId, ChannelId, ProcessEvent, ProcessId, ProcessType};
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};
use tokio::sync::broadcast;

/// Hook for observing agent behavior and sending events.
#[derive(Clone)]
pub struct SpacebotHook {
    agent_id: AgentId,
    process_id: ProcessId,
    process_type: ProcessType,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
}

impl SpacebotHook {
    /// Create a new hook.
    pub fn new(
        agent_id: AgentId,
        process_id: ProcessId,
        process_type: ProcessType,
        channel_id: Option<ChannelId>,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> Self {
        Self {
            agent_id,
            process_id,
            process_type,
            channel_id,
            event_tx,
        }
    }

    /// Send a status update event.
    pub fn send_status(&self, status: impl Into<String>) {
        let event = ProcessEvent::StatusUpdate {
            agent_id: self.agent_id.clone(),
            process_id: self.process_id.clone(),
            status: status.into(),
        };
        let _ = self.event_tx.send(event);
    }

    /// Scan content for potential secret leaks.
    fn scan_for_leaks(&self, content: &str) -> Option<String> {
        use regex::Regex;
        use std::sync::LazyLock;

        static LEAK_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
            vec![
                // OpenAI keys
                Regex::new(r"sk-[a-zA-Z0-9]{20,}").expect("hardcoded regex"),
                // Anthropic keys
                Regex::new(r"sk-ant-[a-zA-Z0-9_-]{20,}").expect("hardcoded regex"),
                // OpenRouter keys
                Regex::new(r"sk-or-[a-zA-Z0-9_-]{20,}").expect("hardcoded regex"),
                // PEM private keys
                Regex::new(r"-----BEGIN.*PRIVATE KEY-----").expect("hardcoded regex"),
                // GitHub personal access tokens
                Regex::new(r"ghp_[a-zA-Z0-9]{36}").expect("hardcoded regex"),
                // Google API keys
                Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("hardcoded regex"),
                // Discord bot tokens (base64 user ID . timestamp . HMAC)
                Regex::new(r"[MN][A-Za-z0-9]{23,}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,}")
                    .expect("hardcoded regex"),
                // Slack bot tokens
                Regex::new(r"xoxb-[0-9]{10,}-[0-9A-Za-z-]+").expect("hardcoded regex"),
                // Slack app tokens
                Regex::new(r"xapp-[0-9]-[A-Z0-9]+-[0-9]+-[a-f0-9]+").expect("hardcoded regex"),
                // Telegram bot tokens
                Regex::new(r"\d{8,}:[A-Za-z0-9_-]{35}").expect("hardcoded regex"),
                // Brave Search API keys
                Regex::new(r"BSA[a-zA-Z0-9]{20,}").expect("hardcoded regex"),
            ]
        });

        for pattern in LEAK_PATTERNS.iter() {
            if let Some(matched) = pattern.find(content) {
                return Some(matched.as_str().to_string());
            }
        }

        None
    }
}

impl<M> PromptHook<M> for SpacebotHook
where
    M: CompletionModel,
{
    async fn on_completion_call(&self, _prompt: &Message, _history: &[Message]) -> HookAction {
        // Log the completion call but don't block it
        tracing::debug!(
            process_id = %self.process_id,
            process_type = %self.process_type,
            "completion call started"
        );

        HookAction::Continue
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        // Tool nudging: check if response has tool calls
        // Note: Rig's CompletionResponse structure varies by model implementation
        // We'll do basic observation here

        tracing::debug!(
            process_id = %self.process_id,
            "completion response received"
        );

        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        // Scan tool arguments for secrets before execution
        if let Some(leak) = self.scan_for_leaks(args) {
            tracing::error!(
                process_id = %self.process_id,
                tool_name = %tool_name,
                leak_prefix = %&leak[..leak.len().min(8)],
                "secret leak detected in tool arguments, blocking call"
            );
            return ToolCallHookAction::Skip {
                reason: "Tool call blocked: arguments contained a secret.".into(),
            };
        }

        // Send event without blocking
        let event = ProcessEvent::ToolStarted {
            agent_id: self.agent_id.clone(),
            process_id: self.process_id.clone(),
            channel_id: self.channel_id.clone(),
            tool_name: tool_name.to_string(),
        };
        let _ = self.event_tx.send(event);

        tracing::debug!(
            process_id = %self.process_id,
            tool_name = %tool_name,
            "tool call started"
        );

        ToolCallHookAction::Continue
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        result: &str,
    ) -> HookAction {
        // Scan for potential leaks in tool output and terminate if found.
        // The result is already in Rig's history at this point, but terminating
        // prevents the agent from forwarding the leaked content to external
        // services via subsequent tool calls.
        if let Some(leak) = self.scan_for_leaks(result) {
            tracing::error!(
                process_id = %self.process_id,
                tool_name = %tool_name,
                leak_prefix = %&leak[..leak.len().min(8)],
                "secret leak detected in tool output, terminating agent"
            );
            return HookAction::Terminate {
                reason: "Tool output contained a secret. Agent terminated to prevent exfiltration."
                    .into(),
            };
        }

        // Cap the result stored in the broadcast event to avoid blowing up
        // event subscribers with multi-MB tool results.
        let capped_result =
            crate::tools::truncate_output(result, crate::tools::MAX_TOOL_OUTPUT_BYTES);
        let event = ProcessEvent::ToolCompleted {
            agent_id: self.agent_id.clone(),
            process_id: self.process_id.clone(),
            channel_id: self.channel_id.clone(),
            tool_name: tool_name.to_string(),
            result: capped_result,
        };
        let _ = self.event_tx.send(event);

        tracing::debug!(
            process_id = %self.process_id,
            tool_name = %tool_name,
            result_bytes = result.len(),
            "tool call completed"
        );

        HookAction::Continue
    }
}
