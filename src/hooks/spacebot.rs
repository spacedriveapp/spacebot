//! SpacebotHook: Prompt hook for channels, branches, and workers.

use crate::{AgentId, ChannelId, ProcessEvent, ProcessId, ProcessType};
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message, Prompt, PromptError};
use tokio::sync::broadcast;

/// Controls whether hook-driven tool nudge retries are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolNudgePolicy {
    Enabled,
    Disabled,
}

impl ToolNudgePolicy {
    /// Default policy by process type.
    pub fn for_process(process_type: ProcessType) -> Self {
        match process_type {
            ProcessType::Worker => Self::Enabled,
            _ => Self::Disabled,
        }
    }

    fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Hook for observing agent behavior and sending events.
#[derive(Clone)]
pub struct SpacebotHook {
    agent_id: AgentId,
    process_id: ProcessId,
    process_type: ProcessType,
    channel_id: Option<ChannelId>,
    event_tx: broadcast::Sender<ProcessEvent>,
    tool_nudge_policy: ToolNudgePolicy,
    completion_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    nudge_request_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set to `true` when the worker calls `set_status` with `kind: "outcome"`.
    /// Once signaled, the nudge system allows text-only responses to pass
    /// through as legitimate completions.
    outcome_signaled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SpacebotHook {
    /// Prompt used to nudge tool-first behavior.
    pub const TOOL_NUDGE_PROMPT: &str = "You have not completed the task yet. Continue working using the available tools. \
         When you have reached a final result, call set_status with kind \"outcome\" \
         before finishing.";
    /// PromptCancelled reason used internally for tool nudge retries.
    pub const TOOL_NUDGE_REASON: &str = "spacebot_tool_nudge_retry";
    /// Maximum nudge retries per prompt request.
    pub const TOOL_NUDGE_MAX_RETRIES: usize = 2;

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
            tool_nudge_policy: ToolNudgePolicy::for_process(process_type),
            completion_calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            nudge_request_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            outcome_signaled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Override the default process-scoped nudge policy.
    pub fn with_tool_nudge_policy(mut self, policy: ToolNudgePolicy) -> Self {
        self.tool_nudge_policy = policy;
        self
    }

    /// Return the current tool nudge policy for this hook.
    pub fn tool_nudge_policy(&self) -> ToolNudgePolicy {
        self.tool_nudge_policy
    }

    /// Reset per-prompt state (tool nudging and outcome tracking).
    pub fn reset_tool_nudge_state(&self) {
        self.completion_calls
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.outcome_signaled
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    fn set_tool_nudge_request_active(&self, active: bool) {
        self.nudge_request_active
            .store(active, std::sync::atomic::Ordering::Relaxed);
    }

    /// Return true if a PromptCancelled reason indicates a tool nudge retry.
    pub fn is_tool_nudge_reason(reason: &str) -> bool {
        reason == Self::TOOL_NUDGE_REASON
    }

    /// Prompt an agent with bounded hook-driven tool nudge retries.
    ///
    /// This keeps hook usage consistent at call sites while preserving
    /// PromptCancelled semantics for non-nudge cancellation reasons.
    pub async fn prompt_with_tool_nudge_retry<M>(
        &self,
        agent: &rig::agent::Agent<M>,
        history: &mut Vec<Message>,
        prompt: &str,
    ) -> std::result::Result<String, PromptError>
    where
        M: CompletionModel,
    {
        self.prompt_with_tool_nudge_retry_with_concurrency(agent, history, prompt, 1)
            .await
    }

    /// Prompt an agent with bounded hook-driven tool nudge retries and request-level
    /// tool concurrency.
    pub async fn prompt_with_tool_nudge_retry_with_concurrency<M>(
        &self,
        agent: &rig::agent::Agent<M>,
        history: &mut Vec<Message>,
        prompt: &str,
        tool_concurrency: usize,
    ) -> std::result::Result<String, PromptError>
    where
        M: CompletionModel,
    {
        self.reset_tool_nudge_state();
        self.set_tool_nudge_request_active(true);

        let mut nudge_attempts = 0usize;
        let mut current_prompt = std::borrow::Cow::Borrowed(prompt);
        let mut using_tool_nudge_prompt = false;

        loop {
            let history_len_before_attempt = history.len();
            let result = if tool_concurrency > 1 {
                agent
                    .prompt(current_prompt.as_ref())
                    .with_history(history)
                    .with_hook(self.clone())
                    .with_tool_concurrency(tool_concurrency)
                    .await
            } else {
                agent
                    .prompt(current_prompt.as_ref())
                    .with_history(history)
                    .with_hook(self.clone())
                    .await
            };

            match &result {
                Err(PromptError::PromptCancelled { reason, .. })
                    if Self::is_tool_nudge_reason(reason)
                        && nudge_attempts < Self::TOOL_NUDGE_MAX_RETRIES =>
                {
                    Self::prune_tool_nudge_retry_history(
                        history,
                        history_len_before_attempt,
                        using_tool_nudge_prompt,
                    );
                    nudge_attempts += 1;
                    tracing::warn!(
                        process_id = %self.process_id,
                        process_type = %self.process_type,
                        attempt = nudge_attempts,
                        "text-only response without outcome signal, nudging tool usage"
                    );
                    current_prompt = std::borrow::Cow::Borrowed(Self::TOOL_NUDGE_PROMPT);
                    using_tool_nudge_prompt = true;
                    continue;
                }
                _ => {
                    if result.is_ok() {
                        Self::prune_successful_tool_nudge_prompt(
                            history,
                            history_len_before_attempt,
                            using_tool_nudge_prompt,
                        );
                    }
                    self.set_tool_nudge_request_active(false);
                    return result;
                }
            }
        }
    }

    fn prune_tool_nudge_retry_history(
        history: &mut Vec<Message>,
        history_len_before_attempt: usize,
        using_tool_nudge_prompt: bool,
    ) {
        if history.len() <= history_len_before_attempt {
            return;
        }

        // Synthetic nudge retries should roll back entirely; only the original
        // task context should persist between attempts.
        if using_tool_nudge_prompt {
            history.truncate(history_len_before_attempt);
            return;
        }

        // First retry should keep the user task prompt added by the failed
        // attempt while removing the failed assistant turn.
        if matches!(
            history.get(history_len_before_attempt),
            Some(Message::User { .. })
        ) {
            history.truncate(history_len_before_attempt.saturating_add(1));
        } else {
            history.truncate(history_len_before_attempt);
        }
    }

    fn prune_successful_tool_nudge_prompt(
        history: &mut Vec<Message>,
        history_len_before_attempt: usize,
        using_tool_nudge_prompt: bool,
    ) {
        if !using_tool_nudge_prompt || history_len_before_attempt >= history.len() {
            return;
        }

        let should_remove_nudge_turn = matches!(
            history.get(history_len_before_attempt),
            Some(Message::User { content })
                if content.iter().any(|item| matches!(
                    item,
                    rig::message::UserContent::Text(text)
                        if text.text.trim() == Self::TOOL_NUDGE_PROMPT
                ))
        );
        if should_remove_nudge_turn {
            history.remove(history_len_before_attempt);
        }
    }

    /// Prompt once with the hook attached and no retry loop.
    pub async fn prompt_once<M>(
        &self,
        agent: &rig::agent::Agent<M>,
        history: &mut Vec<Message>,
        prompt: &str,
    ) -> std::result::Result<String, PromptError>
    where
        M: CompletionModel,
    {
        self.prompt_once_with_tool_concurrency(agent, history, prompt, 1)
            .await
    }

    /// Prompt once with a per-request tool concurrency setting.
    pub async fn prompt_once_with_tool_concurrency<M>(
        &self,
        agent: &rig::agent::Agent<M>,
        history: &mut Vec<Message>,
        prompt: &str,
        tool_concurrency: usize,
    ) -> std::result::Result<String, PromptError>
    where
        M: CompletionModel,
    {
        self.reset_tool_nudge_state();
        self.set_tool_nudge_request_active(false);
        if tool_concurrency > 1 {
            agent
                .prompt(prompt)
                .with_history(history)
                .with_hook(self.clone())
                .with_tool_concurrency(tool_concurrency)
                .await
        } else {
            agent
                .prompt(prompt)
                .with_history(history)
                .with_hook(self.clone())
                .await
        }
    }

    /// Send a status update event.
    pub fn send_status(&self, status: impl Into<String>) {
        let event = ProcessEvent::StatusUpdate {
            agent_id: self.agent_id.clone(),
            process_id: self.process_id.clone(),
            status: status.into(),
        };
        self.event_tx.send(event).ok();
    }

    /// Send a worker idle event. Only valid for worker processes.
    pub fn send_worker_idle(&self) {
        if let ProcessId::Worker(worker_id) = &self.process_id {
            let event = ProcessEvent::WorkerIdle {
                agent_id: self.agent_id.clone(),
                worker_id: *worker_id,
                channel_id: self.channel_id.clone(),
            };
            self.event_tx.send(event).ok();
        }
    }

    /// Scan content for potential secret leaks, including encoded forms.
    ///
    /// Delegates to the shared implementation in `secrets::scrub`.
    fn scan_for_leaks(&self, content: &str) -> Option<String> {
        crate::secrets::scrub::scan_for_leaks(content)
    }

    /// Apply shared safety checks for tool output before any downstream handling.
    ///
    /// For channels, a detected secret terminates the agent immediately to prevent
    /// exfiltration via the `reply` tool. For workers and branches, secrets are
    /// logged but execution continues — these processes cannot communicate with
    /// users directly, and their egress paths (worker results, branch conclusions,
    /// status updates) apply scrubbing before content reaches the channel.
    pub(crate) fn guard_tool_result(&self, tool_name: &str, result: &str) -> HookAction {
        if let Some(leak) = self.scan_for_leaks(result) {
            match self.process_type {
                ProcessType::Worker | ProcessType::Branch => {
                    // Workers and branches cannot communicate with users directly.
                    // Their egress paths (worker results, branch conclusions,
                    // status updates) scrub secrets before content reaches the
                    // channel. Log and continue rather than killing the process.
                    //
                    // Avoid logging any fragment of the matched secret. Only log
                    // the encoding kind (plaintext/url/base64/hex) and length.
                    let kind = if leak.starts_with("url-encoded:") {
                        "url-encoded"
                    } else if leak.starts_with("base64-encoded:") {
                        "base64-encoded"
                    } else if leak.starts_with("hex-encoded:") {
                        "hex-encoded"
                    } else {
                        "plaintext"
                    };
                    tracing::warn!(
                        process_id = %self.process_id,
                        tool_name = %tool_name,
                        leak_kind = kind,
                        leak_len = leak.len(),
                        "secret detected in tool output (non-channel process, continuing)"
                    );
                }
                ProcessType::Channel | ProcessType::Compactor | ProcessType::Cortex => {
                    tracing::error!(
                        process_id = %self.process_id,
                        tool_name = %tool_name,
                        leak_prefix = %&leak[..leak.len().min(8)],
                        "secret leak detected in tool output, terminating agent"
                    );
                    return HookAction::Terminate {
                        reason:
                            "Tool output contained a secret. Agent terminated to prevent exfiltration."
                                .into(),
                    };
                }
            }
        }

        HookAction::Continue
    }

    /// Record metrics for a completed tool call.
    pub(crate) fn record_tool_result_metrics(&self, tool_name: &str, internal_call_id: &str) {
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
                .and_then(|mut timers| timers.remove(internal_call_id))
            {
                metrics
                    .tool_call_duration_seconds
                    .observe(start.elapsed().as_secs_f64());
            }
        }
        #[cfg(not(feature = "metrics"))]
        let _ = (tool_name, internal_call_id);
    }

    pub(crate) fn emit_tool_completed_event(&self, tool_name: &str, result: &str) {
        let capped_result =
            crate::tools::truncate_output(result, crate::tools::MAX_TOOL_OUTPUT_BYTES);
        self.emit_tool_completed_event_from_capped(tool_name, capped_result);
    }

    pub(crate) fn emit_tool_completed_event_from_capped(
        &self,
        tool_name: &str,
        capped_result: String,
    ) {
        let event = ProcessEvent::ToolCompleted {
            agent_id: self.agent_id.clone(),
            process_id: self.process_id.clone(),
            channel_id: self.channel_id.clone(),
            tool_name: tool_name.to_string(),
            result: capped_result,
        };
        self.event_tx.send(event).ok();
    }

    /// Decide whether a text-only response should be rejected and nudged back
    /// into tool usage.
    ///
    /// Workers are not allowed to exit with a text-only response unless they
    /// have first signaled a terminal outcome via `set_status(kind: "outcome")`.
    /// This prevents workers from silently completing with narration instead of
    /// actually doing the work.
    fn should_nudge_tool_usage<M>(&self, response: &CompletionResponse<M::Response>) -> bool
    where
        M: CompletionModel,
    {
        if !self.tool_nudge_policy.is_enabled() {
            return false;
        }
        if !self
            .nudge_request_active
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }

        // If the worker already signaled a terminal outcome, allow text-only
        // responses — the worker is legitimately finishing up.
        if self
            .outcome_signaled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }

