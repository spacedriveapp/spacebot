# Tool Nudging

Automatic retry mechanism that encourages workers to use tools when they respond with text-only instead of calling tools.

## Problem

Workers sometimes respond with text like "I'll help you with that" without actually calling any tools. This is particularly common:
- At the start of a worker loop when the LLM is "thinking out loud"
- When the task description is vague and the LLM wants clarification
- With certain models that have a conversational tendency

Without intervention, the worker wastes tokens on non-actionable responses and may never complete the task.

## Solution

Tool nudging detects text-only responses early in the worker loop and automatically retries with a nudge prompt: "Please proceed and use the available tools."

### How It Works

```
Worker loop starts
  → First completion call
  → If text-only response (no tool calls)
    → Terminate with special "tool_nudge" reason
    → Retry with nudge prompt
    → Max 2 retries per prompt request
  → If tool call present
    → Continue normally
```

### Policy Scoping

Tool nudging is scoped by process type:

| Process Type | Default Policy | Reason |
|--------------|----------------|--------|
| Worker | Enabled | Workers must use tools to complete tasks |
| Branch | Disabled | Branches are for thinking, not doing |
| Channel | Disabled | Channels should be conversational |

The policy can be overridden per-hook:

```rust
let hook = SpacebotHook::new(...)
    .with_tool_nudge_policy(ToolNudgePolicy::Disabled);
```

### Implementation Details

**Detection** (`src/hooks/spacebot.rs:should_nudge_tool_usage`):
- Only active on first 2 completion calls (`TOOL_NUDGE_MAX_RETRIES = 2`)
- Checks if response contains any `AssistantContent::ToolCall`
- Ignores empty text responses
- Stops nudging after any tool call is seen

**Retry Flow** (`prompt_with_tool_nudge_retry`):
1. Reset nudge state at start of prompt
2. Track completion call count via atomic counter
3. On text-only response: terminate with `TOOL_NUDGE_REASON`
4. Catch termination in retry loop, prune history, retry with nudge prompt
5. On success: prune the nudge prompt from history to keep context clean

**History Hygiene**:
- Synthetic nudge prompts are removed from history on both success and retry
- Failed assistant turns are pruned but user prompts are preserved
- Prevents accumulation of "Please proceed..." noise in context

### Configuration

There is no user-facing configuration for tool nudging. The behavior is:
- Always enabled for workers
- Always disabled for branches and channels
- Cannot be configured per-agent or per-task

If you need to disable nudging for a specific worker scenario, override the policy when creating the hook:

```rust
// In worker follow-up handling (already disabled)
let follow_up_hook = hook
    .clone()
    .with_tool_nudge_policy(ToolNudgePolicy::Disabled);
```

### Testing

The nudging behavior has comprehensive test coverage:

- **Unit tests** (`src/hooks/spacebot.rs`):
  - `nudges_only_on_first_two_text_only_completion_calls`
  - `does_not_nudge_when_completion_contains_tool_call`
  - `does_not_nudge_after_any_tool_call_has_started`
  - `process_scoped_policy_*` variants for Branch/Channel/Worker
  - `tool_nudge_retry_history_hygiene_*` for history pruning

- **Integration tests** (`tests/tool_nudge.rs`):
  - End-to-end nudge flow with mock model
  - Verification that branches/channels don't nudge
  - History accumulation prevention

### Metrics

When the `metrics` feature is enabled:
- `tool_calls_total` - Count of tool calls (existing)
- `tool_call_duration_seconds` - Duration of tool calls (existing)
- Nudge retry count is logged but not yet a dedicated metric

### Future Considerations

1. **Per-model tuning**: Some models may need more/fewer nudge retries
2. **Adaptive nudging**: Detect when nudging isn't working and escalate
3. **User-visible indicator**: Show in UI when a worker was nudged
4. **Metric**: Dedicated counter for nudge events
