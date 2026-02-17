//! Branch: Fork context for thinking and delegation.

use crate::agent::compactor::estimate_history_tokens;
use crate::error::Result;
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::llm::routing::is_context_overflow_error;
use crate::{AgentDeps, BranchId, ChannelId, ProcessEvent, ProcessId, ProcessType};
use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use rig::tool::server::ToolServerHandle;
use uuid::Uuid;

/// Max consecutive context overflow recoveries before giving up.
const MAX_OVERFLOW_RETRIES: usize = 2;

/// A branch is a fork of a channel's context for thinking.
pub struct Branch {
    pub id: BranchId,
    pub channel_id: ChannelId,
    pub description: String,
    pub deps: AgentDeps,
    pub hook: SpacebotHook,
    /// System prompt loaded from prompts/BRANCH.md.
    pub system_prompt: String,
    /// Clone of the channel's history at fork time (Rig message format).
    pub history: Vec<rig::message::Message>,
    /// Isolated ToolServer with memory_save + memory_recall.
    pub tool_server: ToolServerHandle,
    /// Maximum LLM turns before the branch is forced to conclude.
    pub max_turns: usize,
}

impl Branch {
    /// Create a new branch from a channel.
    pub fn new(
        channel_id: ChannelId,
        description: impl Into<String>,
        deps: AgentDeps,
        system_prompt: impl Into<String>,
        history: Vec<rig::message::Message>,
        tool_server: ToolServerHandle,
        max_turns: usize,
    ) -> Self {
        let id = Uuid::new_v4();
        let process_id = ProcessId::Branch(id);
        let hook = SpacebotHook::new(
            deps.agent_id.clone(),
            process_id,
            ProcessType::Branch,
            Some(channel_id.clone()),
            deps.event_tx.clone(),
        );

        Self {
            id,
            channel_id,
            description: description.into(),
            deps,
            hook,
            system_prompt: system_prompt.into(),
            history,
            tool_server,
            max_turns,
        }
    }

    /// Run the branch's LLM agent loop and return a conclusion.
    ///
    /// Each branch has its own isolated ToolServer with `memory_save` and
    /// `memory_recall` registered at creation. This keeps `memory_recall` off the
    /// channel's tool list entirely.
    ///
    /// On context overflow, compacts history and retries up to `MAX_OVERFLOW_RETRIES`
    /// times. Branches inherit a full clone of channel history which may already
    /// be large, making them susceptible to overflow on the first LLM call.
    pub async fn run(mut self, prompt: impl Into<String>) -> Result<String> {
        let prompt = prompt.into();

        tracing::info!(
            branch_id = %self.id,
            channel_id = %self.channel_id,
            description = %self.description,
            "branch starting"
        );

        // Pre-flight context check: if the forked history is already large,
        // compact before we even make the first LLM call.
        self.maybe_compact_history();

        let routing = self.deps.runtime_config.routing.load();
        let model_name = routing.resolve(ProcessType::Branch, None).to_string();
        let model = SpacebotModel::make(&self.deps.llm_manager, &model_name)
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(&self.system_prompt)
            .default_max_turns(self.max_turns)
            .tool_server_handle(self.tool_server.clone())
            .build();

        let mut current_prompt = prompt;
        let mut overflow_retries = 0;

        let conclusion = loop {
            match agent
                .prompt(&current_prompt)
                .with_history(&mut self.history)
                .with_hook(self.hook.clone())
                .await
            {
                Ok(response) => break response,
                Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                    let partial = extract_last_assistant_text(&self.history).unwrap_or_else(|| {
                        "Branch exhausted its turns without a final conclusion.".into()
                    });
                    tracing::warn!(branch_id = %self.id, "branch hit max turns, returning partial result");
                    break partial;
                }
                Err(rig::completion::PromptError::PromptCancelled { reason, .. }) => {
                    tracing::info!(branch_id = %self.id, %reason, "branch cancelled");
                    break format!("Branch was cancelled: {reason}");
                }
                Err(error) if is_context_overflow_error(&error.to_string()) => {
                    overflow_retries += 1;
                    if overflow_retries > MAX_OVERFLOW_RETRIES {
                        tracing::error!(
                            branch_id = %self.id,
                            %error,
                            "branch context overflow unrecoverable after {MAX_OVERFLOW_RETRIES} attempts"
                        );
                        // Return partial conclusion if we have one rather than hard-failing
                        break extract_last_assistant_text(&self.history)
                            .unwrap_or_else(|| format!("Branch failed: context overflow after {MAX_OVERFLOW_RETRIES} compaction attempts"));
                    }

                    tracing::warn!(
                        branch_id = %self.id,
                        attempt = overflow_retries,
                        %error,
                        "branch context overflow, compacting and retrying"
                    );
                    self.force_compact_history();
                    current_prompt =
                        "Continue where you left off. Older context has been compacted.".into();
                }
                Err(error) => {
                    tracing::error!(branch_id = %self.id, %error, "branch LLM call failed");
                    return Err(crate::error::AgentError::Other(error.into()).into());
                }
            }
        };

        // Send conclusion back to the channel
        let _ = self.deps.event_tx.send(ProcessEvent::BranchResult {
            agent_id: self.deps.agent_id.clone(),
            branch_id: self.id,
            channel_id: self.channel_id.clone(),
            conclusion: conclusion.clone(),
        });

        tracing::info!(branch_id = %self.id, "branch completed");

        Ok(conclusion)
    }

    /// Compact history if approaching context window limit.
    /// Removes the oldest 50% of messages when usage exceeds 70%.
    fn maybe_compact_history(&mut self) {
        let context_window = **self.deps.runtime_config.context_window.load();
        let estimated = estimate_history_tokens(&self.history);
        let usage = estimated as f32 / context_window as f32;

        if usage < 0.70 {
            return;
        }

        tracing::info!(
            branch_id = %self.id,
            usage = %format!("{:.0}%", usage * 100.0),
            history_len = self.history.len(),
            "branch pre-compacting history"
        );
        self.compact_history(0.50);
    }

    /// Aggressive compaction for overflow recovery. Removes 75% of messages.
    fn force_compact_history(&mut self) {
        tracing::info!(
            branch_id = %self.id,
            history_len = self.history.len(),
            "branch force-compacting history (overflow recovery)"
        );
        self.compact_history(0.75);
    }

    /// Remove a fraction of the oldest messages and insert a summary marker.
    fn compact_history(&mut self, fraction: f32) {
        let total = self.history.len();
        if total <= 4 {
            return;
        }

        let remove_count = ((total as f32 * fraction) as usize)
            .max(1)
            .min(total.saturating_sub(2));
        self.history.drain(..remove_count);

        let marker = format!(
            "[Branch context compacted: {remove_count} older messages removed to stay within context limits. \
             Continue with the information available.]"
        );
        self.history.insert(0, rig::message::Message::from(marker));
    }
}

/// Extract the last assistant text message from a history.
fn extract_last_assistant_text(history: &[rig::message::Message]) -> Option<String> {
    for message in history.iter().rev() {
        if let rig::message::Message::Assistant { content, .. } = message {
            let texts: Vec<String> = content
                .iter()
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
