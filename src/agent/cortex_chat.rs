//! Cortex chat: persistent admin conversation with the cortex.
//!
//! **DEPRECATED:** Cortex chat is being replaced by per-channel settings
//! (Channel Settings UI) which provide fine-grained control. Remaining
//! functionality (skills_search, install_skill, config_inspect, etc.) is
//! being ported to channel/worker toolsets. Do not add new features here.
//!
//! One session per agent. The admin talks to the cortex interactively,
//! with the full toolset (memory, shell, file, browser, web search).
//! When opened on a channel page, the channel's recent history is injected
//! into the system prompt as context.

use crate::conversation::history::ProcessRunLogger;
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::{AgentDeps, ProcessEvent, ProcessId, ProcessType};

use rig::agent::{AgentBuilder, HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{AssistantContent, CompletionModel, CompletionResponse, Message, Prompt};
use rig::tool::server::ToolServerHandle;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::mpsc;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

/// A persisted cortex chat message.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CortexChatMessage {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub content: String,
    pub channel_context: Option<String>,
    pub created_at: String,
    /// Serialized JSON array of tool calls (for assistant messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CortexChatToolCall>>,
}

/// Summary of a cortex chat thread (returned by list_threads).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CortexChatThread {
    pub thread_id: String,
    pub preview: String,
    pub message_count: i64,
    pub first_message_at: String,
    pub last_message_at: String,
}

/// A tool call + result pair persisted alongside assistant messages.
#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CortexChatToolCall {
    pub id: String,
    pub tool: String,
    pub args: String,
    pub result: Option<String>,
    pub status: String, // "running", "completed", "error"
}

/// Events emitted during a cortex chat response (sent via SSE to the client).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CortexChatEvent {
    /// The cortex is processing (before LLM response).
    Thinking,
    /// A tool call started.
    ToolStarted {
        tool: String,
        call_id: String,
        args: String,
    },
    /// A tool call completed.
    ToolCompleted {
        tool: String,
        call_id: String,
        args: String,
        result: String,
        result_preview: String,
    },
    /// The full response is ready.
    Done {
        full_text: String,
        tool_calls: Vec<CortexChatToolCall>,
    },
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
///
/// Accumulates tool calls so they can be included in the `Done` event and
/// persisted alongside the assistant message.
#[derive(Clone)]
struct CortexChatHook {
    event_tx: mpsc::Sender<CortexChatEvent>,
    spacebot_hook: SpacebotHook,
    /// Accumulated tool calls during this response (shared with the spawned task).
    tool_calls: Arc<Mutex<Vec<CortexChatToolCall>>>,
}

impl CortexChatHook {
    fn new(
        event_tx: mpsc::Sender<CortexChatEvent>,
        spacebot_hook: SpacebotHook,
        tool_calls: Arc<Mutex<Vec<CortexChatToolCall>>>,
    ) -> Self {
        Self {
            event_tx,
            spacebot_hook,
            tool_calls,
        }
    }

    async fn send(&self, event: CortexChatEvent) {
        let _ = self.event_tx.send(event).await;
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
    let _ = store
        .save_message(thread_id, "assistant", &message, channel_ref, None)
        .await;
    let _ = event_tx.send(CortexChatEvent::Error { message }).await;
}

impl<M: CompletionModel> PromptHook<M> for CortexChatHook {
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

        let call_id = internal_call_id.to_string();

        // Record tool call start in accumulated list
        {
            let mut calls = self.tool_calls.lock().await;
            calls.push(CortexChatToolCall {
                id: call_id.clone(),
                tool: tool_name.to_string(),
                args: args.to_string(),
                result: None,
                status: "running".to_string(),
            });
        }

        self.send(CortexChatEvent::ToolStarted {
            tool: tool_name.to_string(),
            call_id,
            args: args.to_string(),
        })
        .await;
        action
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        let guard_action = self.spacebot_hook.guard_tool_result(tool_name, result);
        if !matches!(guard_action, HookAction::Continue) {
            self.spacebot_hook
                .record_tool_result_metrics(tool_name, internal_call_id);
            return guard_action;
        }
        let preview = crate::tools::truncate_utf8_ellipsis(result, 200);
        self.spacebot_hook
            .emit_tool_completed_event_from_capped(tool_name, preview.clone());
        self.spacebot_hook
            .record_tool_result_metrics(tool_name, internal_call_id);

        let call_id = internal_call_id.to_string();

        // Update the accumulated tool call with the result
        {
            let mut calls = self.tool_calls.lock().await;
            if let Some(call) = calls.iter_mut().find(|c| c.id == call_id) {
                call.result = Some(result.to_string());
                call.status = "completed".to_string();
            }
        }

