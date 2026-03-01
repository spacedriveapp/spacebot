//! History management and message formatting for channels.
//!
//! Pure functions that operate on `rig::message::Message` vectors —
//! history reconciliation after LLM turns, user message formatting,
//! reply extraction from cancelled turns, and event filtering.

use crate::{ChannelId, InboundMessage, ProcessEvent};

/// Write history back after the agentic loop completes.
///
/// On success or `MaxTurnsError`, the history Rig built is consistent and safe
/// to keep.
///
/// On `PromptCancelled` (e.g. reply tool fired), Rig's carried history has
/// the user prompt + the assistant's tool-call message but no tool results.
/// Writing it back wholesale would leave a dangling tool-call that poisons
/// every subsequent turn. Instead, we preserve only the **first user text
/// message** Rig appended (the real user prompt), while discarding assistant
/// tool-call messages and tool-result user messages.
///
/// On hard errors, we truncate to the pre-turn snapshot since the history
/// state is unpredictable.
///
/// `MaxTurnsError` is safe — Rig pushes all tool results into a `User` message
/// before raising it, so history is consistent.
///
/// Returns true when a retrigger PromptCancelled turn had a reply tool call
/// extracted and persisted as a clean assistant text message.
pub(crate) fn apply_history_after_turn(
    result: &std::result::Result<String, rig::completion::PromptError>,
    guard: &mut Vec<rig::message::Message>,
    history: Vec<rig::message::Message>,
    history_len_before: usize,
    channel_id: &str,
    is_retrigger: bool,
) -> bool {
    match result {
        Ok(_) | Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
            *guard = history;
            false
        }
        Err(rig::completion::PromptError::PromptCancelled { .. }) => {
            let new_messages = &history[history_len_before..];

            // Rig appended the user prompt and possibly an assistant tool-call
            // message to history before cancellation.
            //
            // Retrigger turns use a synthetic system user prompt, so we never
            // preserve user text there. Instead, keep only a clean assistant
            // message extracted from reply tool args when available.
            if is_retrigger {
                let replaced_bridge = pop_retrigger_bridge_message(guard);
                if let Some(reply_content) =
                    extract_reply_content_from_cancelled_history(new_messages)
                {
                    guard.push(rig::message::Message::Assistant {
                        id: None,
                        content: rig::OneOrMany::one(rig::message::AssistantContent::text(
                            reply_content,
                        )),
                    });

                    tracing::debug!(
                        channel_id = %channel_id,
                        total_new = new_messages.len(),
                        replaced_bridge,
                        "preserved retrigger assistant reply after PromptCancelled"
                    );
                    return true;
                }

                tracing::debug!(
                    channel_id = %channel_id,
                    total_new = new_messages.len(),
                    replaced_bridge,
                    "discarding retrigger PromptCancelled messages (no reply content found)"
                );
                return false;
            }

            // For regular turns we preserve:
            // 1. The first user text message (the actual user prompt)
            // 2. A clean assistant text message extracted from the reply tool call
            //
            // We discard: dangling tool calls (without results), tool-result user
            // messages, and internal correction prompts.
            let mut preserved = 0usize;

            // Preserve the user text message
            if let Some(message) = new_messages.iter().find(|m| is_user_text_message(m)) {
                guard.push(message.clone());
                preserved += 1;
            }

            // Extract and preserve the reply content from the assistant's tool call.
            // The assistant message contains ToolCall(reply, {content: "..."}), but
            // we can't store the tool call without its result (it would poison future
            // turns). Instead, extract the content and push a clean text-only message.
            if let Some(reply_content) = extract_reply_content_from_cancelled_history(new_messages)
            {
                guard.push(rig::message::Message::Assistant {
                    id: None,
                    content: rig::OneOrMany::one(rig::message::AssistantContent::text(
                        reply_content,
                    )),
                });
                preserved += 1;
            }

            tracing::debug!(
                channel_id = %channel_id,
                total_new = new_messages.len(),
                preserved,
                discarded = new_messages.len() - preserved,
                "preserved user message and assistant reply after PromptCancelled"
            );

            false
        }
        Err(_) => {
            // Hard errors: history state is unpredictable, truncate to snapshot.
            tracing::debug!(
                channel_id = %channel_id,
                rolled_back = history.len().saturating_sub(history_len_before),
                "rolling back history after failed turn"
            );
            guard.truncate(history_len_before);
            false
        }
    }
}

