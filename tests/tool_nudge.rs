//! Integration test for tool nudging behavior.
//!
//! Tests the public API surface of tool nudging:
//! - Policy enum and its defaults
//! - Hook constants and helpers
//! - Event emission
//! - Process-type scoping

/// Test that the TOOL_NUDGE_REASON constant is accessible and correct.
#[test]
fn tool_nudge_reason_constant_is_valid() {
    assert_eq!(
        spacebot::hooks::SpacebotHook::TOOL_NUDGE_REASON,
        "spacebot_tool_nudge_retry"
    );
    // Prompt should instruct the worker to continue and signal outcome.
    assert!(
        spacebot::hooks::SpacebotHook::TOOL_NUDGE_PROMPT.contains("set_status"),
        "Nudge prompt should reference set_status for outcome signaling"
    );
    assert_eq!(spacebot::hooks::SpacebotHook::TOOL_NUDGE_MAX_RETRIES, 2);
}

/// Test that is_tool_nudge_reason correctly identifies nudge reasons.
#[test]
fn is_tool_nudge_reason_detects_nudge() {
    assert!(spacebot::hooks::SpacebotHook::is_tool_nudge_reason(
        "spacebot_tool_nudge_retry"
    ));
    assert!(!spacebot::hooks::SpacebotHook::is_tool_nudge_reason(
        "some_other_reason"
    ));
    assert!(!spacebot::hooks::SpacebotHook::is_tool_nudge_reason(""));
}

/// Test ToolNudgePolicy enum values and display.
#[test]
fn tool_nudge_policy_enum_values() {
    use spacebot::hooks::ToolNudgePolicy;

    // Test that the enum variants exist and can be matched
    let enabled = ToolNudgePolicy::Enabled;
    let disabled = ToolNudgePolicy::Disabled;

    assert!(matches!(enabled, ToolNudgePolicy::Enabled));
    assert!(matches!(disabled, ToolNudgePolicy::Disabled));

    // Test Debug output
    let debug_str = format!("{:?}", enabled);
    assert!(debug_str.contains("Enabled"));
}

/// Test that ToolNudgePolicy::for_process returns correct defaults.
#[test]
fn tool_nudge_policy_for_process_defaults() {
    use spacebot::hooks::ToolNudgePolicy;

    assert!(matches!(
        ToolNudgePolicy::for_process(spacebot::ProcessType::Worker),
        ToolNudgePolicy::Enabled
    ));

    assert!(matches!(
        ToolNudgePolicy::for_process(spacebot::ProcessType::Branch),
        ToolNudgePolicy::Disabled
    ));

    assert!(matches!(
        ToolNudgePolicy::for_process(spacebot::ProcessType::Channel),
        ToolNudgePolicy::Disabled
    ));
}

/// Test that workers are created with the correct default policy.
#[test]
fn worker_hook_has_enabled_policy_by_default() {
    use spacebot::hooks::ToolNudgePolicy;

    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Worker(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Worker,
        None,
        event_tx,
    );

    assert_eq!(hook.tool_nudge_policy(), ToolNudgePolicy::Enabled);

    // Clone to verify Clone impl works
    let _cloned = hook.clone();
}

/// Test that branches are created with the correct default policy.
#[test]
fn branch_hook_has_disabled_policy_by_default() {
    use spacebot::hooks::ToolNudgePolicy;

    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Branch(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Branch,
        None,
        event_tx,
    );

    assert_eq!(hook.tool_nudge_policy(), ToolNudgePolicy::Disabled);

    let _cloned = hook.clone();
}

/// Test that channels are created with the correct default policy.
#[test]
fn channel_hook_has_disabled_policy_by_default() {
    use spacebot::hooks::ToolNudgePolicy;

    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Channel(std::sync::Arc::from("test-channel")),
        spacebot::ProcessType::Channel,
        Some(std::sync::Arc::from("test-channel")),
        event_tx,
    );

    assert_eq!(hook.tool_nudge_policy(), ToolNudgePolicy::Disabled);

    let _cloned = hook.clone();
}

/// Test hook clone works (used in follow-up handling).
#[test]
fn hook_clone_works() {
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Worker(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Worker,
        None,
        event_tx,
    );

    // Clone the hook (simulating what happens in follow-up handling)
    let _cloned = hook.clone();
}

