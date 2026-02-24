//! Compactor: Programmatic context monitor that triggers background compaction.
//!
//! The compactor is NOT an LLM process. It watches a channel's context size and
//! spawns compaction workers when thresholds are crossed. The LLM work (summarization
//! + memory extraction) happens in the spawned worker, not here.

use crate::error::Result;
use crate::llm::SpacebotModel;
use crate::{AgentDeps, ChannelId, ProcessType};
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel as _, Prompt as _};
use rig::message::{AssistantContent, Message, UserContent};
use rig::tool::server::{ToolServer, ToolServerHandle};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Programmatic monitor that watches channel context size and triggers compaction.
pub struct Compactor {
    pub channel_id: ChannelId,
    pub deps: AgentDeps,
    pub history: Arc<RwLock<Vec<Message>>>,
    /// Is a compaction currently running.
    is_compacting: Arc<RwLock<bool>>,
}

impl Compactor {
    /// Create a new compactor for a channel.
    pub fn new(channel_id: ChannelId, deps: AgentDeps, history: Arc<RwLock<Vec<Message>>>) -> Self {
        Self {
            channel_id,
            deps,
            history,
            is_compacting: Arc::new(RwLock::new(false)),
        }
    }

    /// Check context size and trigger compaction if needed.
    ///
    /// Called by the channel after each turn. Returns the action taken, if any.
    pub async fn check_and_compact(&self) -> Result<Option<CompactionAction>> {
        let is_compacting = *self.is_compacting.read().await;
        if is_compacting {
            return Ok(None);
        }

        let rc = &self.deps.runtime_config;
        let context_window = **rc.context_window.load();
        let compaction_config = **rc.compaction.load();

        let usage = {
            let history = self.history.read().await;
            let estimated_tokens = estimate_history_tokens(&history);
            estimated_tokens as f32 / context_window as f32
        };

        let action = if usage >= compaction_config.emergency_threshold {
            Some(CompactionAction::EmergencyTruncate)
        } else if usage >= compaction_config.aggressive_threshold {
            Some(CompactionAction::Aggressive)
        } else if usage >= compaction_config.background_threshold {
            Some(CompactionAction::Background)
        } else {
            None
        };

        if let Some(action) = action {
            tracing::info!(
                channel_id = %self.channel_id,
                usage = %format!("{:.1}%", usage * 100.0),
                ?action,
                "compaction triggered"
            );

            match action {
                CompactionAction::EmergencyTruncate => {
                    // Emergency is synchronous — fast, no LLM
                    self.emergency_truncate().await?;
                }
                CompactionAction::Background | CompactionAction::Aggressive => {
                    // Background/aggressive spawn a worker
                    self.spawn_compaction_worker(action).await;
                }
            }

            Ok(Some(action))
        } else {
            Ok(None)
        }
    }

    /// Spawn a compaction worker in the background.
    ///
    /// The worker reads old messages, runs an LLM to produce a summary + extract
    /// memories, then swaps the summary into the channel's history.
    async fn spawn_compaction_worker(&self, action: CompactionAction) {
        let mut is_compacting = self.is_compacting.write().await;
        *is_compacting = true;
        drop(is_compacting);

        let fraction = match action {
            CompactionAction::Background => 0.3,
            CompactionAction::Aggressive => 0.5,
            CompactionAction::EmergencyTruncate => unreachable!(),
        };

        let history = self.history.clone();
        let is_compacting = self.is_compacting.clone();
        let channel_id = self.channel_id.clone();
        let deps = self.deps.clone();
        let prompt_engine = deps.runtime_config.prompts.load();
        let compactor_prompt = match prompt_engine.render_static("compactor") {
            Ok(p) => p,
            Err(error) => {
                tracing::error!(%error, "failed to render compactor prompt");
                let mut flag = is_compacting.write().await;
                *flag = false;
                return;
            }
        };

        tokio::spawn(async move {
            let result = run_compaction(&deps, &compactor_prompt, &history, fraction).await;

            match result {
                Ok(turns_compacted) => {
                    tracing::info!(
                        channel_id = %channel_id,
                        turns_compacted,
                        "compaction completed"
                    );
                }
                Err(error) => {
                    tracing::error!(
                        channel_id = %channel_id,
                        %error,
                        "compaction failed"
                    );
                }
            }

            let mut flag = is_compacting.write().await;
            *flag = false;
        });
    }

    /// Emergency truncation: drop oldest messages without LLM summarization.
    ///
    /// Only fires at 95%+ context usage. Removes the oldest half of messages and
    /// inserts a marker. Fast and synchronous.
    async fn emergency_truncate(&self) -> Result<()> {
        let mut history = self.history.write().await;
        let total = history.len();
        if total <= 2 {
            return Ok(());
        }

        let remove_count = total / 2;

        let removed: Vec<Message> = history.drain(..remove_count).collect();
        drop(removed);

        // Insert a marker at the beginning
        let prompt_engine = self.deps.runtime_config.prompts.load();
        let marker = prompt_engine.render_system_truncation(remove_count)?;
        history.insert(0, Message::from(marker));

        tracing::warn!(
            channel_id = %self.channel_id,
            removed = remove_count,
            remaining = history.len(),
            "emergency truncation performed"
        );

        Ok(())
    }
}

