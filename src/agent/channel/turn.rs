use super::*;

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use rig::message::UserContent;
use rig::one_or_many::OneOrMany;

impl Channel {
    /// Get the agent's display name (falls back to agent ID).
    fn agent_display_name(&self) -> &str {
        self.deps
            .agent_names
            .get(self.deps.agent_id.as_ref())
            .map(String::as_str)
            .unwrap_or(self.deps.agent_id.as_ref())
    }

    /// Handle a batch of messages as a single LLM turn.
    ///
    /// Formats all messages with attribution and timestamps, persists each
    /// individually to conversation history, then presents them as one user turn
    /// with a coalesce hint telling the LLM this is a fast-moving conversation.
    #[tracing::instrument(skip(self, messages), fields(channel_id = %self.id, agent_id = %self.deps.agent_id, message_count = messages.len()))]
    pub(super) async fn handle_message_batch(
        &mut self,
        messages: Vec<InboundMessage>,
    ) -> Result<()> {
        let message_count = messages.len();
        let first_timestamp = messages
            .first()
            .map(|m| m.timestamp)
            .unwrap_or_else(chrono::Utc::now);
        let last_timestamp = messages
            .last()
            .map(|m| m.timestamp)
            .unwrap_or(first_timestamp);
        let elapsed = last_timestamp.signed_duration_since(first_timestamp);
        let elapsed_secs = elapsed.num_milliseconds() as f64 / 1000.0;

        tracing::info!(
            channel_id = %self.id,
            message_count,
            elapsed_secs,
            "handling batched messages"
        );

        // Count unique senders for the hint
        let unique_senders: std::collections::HashSet<_> =
            messages.iter().map(|m| &m.sender_id).collect();
        let unique_sender_count = unique_senders.len();

        // Track conversation_id from the first message
        if self.conversation_id.is_none()
            && let Some(first) = messages.first()
        {
            self.conversation_id = Some(first.conversation_id.clone());
        }

        // Capture conversation context from the first message
        if self.conversation_context.is_none()
            && let Some(first) = messages.first()
        {
            let prompt_engine = self.deps.runtime_config.prompts.load();
            let server_name = first
                .metadata
                .get("discord_guild_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    first
                        .metadata
                        .get("telegram_chat_title")
                        .and_then(|v| v.as_str())
                });
            let channel_name = first
                .metadata
                .get("discord_channel_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    first
                        .metadata
                        .get("telegram_chat_type")
                        .and_then(|v| v.as_str())
                });
            self.conversation_context = Some(prompt_engine.render_conversation_context(
                &first.source,
                server_name,
                channel_name,
            )?);
        }

        // Persist each message to conversation log (individual audit trail)
        let mut user_contents: Vec<UserContent> = Vec::new();
        let mut conversation_id = String::new();

        for message in &messages {
            if message.source != "system" {
                let sender_name = message
                    .metadata
                    .get("sender_display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&message.sender_id);

                let (raw_text, attachments) = match &message.content {
                    crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
                    crate::MessageContent::Media { text, attachments } => {
                        (text.clone().unwrap_or_default(), attachments.clone())
                    }
                    // Render interactions as their Display form so the LLM sees plain text.
                    crate::MessageContent::Interaction { .. } => {
                        (message.content.to_string(), Vec::new())
                    }
                };

                self.state.conversation_logger.log_user_message(
                    &self.state.channel_id,
                    sender_name,
                    &message.sender_id,
                    &raw_text,
                    &message.metadata,
                );
                self.state
                    .channel_store
                    .upsert(&message.conversation_id, &message.metadata);

                conversation_id = message.conversation_id.clone();

                // Format with relative timestamp
                let relative_secs = message
                    .timestamp
                    .signed_duration_since(first_timestamp)
                    .num_seconds();
                let relative_text = if relative_secs < 1 {
                    "just now".to_string()
                } else if relative_secs < 60 {
                    format!("{}s ago", relative_secs)
                } else {
                    format!("{}m ago", relative_secs / 60)
                };

                let display_name = message
                    .metadata
                    .get("sender_display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&message.sender_id);

                let formatted_text =
                    format!("[{}] ({}): {}", display_name, relative_text, raw_text);

                // Download attachments for this message
                if !attachments.is_empty() {
                    let attachment_content =
                        attachments::download_attachments(&self.deps, &attachments).await;
                    for content in attachment_content {
                        user_contents.push(content);
                    }
                }

                user_contents.push(UserContent::text(formatted_text));
            }
        }
        // Separate text and non-text (image/audio) content
        let mut text_parts = Vec::new();
        let mut attachment_parts = Vec::new();
        for content in user_contents {
            match content {
                UserContent::Text(t) => text_parts.push(t.text.clone()),
                other => attachment_parts.push(other),
            }
        }

        let combined_text = format!(
            "[{} messages arrived rapidly in this channel]\n\n{}",
            message_count,
            text_parts.join("\n")
        );

        // Build system prompt with coalesce hint
        let system_prompt = self
            .build_system_prompt_with_coalesce(message_count, elapsed_secs, unique_sender_count)
            .await?;

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = messages.iter().rev().find_map(extract_discord_message_id);
        }

        // Run agent turn with any image/audio attachments preserved
        let (result, skip_flag, replied_flag) = self
            .run_agent_turn(
                &combined_text,
                &system_prompt,
                &conversation_id,
                attachment_parts,
                false, // not a retrigger
            )
            .await?;

        self.handle_agent_result(result, &skip_flag, &replied_flag, false)
            .await;
        // Check compaction
        if let Err(error) = self.compactor.check_and_compact().await {
            tracing::warn!(channel_id = %self.id, %error, "compaction check failed");
        }

        // Increment message counter for memory persistence
        self.message_count += message_count;
        self.check_memory_persistence().await;

        Ok(())
    }

    /// Handle an incoming message by running the channel's LLM agent loop.
    ///
    /// The LLM decides which tools to call: reply (to respond), branch (to think),
    /// spawn_worker (to delegate), route (to follow up with a worker), cancel, or
    /// memory_save. The tools act on the channel's shared state directly.
    #[tracing::instrument(skip(self, message), fields(channel_id = %self.id, agent_id = %self.deps.agent_id, message_id = %message.id))]
    pub(super) async fn handle_message(&mut self, message: InboundMessage) -> Result<()> {
        tracing::info!(
            channel_id = %self.id,
            message_id = %message.id,
            "handling message"
        );

        // Track conversation_id for synthetic re-trigger messages
        if self.conversation_id.is_none() {
            self.conversation_id = Some(message.conversation_id.clone());
        }

        let (raw_text, attachments) = match &message.content {
            crate::MessageContent::Text(text) => (text.clone(), Vec::new()),
            crate::MessageContent::Media { text, attachments } => {
                (text.clone().unwrap_or_default(), attachments.clone())
            }
            // Render interactions as their Display form so the LLM sees plain text.
            crate::MessageContent::Interaction { .. } => (message.content.to_string(), Vec::new()),
        };

        let user_text = format_user_message(&raw_text, &message);

        let attachment_content = if !attachments.is_empty() {
            attachments::download_attachments(&self.deps, &attachments).await
        } else {
            Vec::new()
        };

        // Persist user messages (skip system re-triggers)
        if message.source != "system" {
            let sender_name = message
                .metadata
                .get("sender_display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&message.sender_id);
            self.state.conversation_logger.log_user_message(
                &self.state.channel_id,
                sender_name,
                &message.sender_id,
                &raw_text,
                &message.metadata,
            );
            self.state
                .channel_store
                .upsert(&message.conversation_id, &message.metadata);
        }

        // Capture conversation context from the first message (platform, channel, server)
        if self.conversation_context.is_none() {
            let prompt_engine = self.deps.runtime_config.prompts.load();
            let server_name = message
                .metadata
                .get("discord_guild_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    message
                        .metadata
                        .get("telegram_chat_title")
                        .and_then(|v| v.as_str())
                });
            let channel_name = message
                .metadata
                .get("discord_channel_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    message
                        .metadata
                        .get("telegram_chat_type")
                        .and_then(|v| v.as_str())
                });
            self.conversation_context = Some(prompt_engine.render_conversation_context(
                &message.source,
                server_name,
                channel_name,
            )?);
        }

        let system_prompt = self.build_system_prompt().await?;

        {
            let mut reply_target = self.state.reply_target_message_id.write().await;
            *reply_target = extract_discord_message_id(&message);
        }

        let is_retrigger = message.source == "system";

        let (result, skip_flag, replied_flag) = self
            .run_agent_turn(
                &user_text,
                &system_prompt,
                &message.conversation_id,
                attachment_content,
                is_retrigger,
            )
            .await?;

        self.handle_agent_result(result, &skip_flag, &replied_flag, is_retrigger)
            .await;

        // After a successful retrigger relay, inject a compact record into
        // history so the conversation has context about what was relayed.
        // The retrigger turn itself is rolled back by apply_history_after_turn
        // (PromptCancelled leaves dangling tool calls), so without this the
        // LLM would have no memory of the background result on subsequent turns.
        if is_retrigger && replied_flag.load(std::sync::atomic::Ordering::Relaxed) {
            // Extract the result summaries from the metadata we attached in
            // flush_pending_retrigger, so we record only the substance (not
            // the retrigger instructions/template scaffolding).
            let summary = message
                .metadata
                .get("retrigger_result_summary")
                .and_then(|v| v.as_str())
                .unwrap_or("[background work completed and result relayed to user]");

            let mut history = self.state.history.write().await;
            history.push(rig::message::Message::Assistant {
                id: None,
                content: OneOrMany::one(rig::message::AssistantContent::text(summary)),
            });
        }

        // Check context size and trigger compaction if needed
        if let Err(error) = self.compactor.check_and_compact().await {
            tracing::warn!(channel_id = %self.id, %error, "compaction check failed");
        }

        // Increment message counter and spawn memory persistence branch if threshold reached
        if !is_retrigger {
            self.retrigger_count = 0;
            self.message_count += 1;
            self.check_memory_persistence().await;
        }

        Ok(())
    }

    /// Register per-turn tools, run the LLM agentic loop, and clean up.
    ///
    /// Returns the prompt result and skip flag for the caller to dispatch.
    #[allow(clippy::type_complexity)]
    #[tracing::instrument(skip(self, user_text, system_prompt, attachment_content), fields(channel_id = %self.id, agent_id = %self.deps.agent_id))]
    async fn run_agent_turn(
        &self,
        user_text: &str,
        system_prompt: &str,
        conversation_id: &str,
        attachment_content: Vec<UserContent>,
        is_retrigger: bool,
    ) -> Result<(
        std::result::Result<String, rig::completion::PromptError>,
        crate::tools::SkipFlag,
        crate::tools::RepliedFlag,
    )> {
        let skip_flag = crate::tools::new_skip_flag();
        let replied_flag = crate::tools::new_replied_flag();

        if let Err(error) = crate::tools::add_channel_tools(
            &self.tool_server,
            self.state.clone(),
            self.response_tx.clone(),
            conversation_id,
            skip_flag.clone(),
            replied_flag.clone(),
            self.deps.cron_tool.clone(),
            self.send_agent_message_tool.clone(),
        )
        .await
        {
            tracing::error!(%error, "failed to add channel tools");
            return Err(crate::error::AgentError::Other(error.into()).into());
        }

        let rc = &self.deps.runtime_config;
        let routing = rc.routing.load();
        let max_turns = **rc.max_turns.load();
        let model_name = routing.resolve(ProcessType::Channel, None);
        let model = crate::llm::SpacebotModel::make(&self.deps.llm_manager, model_name)
            .with_context(&*self.deps.agent_id, "channel")
            .with_routing((**routing).clone());

        let agent = AgentBuilder::new(model)
            .preamble(system_prompt)
            .default_max_turns(max_turns)
            .tool_server_handle(self.tool_server.clone())
            .build();

        let _ = self
            .response_tx
            .send(OutboundResponse::Status(crate::StatusUpdate::Thinking))
            .await;

        // Inject attachments as a user message before the text prompt
        if !attachment_content.is_empty() {
            let mut history = self.state.history.write().await;
            let content = OneOrMany::many(attachment_content).unwrap_or_else(|_| {
                OneOrMany::one(UserContent::text("[attachment processing failed]"))
            });
            history.push(rig::message::Message::User { content });
            drop(history);
        }

        // Clone history out so the write lock is released before the agentic loop.
        // The branch tool needs a read lock on history to clone it for the branch,
        // and holding a write lock across the entire agentic loop would deadlock.
        let mut history = {
            let guard = self.state.history.read().await;
            guard.clone()
        };
        let history_len_before = history.len();

        let mut result: std::result::Result<String, rig::completion::PromptError> = agent
            .prompt(user_text)
            .with_history(&mut history)
            .with_hook(self.hook.clone())
            .await;

        // If the LLM responded with text that looks like tool call syntax, it failed
        // to use the tool calling API. Inject a correction and retry a couple
        // times so the model can recover by calling `reply` or `skip`.
        const TOOL_SYNTAX_RECOVERY_MAX_ATTEMPTS: usize = 2;
        let mut recovery_attempts = 0;
        while let Ok(response) = result.as_ref() {
            if !crate::tools::should_block_user_visible_text(response)
                || recovery_attempts >= TOOL_SYNTAX_RECOVERY_MAX_ATTEMPTS
            {
                break;
            }

            recovery_attempts += 1;
            tracing::warn!(
                channel_id = %self.id,
                attempt = recovery_attempts,
                "LLM emitted blocked structured output, retrying with correction"
            );

            let prompt_engine = self.deps.runtime_config.prompts.load();
            let correction = match prompt_engine.render_system_tool_syntax_correction() {
                Ok(correction) => correction,
                Err(error) => {
                    tracing::error!(
                        channel_id = %self.id,
                        %error,
                        "failed to render tool syntax correction prompt"
                    );
                    break;
                }
            };
            result = agent
                .prompt(&correction)
                .with_history(&mut history)
                .with_hook(self.hook.clone())
                .await;
        }

        {
            let mut guard = self.state.history.write().await;
            history::apply_history_after_turn(
                &result,
                &mut guard,
                history,
                history_len_before,
                &self.id,
                is_retrigger,
            );
        }

        if let Err(error) = crate::tools::remove_channel_tools(&self.tool_server).await {
            tracing::warn!(%error, "failed to remove channel tools");
        }

        Ok((result, skip_flag, replied_flag))
    }

    /// Dispatch the LLM result: send fallback text, log errors, clean up typing.
    ///
    /// On retrigger turns (`is_retrigger = true`), fallback text is suppressed
    /// unless the LLM called `skip` — in that case, any text the LLM produced
    /// is sent as a fallback to ensure worker/branch results reach the user.
    /// The LLM sometimes incorrectly skips on retrigger turns thinking the
    /// result was "already processed" when the user hasn't seen it yet.
    pub(super) async fn handle_agent_result(
        &self,
        result: std::result::Result<String, rig::completion::PromptError>,
        skip_flag: &crate::tools::SkipFlag,
        replied_flag: &crate::tools::RepliedFlag,
        is_retrigger: bool,
    ) {
        match result {
            Ok(response) => {
                let skipped = skip_flag.load(std::sync::atomic::Ordering::Relaxed);
                let replied = replied_flag.load(std::sync::atomic::Ordering::Relaxed);

                if skipped && is_retrigger {
                    // The LLM skipped on a retrigger turn. This means a worker
                    // or branch completed but the LLM decided not to relay the
                    // result. If the LLM also produced text, send it as a
                    // fallback since the user hasn't seen the result yet.
                    let text = response.trim();
                    if !text.is_empty() {
                        if crate::tools::should_block_user_visible_text(text) {
                            tracing::warn!(
                                channel_id = %self.id,
                                "blocked retrigger fallback output containing structured or tool syntax"
                            );
                        } else {
                            tracing::info!(
                                channel_id = %self.id,
                                response_len = text.len(),
                                "LLM skipped on retrigger but produced text, sending as fallback"
                            );
                            let extracted = extract_reply_from_tool_syntax(text);
                            let source = self
                                .conversation_id
                                .as_deref()
                                .and_then(|conversation_id| conversation_id.split(':').next())
                                .unwrap_or("unknown");
                            let final_text = crate::tools::reply::normalize_discord_mention_tokens(
                                extracted.as_deref().unwrap_or(text),
                                source,
                            );
                            if !final_text.is_empty() {
                                if extracted.is_some() {
                                    tracing::warn!(channel_id = %self.id, "extracted reply from malformed tool syntax in retrigger fallback");
                                }
                                self.state
                                    .conversation_logger
                                    .log_bot_message(&self.state.channel_id, &final_text);
                                if let Err(error) = self
                                    .response_tx
                                    .send(OutboundResponse::Text(final_text))
                                    .await
                                {
                                    tracing::error!(%error, channel_id = %self.id, "failed to send retrigger fallback reply");
                                }
                            }
                        }
                    } else {
                        tracing::warn!(
                            channel_id = %self.id,
                            "LLM skipped on retrigger with no text — worker/branch result may not have been relayed"
                        );
                    }
                } else if skipped {
                    tracing::debug!(channel_id = %self.id, "channel turn skipped (no response)");
                } else if replied {
                    tracing::debug!(channel_id = %self.id, "channel turn replied via tool (fallback suppressed)");
                } else if is_retrigger {
                    // On retrigger turns the LLM should use the reply tool, but
                    // some models return the result as raw text instead. Send it
                    // as a fallback so the user still gets the worker/branch output.
                    let text = response.trim();
                    if !text.is_empty() {
                        if crate::tools::should_block_user_visible_text(text) {
                            tracing::warn!(
                                channel_id = %self.id,
                                "blocked retrigger output containing structured or tool syntax"
                            );
                        } else {
                            tracing::info!(
                                channel_id = %self.id,
                                response_len = text.len(),
                                "retrigger produced text without reply tool, sending as fallback"
                            );
                            let extracted = extract_reply_from_tool_syntax(text);
                            let source = self
                                .conversation_id
                                .as_deref()
                                .and_then(|conversation_id| conversation_id.split(':').next())
                                .unwrap_or("unknown");
                            let final_text = crate::tools::reply::normalize_discord_mention_tokens(
                                extracted.as_deref().unwrap_or(text),
                                source,
                            );
                            if !final_text.is_empty() {
                                self.state
                                    .conversation_logger
                                    .log_bot_message(&self.state.channel_id, &final_text);
                                if let Err(error) = self
                                    .response_tx
                                    .send(OutboundResponse::Text(final_text))
                                    .await
                                {
                                    tracing::error!(%error, channel_id = %self.id, "failed to send retrigger fallback reply");
                                }
                            }
                        }
                    } else {
                        tracing::debug!(
                            channel_id = %self.id,
                            "retrigger turn produced no text and no reply tool call"
                        );
                    }
                } else {
                    // If the LLM returned text without using the reply tool, send it
                    // directly. Some models respond with text instead of tool calls.
                    // When the text looks like tool call syntax (e.g. "[reply]\n{\"content\": \"hi\"}"),
                    // attempt to extract the reply content and send that instead.
                    let text = response.trim();
                    if crate::tools::should_block_user_visible_text(text) {
                        tracing::warn!(
                            channel_id = %self.id,
                            "blocked fallback output containing structured or tool syntax"
                        );
                    } else {
                        let extracted = extract_reply_from_tool_syntax(text);
                        let source = self
                            .conversation_id
                            .as_deref()
                            .and_then(|conversation_id| conversation_id.split(':').next())
                            .unwrap_or("unknown");
                        let final_text = crate::tools::reply::normalize_discord_mention_tokens(
                            extracted.as_deref().unwrap_or(text),
                            source,
                        );
                        if !final_text.is_empty() {
                            if extracted.is_some() {
                                tracing::warn!(channel_id = %self.id, "extracted reply from malformed tool syntax in LLM text output");
                            }
                            self.state.conversation_logger.log_bot_message_with_name(
                                &self.state.channel_id,
                                &final_text,
                                Some(self.agent_display_name()),
                            );
                            if let Err(error) = self
                                .response_tx
                                .send(OutboundResponse::Text(final_text))
                                .await
                            {
                                tracing::error!(%error, channel_id = %self.id, "failed to send fallback reply");
                            }
                        }
                    }

                    tracing::debug!(channel_id = %self.id, "channel turn completed");
                }
            }
            Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
                tracing::warn!(channel_id = %self.id, "channel hit max turns");
            }
            Err(rig::completion::PromptError::PromptCancelled { reason, .. }) => {
                if reason == "reply delivered" {
                    tracing::debug!(channel_id = %self.id, "channel turn completed via reply tool");
                } else {
                    tracing::info!(channel_id = %self.id, %reason, "channel turn cancelled");
                }
            }
            Err(error) => {
                tracing::error!(channel_id = %self.id, %error, "channel LLM call failed");
            }
        }

        // Ensure typing indicator is always cleaned up, even on error paths
        let _ = self
            .response_tx
            .send(OutboundResponse::Status(crate::StatusUpdate::StopTyping))
            .await;
    }
}

