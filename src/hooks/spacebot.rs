//! SpacebotHook: Prompt hook for channels, branches, and workers.

use crate::hooks::loop_guard::{LoopGuard, LoopGuardConfig, LoopGuardVerdict};
use crate::tools::{MemoryPersistenceContractState, MemoryPersistenceTerminalOutcome};
use crate::{AgentId, ChannelId, ProcessEvent, ProcessId, ProcessType};
use futures::StreamExt;
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{
    CompletionModel, CompletionResponse, GetTokenUsage, Message, Prompt, PromptError,
};
use rig::message::{AssistantContent, ToolResultContent, UserContent};
use rig::streaming::{StreamedAssistantContent, StreamingCompletion};
use std::sync::Arc;
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
    completion_contract_request_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set to `true` when the worker calls `set_status` with `kind: "outcome"`.
    /// Once signaled, the nudge system allows text-only responses to pass
    /// through as legitimate completions.
    outcome_signaled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Counts consecutive text-only nudge attempts. Reset to zero whenever a
    /// tool call completes successfully, so the budget tracks *consecutive*
    /// text-only responses rather than total across the worker's lifetime.
    nudge_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Detects repetitive tool calling patterns (identical calls, identical
    /// outcomes, ping-pong cycles) and blocks them before execution.
    loop_guard: std::sync::Arc<std::sync::Mutex<LoopGuard>>,
    /// Receiver for context injection messages. When a channel routes addendum
    /// context to a running worker, the messages arrive on this channel and are
    /// drained in `on_completion_call` before each LLM turn.
    inject_rx: Option<std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<String>>>>,
    /// Buffer of injected messages drained from `inject_rx`. The
    /// `prompt_with_tool_nudge_retry` loop reads and clears this buffer to
    /// append the messages to history before re-prompting.
    injected_messages: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    memory_persistence_contract: Option<Arc<MemoryPersistenceContractState>>,
    reply_tool_delta_state:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, ReplyToolDeltaState>>>,
}

#[derive(Clone, Debug, Default)]
struct ReplyToolDeltaState {
    tool_name: Option<String>,
    raw_args: String,
    emitted_content: String,
}

impl SpacebotHook {
    /// Prompt used to nudge tool-first behavior.
    pub const TOOL_NUDGE_PROMPT: &str = "You have not completed the task yet. Continue working using the available tools. \
         When you have reached a final result, call set_status with kind \"outcome\" \
         before finishing.";
    /// PromptCancelled reason used internally for tool nudge retries.
    pub const TOOL_NUDGE_REASON: &str = "spacebot_tool_nudge_retry";
    /// PromptCancelled reason used when injected context is pending.
    pub const CONTEXT_INJECTION_REASON: &str = "spacebot_context_injection";
    /// PromptCancelled reason used for memory-persistence contract retries.
    pub const MEMORY_PERSISTENCE_CONTRACT_REASON: &str =
        "spacebot_memory_persistence_contract_retry";
    /// Maximum nudge retries per prompt request.
    pub const TOOL_NUDGE_MAX_RETRIES: usize = 2;
    /// Maximum completion-contract retries per prompt request.
    pub const MEMORY_PERSISTENCE_CONTRACT_MAX_RETRIES: usize = 2;
    /// Prompt used to nudge memory-persistence branches toward a terminal tool outcome.
    pub const MEMORY_PERSISTENCE_CONTRACT_PROMPT: &str = "You must finish this memory-persistence run by calling memory_persistence_complete. \
         First recall relevant memories, then save real memories if needed, then call \
         memory_persistence_complete with either outcome=\"saved\" and exact saved_memory_ids \
         from successful memory_save calls in this run, or outcome=\"no_memories\" with a short \
         reason and no saved IDs. Do not invent memory IDs.";

