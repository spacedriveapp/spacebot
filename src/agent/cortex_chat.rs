//! Cortex chat: persistent admin conversation with the cortex.
//!
//! One session per agent. The admin talks to the cortex interactively,
//! with the full toolset (memory, shell, file, exec, browser, web search).
//! When opened on a channel page, the channel's recent history is injected
//! into the system prompt as context.

use crate::conversation::history::ProcessRunLogger;
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::{AgentDeps, ProcessId, ProcessType};

use futures::StreamExt as _;
use rig::agent::MultiTurnStreamItem;
use rig::agent::{AgentBuilder, HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{
    AssistantContent, CompletionModel, CompletionResponse, GetTokenUsage, Message, Prompt,
};
use rig::streaming::StreamingPrompt;
use rig::tool::server::ToolServerHandle;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// A persisted cortex chat message.
#[derive(Debug, Clone, Serialize)]
pub struct CortexChatMessage {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub content: String,
    pub channel_context: Option<String>,
    pub created_at: String,
}

/// Events emitted during a cortex chat response (sent via SSE to the client).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CortexChatEvent {
    /// The cortex is processing (before LLM response).
    Thinking,
    /// A partial text delta arrived from the model stream.
    TextDelta { text_delta: String },
    /// A tool call started.
    ToolStarted { tool: String },
    /// A tool call completed.
    ToolCompleted {
        tool: String,
        result_preview: String,
    },
    /// The full response is ready.
    Done { full_text: String },
    /// An error occurred.
    Error { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CortexChatSendError {
    #[error("cortex chat session is busy")]
    Busy,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Prompt(#[from] crate::error::Error),
}

/// Prompt hook that forwards tool events to an mpsc channel for SSE streaming.
#[derive(Clone)]
struct CortexChatHook {
    event_tx: mpsc::Sender<CortexChatEvent>,
    spacebot_hook: SpacebotHook,
}

impl CortexChatHook {
    fn new(event_tx: mpsc::Sender<CortexChatEvent>, spacebot_hook: SpacebotHook) -> Self {
        Self {
            event_tx,
            spacebot_hook,
        }
    }

    async fn send(&self, event: CortexChatEvent) {
        let _ = self.event_tx.send(event).await;
    }

    fn try_send(&self, event: CortexChatEvent) {
        if let Err(error) = self.event_tx.try_send(event)
            && matches!(error, tokio::sync::mpsc::error::TrySendError::Full(_))
        {
            tracing::debug!("dropping cortex chat streaming event because the SSE channel is full");
        }
    }
}

fn try_acquire_send_lock(
    send_lock: &Arc<Mutex<()>>,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<()>, CortexChatSendError> {
    send_lock
        .clone()
        .try_lock_owned()
        .map_err(|_| CortexChatSendError::Busy)
}

async fn persist_and_emit_cortex_chat_error(
    store: &CortexChatStore,
    event_tx: &mpsc::Sender<CortexChatEvent>,
    thread_id: &str,
    channel_ref: Option<&str>,
    message: String,
) {
    if let Err(error) = store
        .save_message(thread_id, "assistant", &message, channel_ref)
        .await
    {
        tracing::error!(%error, %thread_id, "failed to persist cortex chat error message");
    }
    if let Err(error) = event_tx.send(CortexChatEvent::Error { message }).await {
        tracing::debug!(%error, %thread_id, "failed to emit cortex chat error event");
    }
}

async fn persist_and_emit_cortex_chat_done(
    store: &CortexChatStore,
    event_tx: &mpsc::Sender<CortexChatEvent>,
    thread_id: &str,
    channel_ref: Option<&str>,
    response: String,
) {
    if let Err(error) = store
        .save_message(thread_id, "assistant", &response, channel_ref)
        .await
    {
        tracing::error!(%error, %thread_id, "failed to persist cortex chat response");
    }
    if let Err(error) = event_tx
        .send(CortexChatEvent::Done {
            full_text: response,
        })
        .await
    {
        tracing::debug!(%error, %thread_id, "failed to emit cortex chat done event");
    }
}

fn streaming_error_to_prompt_error(
    error: rig::agent::StreamingError,
) -> rig::completion::PromptError {
    match error {
        rig::agent::StreamingError::Prompt(error) => *error,
        rig::agent::StreamingError::Completion(error) => {
            rig::completion::PromptError::CompletionError(error)
        }
        rig::agent::StreamingError::Tool(error) => rig::completion::PromptError::ToolError(error),
    }
}

fn missing_stream_final_response_error() -> rig::completion::PromptError {
    rig::completion::PromptError::CompletionError(rig::completion::CompletionError::ProviderError(
        "stream ended without final response".to_string(),
    ))
}

async fn prompt_once_streaming<M, P>(
    agent: &rig::agent::Agent<M>,
    hook: P,
    history: Vec<Message>,
    prompt: &str,
) -> std::result::Result<String, rig::completion::PromptError>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: GetTokenUsage,
    P: rig::agent::PromptHook<M> + 'static,
{
    let mut stream = agent
        .stream_prompt(prompt)
        .with_hook(hook)
        .with_history(history)
        .await;

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::FinalResponse(final_response)) => {
                return Ok(final_response.response().to_string());
            }
            Ok(_) => {}
            Err(error) => return Err(streaming_error_to_prompt_error(error)),
        }
    }

    Err(missing_stream_final_response_error())
}

impl<M: CompletionModel> PromptHook<M> for CortexChatHook {
    async fn on_text_delta(&self, text_delta: &str, aggregated_text: &str) -> HookAction {
        let action = <SpacebotHook as PromptHook<M>>::on_text_delta(
            &self.spacebot_hook,
            text_delta,
            aggregated_text,
        )
        .await;
        if !matches!(action, HookAction::Continue) {
            return action;
        }

        self.try_send(CortexChatEvent::TextDelta {
            text_delta: text_delta.to_string(),
        });
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        let action = <SpacebotHook as PromptHook<M>>::on_tool_call(
            &self.spacebot_hook,
            tool_name,
            tool_call_id,
            internal_call_id,
            args,
        )
        .await;
        if !matches!(action, ToolCallHookAction::Continue) {
            return action;
        }

        self.send(CortexChatEvent::ToolStarted {
            tool: tool_name.to_string(),
        })
        .await;
        action
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        _args: &str,
        result: &str,
    ) -> HookAction {
        let guard_action = self.spacebot_hook.guard_tool_result(tool_name, result);
        if !matches!(guard_action, HookAction::Continue) {
            return guard_action;
        }
        let preview = crate::tools::truncate_utf8_ellipsis(result, 200);
        self.spacebot_hook
            .emit_tool_completed_event_from_capped(tool_name, preview.clone());
        self.spacebot_hook
            .record_tool_result_metrics(tool_name, internal_call_id);
        self.send(CortexChatEvent::ToolCompleted {
            tool: tool_name.to_string(),
            result_preview: preview,
        })
        .await;
        HookAction::Continue
    }

    async fn on_completion_call(&self, prompt: &Message, history: &[Message]) -> HookAction {
        <SpacebotHook as PromptHook<M>>::on_completion_call(&self.spacebot_hook, prompt, history)
            .await
    }

    async fn on_completion_response(
        &self,
        prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        <SpacebotHook as PromptHook<M>>::on_completion_response(
            &self.spacebot_hook,
            prompt,
            response,
        )
        .await
    }
}

/// SQLite CRUD for cortex chat messages.
#[derive(Debug, Clone)]
pub struct CortexChatStore {
    pool: SqlitePool,
}

#[derive(sqlx::FromRow)]
struct ChatMessageRow {
    id: String,
    thread_id: String,
    role: String,
    content: String,
    channel_context: Option<String>,
    created_at: chrono::NaiveDateTime,
}

impl ChatMessageRow {
    fn into_message(self) -> CortexChatMessage {
        CortexChatMessage {
            id: self.id,
            thread_id: self.thread_id,
            role: self.role,
            content: self.content,
            channel_context: self.channel_context,
            created_at: self.created_at.and_utc().to_rfc3339(),
        }
    }
}

impl CortexChatStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Load chat history for a thread, newest first, then reverse to chronological order.
    pub async fn load_history(
        &self,
        thread_id: &str,
        limit: i64,
    ) -> Result<Vec<CortexChatMessage>, sqlx::Error> {
        let rows: Vec<ChatMessageRow> = sqlx::query_as(
            "SELECT id, thread_id, role, content, channel_context, created_at \
             FROM cortex_chat_messages WHERE thread_id = ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(thread_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut messages: Vec<CortexChatMessage> =
            rows.into_iter().map(|row| row.into_message()).collect();
        messages.reverse();
        Ok(messages)
    }

    /// Save a message to a thread. Returns the generated ID.
    pub async fn save_message(
        &self,
        thread_id: &str,
        role: &str,
        content: &str,
        channel_context: Option<&str>,
    ) -> Result<String, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO cortex_chat_messages (id, thread_id, role, content, channel_context) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(role)
        .bind(content)
        .bind(channel_context)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Get the most recent thread_id, or None if no threads exist.
    pub async fn latest_thread_id(&self) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT thread_id FROM cortex_chat_messages ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }
}