/// Some models emit tool call syntax as plain text instead of making actual tool calls.
/// When the text starts with a tool-like prefix (e.g. `[reply]`, `(reply)`), try to
/// extract the reply content so we can send it cleanly instead of showing raw JSON.
/// Returns `None` if the text doesn't match or can't be parsed — the caller falls
/// back to sending the original text as-is.
fn extract_reply_from_tool_syntax(text: &str) -> Option<String> {
    // Match patterns like "[reply]\n{...}" or "(reply)\n{...}" (with optional whitespace)
    let tool_prefixes = [
        "[reply]",
        "(reply)",
        "[react]",
        "(react)",
        "[skip]",
        "(skip)",
        "[branch]",
        "(branch)",
        "[spawn_worker]",
        "(spawn_worker)",
        "[route]",
        "(route)",
        "[cancel]",
        "(cancel)",
    ];

    let lower = text.to_lowercase();
    let matched_prefix = tool_prefixes.iter().find(|p| lower.starts_with(*p))?;
    let is_reply = matched_prefix.contains("reply");
    let is_skip = matched_prefix.contains("skip");

    // For skip, just return empty — the user shouldn't see anything
    if is_skip {
        return Some(String::new());
    }

    // For non-reply tools (react, branch, etc.), suppress entirely
    if !is_reply {
        return Some(String::new());
    }

    // Try to extract "content" from the JSON payload after the prefix
    let rest = text[matched_prefix.len()..].trim();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(rest)
        && let Some(content) = parsed.get("content").and_then(|v| v.as_str())
    {
        return Some(content.to_string());
    }

    // If we can't parse JSON, the rest might just be the message itself (no JSON wrapper)
    if !rest.is_empty() && !rest.starts_with('{') {
        return Some(rest.to_string());
    }

    None
}