        let has_tool_calls = response
            .choice
            .iter()
            .any(|content| matches!(content, rig::message::AssistantContent::ToolCall(_)));
        if has_tool_calls {
            return false;
        }

        // Text-only response without a prior outcome signal — nudge.
        response.choice.iter().any(|content| {
            if let rig::message::AssistantContent::Text(text) = content {
                !text.text.trim().is_empty()
            } else {
                false
            }
        })
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
        if self.tool_nudge_policy.is_enabled() {
            self.completion_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

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
        tracing::debug!(
            process_id = %self.process_id,
            "completion response received"
        );

        if self.should_nudge_tool_usage::<M>(response) {
            return HookAction::Terminate {
                reason: Self::TOOL_NUDGE_REASON.into(),
            };
        }

        HookAction::Continue
    }

    async fn on_text_delta(&self, text_delta: &str, _aggregated_text: &str) -> HookAction {
        if self.process_type == ProcessType::Channel
            && let Some(channel_id) = self.channel_id.clone()
        {
            let scrubbed_text_delta = crate::secrets::scrub::scrub_leaks(text_delta);
            let event = ProcessEvent::TextDelta {
                agent_id: self.agent_id.clone(),
                process_id: self.process_id.clone(),
                channel_id: Some(channel_id),
                text_delta: scrubbed_text_delta.clone(),
                aggregated_text: scrubbed_text_delta,
            };
            self.event_tx.send(event).ok();
        }

        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        // Leak blocking is enforced at channel egress (`reply`). Worker and
        // branch tool calls may legitimately handle secrets internally.
        if self.process_type == ProcessType::Channel
            && tool_name == "reply"
            && let Some(leak) = self.scan_for_leaks(args)
        {
            tracing::error!(
                process_id = %self.process_id,
                tool_name = %tool_name,
                leak_prefix = %&leak[..leak.len().min(8)],
                "secret leak detected in reply arguments, blocking call"
            );
            return ToolCallHookAction::Skip {
                reason: "Reply blocked: content contained a secret.".into(),
            };
        }

        // Send event without blocking. Truncate args to keep broadcast payloads bounded.
        let scrubbed_args = crate::secrets::scrub::scrub_leaks(args);
        let capped_args = crate::tools::truncate_output(&scrubbed_args, 2_000);
        let event = ProcessEvent::ToolStarted {
            agent_id: self.agent_id.clone(),
            process_id: self.process_id.clone(),
            channel_id: self.channel_id.clone(),
            tool_name: tool_name.to_string(),
            args: capped_args,
        };
        self.event_tx.send(event).ok();

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
        internal_call_id: &str,
        _args: &str,
        result: &str,
    ) -> HookAction {
        let guard_action = self.guard_tool_result(tool_name, result);
        if !matches!(guard_action, HookAction::Continue) {
            self.record_tool_result_metrics(tool_name, internal_call_id);
            return guard_action;
        }

        // Belt-and-suspenders check specifically for `reply` tool results on
        // channels. `guard_tool_result` already terminates channels on any tool
        // leak, but this catches any edge case where the reply content itself
        // has a different leak than the raw tool output.
        if self.process_type == ProcessType::Channel
            && tool_name == "reply"
            && let Some(leak) = self.scan_for_leaks(result)
        {
            tracing::error!(
                process_id = %self.process_id,
                tool_name = %tool_name,
                leak_prefix = %&leak[..leak.len().min(8)],
                "secret leak detected in reply result, terminating channel turn"
            );
            self.record_tool_result_metrics(tool_name, internal_call_id);
            return HookAction::Terminate {
                reason: "Reply contained a secret. Channel turn terminated.".into(),
            };
        }

        // Cap the result stored in the broadcast event to avoid blowing up
        // event subscribers with multi-MB tool results. For worker/branch
        // processes, scrub leak patterns from the event payload so secrets
        // don't reach the SSE dashboard.
        if matches!(self.process_type, ProcessType::Worker | ProcessType::Branch) {
            let scrubbed = crate::secrets::scrub::scrub_leaks(result);
            let capped =
                crate::tools::truncate_output(&scrubbed, crate::tools::MAX_TOOL_OUTPUT_BYTES);
            self.emit_tool_completed_event_from_capped(tool_name, capped);
        } else {
            self.emit_tool_completed_event(tool_name, result);
        }

        tracing::debug!(
            process_id = %self.process_id,
            tool_name = %tool_name,
            result_bytes = result.len(),
            "tool call completed"
        );

        self.record_tool_result_metrics(tool_name, internal_call_id);

        // Detect terminal outcome signal from successful set_status results.
        // We check the *result* (not the args) so the flag is only set after the
        // tool actually executed successfully. A failed set_status call won't
        // parse as a valid output with `success: true`.
        if self.tool_nudge_policy.is_enabled()
            && tool_name == "set_status"
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(result)
            && parsed.get("success").and_then(|v| v.as_bool()) == Some(true)
            && parsed.get("kind").and_then(|v| v.as_str()) == Some("outcome")
        {
            self.outcome_signaled
                .store(true, std::sync::atomic::Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::{SpacebotHook, ToolNudgePolicy};
    use crate::ProcessEvent;
    use crate::llm::SpacebotModel;
    use crate::llm::model::RawResponse;
    use crate::{ProcessId, ProcessType};
    use rig::OneOrMany;
    use rig::agent::{HookAction, PromptHook};
    use rig::completion::{CompletionResponse, Message, Usage};
    use rig::message::AssistantContent;

    fn make_hook() -> SpacebotHook {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
        SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Worker(uuid::Uuid::new_v4()),
            ProcessType::Worker,
            None,
            event_tx,
        )
    }

    fn prompt_message() -> Message {
        Message::from("test prompt")
    }

    fn text_response(text: &str) -> CompletionResponse<RawResponse> {
        CompletionResponse {
            choice: OneOrMany::one(AssistantContent::text(text)),
            message_id: None,
            usage: Usage::default(),
            raw_response: RawResponse {
                body: serde_json::json!({}),
            },
        }
    }

    fn tool_call_response() -> CompletionResponse<RawResponse> {
        CompletionResponse {
            choice: OneOrMany::one(AssistantContent::tool_call(
                "call_1",
                "reply",
                serde_json::json!({ "content": "hello" }),
            )),
            message_id: None,
            usage: Usage::default(),
            raw_response: RawResponse {
                body: serde_json::json!({}),
            },
        }
    }

    #[tokio::test]
    async fn nudges_on_every_text_only_response_without_outcome() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // Every text-only response should trigger a nudge when no outcome has
        // been signaled, regardless of how many completions have occurred.
        for i in 1..=5 {
            let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(
                &hook,
                &prompt,
                &[],
            )
            .await;

            let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
                &hook,
                &prompt,
                &text_response(&format!("text-only attempt {i}")),
            )
            .await;
            assert!(
                matches!(
                    response,
                    HookAction::Terminate { ref reason }
                    if reason == SpacebotHook::TOOL_NUDGE_REASON
                ),
                "Expected nudge on text-only response #{i}"
            );
        }
    }