/// Run the actual compaction: summarize via LLM, extract memories, swap summary into history.
#[tracing::instrument(skip(deps, compactor_prompt, history), fields(agent_id = %deps.agent_id))]
async fn run_compaction(
    deps: &AgentDeps,
    compactor_prompt: &str,
    history: &Arc<RwLock<Vec<Message>>>,
    fraction: f32,
) -> Result<usize> {
    // 1. Read and remove the oldest messages from history
    let (removed_messages, remove_count) = {
        let mut hist = history.write().await;
        let total = hist.len();
        let remove_count = ((total as f32 * fraction) as usize)
            .max(1)
            .min(total.saturating_sub(2));
        if remove_count == 0 {
            return Ok(0);
        }
        let removed: Vec<Message> = hist.drain(..remove_count).collect();
        (removed, remove_count)
    };

    // 2. Build the transcript text for the LLM
    let transcript = render_messages_as_transcript(&removed_messages);

    // 3. Run the compaction LLM to produce summary + extracted memories
    let routing = deps.runtime_config.routing.load();
    let model_name = routing.resolve(ProcessType::Worker, None).to_string();
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "compactor")
        .with_routing((**routing).clone());

    // Give the compaction worker memory_save so it can directly persist memories
    let tool_server: ToolServerHandle = ToolServer::new()
        .tool(crate::tools::MemorySaveTool::new(
            deps.memory_search.clone(),
        ))
        .run();

    let agent = AgentBuilder::new(model)
        .preamble(compactor_prompt)
        .default_max_turns(10)
        .tool_server_handle(tool_server)
        .build();

    let mut compaction_history = Vec::new();
    let response = agent
        .prompt(&transcript)
        .with_history(&mut compaction_history)
        .await;

    let summary = match response {
        Ok(text) => extract_summary_section(&text),
        Err(error) => {
            tracing::warn!(%error, "compaction LLM failed, using fallback summary");
            format!("[Compaction summary of {remove_count} messages — LLM summarization failed]")
        }
    };

    // 4. Insert the summary at the beginning of the channel's history
    {
        let mut hist = history.write().await;
        let summary_message = format!("[Compaction Summary]: {summary}");
        hist.insert(0, Message::from(summary_message));
    }

    Ok(remove_count)
}

/// Estimate token count for a history using chars/4 heuristic.
///
/// This is intentionally rough — it's only used for threshold checks, not billing.
/// Overestimates slightly, which is the safe direction for compaction triggers.
pub fn estimate_history_tokens(history: &[Message]) -> usize {
    let mut chars = 0usize;

    for message in history {
        match message {
            Message::User { content } => {
                for item in content.iter() {
                    chars += estimate_user_content_chars(item);
                }
            }
            Message::Assistant { content, .. } => {
                for item in content.iter() {
                    chars += estimate_assistant_content_chars(item);
                }
            }
        }
    }

    // ~4 chars per token for English text. Slightly conservative.
    chars / 4
}

fn estimate_user_content_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(t) => t.text.len(),
        UserContent::ToolResult(tr) => {
            let mut size = 0;
            for item in tr.content.iter() {
                match item {
                    rig::message::ToolResultContent::Text(t) => size += t.text.len(),
                    rig::message::ToolResultContent::Image(_) => size += 100,
                }
            }
            size
        }
        UserContent::Image(_) => 500,
        UserContent::Audio(_) => 500,
        UserContent::Video(_) => 500,
        UserContent::Document(_) => 1000,
    }
}

fn estimate_assistant_content_chars(content: &AssistantContent) -> usize {
    match content {
        AssistantContent::Text(t) => t.text.len(),
        AssistantContent::ToolCall(tc) => {
            tc.function.name.len() + tc.function.arguments.to_string().len()
        }
        AssistantContent::Reasoning(r) => r.reasoning.iter().map(|s| s.len()).sum(),
        AssistantContent::Image(_) => 500,
    }
}

/// Render messages into a human-readable transcript for the compaction LLM.
fn render_messages_as_transcript(messages: &[Message]) -> String {
    let mut output = String::new();

    for message in messages {
        match message {
            Message::User { content } => {
                for item in content.iter() {
                    match item {
                        UserContent::Text(t) => {
                            output.push_str("User: ");
                            output.push_str(&t.text);
                            output.push('\n');
                        }
                        UserContent::ToolResult(tr) => {
                            output.push_str("[Tool Result]: ");
                            for c in tr.content.iter() {
                                if let rig::message::ToolResultContent::Text(t) = c {
                                    output.push_str(&t.text);
                                }
                            }
                            output.push('\n');
                        }
                        _ => {}
                    }
                }
            }
            Message::Assistant { content, .. } => {
                for item in content.iter() {
                    match item {
                        AssistantContent::Text(t) => {
                            output.push_str("Assistant: ");
                            output.push_str(&t.text);
                            output.push('\n');
                        }
                        AssistantContent::ToolCall(tc) => {
                            output.push_str(&format!(
                                "[Tool Call: {}({})]\n",
                                tc.function.name, tc.function.arguments
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    output
}

/// Extract the summary from the compaction LLM's first response.
///
/// The prompt asks for plain summary text. If the LLM wraps it in a
/// `## Summary` header anyway, strip it out.
fn extract_summary_section(response: &str) -> String {
    if let Some(start) = response.find("## Summary") {
        let after_header = &response[start + "## Summary".len()..];
        after_header.trim().to_string()
    } else {
        response.trim().to_string()
    }
}

/// Types of compaction actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionAction {
    /// Normal background compaction (~30% of oldest messages).
    Background,
    /// Aggressive compaction (~50% of oldest messages).
    Aggressive,
    /// Emergency truncation (no LLM, drop oldest 50%).
    EmergencyTruncate,
}
