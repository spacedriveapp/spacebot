//! Compactor: Programmatic context monitor that triggers background compaction.
//!
//! The compactor is NOT an LLM process. It watches a channel's context size and
//! spawns compaction workers when thresholds are crossed. The LLM worker only
//! summarizes older history; memory extraction happens through persistence branches.

use crate::error::Result;
use crate::hooks::SpacebotHook;
use crate::llm::SpacebotModel;
use crate::{AgentDeps, ChannelId, ProcessId, ProcessType};
use rig::agent::AgentBuilder;
use rig::completion::CompletionModel;
use rig::message::{AssistantContent, Message, UserContent};
// ToolServerHandle removed — compactor no longer has tools (Phase 5b).
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Programmatic monitor that watches channel context size and triggers compaction.
pub struct Compactor {
    pub channel_id: ChannelId,
    pub deps: AgentDeps,
    pub history: Arc<RwLock<Vec<Message>>>,
    /// Is a compaction currently running.
    is_compacting: Arc<RwLock<bool>>,
    /// Model override from conversation settings.
    model_override: Option<String>,
}

impl Compactor {
    /// Create a new compactor for a channel.
    pub fn new(
        channel_id: ChannelId,
        deps: AgentDeps,
        history: Arc<RwLock<Vec<Message>>>,
        model_override: Option<String>,
    ) -> Self {
        Self {
            channel_id,
            deps,
            history,
            is_compacting: Arc::new(RwLock::new(false)),
            model_override,
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
            if let Err(error) = self
                .deps
                .event_tx
                .send(crate::ProcessEvent::CompactionTriggered {
                    agent_id: self.deps.agent_id.clone(),
                    channel_id: self.channel_id.clone(),
                    threshold_reached: usage,
                })
            {
                tracing::debug!(
                    channel_id = %self.channel_id,
                    %error,
                    "failed to emit compaction-triggered event"
                );
            }

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
    /// The worker reads old messages, runs an LLM to produce a summary, then
    /// swaps the summary into the channel's history.
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
        let model_override = self.model_override.clone();
        let prompt_engine = deps.runtime_config.prompts.load();
        // The compactor is a toolless agent (summary-only), so tool-use
        // enforcement is skipped — there are no tools to enforce.
        let compactor_prompt = match prompt_engine.render_static("compactor") {
            Ok(prompt) => prompt,
            Err(error) => {
                tracing::error!(%error, "failed to render compactor prompt");
                let mut flag = is_compacting.write().await;
                *flag = false;
                return;
            }
        };

        tokio::spawn(async move {
            let result = run_compaction(
                &deps,
                &compactor_prompt,
                &history,
                &channel_id,
                fraction,
                model_override,
            )
            .await;

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

/// Run the actual compaction: summarize via LLM and swap the summary into history.
#[tracing::instrument(skip(deps, compactor_prompt, history), fields(agent_id = %deps.agent_id))]
async fn run_compaction(
    deps: &AgentDeps,
    compactor_prompt: &str,
    history: &Arc<RwLock<Vec<Message>>>,
    channel_id: &ChannelId,
    fraction: f32,
    model_override: Option<String>,
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

    // 3. Run the compaction LLM to produce a summary
    let routing = deps.runtime_config.routing.load();
    let model_name = match model_override {
        Some(ref m) => m.clone(),
        None => routing.resolve(ProcessType::Compactor, None).to_string(),
    };
    let model = SpacebotModel::make(&deps.llm_manager, &model_name)
        .with_context(&*deps.agent_id, "compactor")
        .with_routing((**routing).clone());

    // No tool server — the compactor's sole job is producing a summary.
    // Memory extraction is handled by persistence branches (Phase 5a).
    let agent = AgentBuilder::new(model)
        .preamble(compactor_prompt)
        .default_max_turns(1)
        .build();

    let hook = SpacebotHook::new(
        deps.agent_id.clone(),
        ProcessId::Worker(Uuid::new_v4()),
        ProcessType::Compactor,
        Some(channel_id.clone()),
        deps.event_tx.clone(),
    );

    let mut compaction_history = Vec::new();
    let response = hook
        .prompt_once(&agent, &mut compaction_history, &transcript)
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

/// Share of the context window pre-compaction holds back for the fork's
/// response. The rest of the window covers the fork's prompt input — system
/// prompt, task, and history.
const FORK_RESPONSE_RESERVE: f32 = 0.15;

/// Messages pre-compaction never drops. A fork needs its most recent exchange
/// to be able to act at all, so the budget yields to it rather than the
/// reverse.
const FORK_MIN_RETAINED_MESSAGES: usize = 4;

/// Tokens available to a forked history, once the prompt the caller is about
/// to attach and room for a response are both reserved.
fn forked_history_budget(context_window: usize, prompt_tokens: usize) -> usize {
    let response_reserve = (context_window as f32 * FORK_RESPONSE_RESERVE) as usize;
    context_window.saturating_sub(response_reserve.saturating_add(prompt_tokens))
}

fn forked_compaction_marker(removed_total: usize) -> String {
    format!(
        "[Forked context compacted: {removed_total} older messages removed to stay within context limits. \
         Continue with the information available.]"
    )
}

/// Upper bound on what the marker itself costs, counted against the budget so
/// the notice can't push a fork back over the line it was just trimmed to.
fn forked_compaction_marker_tokens(removed_total: usize) -> usize {
    if removed_total == 0 {
        return 0;
    }
    forked_compaction_marker(removed_total).len().div_ceil(4)
}

/// Compact a forked history in place until it fits the fork's token budget:
/// drop the oldest `fraction` of messages at a time and insert a marker so the
/// fork knows material was removed. Used by branch and worker forks before
/// their first LLM call, so a large parent history doesn't start the fork's
/// life in overflow recovery. Returns the number of messages removed.
///
/// `prompt_tokens` is what the caller is about to attach alongside this
/// history — its rendered system prompt (skills and ambient memory included)
/// and the task or user message. Callers measure it with
/// [`estimate_text_tokens`], so the budget reflects the real first call rather
/// than an assumed prompt size.
///
/// The most recent [`FORK_MIN_RETAINED_MESSAGES`] messages are preserved even
/// when they alone exceed the budget — a single oversized tool result can
/// outweigh the whole window, and dropping it would strip the fork of the
/// context it was forked for.
pub fn precompact_forked_history(
    history: &mut Vec<Message>,
    context_window: usize,
    fraction: f32,
    prompt_tokens: usize,
) -> usize {
    let budget = forked_history_budget(context_window, prompt_tokens);
    let mut removed_total = 0usize;

    // Re-estimate after each drain: one fractional cut isn't guaranteed to
    // land under budget when a few large messages dominate the history.
    while estimate_history_tokens(history) + forked_compaction_marker_tokens(removed_total) > budget
    {
        let total = history.len();
        if total <= FORK_MIN_RETAINED_MESSAGES {
            break;
        }

        let remove_count = ((total as f32 * fraction) as usize)
            .max(1)
            .min(total - FORK_MIN_RETAINED_MESSAGES);
        history.drain(..remove_count);
        removed_total += remove_count;
    }

    let retained_tokens = estimate_history_tokens(history);
    if retained_tokens + forked_compaction_marker_tokens(removed_total) > budget {
        tracing::warn!(
            retained_tokens,
            budget,
            prompt_tokens,
            retained_messages = history.len(),
            "forked history exceeds its budget at the retention floor; the fork \
             may enter context-overflow recovery"
        );
    }

    if removed_total > 0 {
        history.insert(0, Message::from(forked_compaction_marker(removed_total)));
    }
    removed_total
}

/// Estimate the token count of prompt text with the same chars/4 heuristic
/// [`estimate_history_tokens`] uses. Rounds up, so a budget built on it never
/// understates what the prompt will occupy.
pub fn estimate_text_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
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
            Message::System { content } => {
                chars += content.len();
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
        AssistantContent::Reasoning(r) => r
            .content
            .iter()
            .map(|content| match content {
                rig::message::ReasoningContent::Text { text, signature } => {
                    text.len() + signature.as_ref().map_or(0, String::len)
                }
                rig::message::ReasoningContent::Encrypted(data) => data.len(),
                rig::message::ReasoningContent::Redacted { data } => data.len(),
                rig::message::ReasoningContent::Summary(summary) => summary.len(),
                // Future variants default to 0; update this match when new variants are added
                #[allow(unreachable_patterns)]
                _ => 0,
            })
            .sum(),
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
            Message::System { content } => {
                output.push_str("System: ");
                output.push_str(content);
                output.push('\n');
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text_message(size: usize) -> Message {
        Message::from("x".repeat(size))
    }

    #[test]
    fn estimate_text_tokens_rounds_up() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
    }

    #[test]
    fn precompact_noop_under_budget() {
        let mut history: Vec<Message> = (0..10).map(|_| text_message(100)).collect();
        let removed = precompact_forked_history(&mut history, 200_000, 0.50, 0);
        assert_eq!(removed, 0);
        assert_eq!(history.len(), 10);
    }

    #[test]
    fn precompact_drains_until_under_budget() {
        // 40 messages of 4000 chars = 40k tokens against a 20k window: a
        // single 50% cut leaves 20k, still over budget, so the loop must run
        // more than once.
        let mut history: Vec<Message> = (0..40).map(|_| text_message(4000)).collect();
        let removed = precompact_forked_history(&mut history, 20_000, 0.50, 0);
        assert!(removed > 20, "one fractional cut is not enough: {removed}");
        // The marker is part of what the fork carries, so it counts too.
        assert!(estimate_history_tokens(&history) <= forked_history_budget(20_000, 0));
        // Marker inserted once, at the front.
        let front = match &history[0] {
            Message::User { content } => format!("{content:?}"),
            other => format!("{other:?}"),
        };
        assert!(front.contains("Forked context compacted"));
        assert!(!format!("{:?}", history[1]).contains("Forked context compacted"));
    }

    #[test]
    fn precompact_budget_tracks_the_measured_prompt() {
        // 15k tokens of history in a 20k window. It fits once only the
        // response is reserved, and stops fitting when the caller reports a
        // 4k-token preamble — the same history, budgeted against the real
        // first call.
        let history: Vec<Message> = (0..10).map(|_| text_message(6_000)).collect();
        assert_eq!(estimate_history_tokens(&history), 15_000);

        let mut without_prompt = history.clone();
        assert_eq!(
            precompact_forked_history(&mut without_prompt, 20_000, 0.50, 0),
            0
        );

        let mut with_prompt = history;
        let removed = precompact_forked_history(&mut with_prompt, 20_000, 0.50, 4_000);
        assert!(removed > 0, "a 4k preamble must push this history over");
        assert!(estimate_history_tokens(&with_prompt) <= forked_history_budget(20_000, 4_000));
    }

    #[test]
    fn precompact_noop_at_exact_budget() {
        // 8 messages of 4000 chars = 8000 tokens, exactly the budget for a
        // 10k window with a 500-token prompt. Sitting on the line is not over
        // it.
        let mut history: Vec<Message> = (0..8).map(|_| text_message(4000)).collect();
        assert_eq!(
            estimate_history_tokens(&history),
            forked_history_budget(10_000, 500)
        );

        let removed = precompact_forked_history(&mut history, 10_000, 0.50, 500);
        assert_eq!(removed, 0);
        assert_eq!(history.len(), 8);
    }

    #[test]
    fn precompact_trims_one_token_over_budget() {
        let budget = forked_history_budget(10_000, 500);
        let mut history: Vec<Message> = (0..8).map(|_| text_message(4000)).collect();
        history.push(text_message(4));
        assert_eq!(estimate_history_tokens(&history), budget + 1);

        let removed = precompact_forked_history(&mut history, 10_000, 0.50, 500);
        assert!(removed > 0);
        assert!(estimate_history_tokens(&history) <= budget);
    }

    #[test]
    fn precompact_stops_at_retention_floor() {
        // A handful of giant messages that can never fit the budget: the
        // loop must stop at the floor instead of spinning or emptying.
        let mut history: Vec<Message> = (0..6).map(|_| text_message(100_000)).collect();
        let removed = precompact_forked_history(&mut history, 10_000, 0.50, 0);
        assert_eq!(
            history.len(),
            FORK_MIN_RETAINED_MESSAGES + 1,
            "marker + floor"
        );
        assert_eq!(removed, 2);
    }

    #[test]
    fn precompact_keeps_oversized_recent_message() {
        // One recent message larger than the entire window: dropping the rest
        // can't bring the fork under budget, and the message itself is the
        // context the fork was created for, so it survives.
        let mut history: Vec<Message> = (0..12).map(|_| text_message(2_000)).collect();
        history.push(text_message(400_000));

        let removed = precompact_forked_history(&mut history, 10_000, 0.50, 0);
        assert_eq!(
            history.len(),
            FORK_MIN_RETAINED_MESSAGES + 1,
            "marker + floor"
        );
        assert!(removed > 0);
        let last = history.last().unwrap();
        assert!(estimate_history_tokens(std::slice::from_ref(last)) > 10_000);
    }

    #[test]
    fn precompact_handles_prompt_larger_than_the_window() {
        // A preamble that alone outgrows the window leaves no budget at all.
        // The fork still keeps its floor rather than being emptied.
        assert_eq!(forked_history_budget(10_000, 20_000), 0);

        let mut history: Vec<Message> = (0..10).map(|_| text_message(1_000)).collect();
        let removed = precompact_forked_history(&mut history, 10_000, 0.50, 20_000);
        assert_eq!(removed, 6);
        assert_eq!(
            history.len(),
            FORK_MIN_RETAINED_MESSAGES + 1,
            "marker + floor"
        );
    }
}
