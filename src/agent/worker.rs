//! Worker: Independent task execution process.

use crate::agent::compactor::{estimate_history_tokens, CompactionAction};
use crate::config::BrowserConfig;
use crate::error::Result;
use crate::llm::SpacebotModel;
use crate::{WorkerId, ChannelId, ProcessId, ProcessType, AgentDeps};
use crate::hooks::SpacebotHook;
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

/// How many turns per segment before we check context and potentially compact.
const TURNS_PER_SEGMENT: usize = 25;

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
    /// Status updates.
    pub status_tx: watch::Sender<String>,
    pub status_rx: watch::Receiver<String>,
}

impl Worker {
    /// Create a new fire-and-forget worker.
    pub fn new(
        channel_id: Option<ChannelId>,
        task: impl Into<String>,
        system_prompt: impl Into<String>,
        deps: AgentDeps,
        browser_config: BrowserConfig,
        screenshot_dir: PathBuf,
        brave_search_key: Option<String>,
    ) -> Self {
        let id = Uuid::new_v4();
        let process_id = ProcessId::Worker(id);
        let hook = SpacebotHook::new(deps.agent_id.clone(), process_id, ProcessType::Worker, deps.event_tx.clone());
        let (status_tx, status_rx) = watch::channel("starting".to_string());
        
        Self {
            id,
            channel_id,
            task: task.into(),
            state: WorkerState::Running,
            deps,
            hook,
            system_prompt: system_prompt.into(),
            input_rx: None,
            browser_config,
            screenshot_dir,
            brave_search_key,
            status_tx,
            status_rx,
        }
    }
    
    /// Create a new interactive worker.
    pub fn new_interactive(
        channel_id: Option<ChannelId>,
        task: impl Into<String>,
        system_prompt: impl Into<String>,
        deps: AgentDeps,
        browser_config: BrowserConfig,
        screenshot_dir: PathBuf,
        brave_search_key: Option<String>,
    ) -> (Self, mpsc::Sender<String>) {
        let id = Uuid::new_v4();
        let process_id = ProcessId::Worker(id);
        let hook = SpacebotHook::new(deps.agent_id.clone(), process_id, ProcessType::Worker, deps.event_tx.clone());
        let (status_tx, status_rx) = watch::channel("starting".to_string());
        let (input_tx, input_rx) = mpsc::channel(32);
        
        let worker = Self {
            id,
            channel_id,
            task: task.into(),
            state: WorkerState::Running,
            deps,
            hook,
            system_prompt: system_prompt.into(),
            input_rx: Some(input_rx),
            browser_config,
            screenshot_dir,
            brave_search_key,
            status_tx,
            status_rx,
        };
        
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
            return Err(crate::error::AgentError::InvalidStateTransition(
                format!("can't transition from {:?} to {:?}", self.state, new_state)
            ).into());
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

        // Create per-worker ToolServer with task tools
        let worker_tool_server = crate::tools::create_worker_tool_server(
            self.deps.agent_id.clone(),
            self.id,
            self.channel_id.clone(),
            self.deps.event_tx.clone(),
            self.browser_config.clone(),
            self.screenshot_dir.clone(),
            self.brave_search_key.clone(),
        );

        let routing = self.deps.runtime_config.routing.load();
        let model_name = routing.resolve(ProcessType::Worker, None).to_string();
        let model = SpacebotModel::make(&self.deps.llm_manager, &model_name)
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(&self.system_prompt)
            .default_max_turns(TURNS_PER_SEGMENT)
            .tool_server_handle(worker_tool_server)
            .build();

        // Fresh history for the worker (no channel context)
        let mut history = Vec::new();

        // Run the initial task in segments with compaction checkpoints
        let mut prompt = self.task.clone();
        let mut segments_run = 0;

        let result = loop {
            segments_run += 1;

            match agent.prompt(&prompt)
                .with_history(&mut history)
                .with_hook(self.hook.clone())
                .await
            {
                Ok(response) => {
                    // LLM produced final text — task is done
                    break response;
                }
                Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                    // Hit turn limit for this segment — compact and continue
                    self.maybe_compact_history(&mut history).await;

                    // Continue with a follow-up prompt
                    prompt = "Continue where you left off. Do not repeat completed work.".into();
                    self.hook.send_status(&format!("working (segment {segments_run})"));

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
                    tracing::info!(worker_id = %self.id, %reason, "worker cancelled");
                    return Ok(format!("Worker cancelled: {reason}"));
                }
                Err(error) => {
                    self.state = WorkerState::Failed;
                    self.hook.send_status("failed");
                    tracing::error!(worker_id = %self.id, %error, "worker LLM call failed");
                    return Err(crate::error::AgentError::Other(error.into()).into());
                }
            }
        };

