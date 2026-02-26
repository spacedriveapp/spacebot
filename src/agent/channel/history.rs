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
pub(super) fn apply_history_after_turn(
    result: &std::result::Result<String, rig::completion::PromptError>,
    guard: &mut Vec<rig::message::Message>,
    history: Vec<rig::message::Message>,
    history_len_before: usize,
    channel_id: &str,
    is_retrigger: bool,
) {
    match result {
        Ok(_) | Err(rig::completion::PromptError::MaxTurnsError { .. }) => {
            *guard = history;
        }
        Err(rig::completion::PromptError::PromptCancelled { .. }) => {
            // Rig appended the user prompt and possibly an assistant tool-call
            // message to history before cancellation. We keep only the first
            // user text message (the actual user prompt) and discard everything else
            // (assistant tool-calls without results, tool-result user messages).
            //
            // Exception: retrigger turns. The "user prompt" Rig pushed is actually
            // the synthetic system retrigger message (internal template scaffolding),
            // not a real user message. We inject a proper summary record separately
            // in handle_message, so don't preserve anything from retrigger turns.
            if is_retrigger {
                tracing::debug!(
                    channel_id = %channel_id,
                    rolled_back = history.len().saturating_sub(history_len_before),
                    "discarding retrigger turn history (summary injected separately)"
                );
                return;
            }
            let new_messages = &history[history_len_before..];
            let mut preserved = 0usize;
            if let Some(message) = new_messages.iter().find(|m| is_user_text_message(m)) {
                guard.push(message.clone());
                preserved = 1;
            }
            // Skip: Assistant messages (contain tool calls without results),
            // user ToolResult messages, and internal correction prompts.
            tracing::debug!(
                channel_id = %channel_id,
                total_new = new_messages.len(),
                preserved,
                discarded = new_messages.len() - preserved,
                "selectively preserved first user message after PromptCancelled"
            );
        }
        Err(_) => {
            // Hard errors: history state is unpredictable, truncate to snapshot.
            tracing::debug!(
                channel_id = %channel_id,
                rolled_back = history.len().saturating_sub(history_len_before),
                "rolling back history after failed turn"
            );
            guard.truncate(history_len_before);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::super::turn::format_user_message;
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

    /// PromptCancelled preserves user text messages but discards assistant
    /// tool-call messages (which have no matching tool results).
    #[test]
    fn prompt_cancelled_preserves_user_prompt() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        // Simulate what Rig does: push user prompt + assistant tool-call
        let mut history = initial.clone();
        history.push(user_msg("new user prompt")); // should be preserved
        history.push(assistant_msg("tool call without result")); // should be discarded
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        // User prompt should be preserved, assistant tool-call discarded
        let mut expected = initial;
        expected.push(user_msg("new user prompt"));
        assert_eq!(
            guard, expected,
            "user text messages should be preserved, assistant messages discarded"
        );
    }

    /// PromptCancelled discards tool-result User messages (ToolResult content).
    #[test]
    fn prompt_cancelled_discards_tool_results() {
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
        assert_eq!(
            guard, expected,
            "tool-call and tool-result messages should be discarded"
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
        history.push(user_msg("Please proceed and use the available tools.")); // dropped
        history.push(assistant_msg("tool call without result"));
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", false);

        let mut expected = initial;
        expected.push(user_msg("real user prompt"));
        assert_eq!(
            guard, expected,
            "only the first user prompt should be preserved"
        );
    }

    /// PromptCancelled on retrigger turns discards everything — the synthetic
    /// system message is internal scaffolding, not a real user message.
    /// A summary record is injected separately in handle_message.
    #[test]
    fn prompt_cancelled_retrigger_discards_all() {
        let initial = make_history(&["hello", "thinking..."]);
        let mut guard = initial.clone();
        let mut history = initial.clone();
        history.push(user_msg("[System: 1 background process completed...]"));
        history.push(assistant_msg("relaying result..."));
        let len_before = initial.len();

        let err = Err(PromptError::PromptCancelled {
            chat_history: Box::new(history.clone()),
            reason: "reply delivered".to_string(),
        });

        apply_history_after_turn(&err, &mut guard, history, len_before, "test", true);

        assert_eq!(
            guard, initial,
            "retrigger turns should discard all new messages"
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

        // User prompt preserved, assistant tool-call discarded
        assert_eq!(
            guard.len(),
            initial.len() + 1,
            "user prompt should be preserved"
        );
        assert!(
            matches!(&guard[guard.len() - 1], Message::User { .. }),
            "last message should be the preserved user prompt"
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
            metadata: HashMap::new(),
            formatted_author: Some("TestUser".to_string()),
            timestamp: Utc::now(),
        };

        let formatted = format_user_message("", &message);
        assert!(
            !formatted.trim().is_empty(),
            "formatted message should not be empty"
        );
        assert!(
            formatted.contains("[attachment or empty message]"),
            "should use placeholder for empty text"
        );

        // Test whitespace-only text
        let formatted_ws = format_user_message("   ", &message);
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
            metadata: HashMap::new(),
            formatted_author: None,
            timestamp: Utc::now(),
        };

        let formatted_sys = format_user_message("", &system_message);
        assert_eq!(
            formatted_sys, "[system event]",
            "system messages should use [system event] placeholder"
        );

        // Test normal message with text
        let formatted_normal = format_user_message("hello", &message);
        assert!(
            formatted_normal.contains("hello"),
            "normal messages should preserve text"
        );
        assert!(
            !formatted_normal.contains("[attachment or empty message]"),
            "normal messages should not use placeholder"
        );
    }
}
