//! Worker: Independent task execution process.

use crate::agent::compactor::estimate_history_tokens;
use crate::config::BrowserConfig;
use crate::error::Result;
use crate::hooks::{SpacebotHook, ToolNudgePolicy};
use crate::llm::SpacebotModel;
use crate::llm::routing::is_context_overflow_error;
use crate::{AgentDeps, ChannelId, ProcessId, ProcessType, WorkerId};
use rig::agent::AgentBuilder;
use rig::completion::CompletionModel;
use std::fmt::Write as _;
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

/// How many turns per segment before we check context and potentially compact.
const TURNS_PER_SEGMENT: usize = 25;

/// Max consecutive context overflow recoveries before giving up.
/// Prevents infinite compact-retry loops if something is fundamentally wrong.
const MAX_OVERFLOW_RETRIES: usize = 3;

/// Max segments before the worker gives up and returns a partial result.
/// Prevents unbounded worker loops when the LLM keeps hitting max_turns
/// without completing the task.
const MAX_SEGMENTS: usize = 10;

/// Worker state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// Worker is running and processing.
    Running,
    /// Worker is waiting for follow-up input (interactive only).
    WaitingForInput,
    /// Worker has completed successfully.
    Done,
    /// Worker has failed.
    Failed,
}

/// A worker process that executes tasks independently.
pub struct Worker {
    pub id: WorkerId,
    pub channel_id: Option<ChannelId>,
    pub task: String,
    pub state: WorkerState,
    pub deps: AgentDeps,
    pub hook: SpacebotHook,
    /// System prompt loaded from prompts/WORKER.md.
    pub system_prompt: String,
    /// Input channel for interactive workers.
    pub input_rx: Option<mpsc::Receiver<String>>,
    /// Browser automation config.
    pub browser_config: BrowserConfig,
    /// Directory for browser screenshots.
    pub screenshot_dir: PathBuf,
    /// Brave Search API key for web search tool.
    pub brave_search_key: Option<String>,
    /// Directory for writing execution logs on failure.
    pub logs_dir: PathBuf,
    /// Status updates.
    pub status_tx: watch::Sender<String>,
    pub status_rx: watch::Receiver<String>,
    /// Prior conversation history for resumed workers (set by `resume_interactive`).
    pub prior_history: Option<Vec<rig::message::Message>>,
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn build(
        channel_id: Option<ChannelId>,
        task: impl Into<String>,
        system_prompt: impl Into<String>,
        deps: AgentDeps,
        browser_config: BrowserConfig,
        screenshot_dir: PathBuf,
        brave_search_key: Option<String>,
        logs_dir: PathBuf,
        input_rx: Option<mpsc::Receiver<String>>,
    ) -> Self {
        let id = Uuid::new_v4();
        let process_id = ProcessId::Worker(id);
        let hook = SpacebotHook::new(
            deps.agent_id.clone(),
            process_id,
            ProcessType::Worker,
            channel_id.clone(),
            deps.event_tx.clone(),
        );
        let (status_tx, status_rx) = watch::channel("starting".to_string());

        Self {
            id,
            channel_id,
            task: task.into(),
            state: WorkerState::Running,
            deps,
            hook,
            system_prompt: system_prompt.into(),
            input_rx,
            browser_config,
            screenshot_dir,
            brave_search_key,
            logs_dir,
            status_tx,
            status_rx,
            prior_history: None,
        }
    }

