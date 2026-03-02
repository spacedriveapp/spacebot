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
    saw_tool_call: std::sync::Arc<std::sync::atomic::AtomicBool>,
    nudge_request_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SpacebotHook {
    /// Prompt used to nudge tool-first behavior.
    pub const TOOL_NUDGE_PROMPT: &str = "Please proceed and use the available tools.";
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
            saw_tool_call: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            nudge_request_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Override the default process-scoped nudge policy.
    pub fn with_tool_nudge_policy(mut self, policy: ToolNudgePolicy) -> Self {
        self.tool_nudge_policy = policy;
        self
    }

    /// Reset per-prompt tool nudging state.
    pub fn reset_tool_nudge_state(&self) {
        self.completion_calls
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.saw_tool_call
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
        self.reset_tool_nudge_state();
        self.set_tool_nudge_request_active(true);

        let mut nudge_attempts = 0usize;
        let mut current_prompt = std::borrow::Cow::Borrowed(prompt);
        let mut using_tool_nudge_prompt = false;

        loop {
            let history_len_before_attempt = history.len();
            let result = agent
                .prompt(current_prompt.as_ref())
                .with_history(history)
                .with_hook(self.clone())
                .await;

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
                        "response lacked tool calls early in the loop, nudging tool usage"
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
        self.reset_tool_nudge_state();
        self.set_tool_nudge_request_active(false);
        agent
            .prompt(prompt)
            .with_history(history)
            .with_hook(self.clone())
            .await
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

    /// Scan content for potential secret leaks, including encoded forms.
    ///
    /// Delegates to the shared implementation in `secrets::scrub`.
    fn scan_for_leaks(&self, content: &str) -> Option<String> {
        crate::secrets::scrub::scan_for_leaks(content)
    }

    /// Apply shared safety checks for tool output before any downstream handling.
    pub(crate) fn guard_tool_result(&self, tool_name: &str, result: &str) -> HookAction {
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

        let completion_calls = self
            .completion_calls
            .load(std::sync::atomic::Ordering::Relaxed);
        if completion_calls == 0 || completion_calls > Self::TOOL_NUDGE_MAX_RETRIES {
            return false;
        }

        if self
            .saw_tool_call
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

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        self.saw_tool_call
            .store(true, std::sync::atomic::Ordering::Relaxed);

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
        let capped_args = crate::tools::truncate_output(args, 2_000);
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

        // Only enforce hard-stop leak blocking on channel egress (`reply`).
        // Worker and branch tool outputs are internal and should not terminate
        // long-running jobs.
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
        // event subscribers with multi-MB tool results.
        self.emit_tool_completed_event(tool_name, result);

        tracing::debug!(
            process_id = %self.process_id,
            tool_name = %tool_name,
            result_bytes = result.len(),
            "tool call completed"
        );

        self.record_tool_result_metrics(tool_name, internal_call_id);

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
    async fn nudges_only_on_first_two_text_only_completion_calls() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let first_call =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        assert!(matches!(first_call, HookAction::Continue));

        let first_response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("I can help with that."),
        )
        .await;
        assert!(matches!(
            first_response,
            HookAction::Terminate { ref reason }
            if reason == SpacebotHook::TOOL_NUDGE_REASON
        ));

        let second_call =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        assert!(matches!(second_call, HookAction::Continue));

        let second_response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Still no tools."),
        )
        .await;
        assert!(matches!(
            second_response,
            HookAction::Terminate { ref reason }
            if reason == SpacebotHook::TOOL_NUDGE_REASON
        ));

        let third_call =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        assert!(matches!(third_call, HookAction::Continue));

        let third_response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Third attempt should pass through."),
        )
        .await;
        assert!(matches!(third_response, HookAction::Continue));
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
    async fn does_not_nudge_after_any_tool_call_has_started() {
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;

        let tool_call_action = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call(
            &hook,
            "reply",
            None,
            "internal",
            "{\"content\":\"hello\"}",
        )
        .await;
        assert!(matches!(
            tool_call_action,
            rig::agent::ToolCallHookAction::Continue
        ));

        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("final answer"),
        )
        .await;
        assert!(matches!(response, HookAction::Continue));
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
}