/// The cortex chat session for a single agent.
///
/// Holds the deps, tool server, store, and a mutex to prevent concurrent sends.
pub struct CortexChatSession {
    pub deps: AgentDeps,
    pub tool_server: ToolServerHandle,
    pub store: CortexChatStore,
    /// Prevent concurrent sends — only one request at a time per agent.
    send_lock: Arc<Mutex<()>>,
}

impl CortexChatSession {
    pub fn new(deps: AgentDeps, tool_server: ToolServerHandle, store: CortexChatStore) -> Self {
        Self {
            deps,
            tool_server,
            store,
            send_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Send a message and stream events (tool calls, completion) back via an mpsc channel.
    ///
    /// Returns a receiver that yields `CortexChatEvent` items as the agent works.
    /// The agent runs in a spawned task so the caller can forward events to SSE
    /// without blocking.
    pub async fn send_message_with_events(
        self: &Arc<Self>,
        thread_id: &str,
        user_text: &str,
        channel_context_id: Option<&str>,
    ) -> std::result::Result<mpsc::Receiver<CortexChatEvent>, CortexChatSendError> {
        let send_guard = try_acquire_send_lock(&self.send_lock)?;

        // Save the user message
        self.store
            .save_message(thread_id, "user", user_text, channel_context_id)
            .await?;

        // Build the system prompt
        let system_prompt = self.build_system_prompt(channel_context_id).await?;

        // Load chat history and convert to Rig messages
        let chat_messages = self.store.load_history(thread_id, 100).await?;
        let mut history: Vec<rig::message::Message> = Vec::new();
        for message in &chat_messages[..chat_messages.len().saturating_sub(1)] {
            match message.role.as_str() {
                "user" => {
                    history.push(rig::message::Message::from(message.content.as_str()));
                }
                "assistant" => {
                    let content = AssistantContent::from(message.content.clone());
                    history.push(rig::message::Message::from(content));
                }
                _ => {}
            }
        }

        // Resolve model and build agent
        let routing = self.deps.runtime_config.routing.load();
        let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
        let model = SpacebotModel::make(&self.deps.llm_manager, &model_name)
            .with_context(self.deps.agent_id.as_ref(), "cortex")
            .with_rig_alignment((**self.deps.runtime_config.rig_alignment.load()).clone())
            .with_routing(routing.as_ref().clone());

        let agent = AgentBuilder::new(model)
            .preamble(&system_prompt)
            .default_max_turns(50)
            .tool_server_handle(self.tool_server.clone())
            .build();

        let (event_tx, event_rx) = mpsc::channel(256);
        let spacebot_hook = SpacebotHook::new(
            self.deps.agent_id.clone(),
            ProcessId::Worker(uuid::Uuid::new_v4()),
            ProcessType::Cortex,
            channel_context_id.map(std::sync::Arc::<str>::from),
            self.deps.event_tx.clone(),
        );
        let hook = CortexChatHook::new(event_tx.clone(), spacebot_hook);

        // Clone what the spawned task needs
        let user_text = user_text.to_string();
        let thread_id = thread_id.to_string();
        let channel_context_id = channel_context_id.map(|s| s.to_string());
        let store = self.store.clone();
        let prompt_timeout = Duration::from_secs(
            self.deps
                .runtime_config
                .cortex
                .load()
                .branch_timeout_secs
                .max(1),
        );
        let streaming_enabled = self
            .deps
            .runtime_config
            .rig_alignment
            .load()
            .cortex_chat_streaming;

        tokio::spawn(async move {
            let _send_guard = send_guard;
            let channel_ref = channel_context_id.as_deref();
            let prompt_result = tokio::time::timeout(prompt_timeout, async move {
                if streaming_enabled {
                    prompt_once_streaming(&agent, hook.clone(), history, &user_text).await
                } else {
                    agent
                        .prompt(&user_text)
                        .with_hook(hook.clone())
                        .with_history(&mut history)
                        .await
                }
            })
            .await;

            match prompt_result {
                Ok(Ok(response)) => {
                    persist_and_emit_cortex_chat_done(
                        &store,
                        &event_tx,
                        &thread_id,
                        channel_ref,
                        response,
                    )
                    .await;
                }
                Ok(Err(error)) => {
                    let error_text = format!("Cortex chat error: {error}");
                    persist_and_emit_cortex_chat_error(
                        &store,
                        &event_tx,
                        &thread_id,
                        channel_ref,
                        error_text,
                    )
                    .await;
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = prompt_timeout.as_secs(),
                        "cortex chat prompt timed out"
                    );
                    let error_text = format!(
                        "Cortex chat timed out after {}s while waiting for a model response.",
                        prompt_timeout.as_secs()
                    );
                    persist_and_emit_cortex_chat_error(
                        &store,
                        &event_tx,
                        &thread_id,
                        channel_ref,
                        error_text,
                    )
                    .await;
                }
            }
        });

        Ok(event_rx)
    }

    async fn build_system_prompt(
        &self,
        channel_context_id: Option<&str>,
    ) -> crate::error::Result<String> {
        let runtime_config = &self.deps.runtime_config;
        let prompt_engine = runtime_config.prompts.load();

        let identity_context = runtime_config.identity.load().render();
        let memory_bulletin = runtime_config.memory_bulletin.load();
        let agents_manifest = crate::self_awareness::agents_manifest_for_prompt();
        let changelog_highlights = crate::self_awareness::changelog_highlights();
        let runtime_config_snapshot = crate::self_awareness::runtime_snapshot_pretty(
            self.deps.agent_id.as_ref(),
            runtime_config,
        );

        let browser_enabled = runtime_config.browser_config.load().enabled;
        let web_search_enabled = runtime_config.brave_search_key.load().is_some();
        let opencode_enabled = runtime_config.opencode.load().enabled;
        let worker_capabilities = prompt_engine.render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
        )?;

        // Load channel transcript if a channel context is active
        let channel_transcript = if let Some(channel_id) = channel_context_id {
            self.load_channel_transcript(channel_id).await
        } else {
            None
        };

        let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };

