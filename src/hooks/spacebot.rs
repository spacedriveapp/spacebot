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

    /// Check a string against the leak pattern set.
    fn match_patterns(content: &str) -> Option<String> {
        use regex::Regex;
        use std::sync::LazyLock;

        static LEAK_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
            vec![
                Regex::new(r"sk-[a-zA-Z0-9]{20,}").expect("hardcoded regex"),
                Regex::new(r"sk-ant-[a-zA-Z0-9_-]{20,}").expect("hardcoded regex"),
                Regex::new(r"sk-or-[a-zA-Z0-9_-]{20,}").expect("hardcoded regex"),
                Regex::new(r"-----BEGIN.*PRIVATE KEY-----").expect("hardcoded regex"),
                Regex::new(r"ghp_[a-zA-Z0-9]{36}").expect("hardcoded regex"),
                Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("hardcoded regex"),
                Regex::new(r"[MN][A-Za-z0-9]{23,}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,}")
                    .expect("hardcoded regex"),
                Regex::new(r"xoxb-[0-9]{10,}-[0-9A-Za-z-]+").expect("hardcoded regex"),
                Regex::new(r"xapp-[0-9]-[A-Z0-9]+-[0-9]+-[a-f0-9]+").expect("hardcoded regex"),
                Regex::new(r"\d{8,}:[A-Za-z0-9_-]{35}").expect("hardcoded regex"),
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

    /// Scan content for potential secret leaks, including encoded forms.
    ///
    /// Checks raw content first, then attempts URL-decoding, base64-decoding,
    /// and hex-decoding to catch secrets that an LLM might encode to evade
    /// plaintext pattern matching.
    fn scan_for_leaks(&self, content: &str) -> Option<String> {
        use base64::Engine;
        use regex::Regex;
        use std::sync::LazyLock;

        if let Some(matched) = Self::match_patterns(content) {
            return Some(matched);
        }

        // Percent-encoded secrets (e.g. sk%2Dant%2D...)
        let url_decoded = urlencoding::decode(content).unwrap_or(std::borrow::Cow::Borrowed(""));
        if url_decoded != content
            && let Some(matched) = Self::match_patterns(&url_decoded)
        {
            return Some(format!("url-encoded: {matched}"));
        }

        // Base64-wrapped secrets. Minimum 24 chars avoids false positives on
        // short alphanumeric strings while catching any encoded API key.
        static BASE64_SEGMENT: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/]{24,}={0,2}").expect("hardcoded regex"));
        for segment in BASE64_SEGMENT.find_iter(content) {
            if let Ok(decoded_bytes) =
                base64::engine::general_purpose::STANDARD.decode(segment.as_str())
                && let Ok(decoded) = std::str::from_utf8(&decoded_bytes)
                && let Some(matched) = Self::match_patterns(decoded)
            {
                return Some(format!("base64-encoded: {matched}"));
            }
            if let Ok(decoded_bytes) =
                base64::engine::general_purpose::URL_SAFE.decode(segment.as_str())
                && let Ok(decoded) = std::str::from_utf8(&decoded_bytes)
                && let Some(matched) = Self::match_patterns(decoded)
            {
                return Some(format!("base64-encoded: {matched}"));
            }
        }

        // Hex-encoded secrets. Minimum 40 hex chars (20 bytes) to reduce
        // false positives while catching any hex-wrapped API key.
        static HEX_SEGMENT: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)(?:0x)?([0-9a-f]{40,})").expect("hardcoded regex"));
        for caps in HEX_SEGMENT.captures_iter(content) {
            let hex_str = caps.get(1).map_or("", |m| m.as_str());
            if let Ok(decoded_bytes) = hex::decode(hex_str)
                && let Ok(decoded) = std::str::from_utf8(&decoded_bytes)
                && let Some(matched) = Self::match_patterns(decoded)
            {
                return Some(format!("hex-encoded: {matched}"));
            }
        }

        None
    }
}

// Timer map for tool call duration measurement. Entries are inserted in
// on_tool_call and removed in on_tool_result. If the agent terminates between
// the two hooks (e.g. leak detection), orphaned entries stay in the map.
// Bounded by concurrent tool calls so not a practical leak.
#[cfg(feature = "metrics")]
static TOOL_CALL_TIMERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

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
        _response: &CompletionResponse<M::Response>,
    ) -> HookAction {
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

        #[cfg(feature = "metrics")]
        if let Ok(mut timers) = TOOL_CALL_TIMERS.lock() {
            timers.insert(_internal_call_id.to_string(), std::time::Instant::now());
        }

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

        #[cfg(feature = "metrics")]
        {
            let metrics = crate::telemetry::Metrics::global();
            metrics
                .tool_calls_total
                .with_label_values(&[&*self.agent_id, tool_name])
                .inc();
            if let Some(start) = TOOL_CALL_TIMERS
                .lock()
                .ok()
                .and_then(|mut timers| timers.remove(_internal_call_id))
            {
                metrics
                    .tool_call_duration_seconds
                    .observe(start.elapsed().as_secs_f64());
            }
        }

        // Channel turns should end immediately after a successful reply tool call.
        // This avoids extra post-reply LLM iterations that add latency, cost, and
        // noisy logs when providers return empty trailing responses.
        if self.process_type == ProcessType::Channel && tool_name == "reply" {
            return HookAction::Terminate {
                reason: "reply delivered".into(),
            };
        }

        HookAction::Continue
    }
}
