//! OpenCode worker: drives an OpenCode session for coding tasks.
//!
//! Instead of running a Rig agent loop with shell/file tools, this worker
//! delegates to an OpenCode subprocess that has its own codebase exploration,
//! context management, and tool suite. Communication happens over HTTP + SSE.

use crate::opencode::server::OpenCodeServerPool;
use crate::opencode::types::*;
use crate::secrets::store::SecretsStore;
use crate::{AgentId, ChannelId, ProcessEvent, WorkerId};

use anyhow::{Context as _, bail};
use futures::StreamExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, mpsc};
use uuid::Uuid;

/// State for resuming an idle OpenCode session after restart.
pub struct ResumeSession {
    pub session_id: String,
    pub accumulated_parts: Vec<OpenCodePart>,
    pub tool_calls: i64,
}

/// An OpenCode-backed worker that drives a coding session via subprocess.
pub struct OpenCodeWorker {
    pub id: WorkerId,
    pub channel_id: Option<ChannelId>,
    pub agent_id: AgentId,
    pub task: String,
    pub directory: PathBuf,
    pub server_pool: Arc<OpenCodeServerPool>,
    pub event_tx: broadcast::Sender<ProcessEvent>,
    /// Input channel for interactive follow-ups (permissions, questions, user messages).
    pub input_rx: Option<mpsc::Receiver<String>>,
    /// System prompt injected into each OpenCode prompt.
    pub system_prompt: Option<String>,
    /// Model override (provider/model format like "anthropic/claude-sonnet-4").
    pub model: Option<String>,
    /// Secrets store for exact-match scrubbing of tool secret values in SSE output.
    pub secrets_store: Option<Arc<SecretsStore>>,
    /// SQLite pool for incremental transcript persistence (set by channel_dispatch).
    pub sqlite_pool: Option<sqlx::SqlitePool>,
    /// Pre-populated session state for resumed workers (set by `resume_interactive`).
    pub resuming_session: Option<ResumeSession>,
}

/// Accumulated state from SSE event processing.
struct EventState {
    /// The most recent text part (used for status/initial result delivery).
    last_text: String,
    /// Currently running tool name.
    current_tool: Option<String>,
    /// Number of tool calls observed (for status reporting).
    tool_calls: i64,
    /// Guards: don't treat session.idle as completion until we've seen real work.
    has_received_event: bool,
    has_assistant_message: bool,
    /// Accumulated OpenCode parts from SSE events, used as a fallback transcript
    /// source when the post-completion `get_messages()` API call fails.
    accumulated_parts: Vec<OpenCodePart>,
}

impl EventState {
    fn new() -> Self {
        Self {
            last_text: String::new(),
            current_tool: None,
            tool_calls: 0,
            has_received_event: false,
            has_assistant_message: false,
            accumulated_parts: Vec::new(),
        }
    }
}

/// Result of an OpenCode worker run.
pub struct OpenCodeWorkerResult {
    pub session_id: String,
    pub result_text: String,
    /// Transcript steps converted from the OpenCode messages API on completion.
    pub transcript: Vec<crate::conversation::worker_transcript::TranscriptStep>,
    /// Number of tool calls observed during the session.
    pub tool_calls: i64,
}