        // For interactive workers, enter a follow-up loop
        if let Some(mut input_rx) = self.input_rx.take() {
            self.state = WorkerState::WaitingForInput;
            self.hook.send_status("waiting for input");

            while let Some(follow_up) = input_rx.recv().await {
                self.state = WorkerState::Running;
                self.hook.send_status("processing follow-up");

                // Compact before follow-up if needed
                self.maybe_compact_history(&mut history).await;

                match agent.prompt(&follow_up)
                    .with_history(&mut history)
                    .with_hook(self.hook.clone())
                    .await
                {
                    Ok(_response) => {
                        self.state = WorkerState::WaitingForInput;
                        self.hook.send_status("waiting for input");
                    }
                    Err(error) => {
                        tracing::error!(worker_id = %self.id, %error, "worker follow-up failed");
                        self.state = WorkerState::Failed;
                        self.hook.send_status("failed");
                        break;
                    }
                }
            }
        }

        self.state = WorkerState::Done;
        self.hook.send_status("completed");
        
        tracing::info!(worker_id = %self.id, "worker completed");
        Ok(result)
    }

    /// Check context usage and compact history if approaching the limit.
    ///
    /// Workers don't have a full Compactor instance — they do inline compaction
    /// by summarizing older tool calls and results into a condensed recap.
    /// No LLM call, just programmatic truncation with a summary marker.
    async fn maybe_compact_history(&self, history: &mut Vec<rig::message::Message>) {
        let context_window = **self.deps.runtime_config.context_window.load();
        let estimated = estimate_history_tokens(history);
        let usage = estimated as f32 / context_window as f32;

        if usage < 0.70 {
            return;
        }

        let total = history.len();
        if total <= 4 {
            return;
        }

        // Remove the oldest 50% of messages and replace with a summary marker
        let remove_count = total / 2;
        let removed: Vec<rig::message::Message> = history.drain(..remove_count).collect();

        // Build a brief recap of what was removed
        let recap = build_worker_recap(&removed);
        let marker = format!(
            "[System: Earlier work has been summarized to free up context. \
             {remove_count} messages compacted.]\n\n## Work completed so far:\n{recap}"
        );
        history.insert(0, rig::message::Message::from(marker));

        tracing::info!(
            worker_id = %self.id,
            removed = remove_count,
            remaining = history.len(),
            usage = %format!("{:.0}%", usage * 100.0),
            "worker history compacted"
        );
    }
    
    /// Check if worker is in a terminal state.
    pub fn is_done(&self) -> bool {
        matches!(self.state, WorkerState::Done | WorkerState::Failed)
    }
    
    /// Check if worker is interactive.
    pub fn is_interactive(&self) -> bool {
        self.input_rx.is_some()
    }
}

/// Build a brief recap of removed worker history for the compaction marker.
///
/// Extracts tool call names and their results (truncated) so the worker
/// knows what it already did without carrying the full history.
fn build_worker_recap(messages: &[rig::message::Message]) -> String {
    let mut recap = String::new();
    
    for message in messages {
        match message {
            rig::message::Message::Assistant { content, .. } => {
                for item in content.iter() {
                    if let rig::message::AssistantContent::ToolCall(tc) = item {
                        recap.push_str(&format!("- Called `{}` ", tc.function.name));
                        // Include truncated args for context
                        let args = tc.function.arguments.to_string();
                        if args.len() > 100 {
                            recap.push_str(&format!("({}...)\n", &args[..100]));
                        } else {
                            recap.push_str(&format!("({args})\n"));
                        }
                    }
                    if let rig::message::AssistantContent::Text(t) = item {
                        if !t.text.is_empty() {
                            let text = if t.text.len() > 200 {
                                format!("{}...", &t.text[..200])
                            } else {
                                t.text.clone()
                            };
                            recap.push_str(&format!("- Noted: {text}\n"));
                        }
                    }
                }
            }
            rig::message::Message::User { content } => {
                for item in content.iter() {
                    if let rig::message::UserContent::ToolResult(tr) = item {
                        for c in tr.content.iter() {
                            if let rig::message::ToolResultContent::Text(t) = c {
                                let result = if t.text.len() > 150 {
                                    format!("{}...", &t.text[..150])
                                } else {
                                    t.text.clone()
                                };
                                recap.push_str(&format!("  Result: {result}\n"));
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

/// Extract the last assistant text message from a history.
fn extract_last_assistant_text(history: &[rig::message::Message]) -> Option<String> {
    for message in history.iter().rev() {
        if let rig::message::Message::Assistant { content, .. } = message {
            let texts: Vec<String> = content.iter()
                .filter_map(|c| {
                    if let rig::message::AssistantContent::Text(t) = c {
                        Some(t.text.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if !texts.is_empty() {
                return Some(texts.join("\n"));
            }
        }
    }
    None
}