    /// Create a new fire-and-forget worker.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_id: Option<ChannelId>,
        task: impl Into<String>,
        system_prompt: impl Into<String>,
        deps: AgentDeps,
        browser_config: BrowserConfig,
        screenshot_dir: PathBuf,
        brave_search_key: Option<String>,
        logs_dir: PathBuf,
    ) -> Self {
        Self::build(
            channel_id,
            task,
            system_prompt,
            deps,
            browser_config,
            screenshot_dir,
            brave_search_key,
            logs_dir,
            None,
        )
    }

    /// Create a new interactive worker.
    #[allow(clippy::too_many_arguments)]
    pub fn new_interactive(
        channel_id: Option<ChannelId>,
        task: impl Into<String>,
        system_prompt: impl Into<String>,
        deps: AgentDeps,
        browser_config: BrowserConfig,
        screenshot_dir: PathBuf,
        brave_search_key: Option<String>,
        logs_dir: PathBuf,
    ) -> (Self, mpsc::Sender<String>) {
        let (input_tx, input_rx) = mpsc::channel(32);
        let worker = Self::build(
            channel_id,
            task,
            system_prompt,
            deps,
            browser_config,
            screenshot_dir,
            brave_search_key,
            logs_dir,
            Some(input_rx),
        );

        (worker, input_tx)
    }

    /// Resume an interactive worker that was idle at shutdown.
    ///
    /// Instead of running the initial task, skips directly to the follow-up
    /// loop with the prior conversation history restored from the transcript
    /// blob. The worker keeps its original ID so the DB row stays linked.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_interactive(
        existing_id: WorkerId,
        channel_id: Option<ChannelId>,
        task: impl Into<String>,
        system_prompt: impl Into<String>,
        deps: AgentDeps,
        browser_config: BrowserConfig,
        screenshot_dir: PathBuf,
        brave_search_key: Option<String>,
        logs_dir: PathBuf,
        prior_history: Vec<rig::message::Message>,
    ) -> (Self, mpsc::Sender<String>) {
        let (input_tx, input_rx) = mpsc::channel(32);
        let mut worker = Self::build(
            channel_id,
            task,
            system_prompt,
            deps,
            browser_config,
            screenshot_dir,
            brave_search_key,
            logs_dir,
            Some(input_rx),
        );
        // Reuse the original worker ID so DB row stays linked.
        worker.id = existing_id;
        // Rebuild the hook so it publishes events under the correct worker ID
        // (Self::build creates it with a fresh random ID).
        let process_id = ProcessId::Worker(existing_id);
        worker.hook = SpacebotHook::new(
            worker.deps.agent_id.clone(),
            process_id,
            ProcessType::Worker,
            worker.channel_id.clone(),
            worker.deps.event_tx.clone(),
        );
        worker.state = WorkerState::WaitingForInput;
        // Stash the prior history so `run_follow_up_loop()` can pick it up.
        worker.prior_history = Some(prior_history);
        (worker, input_tx)
    }

    /// Check if the worker can transition to a new state.
    pub fn can_transition_to(&self, target: WorkerState) -> bool {
        use WorkerState::*;

        matches!(
            (self.state, target),
            (Running, WaitingForInput)
                | (Running, Done)
                | (Running, Failed)
                | (WaitingForInput, Running)
                | (WaitingForInput, Failed)
        )
    }

    /// Transition to a new state.
    pub fn transition_to(&mut self, new_state: WorkerState) -> Result<()> {
        if !self.can_transition_to(new_state) {
            return Err(crate::error::AgentError::InvalidStateTransition(format!(
                "can't transition from {:?} to {:?}",
                self.state, new_state
            ))
            .into());
        }

        self.state = new_state;
        Ok(())
    }

    /// Run the worker's LLM agent loop until completion.
    ///
    /// Runs in segments of 25 turns. After each segment, checks context usage
    /// and compacts if the worker is approaching the context window limit.
    /// This prevents long-running workers from dying mid-task due to context
    /// exhaustion.
    pub async fn run(mut self) -> Result<String> {
        self.status_tx.send_modify(|s| *s = "running".to_string());
        self.hook.send_status("running");

        tracing::info!(worker_id = %self.id, task = %self.task, "worker starting");

        let mcp_tools = self.deps.mcp_manager.get_tools().await;

        // Create per-worker ToolServer with task tools
        let worker_tool_server = crate::tools::create_worker_tool_server(
            self.deps.agent_id.clone(),
            self.id,
            self.channel_id.clone(),
            self.deps.task_store.clone(),
            self.deps.event_tx.clone(),
            self.browser_config.clone(),
            self.screenshot_dir.clone(),
            self.brave_search_key.clone(),
            self.deps.runtime_config.workspace_dir.clone(),
            self.deps.sandbox.clone(),
            mcp_tools,
            self.deps.runtime_config.clone(),
        );

        let routing = self.deps.runtime_config.routing.load();
        let model_name = routing.resolve(ProcessType::Worker, None).to_string();
        let model = SpacebotModel::make(&self.deps.llm_manager, &model_name)
            .with_context(&*self.deps.agent_id, "worker")
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(&self.system_prompt)
            .default_max_turns(TURNS_PER_SEGMENT)
            .tool_server_handle(worker_tool_server)
            .build();

        // If this is a resumed worker, load the prior history into `history`
        // (not `compacted_history`) so the LLM sees it as conversation context
        // on the next follow-up call.
        let resuming = self.prior_history.is_some();
        let mut history = self.prior_history.take().unwrap_or_default();
        let mut compacted_history = Vec::new();

        if resuming {
            tracing::info!(
                worker_id = %self.id,
                prior_messages = history.len(),
                "resuming interactive worker with prior history"
            );
            self.hook.send_status("resumed — waiting for input");
            self.hook.send_worker_idle();
        }

        // Run the initial task in segments with compaction checkpoints
        // (skipped entirely for resumed workers).
        let mut prompt = self.task.clone();
        let mut segments_run = 0;
        let mut overflow_retries = 0;

        let result = if resuming {
            // For resumed workers, synthesize a "result" from the task
            // since the original initial result was already relayed.
            String::new()
        } else {
            loop {
                segments_run += 1;

                match self
                    .hook
                    .prompt_with_tool_nudge_retry(&agent, &mut history, &prompt)
                    .await
                {
                    Ok(response) => {
                        break response;
                    }
                    Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                        overflow_retries = 0;

                        if segments_run >= MAX_SEGMENTS {
                            tracing::warn!(
                                worker_id = %self.id,
                                segments = segments_run,
                                "worker hit max segments, returning partial result"
                            );
                            self.hook.send_status("done (max segments)");
                            break crate::agent::extract_last_assistant_text(&history)
                                .unwrap_or_else(|| {
                                    "Worker reached maximum segments without a final response."
                                        .to_string()
                                });
                        }

                        self.maybe_compact_history(&mut compacted_history, &mut history)
                            .await;
                        prompt =
                            "Continue where you left off. Do not repeat completed work.".into();
                        self.hook
                            .send_status(format!("working (segment {segments_run})"));

                        tracing::debug!(
                            worker_id = %self.id,
                            segment = segments_run,
                            history_len = history.len(),
                            "continuing to next segment"
                        );
                    }
                    Err(rig::completion::PromptError::PromptCancelled { reason, .. }) => {
                        self.state = WorkerState::Failed;
                        self.hook.send_status("cancelled");
                        self.write_failure_log(&history, &format!("cancelled: {reason}"));
                        self.persist_transcript(&compacted_history, &history).await;
                        tracing::info!(worker_id = %self.id, %reason, "worker cancelled");
                        return Err(crate::error::AgentError::Cancelled { reason }.into());
                    }
                    Err(error) if is_context_overflow_error(&error.to_string()) => {
                        overflow_retries += 1;
                        if overflow_retries > MAX_OVERFLOW_RETRIES {
                            self.state = WorkerState::Failed;
                            self.hook.send_status("failed");
                            self.write_failure_log(&history, &format!("context overflow after {MAX_OVERFLOW_RETRIES} compaction attempts: {error}"));
                            self.persist_transcript(&compacted_history, &history).await;
                            tracing::error!(worker_id = %self.id, %error, "worker context overflow unrecoverable");
                            return Err(crate::error::AgentError::Other(error.into()).into());
                        }

                        tracing::warn!(
                            worker_id = %self.id,
                            attempt = overflow_retries,
                            %error,
                            "context overflow, compacting and retrying"
                        );
                        self.hook.send_status("compacting (overflow recovery)");
                        self.force_compact_history(&mut compacted_history, &mut history)
                            .await;
                        prompt = "Continue where you left off. Do not repeat completed work. \
                              Your previous attempt exceeded the context limit, so older history \
                              has been compacted."
                            .into();
                    }
                    Err(error) => {
                        self.state = WorkerState::Failed;
                        self.hook.send_status("failed");
                        self.write_failure_log(&history, &error.to_string());
                        self.persist_transcript(&compacted_history, &history).await;
                        tracing::error!(worker_id = %self.id, %error, "worker LLM call failed");
                        return Err(crate::error::AgentError::Other(error.into()).into());
                    }
                }
            }
        };

        // For interactive workers, enter a follow-up loop
        let mut follow_up_failure: Option<String> = None;
        if let Some(mut input_rx) = self.input_rx.take() {
            if !resuming {
                // Fresh worker: persist transcript and signal idle for the first time.
                // Resumed workers already did this in the preamble above.
                self.state = WorkerState::WaitingForInput;
                self.persist_transcript(&compacted_history, &history).await;
                self.hook.send_status("waiting for input");
                self.hook.send_worker_idle();
            }

            while let Some(follow_up) = input_rx.recv().await {
                self.state = WorkerState::Running;
                self.hook.send_status("processing follow-up");

                // Compact before follow-up if needed
                self.maybe_compact_history(&mut compacted_history, &mut history)
                    .await;

                let mut follow_up_prompt = follow_up.clone();
                let mut follow_up_overflow_retries = 0;
                let follow_up_hook = self
                    .hook
                    .clone()
                    .with_tool_nudge_policy(ToolNudgePolicy::Disabled);

                let follow_up_result: std::result::Result<String, String> = loop {
                    match follow_up_hook
                        .prompt_once(&agent, &mut history, &follow_up_prompt)
                        .await
                    {
                        Ok(response) => break Ok(response),
                        Err(error) if is_context_overflow_error(&error.to_string()) => {
                            follow_up_overflow_retries += 1;
                            if follow_up_overflow_retries > MAX_OVERFLOW_RETRIES {
                                let failure_reason = format!(
                                    "follow-up context overflow after {MAX_OVERFLOW_RETRIES} compaction attempts: {error}"
                                );
                                self.write_failure_log(&history, &failure_reason);
                                tracing::error!(worker_id = %self.id, %error, "follow-up context overflow unrecoverable");
                                break Err(failure_reason);
                            }
                            tracing::warn!(
                                worker_id = %self.id,
                                attempt = follow_up_overflow_retries,
                                %error,
                                "follow-up context overflow, compacting and retrying"
                            );
                            self.hook.send_status("compacting (overflow recovery)");
                            self.force_compact_history(&mut compacted_history, &mut history)
                                .await;
                            let prompt_engine = self.deps.runtime_config.prompts.load();
                            let overflow_msg = prompt_engine.render_system_worker_overflow()?;
                            follow_up_prompt = format!("{follow_up}\n\n{overflow_msg}");
                        }
                        Err(error) => {
                            let failure_reason = format!("follow-up failed: {error}");
                            self.write_failure_log(&history, &failure_reason);
                            tracing::error!(worker_id = %self.id, %error, "worker follow-up failed");
                            break Err(failure_reason);
                        }
                    }
                };

                match follow_up_result {
                    Ok(response) => {
                        // Emit follow-up result so the channel can retrigger
                        // and relay this to the user — same as initial result.
                        if !response.is_empty() {
                            let scrubbed = if let Some(store) =
                                self.deps.runtime_config.secrets.load().as_ref().as_ref()
                            {
                                crate::secrets::scrub::scrub_with_store(&response, store)
                            } else {
                                response
                            };
                            let scrubbed = crate::secrets::scrub::scrub_leaks(&scrubbed);
                            self.deps
                                .event_tx
                                .send(crate::ProcessEvent::WorkerInitialResult {
                                    agent_id: self.deps.agent_id.clone(),
                                    worker_id: self.id,
                                    channel_id: self.channel_id.clone(),
                                    result: scrubbed,
                                })
                                .ok();
                        }
                    }
                    Err(failure_reason) => {
                        self.state = WorkerState::Failed;
                        self.hook.send_status("failed");
                        follow_up_failure = Some(failure_reason);
                        break;
                    }
                }

                self.state = WorkerState::WaitingForInput;
                self.persist_transcript(&compacted_history, &history).await;
                self.hook.send_status("waiting for input");
                self.hook.send_worker_idle();
            }
        }

        if let Some(failure_reason) = follow_up_failure {
            self.persist_transcript(&compacted_history, &history).await;
            tracing::error!(worker_id = %self.id, reason = %failure_reason, "worker failed");
            return Err(crate::error::AgentError::Other(anyhow::anyhow!(failure_reason)).into());
        }

        self.state = WorkerState::Done;
        self.hook.send_status("completed");

        // Write success log based on the worker log mode setting
        let log_mode = self.get_worker_log_mode();
        if log_mode != crate::settings::WorkerLogMode::ErrorsOnly {
            self.write_success_log(&history);
        }

        // Persist transcript blob
        self.persist_transcript(&compacted_history, &history).await;

        tracing::info!(worker_id = %self.id, "worker completed");
        Ok(result)
    }

    /// Check context usage and compact history if approaching the limit.
    ///
    /// Workers don't have a full Compactor instance — they do inline compaction
    /// by summarizing older tool calls and results into a condensed recap.
    /// No LLM call, just programmatic truncation with a summary marker.
    async fn maybe_compact_history(
        &self,
        compacted_history: &mut Vec<rig::message::Message>,
        history: &mut Vec<rig::message::Message>,
    ) {
        let context_window = **self.deps.runtime_config.context_window.load();
        let estimated = estimate_history_tokens(history);
        let usage = estimated as f32 / context_window as f32;

        if usage < 0.70 {
            return;
        }

        self.compact_history(compacted_history, history, 0.50, "worker history compacted")
            .await;
    }

    /// Aggressive compaction for context overflow recovery.
    ///
    /// Unlike `maybe_compact_history`, this always fires regardless of current
    /// usage and removes 75% of messages. Used when the provider has already
    /// rejected the request for exceeding context limits.
    async fn force_compact_history(
        &self,
        compacted_history: &mut Vec<rig::message::Message>,
        history: &mut Vec<rig::message::Message>,
    ) {
        self.compact_history(
            compacted_history,
            history,
            0.75,
            "worker history force-compacted (overflow recovery)",
        )
        .await;
    }

    /// Compact worker history by removing a fraction of the oldest messages.
    async fn compact_history(
        &self,
        compacted_history: &mut Vec<rig::message::Message>,
        history: &mut Vec<rig::message::Message>,
        fraction: f32,
        log_message: &str,
    ) {
        let total = history.len();
        if total <= 4 {
            return;
        }

        let context_window = **self.deps.runtime_config.context_window.load();
        let estimated = estimate_history_tokens(history);
        let usage = estimated as f32 / context_window as f32;

        let remove_count = ((total as f32 * fraction) as usize)
            .max(1)
            .min(total.saturating_sub(2));
        let removed: Vec<rig::message::Message> = history.drain(..remove_count).collect();
        compacted_history.extend(removed.iter().cloned());

        let recap = build_worker_recap(&removed);
        let prompt_engine = self.deps.runtime_config.prompts.load();
        let marker = match prompt_engine.render_system_worker_compact(remove_count, &recap) {
            Ok(m) => m,
            Err(error) => {
                tracing::error!(%error, "failed to render worker compact marker");
                return;
            }
        };
        history.insert(0, rig::message::Message::from(marker));

        tracing::info!(
            worker_id = %self.id,
            removed = remove_count,
            remaining = history.len(),
            usage = %format!("{:.0}%", usage * 100.0),
            "{log_message}"
        );
    }

    /// Persist the compressed transcript blob to worker_runs.
    ///
    /// Awaited directly so that at idle boundaries "idle implies persisted"
    /// and concurrent snapshots cannot land out of order.
    async fn persist_transcript(
        &self,
        compacted_history: &[rig::message::Message],
        history: &[rig::message::Message],
    ) {
        let mut full_history = compacted_history.to_vec();
        full_history.extend(history.iter().cloned());
        let transcript_blob =
            crate::conversation::worker_transcript::serialize_transcript(&full_history);
        let worker_id = self.id.to_string();

        // Count tool calls from the Rig history (each ToolCall in an Assistant message)
        let tool_calls: i64 = full_history
            .iter()
            .filter_map(|message| match message {
                rig::message::Message::Assistant { content, .. } => Some(
                    content
                        .iter()
                        .filter(|c| matches!(c, rig::message::AssistantContent::ToolCall(_)))
                        .count() as i64,
                ),
                _ => None,
            })
            .sum();

        if let Err(error) =
            sqlx::query("UPDATE worker_runs SET transcript = ?, tool_calls = ? WHERE id = ?")
                .bind(&transcript_blob)
                .bind(tool_calls)
                .bind(&worker_id)
                .execute(&self.deps.sqlite_pool)
                .await
        {
            tracing::warn!(%error, worker_id, "failed to persist worker transcript");
        }
    }

    /// Check if worker is in a terminal state.
    pub fn is_done(&self) -> bool {
        matches!(self.state, WorkerState::Done | WorkerState::Failed)
    }

    /// Check if worker is interactive.
    pub fn is_interactive(&self) -> bool {
        self.input_rx.is_some()
    }

    /// Get the current worker log mode from settings.
    /// Defaults to ErrorsOnly if settings are not available.
    fn get_worker_log_mode(&self) -> crate::settings::WorkerLogMode {
        self.deps
            .runtime_config
            .settings
            .load()
            .as_ref()
            .as_ref()
            .map(|s| s.worker_log_mode())
            .unwrap_or_default()
    }

    /// Get the log directory path based on the log mode and success/failure.
    /// For AllSeparate mode, uses "failed" or "successful" subdirectories.
    fn get_log_directory(&self, is_success: bool) -> PathBuf {
        let mode = self.get_worker_log_mode();

        match mode {
            crate::settings::WorkerLogMode::AllSeparate => {
                let subdir = if is_success { "successful" } else { "failed" };
                self.logs_dir.join(subdir)
            }
            _ => self.logs_dir.clone(),
        }
    }

    /// Build the log content for a worker execution.
    /// Shared logic for both success and failure logs.
    fn build_log_content(&self, history: &[rig::message::Message], error: Option<&str>) -> String {
        let mut log = String::with_capacity(4096);

        let log_type = if error.is_some() {
            "Failure"
        } else {
            "Success"
        };
        let _ = writeln!(log, "=== Worker {log_type} Log ===");
        let _ = writeln!(log, "Worker ID: {}", self.id);
        if let Some(channel_id) = &self.channel_id {
            let _ = writeln!(log, "Channel ID: {channel_id}");
        }
        let _ = writeln!(log, "Timestamp: {}", chrono::Utc::now().to_rfc3339());
        let _ = writeln!(log, "State: {:?}", self.state);
        let _ = writeln!(log);
        let _ = writeln!(log, "--- Task ---");
        let _ = writeln!(log, "{}", self.task);

        if let Some(err) = error {
            let _ = writeln!(log);
            let _ = writeln!(log, "--- Error ---");
            let _ = writeln!(log, "{err}");
        }

        let _ = writeln!(log);
        let _ = writeln!(log, "--- History ({} messages) ---", history.len());

        for (index, message) in history.iter().enumerate() {
            let _ = writeln!(log);
            match message {
                rig::message::Message::User { content } => {
                    let _ = writeln!(log, "[{index}] User:");
                    for item in content.iter() {
                        match item {
                            rig::message::UserContent::Text(t) => {
                                let _ = writeln!(log, "  {}", t.text);
                            }
                            rig::message::UserContent::ToolResult(tr) => {
                                let call_id = tr.call_id.as_deref().unwrap_or("unknown");
                                let _ = writeln!(log, "  Tool Result (id: {call_id}):");
                                for c in tr.content.iter() {
                                    if let rig::message::ToolResultContent::Text(t) = c {
                                        let text = if t.text.len() > 2000 {
                                            let end = t.text.floor_char_boundary(2000);
                                            format!("{}...[truncated]", &t.text[..end])
                                        } else {
                                            t.text.clone()
                                        };
                                        let _ = writeln!(log, "    {text}");
                                    }
                                }
                            }
                            _ => {
                                let _ = writeln!(log, "  [non-text content]");
                            }
                        }
                    }
                }
                rig::message::Message::Assistant { content, .. } => {
                    let _ = writeln!(log, "[{index}] Assistant:");
                    for item in content.iter() {
                        match item {
                            rig::message::AssistantContent::Text(t) => {
                                let _ = writeln!(log, "  {}", t.text);
                            }
                            rig::message::AssistantContent::ToolCall(tc) => {
                                let args = tc.function.arguments.to_string();
                                let args_display = if args.len() > 500 {
                                    let end = args.floor_char_boundary(500);
                                    format!("{}...[truncated]", &args[..end])
                                } else {
                                    args
                                };
                                let _ = writeln!(
                                    log,
                                    "  Tool Call: {} (id: {})\n    Args: {args_display}",
                                    tc.function.name, tc.id
                                );
                            }
                            _ => {
                                let _ = writeln!(log, "  [other content]");
                            }
                        }
                    }
                }
            }
        }

        log
    }

    /// Write a structured log file for a successful worker execution.
    fn write_success_log(&self, history: &[rig::message::Message]) {
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("worker_{}_{}.log", self.id, timestamp);
        let log_dir = self.get_log_directory(true);
        let path = log_dir.join(&filename);

        let log = self.build_log_content(history, None);

        // Best-effort write
        if let Err(write_error) =
            std::fs::create_dir_all(&log_dir).and_then(|()| std::fs::write(&path, &log))
        {
            tracing::warn!(
                worker_id = %self.id,
                path = %path.display(),
                %write_error,
                "failed to write worker success log"
            );
        } else {
            tracing::info!(
                worker_id = %self.id,
                path = %path.display(),
                "worker success log written"
            );
        }
    }

    /// Write a structured log file to disk capturing the worker's execution
    /// trace (task, history, error). Called on failure so we have something
    /// to inspect after the fact.
    fn write_failure_log(&self, history: &[rig::message::Message], error: &str) {
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("worker_{}_{}.log", self.id, timestamp);
        let log_dir = self.get_log_directory(false);
        let path = log_dir.join(&filename);

        let log = self.build_log_content(history, Some(error));

        // Best-effort write
        if let Err(write_error) =
            std::fs::create_dir_all(&log_dir).and_then(|()| std::fs::write(&path, &log))
        {
            tracing::warn!(
                worker_id = %self.id,
                path = %path.display(),
                %write_error,
                "failed to write worker failure log"
            );
        } else {
            tracing::info!(
                worker_id = %self.id,
                path = %path.display(),
                "worker failure log written"
            );
        }
    }
}