        self.send(CortexChatEvent::ToolCompleted {
            tool: tool_name.to_string(),
            call_id,
            args: args.to_string(),
            result: result.to_string(),
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
    tool_calls: Option<String>,
    created_at: chrono::NaiveDateTime,
}

impl ChatMessageRow {
    fn into_message(self) -> CortexChatMessage {
        let tool_calls = self
            .tool_calls
            .as_deref()
            .and_then(|json| serde_json::from_str::<Vec<CortexChatToolCall>>(json).ok());
        CortexChatMessage {
            id: self.id,
            thread_id: self.thread_id,
            role: self.role,
            content: self.content,
            channel_context: self.channel_context,
            created_at: self.created_at.and_utc().to_rfc3339(),
            tool_calls,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CortexChatThreadRow {
    thread_id: String,
    message_count: i32,
    first_message_at: chrono::NaiveDateTime,
    last_message_at: chrono::NaiveDateTime,
    preview: String,
}

impl CortexChatThreadRow {
    fn into_thread(self) -> CortexChatThread {
        CortexChatThread {
            thread_id: self.thread_id,
            preview: self.preview,
            message_count: self.message_count as i64,
            first_message_at: self.first_message_at.and_utc().to_rfc3339(),
            last_message_at: self.last_message_at.and_utc().to_rfc3339(),
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
            "SELECT id, thread_id, role, content, channel_context, tool_calls, created_at \
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
    ///
    /// `tool_calls` is an optional JSON string of tool call data (for assistant messages).
    pub async fn save_message(
        &self,
        thread_id: &str,
        role: &str,
        content: &str,
        channel_context: Option<&str>,
        tool_calls: Option<&str>,
    ) -> Result<String, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO cortex_chat_messages (id, thread_id, role, content, channel_context, tool_calls) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(role)
        .bind(content)
        .bind(channel_context)
        .bind(tool_calls)
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

    /// List all threads with metadata (first message preview, message count, timestamps).
    pub async fn list_threads(&self) -> Result<Vec<CortexChatThread>, sqlx::Error> {
        let rows: Vec<CortexChatThreadRow> = sqlx::query_as(
            "SELECT \
                 thread_id, \
                 COUNT(*) as message_count, \
                 MIN(created_at) as first_message_at, \
                 MAX(created_at) as last_message_at, \
                 ( \
                     SELECT content FROM cortex_chat_messages m2 \
                     WHERE m2.thread_id = cortex_chat_messages.thread_id \
                     ORDER BY m2.created_at ASC LIMIT 1 \
                 ) as preview \
             FROM cortex_chat_messages \
             GROUP BY thread_id \
             ORDER BY MAX(created_at) DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|row| row.into_thread()).collect())
    }

    /// Delete all messages in a thread.
    pub async fn delete_thread(&self, thread_id: &str) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM cortex_chat_messages WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

/// The cortex chat session for a single agent.
///
/// Holds the deps, tool server, store, and a mutex to prevent concurrent sends.
/// Tracked worker entry: maps a cortex-spawned worker to the thread it was
/// spawned from so the event loop can deliver results to the right conversation.
#[derive(Debug, Clone)]
pub struct TrackedWorker {
    pub thread_id: String,
    pub channel_context: Option<String>,
}

pub struct CortexChatSession {
    pub deps: AgentDeps,
    pub tool_server: ToolServerHandle,
    pub store: CortexChatStore,
    /// Whether factory tools (agent creation/refinement) are available.
    pub factory_enabled: bool,
    /// Prevent concurrent sends — only one request at a time per agent.
    send_lock: Arc<Mutex<()>>,
    /// Shared context between the session and DetachedSpawnWorkerTool.
    cortex_ctx: crate::tools::spawn_worker::CortexChatContext,
}

impl CortexChatSession {
    pub fn new(
        deps: AgentDeps,
        tool_server: ToolServerHandle,
        store: CortexChatStore,
        cortex_ctx: crate::tools::spawn_worker::CortexChatContext,
    ) -> Self {
        Self {
            deps,
            tool_server,
            store,
            factory_enabled: false,
            send_lock: Arc::new(Mutex::new(())),
            cortex_ctx,
        }
    }

    /// Create a default `CortexChatContext` for use when building the tool server
    /// and session together. Pass the same context to both.
    pub fn create_context() -> crate::tools::spawn_worker::CortexChatContext {
        crate::tools::spawn_worker::CortexChatContext {
            current_thread_id: Arc::new(RwLock::new(None)),
            current_channel_context: Arc::new(RwLock::new(None)),
            tracked_workers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Enable factory tools for this session (agent creation/refinement).
    pub fn with_factory(mut self, enabled: bool) -> Self {
        self.factory_enabled = enabled;
        self
    }

    /// Get the cortex chat context (used by DetachedSpawnWorkerTool).
    pub fn cortex_context(&self) -> crate::tools::spawn_worker::CortexChatContext {
        self.cortex_ctx.clone()
    }

    /// Start the background event loop that listens for worker completions and
    /// auto-triggers follow-up cortex chat turns.
    pub fn start_event_loop(self: &Arc<Self>) {
        let session = Arc::clone(self);
        let mut event_rx = session.deps.event_tx.subscribe();
        let agent_id = session.deps.agent_id.clone();

        tokio::spawn(async move {
            loop {
                let event = match event_rx.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::debug!(count, "cortex chat event loop lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };

                let (worker_id, result, success) = match &event {
                    ProcessEvent::WorkerComplete {
                        agent_id: event_agent_id,
                        worker_id,
                        result,
                        success,
                        ..
                    } if *event_agent_id == agent_id => (*worker_id, result.clone(), *success),
                    _ => continue,
                };

                let tracked: Option<TrackedWorker> = session
                    .cortex_ctx
                    .tracked_workers
                    .write()
                    .await
                    .remove(&worker_id);
                let Some(tracked) = tracked else {
                    continue;
                };

                // Persist completion to worker_runs (no channel handler does this
                // for channel_id: None workers).
                let run_logger = ProcessRunLogger::new(session.deps.sqlite_pool.clone());
                run_logger.log_worker_completed(worker_id, &result, success);

                tracing::info!(
                    %worker_id,
                    thread_id = %tracked.thread_id,
                    success,
                    "cortex chat worker completed, auto-triggering follow-up"
                );

                let status_label = if success { "completed" } else { "failed" };
                let retrigger_message = format!("[Worker {worker_id} {status_label}]\n\n{result}");

                let channel_ref = tracked.channel_context.as_deref();

                // The send_lock may be held if the admin is mid-conversation.
                // Wait for it rather than dropping the result.
                let event_rx = match session
                    .send_message_blocking(&tracked.thread_id, &retrigger_message, channel_ref)
                    .await
                {
                    Ok(rx) => rx,
                    Err(error) => {
                        tracing::warn!(
                            %worker_id,
                            %error,
                            "failed to auto-trigger cortex chat for worker result"
                        );
                        continue;
                    }
                };

                // Drain the event stream and emit the final response through the
                // ProcessEvent pipeline so the frontend SSE picks it up.
                Self::drain_auto_trigger_events(event_rx, &session.deps, &tracked.thread_id).await;
            }
        });
    }

    /// Drain events from an auto-triggered cortex chat turn and emit the final
    /// response as a `ProcessEvent::CortexChatUpdate`.
    async fn drain_auto_trigger_events(
        mut event_rx: mpsc::Receiver<CortexChatEvent>,
        deps: &AgentDeps,
        thread_id: &str,
    ) {
        while let Some(event) = event_rx.recv().await {
            match event {
                CortexChatEvent::Done {
                    full_text,
                    tool_calls,
                } => {
                    let tool_calls_json = if tool_calls.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&tool_calls).ok()
                    };
                    let _ = deps.event_tx.send(ProcessEvent::CortexChatUpdate {
                        agent_id: deps.agent_id.clone(),
                        thread_id: thread_id.to_string(),
                        content: full_text,
                        tool_calls_json,
                    });
                }
                CortexChatEvent::Error { message } => {
                    tracing::warn!(
                        thread_id,
                        error = %message,
                        "cortex chat auto-trigger turn errored"
                    );
                }
                _ => {}
            }
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
        self.send_message_impl(send_guard, thread_id, user_text, channel_context_id)
            .await
    }

    /// Like `send_message_with_events` but waits for the send lock instead of
    /// returning `Busy`. Used by the background event loop for auto-triggered turns.
    /// The input message is NOT persisted — only the assistant's synthesis gets saved.
    async fn send_message_blocking(
        self: &Arc<Self>,
        thread_id: &str,
        user_text: &str,
        channel_context_id: Option<&str>,
    ) -> std::result::Result<mpsc::Receiver<CortexChatEvent>, CortexChatSendError> {
        let send_guard = self.send_lock.clone().lock_owned().await;
        self.send_message_inner(send_guard, thread_id, user_text, channel_context_id, false)
            .await
    }

    async fn send_message_impl(
        self: &Arc<Self>,
        send_guard: tokio::sync::OwnedMutexGuard<()>,
        thread_id: &str,
        user_text: &str,
        channel_context_id: Option<&str>,
    ) -> std::result::Result<mpsc::Receiver<CortexChatEvent>, CortexChatSendError> {
        self.send_message_inner(send_guard, thread_id, user_text, channel_context_id, true)
            .await
    }

    /// Core send implementation. When `persist_input` is false the incoming
    /// message is sent to the LLM but not saved to the thread — used for
    /// auto-triggered worker results where only the assistant's synthesis
    /// should appear in history.
    async fn send_message_inner(
        self: &Arc<Self>,
        send_guard: tokio::sync::OwnedMutexGuard<()>,
        thread_id: &str,
        user_text: &str,
        channel_context_id: Option<&str>,
        persist_input: bool,
    ) -> std::result::Result<mpsc::Receiver<CortexChatEvent>, CortexChatSendError> {
        // Update the shared context so DetachedSpawnWorkerTool knows which
        // thread to associate spawned workers with.
        *self.cortex_ctx.current_thread_id.write().await = Some(thread_id.to_string());
        *self.cortex_ctx.current_channel_context.write().await =
            channel_context_id.map(|s| s.to_string());

        if persist_input {
            self.store
                .save_message(thread_id, "user", user_text, channel_context_id, None)
                .await?;
        }

        // Build the system prompt
        let system_prompt = self.build_system_prompt(channel_context_id).await?;

        // Load chat history and convert to Rig messages.
        // When we persisted the input, the last message in history is the one
        // we just saved — skip it since Rig adds it via `agent.prompt()`.
        // When the input wasn't persisted (auto-triggered turns), include all
        // messages since none of them duplicate the prompt.
        let chat_messages = self.store.load_history(thread_id, 100).await?;
        let history_end = if persist_input {
            chat_messages.len().saturating_sub(1)
        } else {
            chat_messages.len()
        };
        let mut history: Vec<rig::message::Message> = Vec::new();
        for message in &chat_messages[..history_end] {
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
        let tool_calls = Arc::new(Mutex::new(Vec::new()));
        let hook = CortexChatHook::new(event_tx.clone(), spacebot_hook, tool_calls.clone());

        // Clone what the spawned task needs
        let user_text = user_text.to_string();
        let thread_id = thread_id.to_string();
        let channel_context_id = channel_context_id.map(|s| s.to_string());
        let store = self.store.clone();
        // Cortex chat is an interactive admin session that can do complex multi-step
        // work (agent creation, memory audits, etc). Use worker_timeout_secs (default
        // 600s) rather than branch_timeout_secs (60s) which is far too short.
        let prompt_timeout = Duration::from_secs(
            self.deps
                .runtime_config
                .cortex
                .load()
                .worker_timeout_secs
                .max(60),
        );

        tokio::spawn(async move {
            let _send_guard = send_guard;
            let channel_ref = channel_context_id.as_deref();
            let prompt_result = tokio::time::timeout(
                prompt_timeout,
                agent
                    .prompt(&user_text)
                    .with_hook(hook.clone())
                    .with_history(&mut history),
            )
            .await;

            match prompt_result {
                Ok(Ok(response)) => {
                    let accumulated_tool_calls = tool_calls.lock().await.clone();
                    let tool_calls_json = if accumulated_tool_calls.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&accumulated_tool_calls).ok()
                    };
                    let _ = store
                        .save_message(
                            &thread_id,
                            "assistant",
                            &response,
                            channel_ref,
                            tool_calls_json.as_deref(),
                        )
                        .await;
                    let _ = event_tx
                        .send(CortexChatEvent::Done {
                            full_text: response,
                            tool_calls: accumulated_tool_calls,
                        })
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
        let mcp_tool_names = self.deps.mcp_manager.get_tool_names().await;
        let worker_capabilities = prompt_engine.render_worker_capabilities(
            browser_enabled,
            web_search_enabled,
            opencode_enabled,
            &mcp_tool_names,
        )?;

        // Load channel transcript if a channel context is active
        let channel_transcript = if let Some(channel_id) = channel_context_id {
            self.load_channel_transcript(channel_id).await
        } else {
            None
        };

        let empty_to_none = |s: String| if s.is_empty() { None } else { Some(s) };
        let routing = runtime_config.routing.load();
        let model_name = routing.resolve(ProcessType::Cortex, None).to_string();
        let tool_use_enforcement = runtime_config.tool_use_enforcement.load();

        let system_prompt = prompt_engine.render_cortex_chat_prompt(
            empty_to_none(identity_context),
            empty_to_none(memory_bulletin.to_string()),
            channel_transcript,
            empty_to_none(agents_manifest),
            empty_to_none(changelog_highlights),
            empty_to_none(runtime_config_snapshot),
            worker_capabilities,
            self.factory_enabled,
        )?;

        prompt_engine.maybe_append_tool_use_enforcement(
            system_prompt,
            tool_use_enforcement.as_ref(),
            &model_name,
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
                        crate::conversation::history::TimelineItem::ToolCallRun {
                            tool_name,
                            result,
                            ..
                        } => {
                            if let Some(result) = result {
                                transcript
                                    .push_str(&format!("*[Tool: {tool_name}]*: {result}\n\n"));
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
    use super::{CortexChatSendError, try_acquire_send_lock};
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
}