impl OpenCodeWorker {
    /// Create a new OpenCode worker.
    pub fn new(
        channel_id: Option<ChannelId>,
        agent_id: AgentId,
        task: impl Into<String>,
        directory: PathBuf,
        server_pool: Arc<OpenCodeServerPool>,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            channel_id,
            agent_id,
            task: task.into(),
            directory,
            server_pool,
            event_tx,
            input_rx: None,
            system_prompt: None,
            model: None,
            secrets_store: None,
            sqlite_pool: None,
            resuming_session: None,
        }
    }

    /// Create an interactive OpenCode worker that accepts follow-up messages.
    pub fn new_interactive(
        channel_id: Option<ChannelId>,
        agent_id: AgentId,
        task: impl Into<String>,
        directory: PathBuf,
        server_pool: Arc<OpenCodeServerPool>,
        event_tx: broadcast::Sender<ProcessEvent>,
    ) -> (Self, mpsc::Sender<String>) {
        let (input_tx, input_rx) = mpsc::channel(32);
        let mut worker = Self::new(channel_id, agent_id, task, directory, server_pool, event_tx);
        worker.input_rx = Some(input_rx);
        (worker, input_tx)
    }

    /// Set the system prompt injected into OpenCode prompts.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Set the model to use for this worker.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the secrets store for exact-match scrubbing of tool secret values.
    pub fn with_secrets_store(mut self, store: Arc<SecretsStore>) -> Self {
        self.secrets_store = Some(store);
        self
    }

    /// Set the SQLite pool for incremental transcript persistence.
    pub fn with_sqlite_pool(mut self, pool: sqlx::SqlitePool) -> Self {
        self.sqlite_pool = Some(pool);
        self
    }

    /// Create a resumed interactive OpenCode worker for an idle session.
    ///
    /// Instead of creating a new session, reconnects to `session_id` on the
    /// existing OpenCode server. The prior transcript (from the DB blob) is
    /// loaded into `accumulated_parts` so subsequent `persist_transcript_snapshot`
    /// calls produce a complete history.
    ///
    /// Returns `None` if reconnection fails (server dead, session gone).
    #[allow(clippy::too_many_arguments)]
    pub async fn resume_interactive(
        existing_id: WorkerId,
        channel_id: Option<ChannelId>,
        agent_id: AgentId,
        task: impl Into<String>,
        directory: PathBuf,
        server_pool: Arc<OpenCodeServerPool>,
        event_tx: broadcast::Sender<ProcessEvent>,
        session_id: String,
        _prior_transcript_blob: Option<Vec<u8>>,
    ) -> Option<(Self, mpsc::Sender<String>)> {
        // Try to reconnect to the OpenCode server for this directory.
        let server = match server_pool.get_or_create(&directory).await {
            Ok(server) => server,
            Err(error) => {
                tracing::warn!(
                    worker_id = %existing_id,
                    %error,
                    directory = %directory.display(),
                    "failed to reconnect to OpenCode server for idle worker"
                );
                return None;
            }
        };

        // Verify the session still exists by fetching its messages.
        let messages = {
            let guard = server.lock().await;
            guard.get_messages(&session_id).await
        };
        if let Err(error) = &messages {
            tracing::warn!(
                worker_id = %existing_id,
                %error,
                session_id = %session_id,
                "OpenCode session no longer exists, cannot resume"
            );
            return None;
        }

        // Reconstruct accumulated_parts from the session messages (preferred)
        // or from the persisted transcript blob (fallback).
        let accumulated_parts = if let Ok(messages) = &messages {
            // Re-parse the parts from the session messages API.
            // This gives us the authoritative state.
            let mut parts = Vec::new();
            for message in messages {
                if let Some(msg_parts) = message.get("parts").and_then(|p| p.as_array()) {
                    for part_value in msg_parts {
                        if let Ok(part) = serde_json::from_value::<OpenCodePart>(part_value.clone())
                        {
                            parts.push(part);
                        }
                    }
                }
            }
            parts
        } else {
            Vec::new()
        };

        // Count tool calls from accumulated parts
        let tool_calls = accumulated_parts
            .iter()
            .filter(|p| matches!(p, OpenCodePart::Tool { .. }))
            .count() as i64;

        let (input_tx, input_rx) = mpsc::channel(32);
        let mut worker = Self::new(channel_id, agent_id, task, directory, server_pool, event_tx);
        worker.id = existing_id;
        worker.input_rx = Some(input_rx);
        worker.resuming_session = Some(ResumeSession {
            session_id,
            accumulated_parts,
            tool_calls,
        });

        Some((worker, input_tx))
    }

    /// Scrub tool secret values from text, replacing each with `[REDACTED:<name>]`.
    /// Returns the scrubbed text. If no secrets store is set, returns the input unchanged.
    fn scrub_text(&self, text: &str) -> String {
        match &self.secrets_store {
            Some(store) => crate::secrets::scrub::scrub_with_store(text, store, &self.agent_id),
            None => text.to_string(),
        }
    }

    /// Run the worker: spawn/reuse an OpenCode server, create a session,
    /// send the task, monitor via SSE, and return the result.
    pub async fn run(mut self) -> anyhow::Result<OpenCodeWorkerResult> {
        let resuming = self.resuming_session.is_some();

        // --- Session setup: either resume an existing session or create a new one ---
        let (server, session_id, mut event_state, result_text) =
            if let Some(resume) = self.resuming_session.take() {
                // Resumed worker: reconnect to the existing server + session.
                self.send_status("reconnecting to OpenCode session");

                let server = self
                    .server_pool
                    .get_or_create(&self.directory)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to reconnect to OpenCode server for '{}'",
                            self.directory.display()
                        )
                    })?;

                let opencode_port = {
                    let guard = server.lock().await;
                    guard.port()
                };

                // Re-emit session metadata so the frontend can show the embed.
                self.event_tx
                    .send(ProcessEvent::OpenCodeSessionCreated {
                        agent_id: self.agent_id.clone(),
                        worker_id: self.id,
                        channel_id: self.channel_id.clone(),
                        session_id: resume.session_id.clone(),
                        port: opencode_port,
                    })
                    .ok();

                tracing::info!(
                    worker_id = %self.id,
                    session_id = %resume.session_id,
                    port = opencode_port,
                    prior_parts = resume.accumulated_parts.len(),
                    "resumed OpenCode worker, reconnected to session"
                );

                let mut event_state = EventState::new();
                event_state.accumulated_parts = resume.accumulated_parts;
                event_state.tool_calls = resume.tool_calls;
                event_state.has_received_event = true;
                event_state.has_assistant_message = true;

                (server, resume.session_id, event_state, String::new())
            } else {
                // Fresh worker: create a new server + session.
                self.send_status("starting OpenCode server");

                let server = self
                    .server_pool
                    .get_or_create(&self.directory)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to get OpenCode server for '{}'",
                            self.directory.display()
                        )
                    })?;

                self.send_status("creating session");

                let session = {
                    let guard = server.lock().await;
                    guard
                        .create_session(Some(format!("spacebot-worker-{}", self.id)))
                        .await?
                };
                let session_id = session.id.clone();

                let opencode_port = {
                    let guard = server.lock().await;
                    guard.port()
                };
                self.event_tx
                    .send(ProcessEvent::OpenCodeSessionCreated {
                        agent_id: self.agent_id.clone(),
                        worker_id: self.id,
                        channel_id: self.channel_id.clone(),
                        session_id: session_id.clone(),
                        port: opencode_port,
                    })
                    .ok();

                tracing::info!(
                    worker_id = %self.id,
                    session_id = %session_id,
                    port = opencode_port,
                    directory = %self.directory.display(),
                    "OpenCode session created"
                );

                // Subscribe to SSE events before sending the prompt
                let event_response = {
                    let guard = server.lock().await;
                    guard.subscribe_events().await?
                };

                let model_param = self.model.as_ref().and_then(|m| parse_model_param(m));
                let prompt_request = SendPromptRequest {
                    parts: vec![PartInput::Text {
                        text: self.task.clone(),
                        synthetic: None,
                    }],
                    system: self.system_prompt.clone(),
                    model: model_param,
                    agent: None,
                };

                self.send_status("sending task to OpenCode");
                {
                    let guard = server.lock().await;
                    guard
                        .send_prompt_async(&session_id, &prompt_request)
                        .await?;
                }

                let mut event_state = EventState::new();
                self.process_events(event_response, &session_id, &server, &mut event_state)
                    .await?;

                let result_text = event_state.last_text.clone();
                (server, session_id, event_state, result_text)
            };

        // Interactive follow-up loop
        if let Some(mut input_rx) = self.input_rx.take() {
            if resuming {
                // Resumed worker: go straight to idle without emitting initial result
                // (it was already relayed before the restart). Persist the recovered
                // transcript so a second crash doesn't lose it.
                self.persist_transcript_snapshot(&event_state).await;
                self.send_status("resumed — waiting for follow-up");
                self.send_idle();
            } else {
                // Fresh worker: emit the initial result so the channel can retrigger.
                let scrubbed_result = self.scrub_text(&result_text);
                let scrubbed_result = crate::secrets::scrub::scrub_leaks(&scrubbed_result);
                let _ = self.event_tx.send(ProcessEvent::WorkerInitialResult {
                    agent_id: self.agent_id.clone(),
                    worker_id: self.id,
                    channel_id: self.channel_id.clone(),
                    result: scrubbed_result,
                });

                self.persist_transcript_snapshot(&event_state).await;
                self.send_status("waiting for follow-up");
                self.send_idle();
            }

            while let Some(follow_up) = input_rx.recv().await {
                self.send_status("processing follow-up");

                // Subscribe to fresh events for the follow-up
                let event_response = {
                    let guard = server.lock().await;
                    guard.subscribe_events().await?
                };

                let follow_up_request = SendPromptRequest {
                    parts: vec![PartInput::Text {
                        text: follow_up,
                        synthetic: None,
                    }],
                    system: self.system_prompt.clone(),
                    model: self.model.as_ref().and_then(|m| parse_model_param(m)),
                    agent: None,
                };

                {
                    let guard = server.lock().await;
                    guard
                        .send_prompt_async(&session_id, &follow_up_request)
                        .await?;
                }

                match self
                    .process_events(event_response, &session_id, &server, &mut event_state)
                    .await
                {
                    Ok(_) => {
                        // Emit follow-up result so the channel can retrigger
                        // and relay this to the user — same as initial result.
                        let follow_up_text = event_state.last_text.clone();
                        if !follow_up_text.is_empty() {
                            let scrubbed = self.scrub_text(&follow_up_text);
                            let scrubbed = crate::secrets::scrub::scrub_leaks(&scrubbed);
                            let _ = self.event_tx.send(ProcessEvent::WorkerInitialResult {
                                agent_id: self.agent_id.clone(),
                                worker_id: self.id,
                                channel_id: self.channel_id.clone(),
                                result: scrubbed,
                            });
                        }
                        self.persist_transcript_snapshot(&event_state).await;
                        self.send_status("waiting for follow-up");
                        self.send_idle();
                    }
                    Err(error) => {
                        tracing::error!(
                            worker_id = %self.id,
                            %error,
                            "OpenCode follow-up failed"
                        );
                        self.send_status("failed");
                        break;
                    }
                }
            }
        }

        self.send_status("completed");

        // Fetch the full message history from the OpenCode API and convert
        // to TranscriptStep[] for persistence + extract all assistant text
        // as the definitive result_text.
        let (transcript, api_result_text) =
            match server.lock().await.get_messages(&session_id).await {
                Ok(messages) => {
                    let (steps, all_text) =
                        crate::conversation::worker_transcript::convert_opencode_messages(
                            &messages,
                        );
                    (
                        steps,
                        if all_text.is_empty() {
                            None
                        } else {
                            Some(all_text)
                        },
                    )
                }
                Err(error) => {
                    // API call failed (server recycled, process exited, etc.).
                    // Fall back to the OpenCodeParts accumulated from SSE events
                    // during the session — better than losing the transcript entirely.
                    let fallback_steps =
                        crate::conversation::worker_transcript::convert_opencode_parts(
                            &event_state.accumulated_parts,
                        );
                    tracing::warn!(
                        worker_id = %self.id,
                        %error,
                        fallback_steps = fallback_steps.len(),
                        "failed to fetch OpenCode messages for transcript, using SSE fallback"
                    );
                    (fallback_steps, None)
                }
            };

        // Prefer API-fetched result text, fall back to SSE last_text
        let final_result_text = api_result_text.unwrap_or(result_text);

        tracing::info!(
            worker_id = %self.id,
            session_id = %session_id,
            transcript_steps = transcript.len(),
            "OpenCode worker completed"
        );

        Ok(OpenCodeWorkerResult {
            session_id,
            result_text: final_result_text,
            transcript,
            tool_calls: event_state.tool_calls,
        })
    }

    /// Process SSE events from the OpenCode event stream until the session
    /// goes idle or encounters an error.
    async fn process_events(
        &self,
        response: reqwest::Response,
        session_id: &str,
        server: &Arc<Mutex<crate::opencode::server::OpenCodeServer>>,
        event_state: &mut EventState,
    ) -> anyhow::Result<String> {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        loop {
            let chunk = tokio::select! {
                chunk = stream.next() => chunk,
                _ = tokio::time::sleep(std::time::Duration::from_secs(600)) => {
                    bail!("OpenCode session timed out after 10 minutes of inactivity");
                }
            };

            let Some(chunk) = chunk else {
                // Stream ended -- if we have results, return them
                if event_state.has_assistant_message && !event_state.last_text.is_empty() {
                    return Ok(event_state.last_text.clone());
                }
                bail!("OpenCode event stream ended before session completed");
            };

            let bytes = chunk.context("failed to read SSE chunk")?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Parse SSE lines from buffer
            while let Some(event) = extract_sse_event(&mut buffer) {
                match self
                    .handle_sse_event(&event, session_id, server, event_state)
                    .await
                {
                    EventAction::Continue => {}
                    EventAction::Complete => return Ok(event_state.last_text.clone()),
                    EventAction::Error(message) => bail!("OpenCode session error: {message}"),
                }
            }
        }
    }

    /// Handle a single SSE event. Returns whether to continue, complete, or error.
    async fn handle_sse_event(
        &self,
        event: &SseEvent,
        session_id: &str,
        server: &Arc<Mutex<crate::opencode::server::OpenCodeServer>>,
        state: &mut EventState,
    ) -> EventAction {
        match event {
            SseEvent::MessageUpdated { info } => {
                state.has_received_event = true;
                // Track assistant messages for idle guard
                if let Some(msg) = info
                    && msg.role == "assistant"
                    && let Some(sid) = &msg.session_id
                    && sid == session_id
                {
                    state.has_assistant_message = true;
                }
                EventAction::Continue
            }

            SseEvent::MessagePartUpdated { part, .. } => {
                state.has_received_event = true;

                // Filter out parts from other sessions
                let part_session_id = match part {
                    Part::Text { session_id: s, .. } => s.as_deref(),
                    Part::Tool { session_id: s, .. } => s.as_deref(),
                    Part::StepStart { session_id: s, .. } => s.as_deref(),
                    Part::StepFinish { session_id: s, .. } => s.as_deref(),
                    Part::Other => None,
                };
                if let Some(sid) = part_session_id
                    && sid != session_id
                {
                    return EventAction::Continue;
                }

                // Emit OpenCodePartUpdated for the frontend live transcript
                // and accumulate for fallback transcript persistence.
                if let Some(opencode_part) = part_to_opencode_part(part) {
                    let _ = self.event_tx.send(ProcessEvent::OpenCodePartUpdated {
                        agent_id: self.agent_id.clone(),
                        worker_id: self.id,
                        part: opencode_part.clone(),
                    });
                    state.accumulated_parts.push(opencode_part);
                }

                // Continue processing for status updates and state tracking
                match part {
                    Part::Text { text, .. } => {
                        state.has_assistant_message = true;

                        // Exact-match scrubbing for leak detection
                        let scrubbed = self.scrub_text(text);
                        if let Some(leak) = crate::secrets::scrub::scan_for_leaks(&scrubbed) {
                            tracing::warn!(
                                worker_id = %self.id,
                                leak_prefix = %&leak[..leak.len().min(8)],
                                "potential secret detected in OpenCode worker output"
                            );
                        }

                        state.last_text = scrubbed;
                    }
                    Part::Tool {
                        tool,
                        state: tool_state,
                        ..
                    } => {
                        state.has_assistant_message = true;
                        if let Some(tool_name) = tool
                            && let Some(tool_state) = tool_state
                        {
                            match tool_state {
                                ToolState::Running { title, input, .. } => {
                                    state.current_tool = Some(tool_name.clone());
                                    state.tool_calls += 1;
                                    let label = title
                                        .as_deref()
                                        .map(String::from)
                                        .or_else(|| describe_tool_input(tool_name, input.as_ref()))
                                        .unwrap_or_else(|| tool_name.clone());
                                    self.send_status(&format!("running: {label}"));
                                }
                                ToolState::Completed { output, title, .. } => {
                                    // Scrub and log potential secret-pattern hits
                                    if let Some(output) = output {
                                        let scrubbed = self.scrub_text(output);
                                        if let Some(leak) =
                                            crate::secrets::scrub::scan_for_leaks(&scrubbed)
                                        {
                                            tracing::warn!(
                                                worker_id = %self.id,
                                                tool = %tool_name,
                                                leak_prefix = %&leak[..leak.len().min(8)],
                                                "potential secret detected in OpenCode tool output"
                                            );
                                        }
                                    }

                                    if state.current_tool.as_deref() == Some(tool_name.as_str()) {
                                        state.current_tool = None;
                                    }
                                    let done_label = title
                                        .as_deref()
                                        .filter(|t| !t.is_empty())
                                        .unwrap_or(tool_name.as_str());
                                    self.send_status(&format!("done: {done_label}"));
                                }
                                ToolState::Error { error, .. } => {
                                    let description = error.as_deref().unwrap_or("unknown");
                                    self.send_status(&format!(
                                        "tool error: {tool_name}: {description}"
                                    ));
                                }
                                ToolState::Pending { .. } => {
                                    // Tool queued, no status update needed
                                }
                            }
                        }
                    }
                    _ => {}
                }
                EventAction::Continue
            }

            SseEvent::SessionIdle {
                session_id: event_session_id,
            } => {
                if event_session_id != session_id {
                    return EventAction::Continue;
                }

                // Guard: don't complete until we've seen actual work.
                // OpenCode can send an early idle event before the prompt is processed.
                if !state.has_received_event || !state.has_assistant_message {
                    tracing::trace!(
                        worker_id = %self.id,
                        has_received_event = state.has_received_event,
                        has_assistant_message = state.has_assistant_message,
                        "ignoring early session.idle"
                    );
                    return EventAction::Continue;
                }

                EventAction::Complete
            }

            SseEvent::SessionError {
                session_id: event_session_id,
                error,
            } => {
                if event_session_id.as_deref() != Some(session_id) {
                    return EventAction::Continue;
                }
                let message = error
                    .as_ref()
                    .and_then(|e| e.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error")
                    .to_string();
                EventAction::Error(message)
            }

            SseEvent::PermissionAsked(permission) => {
                if permission.session_id != session_id {
                    return EventAction::Continue;
                }

                tracing::info!(
                    worker_id = %self.id,
                    permission_id = %permission.id,
                    permission_type = ?permission.permission,
                    patterns = ?permission.patterns,
                    "OpenCode requesting permission"
                );

                let _ = self.event_tx.send(ProcessEvent::WorkerPermission {
                    agent_id: self.agent_id.clone(),
                    worker_id: self.id,
                    channel_id: self.channel_id.clone(),
                    permission_id: permission.id.clone(),
                    description: format!(
                        "{}: {}",
                        permission.permission.as_deref().unwrap_or("unknown"),
                        permission.patterns.join(", ")
                    ),
                    patterns: permission.patterns.clone(),
                });

                // Auto-allow (OPENCODE_CONFIG_CONTENT should prevent most prompts)
                let guard = server.lock().await;
                if let Err(error) = guard
                    .reply_permission(&permission.id, PermissionReply::Once)
                    .await
                {
                    tracing::warn!(
                        worker_id = %self.id,
                        permission_id = %permission.id,
                        %error,
                        "failed to auto-reply permission"
                    );
                }

                EventAction::Continue
            }

            SseEvent::QuestionAsked(question) => {
                if question.session_id != session_id {
                    return EventAction::Continue;
                }

                tracing::info!(
                    worker_id = %self.id,
                    question_id = %question.id,
                    question_count = question.questions.len(),
                    "OpenCode asking question"
                );

                let _ = self.event_tx.send(ProcessEvent::WorkerQuestion {
                    agent_id: self.agent_id.clone(),
                    worker_id: self.id,
                    channel_id: self.channel_id.clone(),
                    question_id: question.id.clone(),
                    questions: question
                        .questions
                        .iter()
                        .map(|q| QuestionInfo {
                            question: q.question.clone(),
                            header: q.header.clone(),
                            options: q.options.clone(),
                        })
                        .collect(),
                });

                // Auto-select first option
                let answers: Vec<QuestionAnswer> = question
                    .questions
                    .iter()
                    .map(|q| {
                        if let Some(first_option) = q.options.first() {
                            QuestionAnswer {
                                label: first_option.label.clone(),
                                description: first_option.description.clone(),
                            }
                        } else {
                            QuestionAnswer {
                                label: "continue".to_string(),
                                description: None,
                            }
                        }
                    })
                    .collect();

                let guard = server.lock().await;
                if let Err(error) = guard.reply_question(&question.id, answers).await {
                    tracing::warn!(
                        worker_id = %self.id,
                        question_id = %question.id,
                        %error,
                        "failed to auto-reply question"
                    );
                }

                EventAction::Continue
            }

            SseEvent::SessionStatus {
                session_id: event_session_id,
                status,
            } => {
                if event_session_id != session_id {
                    return EventAction::Continue;
                }
                match status {
                    SessionStatusPayload::Retry {
                        attempt, message, ..
                    } => {
                        let description = message.as_deref().unwrap_or("rate limited");
                        self.send_status(&format!("retry attempt {attempt}: {description}"));
                    }
                    SessionStatusPayload::Busy => {
                        self.send_status("working");
                    }
                    SessionStatusPayload::Idle => {}
                }
                EventAction::Continue
            }

            _ => EventAction::Continue,
        }
    }

    /// Send a status update via the process event bus.
    fn send_status(&self, status: &str) {
        let _ = self.event_tx.send(ProcessEvent::WorkerStatus {
            agent_id: self.agent_id.clone(),
            worker_id: self.id,
            channel_id: self.channel_id.clone(),
            status: status.to_string(),
        });
    }

    /// Send an idle event to mark this worker as waiting for follow-up input.
    fn send_idle(&self) {
        let _ = self.event_tx.send(ProcessEvent::WorkerIdle {
            agent_id: self.agent_id.clone(),
            worker_id: self.id,
            channel_id: self.channel_id.clone(),
        });
    }

    /// Persist a snapshot of the transcript built from accumulated SSE parts.
    ///
    /// Called each time the worker goes idle so that if spacebot restarts
    /// while the worker is waiting for follow-up, the transcript survives.
    /// Awaited directly so "idle implies persisted" — no out-of-order writes.
    async fn persist_transcript_snapshot(&self, event_state: &EventState) {
        let Some(pool) = &self.sqlite_pool else {
            return;
        };
        if event_state.accumulated_parts.is_empty() {
            return;
        }

        let steps = crate::conversation::worker_transcript::convert_opencode_parts(
            &event_state.accumulated_parts,
        );
        if steps.is_empty() {
            return;
        }

        let blob = crate::conversation::worker_transcript::serialize_steps(&steps);
        let tool_calls = event_state.tool_calls;
        let worker_id = self.id.to_string();

        if let Err(error) =
            sqlx::query("UPDATE worker_runs SET transcript = ?, tool_calls = ? WHERE id = ?")
                .bind(&blob)
                .bind(tool_calls)
                .bind(&worker_id)
                .execute(pool)
                .await
        {
            tracing::warn!(%error, worker_id, "failed to persist transcript snapshot");
        }
    }
}