/// Build a recap of removed worker history for the compaction marker.
///
/// Extracts tool calls, assistant text, and tool results so the worker
/// retains full context of what it already did after compaction.
fn build_worker_recap(messages: &[rig::message::Message]) -> String {
    let mut recap = String::new();

    for message in messages {
        match message {
            rig::message::Message::Assistant { content, .. } => {
                for item in content.iter() {
                    if let rig::message::AssistantContent::ToolCall(tc) = item {
                        let args =
                            crate::tools::truncate_output(&tc.function.arguments.to_string(), 200);
                        recap.push_str(&format!("- Called `{}` ({args})\n", tc.function.name));
                    }
                    if let rig::message::AssistantContent::Text(t) = item
                        && !t.text.is_empty()
                    {
                        recap.push_str(&format!("- Noted: {}\n", t.text));
                    }
                }
            }
            rig::message::Message::User { content } => {
                for item in content.iter() {
                    if let rig::message::UserContent::ToolResult(tr) = item {
                        for c in tr.content.iter() {
                            if let rig::message::ToolResultContent::Text(t) = c {
                                let truncated = crate::tools::truncate_output(&t.text, 200);
                                recap.push_str(&format!("  Result: {truncated}\n"));
                            }
                        }
                    }
                }
            }
        }
    }

    if recap.is_empty() {
        "No significant actions recorded in compacted history.".into()
    } else {
        recap
    }
}