    /// Create a new hook.
    pub fn new(
        agent_id: AgentId,
        process_id: ProcessId,
        process_type: ProcessType,
        channel_id: Option<ChannelId>,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> Self {
        let loop_guard_config = LoopGuardConfig::for_process(process_type);
        Self {
            agent_id,
            process_id,
            process_type,
            channel_id,
            event_tx,
            tool_nudge_policy: ToolNudgePolicy::for_process(process_type),
            completion_calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            nudge_request_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            completion_contract_request_active: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            outcome_signaled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            nudge_attempts: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            loop_guard: std::sync::Arc::new(std::sync::Mutex::new(LoopGuard::new(
                loop_guard_config,
            ))),
            inject_rx: None,
            injected_messages: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            memory_persistence_contract: None,
            reply_tool_delta_state: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    /// Override the default process-scoped nudge policy.
    pub fn with_tool_nudge_policy(mut self, policy: ToolNudgePolicy) -> Self {
        self.tool_nudge_policy = policy;
        self
    }

    pub fn with_memory_persistence_contract(
        mut self,
        contract_state: Arc<MemoryPersistenceContractState>,
    ) -> Self {
        self.memory_persistence_contract = Some(contract_state);
        self
    }

    /// Attach a context injection receiver to this hook.
    ///
    /// When set, `on_completion_call` will drain pending messages from the
    /// receiver into `injected_messages` and return `Terminate` so the retry
    /// loop can append them to history.
    pub fn with_inject_rx(mut self, inject_rx: tokio::sync::mpsc::Receiver<String>) -> Self {
        self.inject_rx = Some(std::sync::Arc::new(tokio::sync::Mutex::new(inject_rx)));
        self
    }

    /// Return the current tool nudge policy for this hook.
    pub fn tool_nudge_policy(&self) -> ToolNudgePolicy {
        self.tool_nudge_policy
    }

    /// Returns `true` if the worker has called `set_status` with `kind: "outcome"`.
    pub fn outcome_signaled(&self) -> bool {
        self.outcome_signaled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Reset per-prompt state (tool nudging, outcome tracking, and loop guard).
    pub fn reset_tool_nudge_state(&self) {
        self.completion_calls
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.outcome_signaled
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.nudge_attempts
            .store(0, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut guard) = self.loop_guard.lock() {
            guard.reset();
        }
    }

    fn set_tool_nudge_request_active(&self, active: bool) {
        self.nudge_request_active
            .store(active, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_completion_contract_request_active(&self, active: bool) {
        self.completion_contract_request_active
            .store(active, std::sync::atomic::Ordering::Relaxed);
    }

    fn extract_partial_reply_content(raw_args: &str) -> Option<String> {
        let key_index = raw_args.find("\"content\"")?;
        let after_key = &raw_args[key_index + "\"content\"".len()..];
        let colon_index = after_key.find(':')?;
        let after_colon = &after_key[colon_index + 1..];
        let quote_index = after_colon.find('"')?;
        let content_slice = &after_colon[quote_index + 1..];

        let mut result = String::new();
        let mut chars = content_slice.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => break,
                '\\' => {
                    let Some(escaped) = chars.next() else {
                        break;
                    };
                    match escaped {
                        '"' => result.push('"'),
                        '\\' => result.push('\\'),
                        '/' => result.push('/'),
                        'b' => result.push('\u{0008}'),
                        'f' => result.push('\u{000C}'),
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        'u' => {
                            let hex: String = chars.by_ref().take(4).collect();
                            if hex.len() == 4
                                && let Ok(value) = u32::from_str_radix(&hex, 16)
                                && let Some(decoded) = char::from_u32(value)
                            {
                                result.push(decoded);
                            }
                        }
                        other => result.push(other),
                    }
                }
                other => result.push(other),
            }
        }

        Some(result)
    }

    /// Return true if a PromptCancelled reason indicates a tool nudge retry.
    pub fn is_tool_nudge_reason(reason: &str) -> bool {
        reason == Self::TOOL_NUDGE_REASON
    }

    /// Return true if a PromptCancelled reason indicates context injection.
    pub fn is_context_injection_reason(reason: &str) -> bool {
        reason == Self::CONTEXT_INJECTION_REASON
    }

    pub fn is_memory_persistence_contract_reason(reason: &str) -> bool {
        reason == Self::MEMORY_PERSISTENCE_CONTRACT_REASON
    }

    /// Drain and return all buffered injected messages.
    pub fn take_injected_messages(&self) -> Vec<String> {
        self.injected_messages
            .lock()
            .map(|mut messages| std::mem::take(&mut *messages))
            .unwrap_or_default()
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
        self.set_completion_contract_request_active(false);

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
                // Context injection: the hook detected pending injected
                // messages and terminated the agent loop. Drain the buffer,
                // append each message to history as a User message, and
                // re-prompt with a continuation hint. This does NOT count
                // against the nudge attempt budget.
                Err(PromptError::PromptCancelled { reason, .. })
                    if Self::is_context_injection_reason(reason) =>
                {
                    let injected = self.take_injected_messages();
                    if injected.is_empty() {
                        // Shouldn't happen, but guard against it.
                        tracing::warn!(
                            process_id = %self.process_id,
                            "context injection termination but no buffered messages"
                        );
                    }

                    for message in &injected {
                        tracing::info!(
                            process_id = %self.process_id,
                            "injecting context into worker history"
                        );
                        history.push(Message::user(format!(
                            "[Context update from the user]: {message}"
                        )));
                    }

                    // Re-prompt asking the worker to incorporate the new context.
                    current_prompt = std::borrow::Cow::Borrowed(
                        "New context has been provided above. Incorporate this \
                         information and continue working on your task. Do not \
                         repeat completed work.",
                    );
                    using_tool_nudge_prompt = false;
                    continue;
                }
                Err(PromptError::PromptCancelled { reason, .. })
                    if Self::is_tool_nudge_reason(reason) =>
                {
                    // Read the current attempt count. on_tool_result resets
                    // this to zero whenever a tool call completes, so this
                    // tracks *consecutive* text-only responses rather than
                    // total nudges across the worker's lifetime.
                    let attempts = self
                        .nudge_attempts
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if attempts >= Self::TOOL_NUDGE_MAX_RETRIES {
                        // Retries exhausted — propagate the cancellation.
                        self.set_tool_nudge_request_active(false);
                        self.set_completion_contract_request_active(false);
                        return result;
                    }
                    Self::prune_tool_nudge_retry_history(
                        history,
                        history_len_before_attempt,
                        using_tool_nudge_prompt,
                    );
                    tracing::warn!(
                        process_id = %self.process_id,
                        process_type = %self.process_type,
                        attempt = attempts + 1,
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
                    self.set_completion_contract_request_active(false);
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

    /// Prompt once using Rig's streaming path so text/tool deltas reach the hook.
    pub async fn prompt_once_streaming<M>(
        &self,
        agent: &rig::agent::Agent<M>,
        history: &mut Vec<Message>,
        prompt: &str,
        max_turns: usize,
    ) -> std::result::Result<String, PromptError>
    where
        M: CompletionModel + 'static,
        M::StreamingResponse: GetTokenUsage + Send,
    {
        self.reset_tool_nudge_state();
        self.set_tool_nudge_request_active(false);

        let mut chat_history = history.clone();
        let prompt_message = Message::from(prompt);
        chat_history.push(prompt_message.clone());

        let mut current_max_turns = 0usize;
        let mut last_text_response = String::new();
        let mut did_call_tool = false;

        loop {
            let current_prompt = chat_history
                .last()
                .cloned()
                .expect("chat history should always include current prompt");

            if current_max_turns > max_turns + 1 {
                return Err(PromptError::MaxTurnsError {
                    max_turns,
                    chat_history: Box::new(chat_history),
                    prompt: Box::new(prompt.to_string().into()),
                });
            }

            current_max_turns += 1;

            if let HookAction::Terminate { reason } =
                <SpacebotHook as PromptHook<M>>::on_completion_call(
                    self,
                    &current_prompt,
                    &chat_history[..chat_history.len() - 1],
                )
                .await
            {
                return Err(PromptError::PromptCancelled {
                    chat_history: Box::new(chat_history),
                    reason,
                });
            }

            let request = agent
                .stream_completion(
                    current_prompt.clone(),
                    chat_history[..chat_history.len() - 1].to_vec(),
                )
                .await
                .map_err(PromptError::CompletionError)?;

            let mut stream = request
                .stream()
                .await
                .map_err(PromptError::CompletionError)?;

            let mut tool_calls = vec![];
            let mut tool_results = vec![];
            let mut is_text_response = false;

            while let Some(content) = stream.next().await {
                match content.map_err(PromptError::CompletionError)? {
                    StreamedAssistantContent::Text(text) => {
                        if !is_text_response {
                            last_text_response.clear();
                            is_text_response = true;
                        }
                        last_text_response.push_str(&text.text);
                        if let HookAction::Terminate { reason } =
                            <SpacebotHook as PromptHook<M>>::on_text_delta(
                                self,
                                &text.text,
                                &last_text_response,
                            )
                            .await
                        {
                            return Err(PromptError::PromptCancelled {
                                chat_history: Box::new(chat_history),
                                reason,
                            });
                        }
                        did_call_tool = false;
                    }
                    StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id,
                    } => {
                        let tool_args = serde_json::to_string(&tool_call.function.arguments)
                            .unwrap_or_else(|_| "{}".to_string());
                        match <SpacebotHook as PromptHook<M>>::on_tool_call(
                            self,
                            &tool_call.function.name,
                            tool_call.call_id.clone(),
                            &internal_call_id,
                            &tool_args,
                        )
                        .await
                        {
                            ToolCallHookAction::Terminate { reason } => {
                                return Err(PromptError::PromptCancelled {
                                    chat_history: Box::new(chat_history),
                                    reason,
                                });
                            }
                            ToolCallHookAction::Skip { reason } => {
                                tool_calls.push(AssistantContent::ToolCall(tool_call.clone()));
                                tool_results.push((
                                    tool_call.id.clone(),
                                    tool_call.call_id.clone(),
                                    reason,
                                ));
                                did_call_tool = true;
                            }
                            ToolCallHookAction::Continue => {
                                let tool_result = match agent
                                    .tool_server_handle
                                    .call_tool(&tool_call.function.name, &tool_args)
                                    .await
                                {
                                    Ok(result) => result,
                                    Err(error) => error.to_string(),
                                };

                                if let HookAction::Terminate { reason } =
                                    <SpacebotHook as PromptHook<M>>::on_tool_result(
                                        self,
                                        &tool_call.function.name,
                                        tool_call.call_id.clone(),
                                        &internal_call_id,
                                        &tool_args,
                                        &tool_result,
                                    )
                                    .await
                                {
                                    return Err(PromptError::PromptCancelled {
                                        chat_history: Box::new(chat_history),
                                        reason,
                                    });
                                }

                                tool_calls.push(AssistantContent::ToolCall(tool_call.clone()));
                                tool_results.push((
                                    tool_call.id.clone(),
                                    tool_call.call_id.clone(),
                                    tool_result,
                                ));
                                did_call_tool = true;
                            }
                        }
                    }
                    StreamedAssistantContent::ToolCallDelta {
                        id,
                        internal_call_id,
                        content,
                    } => {
                        let (name, delta) = match &content {
                            rig::streaming::ToolCallDeltaContent::Name(name) => {
                                (Some(name.as_str()), "")
                            }
                            rig::streaming::ToolCallDeltaContent::Delta(delta) => {
                                (None, delta.as_str())
                            }
                        };
                        if let HookAction::Terminate { reason } =
                            <SpacebotHook as PromptHook<M>>::on_tool_call_delta(
                                self,
                                &id,
                                &internal_call_id,
                                name,
                                delta,
                            )
                            .await
                        {
                            return Err(PromptError::PromptCancelled {
                                chat_history: Box::new(chat_history),
                                reason,
                            });
                        }
                    }
                    StreamedAssistantContent::Final(final_response) => {
                        if is_text_response {
                            if let HookAction::Terminate { reason } =
								<SpacebotHook as PromptHook<M>>::on_stream_completion_response_finish(
									self,
									&current_prompt,
									&final_response,
								)
								.await
							{
								return Err(PromptError::PromptCancelled {
									chat_history: Box::new(chat_history),
									reason,
								});
							}
                            is_text_response = false;
                        }
                    }
                    StreamedAssistantContent::Reasoning(_)
                    | StreamedAssistantContent::ReasoningDelta { .. } => {
                        did_call_tool = false;
                    }
                }
            }

            if !tool_calls.is_empty() {
                chat_history.push(Message::Assistant {
                    id: None,
                    content: rig::OneOrMany::many(tool_calls)
                        .expect("tool call list should not be empty"),
                });
            }

            for (id, call_id, tool_result) in tool_results {
                if let Some(call_id) = call_id {
                    chat_history.push(Message::User {
                        content: rig::OneOrMany::one(UserContent::tool_result_with_call_id(
                            &id,
                            call_id,
                            rig::OneOrMany::one(ToolResultContent::text(&tool_result)),
                        )),
                    });
                } else {
                    chat_history.push(Message::User {
                        content: rig::OneOrMany::one(UserContent::tool_result(
                            &id,
                            rig::OneOrMany::one(ToolResultContent::text(&tool_result)),
                        )),
                    });
                }
            }

            if !did_call_tool {
                chat_history.push(Message::Assistant {
                    id: None,
                    content: rig::OneOrMany::one(AssistantContent::text(
                        last_text_response.clone(),
                    )),
                });
                *history = chat_history;
                return Ok(last_text_response);
            }
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
            let process_label = match self.process_type {
                crate::ProcessType::Channel => "channel",
                crate::ProcessType::Branch => "branch",
                crate::ProcessType::Worker => "worker",
                crate::ProcessType::Compactor => "compactor",
                crate::ProcessType::Cortex => "cortex",
            };
            metrics
                .tool_calls_total
                .with_label_values(&[&*self.agent_id, tool_name, process_label])
                .inc();
            if let Some(start) = TOOL_CALL_TIMERS
                .lock()
                .ok()
                .and_then(|mut timers| timers.remove(internal_call_id))
            {
                metrics
                    .tool_call_duration_seconds
                    .with_label_values(&[&*self.agent_id, tool_name, process_label])
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

        // Response without tool calls and without a prior outcome signal.
        // Nudge if the response contains any non-empty text, OR if it
        // contains no text at all (e.g. reasoning-only). A response that
        // is purely reasoning/image with no text and no tool calls means
        // the worker hasn't actually done anything — send it back.
        let has_any_text = response.choice.iter().any(|content| {
            if let rig::message::AssistantContent::Text(text) = content {
                !text.text.trim().is_empty()
            } else {
                false
            }
        });

        // Nudge on non-empty text (worker tried to narrate instead of
        // working) or on no text at all (reasoning-only exit attempt).
        has_any_text
            || !response.choice.iter().any(|content| {
                matches!(
                    content,
                    rig::message::AssistantContent::Text(_)
                        | rig::message::AssistantContent::ToolCall(_)
                )
            })
    }

    fn should_reject_memory_persistence_completion<M>(
        &self,
        response: &CompletionResponse<M::Response>,
    ) -> bool
    where
        M: CompletionModel,
    {
        let Some(contract_state) = &self.memory_persistence_contract else {
            return false;
        };
        if !self
            .completion_contract_request_active
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }
        if contract_state.has_terminal_outcome() {
            return false;
        }

        !response
            .choice
            .iter()
            .any(|content| matches!(content, rig::message::AssistantContent::ToolCall(_)))
    }

    fn parse_memory_persistence_terminal_outcome(
        result: &str,
    ) -> Option<MemoryPersistenceTerminalOutcome> {
        let parsed = serde_json::from_str::<serde_json::Value>(result).ok()?;
        if parsed.get("success").and_then(|value| value.as_bool()) != Some(true) {
            return None;
        }

        match parsed.get("outcome").and_then(|value| value.as_str()) {
            Some("saved") => {
                let saved_memory_ids = parsed
                    .get("saved_memory_ids")
                    .and_then(|value| value.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str())
                            .map(str::trim)
                            .filter(|memory_id| !memory_id.is_empty())
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if saved_memory_ids.is_empty() {
                    return None;
                }
                Some(MemoryPersistenceTerminalOutcome::Saved { saved_memory_ids })
            }
            Some("no_memories") => {
                let reason = parsed
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())?;
                Some(MemoryPersistenceTerminalOutcome::NoMemories {
                    reason: reason.to_string(),
                })
            }
            _ => None,
        }
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

        // Check for pending injected context before the LLM call.
        // Drain all available messages from the injection channel and buffer
        // them for the retry loop to append to history.
        if let Some(inject_rx) = &self.inject_rx {
            let mut drained = Vec::new();
            if let Ok(mut receiver) = inject_rx.try_lock() {
                while let Ok(message) = receiver.try_recv() {
                    drained.push(message);
                }
            }
            if !drained.is_empty() {
                let count = drained.len();
                if let Ok(mut buffer) = self.injected_messages.lock() {
                    buffer.extend(drained);
                }
                tracing::info!(
                    process_id = %self.process_id,
                    count,
                    "context injection: drained pending messages, terminating for re-prompt"
                );
                return HookAction::Terminate {
                    reason: Self::CONTEXT_INJECTION_REASON.into(),
                };
            }
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

        if self.should_reject_memory_persistence_completion::<M>(response) {
            return HookAction::Terminate {
                reason: Self::MEMORY_PERSISTENCE_CONTRACT_REASON.into(),
            };
        }

        // Emit text content from worker completion responses so the live
        // transcript can show the model's reasoning between tool calls.
        if self.process_type == ProcessType::Worker {
            let text: String = response
                .choice
                .iter()
                .filter_map(|content| {
                    if let rig::message::AssistantContent::Text(text) = content {
                        let trimmed = text.text.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            if !text.is_empty()
                && let ProcessId::Worker(worker_id) = &self.process_id
            {
                let event = ProcessEvent::WorkerText {
                    agent_id: self.agent_id.clone(),
                    worker_id: *worker_id,
                    channel_id: self.channel_id.clone(),
                    text,
                };
                self.event_tx.send(event).ok();
            }
        }

        HookAction::Continue
    }

    async fn on_text_delta(&self, text_delta: &str, aggregated_text: &str) -> HookAction {
        if self.process_type == ProcessType::Channel
            && let Some(channel_id) = self.channel_id.clone()
        {
            let event = ProcessEvent::TextDelta {
                agent_id: self.agent_id.clone(),
                process_id: self.process_id.clone(),
                channel_id: Some(channel_id),
                text_delta: text_delta.to_string(),
                aggregated_text: aggregated_text.to_string(),
            };
            self.event_tx.send(event).ok();
        }

        HookAction::Continue
    }

    async fn on_tool_call_delta(
        &self,
        _tool_call_id: &str,
        internal_call_id: &str,
        tool_name: Option<&str>,
        tool_call_delta: &str,
    ) -> HookAction {
        if self.process_type != ProcessType::Channel {
            return HookAction::Continue;
        }

        let Some(channel_id) = self.channel_id.clone() else {
            return HookAction::Continue;
        };

        let mut guard = match self.reply_tool_delta_state.lock() {
            Ok(guard) => guard,
            Err(_) => return HookAction::Continue,
        };

        let state = guard
            .entry(internal_call_id.to_string())
            .or_insert_with(ReplyToolDeltaState::default);

        if let Some(tool_name) = tool_name {
            state.tool_name = Some(tool_name.to_string());
        }

        if state.tool_name.as_deref() != Some("reply") {
            return HookAction::Continue;
        }

        state.raw_args.push_str(tool_call_delta);
        let Some(content) = Self::extract_partial_reply_content(&state.raw_args) else {
            return HookAction::Continue;
        };

        if !content.starts_with(&state.emitted_content) {
            return HookAction::Continue;
        }

        let delta = &content[state.emitted_content.len()..];
        if delta.is_empty() {
            return HookAction::Continue;
        }

        state.emitted_content = content.clone();
        self.event_tx
            .send(ProcessEvent::TextDelta {
                agent_id: self.agent_id.clone(),
                process_id: self.process_id.clone(),
                channel_id: Some(channel_id),
                text_delta: delta.to_string(),
                aggregated_text: content,
            })
            .ok();

        HookAction::Continue
    }

    async fn on_stream_completion_response_finish(
        &self,
        _prompt: &Message,
        _response: &<M as CompletionModel>::StreamingResponse,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        if tool_name == "reply"
            && let Ok(mut guard) = self.reply_tool_delta_state.lock()
        {
            guard.remove(_internal_call_id);
        }
        // Loop guard: check for repetitive tool calling before execution.
        // Runs for all process types. Block → Skip (message becomes tool
        // result), CircuitBreak → Terminate.
        if let Ok(mut guard) = self.loop_guard.lock() {
            match guard.check(tool_name, args) {
                LoopGuardVerdict::Allow => {}
                LoopGuardVerdict::Block(reason) => {
                    tracing::warn!(
                        process_id = %self.process_id,
                        tool_name = %tool_name,
                        "loop guard blocked tool call"
                    );
                    return ToolCallHookAction::Skip { reason };
                }
                LoopGuardVerdict::CircuitBreak(reason) => {
                    tracing::warn!(
                        process_id = %self.process_id,
                        tool_name = %tool_name,
                        "loop guard circuit-breaking agent loop"
                    );
                    return ToolCallHookAction::Terminate { reason };
                }
            }
        }

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
        if tool_name == "reply"
            && let Ok(mut guard) = self.reply_tool_delta_state.lock()
        {
            guard.remove(internal_call_id);
        }
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

        // Record outcome for loop guard (outcome-aware repetition detection).
        // The guard uses the (tool_name, args, result) triple to detect when
        // the same call produces the same result repeatedly, and poisons the
        // call hash so the next check() in on_tool_call auto-blocks.
        if let Ok(mut guard) = self.loop_guard.lock() {
            guard.record_outcome(tool_name, _args, result);
        }

        let is_tool_error = result.starts_with("Toolset error:");

        // Log tool errors so operators can see when tools fail (including
        // deserialization errors that happen before the tool's call() method).
        if is_tool_error {
            tracing::warn!(
                process_id = %self.process_id,
                tool_name = %tool_name,
                error = %result,
                "tool call failed"
            );
        }

        if !is_tool_error
            && tool_name == "memory_persistence_complete"
            && let Some(contract_state) = &self.memory_persistence_contract
            && let Some(outcome) = Self::parse_memory_persistence_terminal_outcome(result)
        {
            contract_state.set_terminal_outcome(outcome);
        }

        // A successful tool call proves the worker is still productive.
        // Reset the consecutive nudge counter so a brief narration blip
        // after many tool calls doesn't exhaust the retry budget.
        // Tool errors (from Rig's error path) don't count as productive.
        if self.tool_nudge_policy.is_enabled() && !is_tool_error {
            self.nudge_attempts
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }

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

        // Channel turns should end immediately after a successful reply or skip
        // tool call. This avoids extra post-reply LLM iterations that add latency,
        // cost, and noisy logs when providers return empty trailing responses.
        // For skip, terminating is critical: without it the model receives the tool
        // result and almost always generates narration like "The skip was successful"
        // which either leaks to the user (retrigger path) or wastes tokens.
        if !is_tool_error
            && self.process_type == ProcessType::Channel
            && (tool_name == "reply" || tool_name == "skip")
        {
            return HookAction::Terminate {
                reason: if tool_name == "reply" {
                    "reply delivered".into()
                } else {
                    "skip".into()
                },
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
    use crate::tools::MemoryPersistenceContractState;
    use crate::{ProcessId, ProcessType};
    use rig::OneOrMany;
    use rig::agent::{HookAction, PromptHook};
    use rig::completion::{CompletionResponse, Message, Usage};
    use rig::message::AssistantContent;
    use std::sync::Arc;

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

    fn make_memory_persistence_hook() -> (SpacebotHook, Arc<MemoryPersistenceContractState>) {
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
        let contract_state = Arc::new(MemoryPersistenceContractState::default());
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Branch(uuid::Uuid::new_v4()),
            ProcessType::Branch,
            None,
            event_tx,
        )
        .with_memory_persistence_contract(contract_state.clone());
        (hook, contract_state)
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
    async fn reply_tool_call_delta_emits_process_event() {
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
        let hook = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Channel(std::sync::Arc::<str>::from("channel")),
            ProcessType::Channel,
            Some(std::sync::Arc::<str>::from("channel")),
            event_tx,
        );

        let first = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call_delta(
            &hook,
            "reply-call",
            "internal-reply",
            Some("reply"),
            "{\"content\":\"hel",
        )
        .await;
        let second = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_call_delta(
            &hook,
            "reply-call",
            "internal-reply",
            None,
            "lo\"}",
        )
        .await;

        assert!(matches!(first, HookAction::Continue));
        assert!(matches!(second, HookAction::Continue));

        let event = event_rx.recv().await.expect("first reply delta event");
        assert!(matches!(
            event,
            ProcessEvent::TextDelta {
                ref text_delta,
                ref aggregated_text,
                ..
            } if text_delta == "hel" && aggregated_text == "hel"
        ));

        let event = event_rx.recv().await.expect("second reply delta event");
        assert!(matches!(
            event,
            ProcessEvent::TextDelta {
                ref text_delta,
                ref aggregated_text,
                ..
            } if text_delta == "lo" && aggregated_text == "hello"
        ));
    }

    #[tokio::test]
    async fn tool_result_resets_consecutive_nudge_counter() {
        // The exact scenario from the Railway browser worker failure:
        // worker makes many tool calls, then narrates, gets nudged, but the
        // nudge counter should have been reset by the prior tool calls so
        // the budget isn't exhausted from earlier narration blips.
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // Simulate 2 nudge attempts (would exhaust budget under old behavior)
        hook.nudge_attempts
            .store(2, std::sync::atomic::Ordering::Relaxed);

        // A successful tool call should reset the counter
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "browser_click",
            None,
            "internal_1",
            "{\"index\": 13}",
            "{\"success\":true,\"message\":\"Clicked element at index 13\"}",
        )
        .await;

        assert_eq!(
            hook.nudge_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "Tool result should reset consecutive nudge counter to zero"
        );
    }

    #[tokio::test]
    async fn nudge_counter_not_reset_when_policy_disabled() {
        // When nudge policy is disabled, on_tool_result should not touch
        // the counter (it's irrelevant but ensures no side effects).
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Disabled);
        hook.reset_tool_nudge_state();

        hook.nudge_attempts
            .store(2, std::sync::atomic::Ordering::Relaxed);

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "shell",
            None,
            "internal_1",
            "{\"command\":\"ls\"}",
            "file1.txt\nfile2.txt",
        )
        .await;

        assert_eq!(
            hook.nudge_attempts
                .load(std::sync::atomic::Ordering::Relaxed),
            2,
            "Nudge counter should not be reset when policy is disabled"
        );
    }

    #[tokio::test]
    async fn nudges_on_reasoning_only_response_without_outcome() {
        // A response with only Reasoning content (no Text, no ToolCall) should
        // trigger a nudge. This is the exact bug case: models with extended
        // thinking produce a Reasoning block and exit the agent loop without
        // doing any work.
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let prompt = prompt_message();
        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        let reasoning_response = CompletionResponse {
            choice: OneOrMany::one(AssistantContent::reasoning("thinking about the task...")),
            message_id: None,
            usage: Usage::default(),
            raw_response: RawResponse {
                body: serde_json::json!({}),
            },
        };

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let response = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &reasoning_response,
        )
        .await;
        assert!(
            matches!(
                response,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::TOOL_NUDGE_REASON
            ),
            "Expected nudge — reasoning-only response without outcome should be rejected"
        );
    }

    // ---- Context injection tests ----

    #[tokio::test]
    async fn injection_terminates_on_pending_messages() {
        let hook = make_hook();
        let (inject_tx, inject_rx) = tokio::sync::mpsc::channel(8);
        let hook = hook.with_inject_rx(inject_rx);
        let prompt = prompt_message();

        // Send a message before the completion call
        inject_tx
            .send("additional context".to_string())
            .await
            .unwrap();

        let action =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;

        assert!(
            matches!(
                action,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::CONTEXT_INJECTION_REASON
            ),
            "Expected termination for context injection, got {action:?}"
        );

        // The message should be buffered for the retry loop
        let messages = hook.take_injected_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], "additional context");
    }

    #[tokio::test]
    async fn injection_continues_when_no_pending_messages() {
        let hook = make_hook();
        let (_inject_tx, inject_rx) = tokio::sync::mpsc::channel(8);
        let hook = hook.with_inject_rx(inject_rx);
        let prompt = prompt_message();

        let action =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;

        assert!(
            matches!(action, HookAction::Continue),
            "Expected Continue when no injected messages, got {action:?}"
        );
    }

    #[tokio::test]
    async fn injection_drains_multiple_messages() {
        let hook = make_hook();
        let (inject_tx, inject_rx) = tokio::sync::mpsc::channel(8);
        let hook = hook.with_inject_rx(inject_rx);
        let prompt = prompt_message();

        inject_tx.send("first".to_string()).await.unwrap();
        inject_tx.send("second".to_string()).await.unwrap();
        inject_tx.send("third".to_string()).await.unwrap();

        let action =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;

        assert!(matches!(
            action,
            HookAction::Terminate { ref reason }
            if reason == SpacebotHook::CONTEXT_INJECTION_REASON
        ));

        let messages = hook.take_injected_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], "first");
        assert_eq!(messages[1], "second");
        assert_eq!(messages[2], "third");
    }

    #[tokio::test]
    async fn injection_take_clears_buffer() {
        let hook = make_hook();
        let (inject_tx, inject_rx) = tokio::sync::mpsc::channel(8);
        let hook = hook.with_inject_rx(inject_rx);
        let prompt = prompt_message();

        inject_tx.send("context".to_string()).await.unwrap();

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;

        let first = hook.take_injected_messages();
        assert_eq!(first.len(), 1);

        // Second take should be empty
        let second = hook.take_injected_messages();
        assert!(second.is_empty(), "Buffer should be empty after take");
    }

    #[tokio::test]
    async fn injection_reason_detection() {
        assert!(SpacebotHook::is_context_injection_reason(
            SpacebotHook::CONTEXT_INJECTION_REASON
        ));
        assert!(!SpacebotHook::is_context_injection_reason(
            SpacebotHook::TOOL_NUDGE_REASON
        ));
        assert!(!SpacebotHook::is_tool_nudge_reason(
            SpacebotHook::CONTEXT_INJECTION_REASON
        ));
    }

    #[tokio::test]
    async fn injection_does_not_interfere_with_nudge() {
        // A hook with both injection and nudge enabled should handle them
        // independently.
        let hook = make_hook().with_tool_nudge_policy(ToolNudgePolicy::Enabled);
        let (_inject_tx, inject_rx) = tokio::sync::mpsc::channel(8);
        let hook = hook.with_inject_rx(inject_rx);
        let prompt = prompt_message();

        hook.reset_tool_nudge_state();
        hook.set_tool_nudge_request_active(true);

        // No injection pending, so on_completion_call should Continue
        let action =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        assert!(matches!(action, HookAction::Continue));

        // A text-only response should still trigger the nudge
        let response = text_response("some text without outcome");
        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook, &prompt, &response,
        )
        .await;
        assert!(
            matches!(
                action,
                HookAction::Terminate { ref reason }
                if reason == SpacebotHook::TOOL_NUDGE_REASON
            ),
            "Nudge should still work when inject_rx is attached but empty"
        );
    }

    #[tokio::test]
    async fn memory_persistence_plain_text_completion_is_rejected_without_terminal_tool() {
        let (hook, _contract_state) = make_memory_persistence_hook();
        let prompt = prompt_message();
        hook.set_completion_contract_request_active(true);

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Saved the memories."),
        )
        .await;

        assert!(matches!(
            action,
            HookAction::Terminate { ref reason }
            if reason == SpacebotHook::MEMORY_PERSISTENCE_CONTRACT_REASON
        ));
    }

    #[tokio::test]
    async fn memory_persistence_fabricated_saved_ids_are_rejected() {
        let (hook, contract_state) = make_memory_persistence_hook();
        let prompt = prompt_message();
        hook.set_completion_contract_request_active(true);

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_save",
            None,
            "internal_1",
            "{}",
            "{\"success\":true,\"memory_id\":\"mem_real_1\"}",
        )
        .await;

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_persistence_complete",
            None,
            "internal_2",
            "{\"outcome\":\"saved\",\"saved_memory_ids\":[\"mem_fake\"]}",
            "Toolset error: memory_persistence_complete failed: saved_memory_ids mismatch",
        )
        .await;

        assert!(
            !contract_state.has_terminal_outcome(),
            "terminal outcome must not be recorded for fabricated IDs"
        );

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Done."),
        )
        .await;