/// Extract a human-readable description from a tool's input JSON.
///
/// OpenCode tool inputs have well-known shapes (e.g. `read` has `filePath`,
/// `bash` has `description` and `command`, `grep` has `pattern`). When the
/// running-state `title` is absent we derive a label from the input fields
/// so the transcript shows "reading src/main.rs" instead of "running: read".
fn describe_tool_input(tool_name: &str, input: Option<&serde_json::Value>) -> Option<String> {
    let input = input?.as_object()?;
    match tool_name {
        "read" | "write" | "edit" => {
            let file_path = input.get("filePath")?.as_str()?;
            // Show just the last 3 path components to keep it short
            let short = short_path(file_path);
            Some(short.to_string())
        }
        "bash" => {
            // Prefer the LLM-provided description, fall back to command
            if let Some(description) = input.get("description").and_then(|v| v.as_str())
                && !description.is_empty()
            {
                return Some(truncate_status(description, 80));
            }
            let command = input.get("command")?.as_str()?;
            Some(truncate_status(command, 60))
        }
        "glob" => {
            let pattern = input.get("pattern")?.as_str()?;
            Some(format!("glob {pattern}"))
        }
        "grep" => {
            let pattern = input.get("pattern")?.as_str()?;
            Some(format!("search \"{pattern}\""))
        }
        "task" => {
            let description = input.get("description")?.as_str()?;
            Some(truncate_status(description, 80))
        }
        _ => None,
    }
}