        prompt_engine.render_cortex_chat_prompt(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            channel_transcript,
            empty_to_none(agents_manifest),
            empty_to_none(changelog_highlights),
            empty_to_none(runtime_config_snapshot),
            worker_capabilities,
        )
    }

    /// Load the last 50 messages from a channel as a formatted transcript.
    async fn load_channel_transcript(&self, channel_id: &str) -> Option<String> {
        let logger = ProcessRunLogger::new(self.deps.sqlite_pool.clone());

        match logger.load_channel_timeline(channel_id, 50, None).await {
            Ok(items) if !items.is_empty() => {
                let mut transcript = String::new();
                for item in &items {
                    match item {
                        crate::conversation::history::TimelineItem::Message {
                            role,
                            content,
                            sender_name,
                            ..
                        } => {
                            let name = sender_name.as_deref().unwrap_or(role);
                            transcript.push_str(&format!("**{name}**: {content}\n\n"));
                        }
                        crate::conversation::history::TimelineItem::BranchRun {
                            description,
                            conclusion,
                            ..
                        } => {
                            if let Some(conclusion) = conclusion {
                                transcript.push_str(&format!(
                                    "*[Branch: {description}]*: {conclusion}\n\n"
                                ));
                            }
                        }
                        crate::conversation::history::TimelineItem::WorkerRun {
                            task,
                            result,
                            ..
                        } => {
                            if let Some(result) = result {
                                transcript.push_str(&format!("*[Worker: {task}]*: {result}\n\n"));
                            }
                        }
                    }
                }
                Some(transcript)
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, channel_id, "failed to load channel transcript for cortex chat");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CortexChatEvent, CortexChatHook, CortexChatSendError, CortexChatStore,
        missing_stream_final_response_error, persist_and_emit_cortex_chat_done,
        try_acquire_send_lock,
    };
    use crate::hooks::SpacebotHook;
    use crate::llm::SpacebotModel;
    use crate::{ProcessId, ProcessType};
    use rig::agent::{HookAction, PromptHook};
    use sqlx::SqlitePool;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    #[test]
    fn preview_utf8_truncates_on_char_boundary() {
        let text = "🙂🙂🙂";
        // For max_bytes=5: can fit "🙂" (4 bytes) but not "🙂..." (7 bytes)
        let preview = crate::tools::truncate_utf8_ellipsis(text, 5);
        assert_eq!(preview, "🙂");
        // For max_bytes=10: can fit "🙂..." (7 bytes)
        let preview = crate::tools::truncate_utf8_ellipsis(text, 10);
        assert_eq!(preview, "🙂...");
    }

    #[test]
    fn preview_utf8_keeps_short_text() {
        let text = "done";
        let preview = crate::tools::truncate_utf8_ellipsis(text, 200);
        assert_eq!(preview, text);
    }

    #[tokio::test]
    async fn send_lock_returns_busy_when_already_held() {
        let send_lock = Arc::new(Mutex::new(()));
        let _first_guard = try_acquire_send_lock(&send_lock).expect("first lock should succeed");
        let second = try_acquire_send_lock(&send_lock);
        assert!(matches!(second, Err(CortexChatSendError::Busy)));
    }

    #[tokio::test]
    async fn send_lock_released_after_timeout_path() {
        let send_lock = Arc::new(Mutex::new(()));
        let send_guard = try_acquire_send_lock(&send_lock).expect("first lock should succeed");
        let timed_task = tokio::spawn(async move {
            let _send_guard = send_guard;
            tokio::time::timeout(Duration::from_millis(5), std::future::pending::<()>()).await
        });
        let timeout_result = timed_task.await.expect("timeout task should complete");
        assert!(timeout_result.is_err(), "pending prompt should time out");

        let second = try_acquire_send_lock(&send_lock);
        assert!(
            second.is_ok(),
            "single-flight lock should be released after timeout path"
        );
    }

    #[tokio::test]
    async fn cortex_chat_streaming_persists_final() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        sqlx::query(
            "CREATE TABLE cortex_chat_messages ( \
                id TEXT PRIMARY KEY, \
                thread_id TEXT NOT NULL, \
                role TEXT NOT NULL, \
                content TEXT NOT NULL, \
                channel_context TEXT, \
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP \
            )",
        )
        .execute(&pool)
        .await
        .expect("cortex chat table should be created");

        let store = CortexChatStore::new(pool);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let (process_event_tx, _process_event_rx) = tokio::sync::broadcast::channel(8);

        let spacebot_hook = SpacebotHook::new(
            Arc::from("agent"),
            ProcessId::Worker(uuid::Uuid::new_v4()),
            ProcessType::Cortex,
            None,
            process_event_tx,
        );
        let hook = CortexChatHook::new(event_tx.clone(), spacebot_hook);

        let hook_action = <CortexChatHook as PromptHook<SpacebotModel>>::on_text_delta(
            &hook,
            "partial ",
            "partial response",
        )
        .await;
        assert!(
            matches!(hook_action, HookAction::Continue),
            "streaming text deltas should continue"
        );

        let final_response = "final response".to_string();
        persist_and_emit_cortex_chat_done(&store, &event_tx, "thread-1", None, final_response)
            .await;

        let first_event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("should receive first event")
            .expect("first event should exist");
        match first_event {
            CortexChatEvent::TextDelta { text_delta } => {
                assert_eq!(text_delta, "partial ");
            }
            other => panic!("expected TextDelta event, got {other:?}"),
        }

        let second_event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("should receive done event")
            .expect("done event should exist");
        match second_event {
            CortexChatEvent::Done { full_text } => {
                assert_eq!(full_text, "final response");
            }
            other => panic!("expected Done event, got {other:?}"),
        }

        let history = store
            .load_history("thread-1", 10)
            .await
            .expect("history should load");
        assert_eq!(
            history.len(),
            1,
            "assistant final response should be persisted"
        );
        assert_eq!(history[0].role, "assistant");
        assert_eq!(history[0].content, "final response");
    }

    #[tokio::test]
    async fn text_delta_backpressure_does_not_timeout_prompt() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .send(CortexChatEvent::Thinking)
            .await
            .expect("prefill event channel");
        let (process_event_tx, _process_event_rx) = tokio::sync::broadcast::channel(8);

        let spacebot_hook = SpacebotHook::new(
            Arc::from("agent"),
            ProcessId::Worker(uuid::Uuid::new_v4()),
            ProcessType::Cortex,
            None,
            process_event_tx,
        );
        let hook = CortexChatHook::new(event_tx, spacebot_hook);

        let action = tokio::time::timeout(
            Duration::from_millis(20),
            <CortexChatHook as PromptHook<SpacebotModel>>::on_text_delta(
                &hook,
                "partial ",
                "partial response",
            ),
        )
        .await
        .expect("text delta should not block on a full SSE channel");

        assert!(matches!(action, HookAction::Continue));
        let first_event = event_rx
            .recv()
            .await
            .expect("prefilled event should remain");
        assert!(matches!(first_event, CortexChatEvent::Thinking));
        assert!(
            event_rx.try_recv().is_err(),
            "full SSE channel should drop text deltas instead of backpressuring the prompt"
        );
    }

    #[test]
    fn missing_stream_final_response_is_an_error() {
        let error = missing_stream_final_response_error();
        assert!(matches!(
            error,
            rig::completion::PromptError::CompletionError(
                rig::completion::CompletionError::ProviderError(_)
            )
        ));
        assert!(
            error
                .to_string()
                .contains("stream ended without final response")
        );
    }
}