/// Test that send_status works and generates the correct event.
#[tokio::test]
async fn hook_send_status_generates_event() {
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Worker(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Worker,
        None,
        event_tx,
    );

    hook.send_status("test status");

    let event = event_rx.try_recv().expect("Should receive status event");
    assert!(
        matches!(&event, spacebot::ProcessEvent::StatusUpdate { .. }),
        "Expected StatusUpdate event, got {:?}",
        event
    );
    if let spacebot::ProcessEvent::StatusUpdate { status, .. } = event {
        assert_eq!(status, "test status");
    }
}

/// Test that tool call events are emitted correctly.
#[tokio::test]
async fn tool_call_emits_started_event() {
    use rig::agent::PromptHook;

    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Worker(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Worker,
        None,
        event_tx,
    );

    let action =
        <spacebot::hooks::SpacebotHook as PromptHook<spacebot::llm::SpacebotModel>>::on_tool_call(
            &hook,
            "test_tool",
            Some("call_123".to_string()),
            "internal_id",
            "{}",
        )
        .await;

    assert!(
        matches!(action, rig::agent::ToolCallHookAction::Continue),
        "Tool call should be allowed"
    );

    let event = event_rx
        .try_recv()
        .expect("Should receive tool started event");
    assert!(
        matches!(&event, spacebot::ProcessEvent::ToolStarted { .. }),
        "Expected ToolStarted event, got {:?}",
        event
    );
    if let spacebot::ProcessEvent::ToolStarted {
        tool_name, call_id, ..
    } = event
    {
        assert_eq!(tool_name, "test_tool");
        assert_eq!(call_id, "call_123");
    }
}

/// Test that tool result events are emitted correctly.
#[tokio::test]
async fn tool_result_emits_completed_event() {
    use rig::agent::PromptHook;

    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Worker(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Worker,
        None,
        event_tx,
    );

    // First emit a tool started event to get a call_id
    let _ =
        <spacebot::hooks::SpacebotHook as PromptHook<spacebot::llm::SpacebotModel>>::on_tool_call(
            &hook,
            "test_tool",
            Some("call_123".to_string()),
            "internal_id",
            "{}",
        )
        .await;

    // Consume the started event
    let _ = event_rx.try_recv();

    // Now emit the result
    let action = <spacebot::hooks::SpacebotHook as PromptHook<spacebot::llm::SpacebotModel>>::on_tool_result(
        &hook,
        "test_tool",
        Some("call_123".to_string()),
        "internal_id",
        "{}",
        "Tool result content",
    )
    .await;

    assert!(
        matches!(action, rig::agent::HookAction::Continue),
        "Tool result should continue, got {:?}",
        action
    );

    let event = event_rx
        .try_recv()
        .expect("Should receive tool completed event");
    assert!(
        matches!(&event, spacebot::ProcessEvent::ToolCompleted { .. }),
        "Expected ToolCompleted event, got {:?}",
        event
    );
    if let spacebot::ProcessEvent::ToolCompleted {
        tool_name,
        call_id,
        result,
        ..
    } = event
    {
        assert_eq!(tool_name, "test_tool");
        assert_eq!(call_id, "call_123");
        assert_eq!(result, "Tool result content");
    }
}

/// Test that completion call and response hooks work.
#[tokio::test]
async fn completion_hooks_work() {
    use rig::OneOrMany;
    use rig::agent::{HookAction, PromptHook};
    use rig::completion::{CompletionResponse, Message, Usage};

    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(8);
    let hook = spacebot::hooks::SpacebotHook::new(
        std::sync::Arc::from("test-agent"),
        spacebot::ProcessId::Worker(uuid::Uuid::new_v4()),
        spacebot::ProcessType::Worker,
        None,
        event_tx,
    );

    let prompt = Message::from("Test prompt");

    // Test on_completion_call
    let action = <spacebot::hooks::SpacebotHook as PromptHook<spacebot::llm::SpacebotModel>>::on_completion_call(
        &hook, &prompt, &[],
    )
    .await;
    assert!(matches!(action, HookAction::Continue));

    // Test on_completion_response (will continue since nudging is not activated)
    let response = CompletionResponse {
        choice: OneOrMany::one(rig::message::AssistantContent::text("Response")),
        message_id: None,
        usage: Usage::default(),
        raw_response: spacebot::llm::model::RawResponse {
            body: serde_json::json!({}),
        },
    };

    let action = <spacebot::hooks::SpacebotHook as PromptHook<spacebot::llm::SpacebotModel>>::on_completion_response(
        &hook, &prompt, &response,
    )
    .await;

    // Without nudge activation, should continue
    assert!(matches!(action, HookAction::Continue));
}