        assert!(matches!(
            action,
            HookAction::Terminate { ref reason }
            if reason == SpacebotHook::MEMORY_PERSISTENCE_CONTRACT_REASON
        ));
    }

    #[tokio::test]
    async fn memory_persistence_saved_outcome_accepts_real_memory_save_ids() {
        let (hook, contract_state) = make_memory_persistence_hook();
        let prompt = prompt_message();
        hook.set_completion_contract_request_active(true);

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_save",
            None,
            "internal_1",
            "{}",
            "{\"success\":true,\"memory_id\":\"mem_real_1\"}",
        )
        .await;
        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_save",
            None,
            "internal_2",
            "{}",
            "{\"success\":true,\"memory_id\":\"mem_real_2\"}",
        )
        .await;

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_persistence_complete",
            None,
            "internal_3",
            "{}",
            "{\"success\":true,\"outcome\":\"saved\",\"saved_memory_ids\":[\"mem_real_1\",\"mem_real_2\"]}",
        )
        .await;

        assert!(contract_state.has_terminal_outcome());

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("Persisted memories."),
        )
        .await;

        assert!(matches!(action, HookAction::Continue));
    }

    #[tokio::test]
    async fn memory_persistence_no_memories_outcome_is_accepted_without_saves() {
        let (hook, contract_state) = make_memory_persistence_hook();
        let prompt = prompt_message();
        hook.set_completion_contract_request_active(true);

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_persistence_complete",
            None,
            "internal_1",
            "{}",
            "{\"success\":true,\"outcome\":\"no_memories\",\"saved_memory_ids\":[],\"reason\":\"No durable facts found\"}",
        )
        .await;

        assert!(contract_state.has_terminal_outcome());

        let _ =
            <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_call(&hook, &prompt, &[])
                .await;
        let action = <SpacebotHook as PromptHook<SpacebotModel>>::on_completion_response(
            &hook,
            &prompt,
            &text_response("No memories persisted."),
        )
        .await;

        assert!(matches!(action, HookAction::Continue));
    }

    /// Regression test for #538: ingestion hook must wire the memory persistence
    /// contract so that `has_terminal_outcome()` returns true after a successful
    /// `memory_persistence_complete` tool call.
    #[tokio::test]
    async fn ingestion_hook_without_contract_does_not_record_terminal_outcome() {
        // Simulate the OLD ingestion code path: hook created without
        // with_memory_persistence_contract — terminal outcome is never recorded.
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
        let contract_state = Arc::new(MemoryPersistenceContractState::default());
        let hook_without_contract = SpacebotHook::new(
            std::sync::Arc::<str>::from("agent"),
            ProcessId::Branch(uuid::Uuid::new_v4()),
            ProcessType::Branch,
            None,
            event_tx,
        );
        // Do NOT call .with_memory_persistence_contract()

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook_without_contract,
            "memory_persistence_complete",
            None,
            "internal_1",
            "{}",
            "{\"success\":true,\"outcome\":\"no_memories\",\"saved_memory_ids\":[],\"reason\":\"No durable facts\"}",
        )
        .await;

        // Without contract wiring, has_terminal_outcome stays false — this was the #538 bug.
        assert!(
            !contract_state.has_terminal_outcome(),
            "contract state should NOT be set when hook lacks with_memory_persistence_contract"
        );
    }

    /// Regression test for #538: ingestion hook WITH the contract wired correctly
    /// records the terminal outcome after memory_persistence_complete succeeds.
    #[tokio::test]
    async fn ingestion_hook_with_contract_records_terminal_outcome() {
        // Simulate the FIXED ingestion code path: hook created with
        // with_memory_persistence_contract — terminal outcome is recorded.
        let (hook, contract_state) = make_memory_persistence_hook();

        let _ = <SpacebotHook as PromptHook<SpacebotModel>>::on_tool_result(
            &hook,
            "memory_persistence_complete",
            None,
            "internal_1",
            "{}",
            "{\"success\":true,\"outcome\":\"no_memories\",\"saved_memory_ids\":[],\"reason\":\"No durable facts\"}",
        )
        .await;

        // With contract wiring, has_terminal_outcome returns true — the fix for #538.
        assert!(
            contract_state.has_terminal_outcome(),
            "contract state MUST be set when hook has with_memory_persistence_contract"
        );
    }
}