    #[tokio::test]
    async fn does_not_nudge_when_completion_contains_tool_call() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;

        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &tool_call_response(),
        )
        .await;
        assert!(matches!(response, HookAction::Continue));
    }

    #[tokio::test]
    async fn nudges_after_tool_calls_without_outcome() {
        // This is the exact bug case: worker calls read_skill + set_status(progress),
        // then returns text-only. The nudge must still fire.
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // Simulate read_skill tool call
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "read_skill",
            None,
            "internal_1",
            "{\"name\":\"proton-email\"}",
        )
        .await;

        // Simulate set_status(progress) tool call
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "set_status",
            None,
            "internal_2",
            "{\"status\":\"Researching...\"}",
        )
        .await;

        // Now the model returns text-only — must still nudge
        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Let me create the email now..."),
        )
        .await;
        assert!(
            matches!(
                response,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::TOOL_NUDGE_REASON
            ),
            "Expected nudge after tool calls without outcome signal"
        );
    }

    #[tokio::test]
    async fn outcome_signal_allows_text_only_completion() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // Simulate some work
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "shell",
            None,
            "internal_1",
            "{\"command\":\"echo hello\"}",
        )
        .await;

        // Signal outcome via set_status tool call + successful result
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "set_status",
            None,
            "internal_2",
            "{\"status\":\"Email sent successfully\",\"kind\":\"outcome\"}",
        )
        .await;
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "set_status",
            None,
            "internal_2",
            "{\"status\":\"Email sent successfully\",\"kind\":\"outcome\"}",
            "{\"success\":true,\"worker_id\":1,\"status\":\"Email sent successfully\",\"kind\":\"outcome\"}",
        )
        .await;

        // Now text-only response should be allowed
        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Email sent to jamie@spacedrive.com"),
        )
        .await;
        assert!(
            matches!(response, HookAction::Continue),
            "Expected text-only to pass through after outcome signal"
        );
    }

    #[tokio::test]
    async fn progress_status_does_not_signal_outcome() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // set_status with kind "progress" should NOT signal outcome, even after
        // successful execution.
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "set_status",
            None,
            "internal_1",
            "{\"status\":\"Working on it...\",\"kind\":\"progress\"}",
        )
        .await;
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "set_status",
            None,
            "internal_1",
            "{\"status\":\"Working on it...\",\"kind\":\"progress\"}",
            "{\"success\":true,\"worker_id\":1,\"status\":\"Working on it...\",\"kind\":\"progress\"}",
        )
        .await;

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("I'll help with that"),
        )
        .await;
        assert!(
            matches!(
                response,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::TOOL_NUDGE_REASON
            ),
            "Expected nudge — progress status is not an outcome signal"
        );
    }

    #[tokio::test]
    async fn default_status_kind_does_not_signal_outcome() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // set_status without kind field — defaults to progress. Even after
        // successful execution, this should NOT signal outcome.
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "set_status",
            None,
            "internal_1",
            "{\"status\":\"Working on it...\"}",
        )
        .await;
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "set_status",
            None,
            "internal_1",
            "{\"status\":\"Working on it...\"}",
            "{\"success\":true,\"worker_id\":1,\"status\":\"Working on it...\",\"kind\":\"progress\"}",
        )
        .await;

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("done"),
        )
        .await;
        assert!(
            matches!(
                response,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::TOOL_NUDGE_REASON
            ),
            "Expected nudge — status without kind is not an outcome signal"
        );
    }

    #[tokio::test]
    async fn failed_set_status_does_not_signal_outcome() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // Simulate a set_status call with outcome kind that fails. Rig passes
        // the error string as the result, which won't parse as valid JSON with
        // success: true.
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "set_status",
            None,
            "internal_1",
            "{\"status\":\"Task complete\",\"kind\":\"outcome\"}",
        )
        .await;
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "set_status",
            None,
            "internal_1",
            "{\"status\":\"Task complete\",\"kind\":\"outcome\"}",
            "Failed to set status: worker not found",
        )
        .await;

        // Text-only response should still be nudged because the outcome tool
        // call failed.
        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("All done!"),
        )
        .await;
        assert!(
            matches!(
                response,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::TOOL_NUDGE_REASON
            ),
            "Expected nudge — failed set_status should not signal outcome"
        );
    }

    #[tokio::test]
    async fn process_scoped_policy_disables_nudge_for_branch() {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Branch(uuid::Uuid::new_v4()),
            ProcessType::Branch,
            None,
            event_tx,
        );
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("text-only branch response"),
        )
        .await;

        assert!(matches!(response, HookAction::Continue));
    }

    #[tokio::test]
    async fn process_scoped_policy_disables_nudge_for_channel() {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Channel(std::sync::Arc::<str>::from("channel")),
            ProcessType::Channel,
            Some(std::sync::Arc::<str>::from("channel")),
            event_tx,
        );
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("text-only channel response"),
        )
        .await;

        assert!(matches!(response, HookAction::Continue));
    }

    #[tokio::test]
    async fn explicit_policy_override_disables_nudge() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Disabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("text-only worker response"),
        )
        .await;

        assert!(matches!(response, HookAction::Continue));
    }

    #[tokio::test]
    async fn process_scoped_policy_enables_nudge_for_worker_by_default() {
        let hook = make_hook();
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("text-only worker response"),
        )
        .await;

        assert!(matches!(
            response,
            HookAction::Terminate { ref reason } if reason == SpacebotHook::TOOL_NUDGE_REASON
        ));
    }

    #[test]
    fn tool_nudge_retry_history_hygiene_prevents_stacked_retry_turns() {
        let mut history = vec![Message::from("original task")];
        let base_len = history.len();

        history.push(Message::from(SpacebotHook::TOOL_NUDGE_PROMPT));
        history.push(Message::from(rig::message::AssistantContent::text(
            "text-only response",
        )));
        SpacebotHook::prune_tool_nudge_retry_history(&mut history, base_len, true);
        assert_eq!(history.len(), base_len);

        history.push(Message::from(SpacebotHook::TOOL_NUDGE_PROMPT));
        history.push(Message::from(rig::message::AssistantContent::text(
            "second text-only response",
        )));
        SpacebotHook::prune_tool_nudge_retry_history(&mut history, base_len, true);
        assert_eq!(history.len(), base_len);
        assert!(matches!(history[0], Message::User { .. }));
    }

    #[test]
    fn first_nudge_retry_prunes_failed_assistant_turn_but_keeps_task_prompt() {
        let mut history = vec![Message::from("prior context")];
        let base_len = history.len();

        history.push(Message::from("current task"));
        history.push(Message::from(rig::message::AssistantContent::text(
            "text-only response",
        )));

        SpacebotHook::prune_tool_nudge_retry_history(&mut history, base_len, false);

        assert_eq!(history.len(), base_len + 1);
        assert!(matches!(history[base_len], Message::User { .. }));
    }

    #[test]
    fn prompt_with_tool_nudge_retry_prunes_nudge_prompt_on_success() {
        let mut history = vec![Message::from("original task")];
        let base_len = history.len();

        history.push(Message::from(SpacebotHook::TOOL_NUDGE_PROMPT));
        history.push(Message::from(rig::message::AssistantContent::text(
            "tool execution completed",
        )));

        SpacebotHook::prune_successful_tool_nudge_prompt(&mut history, base_len, true);

        assert_eq!(history.len(), base_len + 1);
        assert!(matches!(history[base_len], Message::Assistant { .. }));
    }

    #[tokio::test]
    async fn channel_text_delta_emits_process_event() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Channel(std::sync::Arc::<str>::from("channel")),
            ProcessType::Channel,
            Some(std::sync::Arc::<str>::from("channel")),
            event_tx,
        );

        let action =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_text_delta(&hook, "hi", "hi").await;
        assert!(matches!(action, HookAction::Continue));

        let event = event_rx.recv().await.expect("text delta event");
        assert!(matches!(
            event,
            ProcessEvent::TextDelta {
                ref text_delta,
                ref aggregated_text,
                ..
            } if text_delta == "hi" && aggregated_text == "hi"
        ));
    }

    #[tokio::test]
    async fn channel_text_delta_scrubs_secret_patterns() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Channel(std::sync::Arc::<str>::from("channel")),
            ProcessType::Channel,
            Some(std::sync::Arc::<str>::from("channel")),
            event_tx,
        );

        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_text_delta(
            &hook,
            "sk-ant-api03-abc1234567890123456789012345678901234567",
            "token sk-ant-api03-abc1234567890123456789012345678901234567",
        )
        .await;
        assert!(matches!(action, HookAction::Continue));

        let event = event_rx.recv().await.expect("text delta event");
        assert!(matches!(
            event,
            ProcessEvent::TextDelta {
                ref text_delta,
                ref aggregated_text,
                ..
            } if !text_delta.contains("sk-ant-") && !aggregated_text.contains("sk-ant-")
        ));
    }

    #[tokio::test]
    async fn tool_started_event_scrubs_secret_patterns() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Worker(uuid::Uuid::new_v4()),
            ProcessType::Worker,
            None,
            event_tx,
        );

        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "shell",
            None,
            "internal-call",
            r#"{"command":"export API_KEY=sk-ant-api03-abc1234567890123456789012345678901234567"}"#,
        )
        .await;
        assert!(matches!(action, rig::agent::ToolCallHookAction::Continue));

        let event = event_rx.recv().await.expect("tool started event");
        assert!(matches!(
            event,
            ProcessEvent::ToolStarted { ref args, .. } if !args.contains("sk-ant-")
        ));
    }
}