pub(crate) fn pop_retrigger_bridge_message(history: &mut Vec<rig::message::Message>) -> bool {
    if history.last().is_some_and(is_retrigger_bridge_message) {
        history.pop();
        true
    } else {
        false
    }
}

fn is_retrigger_bridge_message(message: &rig::message::Message) -> bool {
    match message {
        rig::message::Message::Assistant { content, .. } => content.iter().any(|item| {
            matches!(
                item,
                rig::message::AssistantContent::Text(text)
                    if text.text.contains("[acknowledged")
            )
        }),
        _ => false,
    }
}

/// Extract reply content from a cancelled turn's assistant message.
///
/// When the reply tool fires, Rig's history contains an Assistant message with
/// a ToolCall for `reply` with args like `{"content": "Hey there!"}`. This
/// extracts that content string so we can inject a clean text-only assistant
/// message into history (the tool call itself can't be preserved since it has
/// no matching result).
fn extract_reply_content_from_cancelled_history(
    new_messages: &[rig::message::Message],
) -> Option<String> {
    for message in new_messages {
        if let rig::message::Message::Assistant { content, .. } = message {
            for item in content.iter() {
                if let rig::message::AssistantContent::ToolCall(tool_call) = item
                    && tool_call.function.name == "reply"
                {
                    // Extract the "content" field from the reply tool args
                    if let Some(content_value) = tool_call.function.arguments.get("content")
                        && let Some(text) = content_value.as_str()
                    {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Returns true if a message is a User message containing only text content
/// (i.e., an actual user prompt, not a tool result).
fn is_user_text_message(message: &rig::message::Message) -> bool {
    match message {
        rig::message::Message::User { content } => content
            .iter()
            .all(|c| matches!(c, rig::message::UserContent::Text(_))),
        _ => false,
    }
}

/// Some models emit tool call syntax as plain text instead of making actual tool calls.
/// When the text starts with a tool-like prefix (e.g. `[reply]`, `(reply)`), try to
/// extract the reply content so we can send it cleanly instead of showing raw JSON.
/// Returns `None` if the text doesn't match or can't be parsed — the caller falls
/// back to sending the original text as-is.
pub(crate) fn extract_reply_from_tool_syntax(text: &str) -> Option<String> {
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
pub(crate) fn message_display_name(message: &InboundMessage) -> &str {
    message
        .formatted_author
        .as_deref()
        .or_else(|| {
            message
                .metadata
                .get("sender_display_name")
                .and_then(|v| v.as_str())
        })
        .unwrap_or(&message.sender_id)
}

pub(crate) fn format_user_message(
    raw_text: &str,
    message: &InboundMessage,
    timestamp_text: &str,
) -> String {
    if message.source == "system" {
        // System messages should never be empty, but guard against it
        return if raw_text.trim().is_empty() {
            "[system event]".to_string()
        } else {
            raw_text.to_string()
        };
    }

    let display_name = message_display_name(message);

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
                .get(crate::metadata_keys::REPLY_TO_TEXT)
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

    format!("{display_name}{bot_tag}{reply_context} [{timestamp_text}]: {text_content}")
}

pub(crate) fn format_batched_user_message(
    display_name: &str,
    absolute_timestamp: &str,
    relative_text: &str,
    raw_text: &str,
) -> String {
    let text_content = if raw_text.trim().is_empty() {
        "[attachment or empty message]"
    } else {
        raw_text
    };
    format!("[{display_name}] ({absolute_timestamp}; {relative_text}): {text_content}")
}

pub(crate) fn extract_message_id(message: &InboundMessage) -> Option<String> {
    message
        .metadata
        .get(crate::metadata_keys::MESSAGE_ID)
        .and_then(|value| match value {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
}

/// Check if a ProcessEvent is targeted at a specific channel.
///
/// Events from branches and workers carry a channel_id. We only process events
/// that originated from this channel — otherwise broadcast events from one
/// channel's workers would leak into sibling channels (e.g. threads).
pub(crate) fn event_is_for_channel(event: &ProcessEvent, channel_id: &ChannelId) -> bool {
    match event {
        ProcessEvent::BranchResult {
            channel_id: event_channel,
            ..
        } => event_channel == channel_id,
        ProcessEvent::WorkerComplete {
            channel_id: event_channel,
            ..
        } => event_channel.as_ref() == Some(channel_id),
        ProcessEvent::WorkerStatus {
            channel_id: event_channel,
            ..
        } => event_channel.as_ref() == Some(channel_id),
        // Status block updates, tool events, etc. — match on agent_id which
        // is already filtered by the event bus subscription. Let them through.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::apply_history_after_turn;
    use rig::completion::{CompletionError, PromptError};
    use rig::message::Message;
    use rig::tool::ToolSetError;

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: rig::OneOrMany::one(rig::message::UserContent::text(text)),
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            id: None,
            content: rig::OneOrMany::one(rig::message::AssistantContent::text(text)),
        }
    }

    fn make_history(msgs: &[&str]) -> Vec<Message> {
        msgs.iter()
            .enumerate()
            .map(|(i, text)| {
                if i % 2 == 0 {
                    user_msg(text)
                } else {
                    assistant_msg(text)
                }
            })
            .collect()
    }

    /// On success, the full post-turn history is written back.
    #[test]
    fn ok_writes_history_back() {
        let mut guard = make_history(&["hello"]);
        let history = make_history(&["hello", "hi there", "how are you?"]);
        let len_before = 1;

        apply_history_after_turn(
            &Ok("hi there".to_string()),
            &mut guard,
            history.clone(),
            len_before,
            "test",
            false,
        );

        assert_eq!(guard, history);
    }

    /// MaxTurnsError carries consistent history (tool results included) — write it back.
    #[test]
    fn max_turns_writes_history_back() {
        let mut guard = make_history(&["hello"]);
        let history = make_history(&["hello", "hi there", "how are you?"]);
        let len_before = 1;

        let err = Err(PromptError::MaxTurnsError {
            max_turns: 5,
            chat_history: Box::new(history.clone()),
            prompt: Box::new(user_msg("prompt")),
        });

        apply_history_after_turn(&err, &mut guard, history.clone(), len_before, "test", false);

        assert_eq!(guard, history);
    }

    /// PromptCancelled preserves user text messages and extracts reply content
    /// from the assistant's tool call, discarding the dangling tool call itself.
    #[test]
    fn prompt_cancelled_preserves_user_and_reply() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        // Simulate what Rig does: push user prompt + assistant reply tool-call
        let mut history = initial.clone();
        history.push(user_msg("new user prompt")); // should be preserved
        history.push(Message::Assistant {
            id: None,
            content: rig::OneOrMany::one(rig::message::AssistantContent::tool_call(
                "call_1",
                "reply",
                serde_json::json!({"content": "Hey there!"}),
            )),
        });
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        // User prompt and reply content should be preserved, tool-call structure discarded
        let mut expected = initial;
        expected.push(user_msg("new user prompt"));
        expected.push(assistant_msg("Hey there!"));
        assert_eq!(
            guard, expected,
            "user text message and reply content should be preserved"
        );
    }

    /// PromptCancelled extracts reply content and discards tool-result User messages.
    #[test]
    fn prompt_cancelled_extracts_reply_discards_tool_results() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        let mut history = initial.clone();
        history.push(user_msg("new user prompt")); // preserved
        // Simulate an assistant tool-call followed by a tool-result user message
        history.push(Message::Assistant {
            id: None,
            content: rig::OneOrMany::one(rig::message::AssistantContent::tool_call(
                "call_1",
                "reply",
                serde_json::json!({"content": "hello"}),
            )),
        });
        // A tool-result message is a User message with ToolResult content —
        // is_user_text_message returns false for these, so they get discarded.
        history.push(Message::User {
            content: rig::OneOrMany::one(rig::message::UserContent::ToolResult(
                rig::message::ToolResult {
                    id: "call_1".to_string(),
                    call_id: None,
                    content: rig::OneOrMany::one(rig::message::ToolResultContent::text("ok")),
                },
            )),
        });
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        let mut expected = initial;
        expected.push(user_msg("new user prompt"));
        expected.push(assistant_msg("hello"));
        assert_eq!(
            guard, expected,
            "reply content should be extracted, tool-result messages discarded"
        );
    }

    /// PromptCancelled preserves only the first user prompt and drops any
    /// internal correction prompts that may have been appended on retry.
    #[test]
    fn prompt_cancelled_preserves_only_first_user_prompt() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        let mut history = initial.clone();
        history.push(user_msg("real user prompt")); // preserved
        history.push(assistant_msg("bad tool syntax"));
        history.push(user_msg("Please proceed and use the available tools.")); // dropped (correction)
        history.push(Message::Assistant {
            id: None,
            content: rig::OneOrMany::one(rig::message::AssistantContent::tool_call(
                "call_1",
                "reply",
                serde_json::json!({"content": "Got it!"}),
            )),
        });
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        let mut expected = initial;
        expected.push(user_msg("real user prompt"));
        expected.push(assistant_msg("Got it!"));
        assert_eq!(
            guard, expected,
            "only the first user prompt and final reply should be preserved"
        );
    }

    /// PromptCancelled on retrigger turns preserves only the assistant relay
    /// text extracted from the reply tool args and drops retrigger scaffolding.
    #[test]
    fn prompt_cancelled_retrigger_preserves_reply_only() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        guard.push(assistant_msg(
            "[acknowledged — working on it in background]",
        ));

        let mut history = guard.clone();
        history.push(user_msg("[System: 1 background process completed...]"));
        history.push(Message::Assistant {
            id: None,
            content: rig::OneOrMany::one(rig::message::AssistantContent::tool_call(
                "call_1",
                "reply",
                serde_json::json!({"content": "Relayed branch result to user."}),
            )),
        });
        let len_before = guard.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        let preserved =
            apply_history_after_turn(&err, &mut guard, history, len_before, "test", true);

        let mut expected = initial;
        expected.push(assistant_msg("Relayed branch result to user."));
        assert!(
            preserved,
            "retrigger PromptCancelled should report reply preservation"
        );
        assert_eq!(
            guard, expected,
            "retrigger history should keep only the extracted relay text"
        );
    }

    /// PromptCancelled retrigger turns with no reply tool call remove the
    /// synthetic bridge and preserve no new messages.
    #[test]
    fn prompt_cancelled_retrigger_without_reply_discards_scaffolding() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        guard.push(assistant_msg(
            "[acknowledged — working on it in background]",
        ));

        let mut history = guard.clone();
        history.push(user_msg("[System: 1 background process completed...]"));
        history.push(assistant_msg("relay attempt without tool call"));
        let len_before = guard.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        let preserved =
            apply_history_after_turn(&err, &mut guard, history, len_before, "test", true);

        assert!(
            !preserved,
            "retrigger PromptCancelled should report no reply preservation"
        );
        assert_eq!(
            guard, initial,
            "retrigger scaffolding should be removed when no reply payload exists"
        );
    }

    /// Hard completion errors also roll back to prevent dangling tool-calls.
    #[test]
    fn completion_error_rolls_back() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        let mut history = initial.clone();
        history.push(user_msg("[dangling tool-call]"));
        let len_before = initial.len();

        let err = Err(PromptError::CompletionError(
            CompletionError::ResponseError("API error".to_string()),
        ));

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        assert_eq!(
            guard, initial,
            "history should be rolled back after hard error"
        );
    }

    /// ToolError (tool not found) rolls back — same catch-all arm as hard errors.
    #[test]
    fn tool_error_rolls_back() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        let mut history = initial.clone();
        history.push(user_msg("[dangling tool-call]"));
        let len_before = initial.len();

        let err = Err(PromptError::ToolError(ToolSetError::ToolNotFoundError(
            "nonexistent_tool".to_string(),
        )));

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        assert_eq!(
            guard, initial,
            "history should be rolled back after tool error"
        );
    }

    /// Rollback on empty history is a no-op and must not panic.
    #[test]
    fn rollback_on_empty_history_is_noop() {
        let mut guard: Vec<Message> = vec![];
        let history: Vec<Message> = vec![];
        let len_before = 0;

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        assert!(
            guard.is_empty(),
            "empty history should stay empty after rollback"
        );
    }

    /// Rollback when nothing was appended is also a no-op (len unchanged).
    #[test]
    fn rollback_when_nothing_appended_is_noop() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        // history has same length as before — Rig cancelled before appending anything
        let history = initial.clone();
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "skip delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        assert_eq!(
            guard, initial,
            "history should be unchanged when nothing was appended"
        );
    }

    /// After PromptCancelled, the next turn starts clean with user messages
    /// preserved but no dangling assistant tool-calls.
    #[test]
    fn next_turn_is_clean_after_prompt_cancelled() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        let mut poisoned_history = initial.clone();
        // Rig appends: user prompt + assistant tool-call (dangling, no result)
        poisoned_history.push(user_msg("what's up"));
        poisoned_history.push(Message::Assistant {
            id: None,
            content: rig::OneOrMany::one(rig::message::AssistantContent::tool_call(
                "call_1",
                "reply",
                serde_json::json!({"content": "hey!"}),
            )),
        });
        let len_before = initial.len();

        // First turn: cancelled (reply tool fired) — not a retrigger
        apply_history_after_turn(
            &Err(PromptError::PromptCancelled {
                chat_history: Box::new(poisoned_history.clone()),
                reason: "reply delivered".to_string(),
            }),
            &mut guard,
            poisoned_history,
            len_before,
            "test",
            false,
        );

        // User prompt and reply content preserved, tool-call structure discarded
        assert_eq!(
            guard.len(),
            initial.len() + 2,
            "user prompt and assistant reply should be preserved"
        );
        assert!(
            matches!(&guard[guard.len() - 2], Message::User { .. }),
            "second-to-last message should be the preserved user prompt"
        );
        assert!(
            matches!(&guard[guard.len() - 1], Message::Assistant { .. }),
            "last message should be the extracted reply content"
        );

        // Second turn: new user message appended, successful response
        guard.push(user_msg("follow-up question"));
        let len_before2 = guard.len();
        let mut history2 = guard.clone();
        history2.push(assistant_msg("clean response"));

        apply_history_after_turn(
            &Ok("clean response".to_string()),
            &mut guard,
            history2.clone(),
            len_before2,
            "test",
            false,
        );

        assert_eq!(
            guard, history2,
            "second turn should succeed with clean history"
        );
        // No dangling tool-call assistant messages in history
        let has_dangling = guard.iter().any(|m| {
            if let Message::Assistant { content, .. } = m {
                content
                    .iter()
                    .any(|c| matches!(c, rig::message::AssistantContent::ToolCall(_)))
            } else {
                false
            }
        });
        assert!(
            !has_dangling,
            "no dangling tool-call messages in history after rollback"
        );
    }

    #[test]
    fn format_user_message_handles_empty_text() {
        use super::format_user_message;
        use crate::{Arc, InboundMessage};
        use chrono::Utc;
        use std::collections::HashMap;

        // Test empty text with user message
        let message = InboundMessage {
            id: "test".to_string(),
            agent_id: Some(Arc::from("test_agent")),
            sender_id: "user123".to_string(),
            conversation_id: "conv".to_string(),
            content: crate::MessageContent::Text("".to_string()),
            source: "discord".to_string(),
            adapter: Some("discord".to_string()),
            metadata: HashMap::new(),
            formatted_author: Some("TestUser".to_string()),
            timestamp: Utc::now(),
        };

        let formatted = format_user_message("", &message, "2026-02-26 12:00:00 UTC");
        assert!(
            !formatted.trim().is_empty(),
            "formatted message should not be empty"
        );
        assert!(
            formatted.contains("[attachment or empty message]"),
            "should use placeholder for empty text"
        );

        // Test whitespace-only text
        let formatted_ws = format_user_message("   ", &message, "2026-02-26 12:00:00 UTC");
        assert!(
            formatted_ws.contains("[attachment or empty message]"),
            "should use placeholder for whitespace-only text"
        );

        // Test empty system message
        let system_message = InboundMessage {
            id: "test".to_string(),
            agent_id: Some(Arc::from("test_agent")),
            sender_id: "system".to_string(),
            conversation_id: "conv".to_string(),
            content: crate::MessageContent::Text("".to_string()),
            source: "system".to_string(),
            adapter: None,
            metadata: HashMap::new(),
            formatted_author: None,
            timestamp: Utc::now(),
        };

        let formatted_sys = format_user_message("", &system_message, "2026-02-26 12:00:00 UTC");
        assert_eq!(
            formatted_sys, "[system event]",
            "system messages should use [system event] placeholder"
        );

        // Test normal message with text
        let formatted_normal = format_user_message("hello", &message, "2026-02-26 12:00:00 UTC");
        assert!(
            formatted_normal.contains("hello"),
            "normal messages should preserve text"
        );
        assert!(
            formatted_normal.contains("[2026-02-26 12:00:00 UTC]"),
            "normal messages should include absolute timestamp context"
        );
        assert!(
            !formatted_normal.contains("[attachment or empty message]"),
            "normal messages should not use placeholder"
        );
    }

    #[test]
    fn message_display_name_uses_consistent_fallback_order() {
        use super::message_display_name;
        use crate::{Arc, InboundMessage};
        use chrono::Utc;
        use std::collections::HashMap;

        let mut metadata_only = HashMap::new();
        metadata_only.insert(
            "sender_display_name".to_string(),
            serde_json::Value::String("Metadata User".to_string()),
        );
        let metadata_message = InboundMessage {
            id: "metadata".to_string(),
            agent_id: Some(Arc::from("test_agent")),
            sender_id: "sender123".to_string(),
            conversation_id: "conv".to_string(),
            content: crate::MessageContent::Text("hello".to_string()),
            source: "discord".to_string(),
            adapter: Some("discord".to_string()),
            metadata: metadata_only,
            formatted_author: None,
            timestamp: Utc::now(),
        };
        assert_eq!(message_display_name(&metadata_message), "Metadata User");

        let mut both_metadata = HashMap::new();
        both_metadata.insert(
            "sender_display_name".to_string(),
            serde_json::Value::String("Metadata User".to_string()),
        );
        let formatted_author_message = InboundMessage {
            id: "formatted".to_string(),
            agent_id: Some(Arc::from("test_agent")),
            sender_id: "sender123".to_string(),
            conversation_id: "conv".to_string(),
            content: crate::MessageContent::Text("hello".to_string()),
            source: "discord".to_string(),
            adapter: Some("discord".to_string()),
            metadata: both_metadata,
            formatted_author: Some("Formatted Author".to_string()),
            timestamp: Utc::now(),
        };
        assert_eq!(
            message_display_name(&formatted_author_message),
            "Formatted Author"
        );

        let sender_fallback_message = InboundMessage {
            id: "fallback".to_string(),
            agent_id: Some(Arc::from("test_agent")),
            sender_id: "sender123".to_string(),
            conversation_id: "conv".to_string(),
            content: crate::MessageContent::Text("hello".to_string()),
            source: "discord".to_string(),
            adapter: Some("discord".to_string()),
            metadata: HashMap::new(),
            formatted_author: None,
            timestamp: Utc::now(),
        };
        assert_eq!(message_display_name(&sender_fallback_message), "sender123");
    }

    #[test]
    fn worker_task_temporal_context_preamble_includes_absolute_dates() {
        let prompt_engine =
            crate::prompts::PromptEngine::new("en").expect("prompt engine should initialize");
        let temporal_context = crate::agent::channel_prompt::TemporalContext {
            now_utc: chrono::DateTime::parse_from_rfc3339("2026-02-26T20:30:00Z")
                .expect("valid RFC3339 timestamp")
                .with_timezone(&chrono::Utc),
            timezone: crate::agent::channel_prompt::TemporalTimezone::Named {
                timezone_name: "America/New_York".to_string(),
                timezone: "America/New_York"
                    .parse()
                    .expect("valid timezone identifier"),
            },
        };

        let worker_task = crate::agent::channel_prompt::build_worker_task_with_temporal_context(
            "Run the migration checks",
            &temporal_context,
            &prompt_engine,
        )
        .expect("worker task preamble should render");
        assert!(
            worker_task.contains("Current local date/time:"),
            "worker task should include local time context"
        );
        assert!(
            worker_task.contains("Current UTC date/time:"),
            "worker task should include UTC time context"
        );
        assert!(
            worker_task.contains("Run the migration checks"),
            "worker task should preserve the original task body"
        );
    }

    #[test]
    fn temporal_context_uses_cron_timezone_when_user_timezone_is_invalid() {
        let resolved = crate::agent::channel_prompt::TemporalContext::resolve_timezone_from_names(
            Some("Not/A-Real-Tz".to_string()),
            Some("America/Los_Angeles".to_string()),
        );
        match resolved {
            crate::agent::channel_prompt::TemporalTimezone::Named { timezone_name, .. } => {
                assert_eq!(timezone_name, "America/Los_Angeles");
            }
            crate::agent::channel_prompt::TemporalTimezone::SystemLocal => {
                panic!("expected cron timezone fallback, got system local")
            }
        }
    }

    #[test]
    fn format_batched_message_includes_absolute_and_relative_time() {
        let formatted = super::format_batched_user_message(
            "alice",
            "2026-02-26 15:04:05 PST (America/Los_Angeles, UTC-08:00)",
            "12s ago",
            "ship it",
        );
        assert!(
            formatted.contains("2026-02-26 15:04:05 PST"),
            "batched formatting should include absolute timestamp"
        );
        assert!(
            formatted.contains("12s ago"),
            "batched formatting should include relative timestamp hint"
        );
        assert!(
            formatted.contains("ship it"),
            "batched formatting should include original message text"
        );
    }

    #[test]
    fn format_batched_message_uses_placeholder_for_empty_text() {
        let formatted = super::format_batched_user_message(
            "alice",
            "2026-02-26 15:04:05 PST (America/Los_Angeles, UTC-08:00)",
            "just now",
            "   ",
        );
        assert!(
            formatted.contains("[attachment or empty message]"),
            "batched formatting should use placeholder for empty/whitespace text"
        );
    }
}