/// Shorten an absolute file path to at most the last 3 components.
fn short_path(path: &str) -> &str {
    let mut count = 0;
    for (idx, byte) in path.bytes().enumerate().rev() {
        if byte == b'/' {
            count += 1;
            if count == 3 {
                return &path[idx + 1..];
            }
        }
    }
    path
}

/// Truncate a status string to `max` characters, appending "…" if trimmed.
fn truncate_status(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        let boundary = text
            .char_indices()
            .nth(max.saturating_sub(1))
            .map(|(idx, _)| idx)
            .unwrap_or(max);
        format!("{}…", &text[..boundary])
    }
}

/// Result of processing a single SSE event.
enum EventAction {
    Continue,
    Complete,
    Error(String),
}

/// Parse an SSE event from a buffer. Parses the `{ type, properties }` envelope
/// and converts to our `SseEvent` enum. Returns None if no complete event is available.
fn extract_sse_event(buffer: &mut String) -> Option<SseEvent> {
    // SSE format: lines starting with "data: " followed by JSON, terminated by
    // a blank line. We may also see "event:" and "id:" lines which we ignore.
    loop {
        let double_newline = buffer.find("\n\n")?;
        let block = buffer[..double_newline].to_string();
        *buffer = buffer[double_newline + 2..].to_string();

        // Extract all data lines from the block
        let mut data_parts = Vec::new();
        for line in block.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                data_parts.push(data);
            } else if let Some(data) = line.strip_prefix("data:") {
                data_parts.push(data);
            }
        }

        if data_parts.is_empty() {
            continue;
        }

        let json_str = data_parts.join("\n");
        if json_str.is_empty() {
            continue;
        }

        // Parse the envelope first, then convert to our event type
        match serde_json::from_str::<SseEventEnvelope>(&json_str) {
            Ok(envelope) => return Some(SseEvent::from_envelope(envelope)),
            Err(error) => {
                tracing::trace!(
                    %error,
                    json = %json_str,
                    "failed to parse SSE event envelope, skipping"
                );
                continue;
            }
        }
    }
}

/// Parse a model string like "anthropic/claude-sonnet-4" into a ModelParam.
fn parse_model_param(model: &str) -> Option<ModelParam> {
    let (provider, model_id) = model.split_once('/')?;
    Some(ModelParam {
        provider_id: provider.to_string(),
        model_id: model_id.to_string(),
    })
}