/// Format a user message with sender attribution from message metadata.
///
/// In multi-user channels, this lets the LLM distinguish who said what.
/// System-generated messages (re-triggers) are passed through as-is.
pub(super) fn format_user_message(raw_text: &str, message: &InboundMessage) -> String {
    if message.source == "system" {
        // System messages should never be empty, but guard against it
        return if raw_text.trim().is_empty() {
            "[system event]".to_string()
        } else {
            raw_text.to_string()
        };
    }

    // Use platform-formatted author if available, fall back to metadata
    let display_name = message
        .formatted_author
        .as_deref()
        .or_else(|| {
            message
                .metadata
                .get("sender_display_name")
                .and_then(|v| v.as_str())
        })
        .unwrap_or(&message.sender_id);

    let bot_tag = if message
        .metadata
        .get("sender_is_bot")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        " (bot)"
    } else {
        ""
    };

    let reply_context = message
        .metadata
        .get("reply_to_author")
        .and_then(|v| v.as_str())
        .map(|author| {
            let content_preview = message
                .metadata
                .get("reply_to_text")
                .or_else(|| message.metadata.get("reply_to_content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if content_preview.is_empty() {
                format!(" (replying to {author})")
            } else {
                format!(" (replying to {author}: \"{content_preview}\")")
            }
        })
        .unwrap_or_default();

    // If raw_text is empty or just whitespace, use a placeholder to avoid
    // sending empty text content blocks to the LLM API.
    let text_content = if raw_text.trim().is_empty() {
        "[attachment or empty message]"
    } else {
        raw_text
    };

    format!("{display_name}{bot_tag}{reply_context}: {text_content}")
}

pub(super) fn extract_discord_message_id(message: &InboundMessage) -> Option<u64> {
    if message.source != "discord" {
        return None;
    }

    message
        .metadata
        .get("discord_message_id")
        .and_then(|value| value.as_u64())
}
