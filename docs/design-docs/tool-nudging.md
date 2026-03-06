# Tool Nudging

Automatic retry mechanism that prevents workers from exiting with text-only responses before signaling a terminal outcome.

## Problem

Workers sometimes respond with text like "I'll help you with that" or "Let me create the email now..." without actually calling any tools. This is common:
- At the start of a worker loop when the LLM is "thinking out loud"
- When the task description is vague and the LLM wants clarification
- With certain models that have a conversational tendency
- **Mid-task**, after making a few tool calls (e.g. `read_skill`, `set_status`), the model returns narration instead of continuing with tools

Without intervention, the worker silently reaches `Done` state with no useful output. In Rig's agent loop, any text-only response (no tool calls) terminates the loop — the worker exits as if it completed successfully.

## Solution

Workers must explicitly signal a terminal outcome via `set_status(kind: "outcome")` before they can exit with a text-only response. Until that signal is received, any text-only response triggers a nudge that sends the worker back to work.

### How It Works

```
Worker loop starts
  → LLM completion
  → If response includes tool calls → continue normally
  → If text-only response:
    → Has outcome been signaled via set_status(kind: "outcome")? → allow exit
    → No outcome signal? → Terminate with "tool_nudge" reason → retry with nudge prompt
    → Max 2 retries per prompt request
    → If retries exhausted → worker fails (PromptCancelled)
```

### Outcome Signaling

The `set_status` tool has a `kind` field:

- `kind: "progress"` (default) — intermediate status update, does not unlock exit
- `kind: "outcome"` — terminal result signal, allows text-only exit

Workers are instructed to call `set_status(kind: "outcome")` with a result summary before finishing. The hook marks `outcome_signaled` only after a successful `set_status` tool result is observed in `on_tool_result` (`success: true` and `kind: "outcome"`), so failed status calls do not unlock text-only exit.

### Policy Scoping

Tool nudging is scoped by process type:

| Process Type | Default Policy | Reason |
|--------------|----------------|--------|
| Worker | Enabled | Workers must complete tasks before exiting |
| Branch | Disabled | Branches are for thinking, not doing |
| Channel | Disabled | Channels should be conversational |

The policy can be overridden per-hook:

```rust
let hook = SpacebotHook::new(...)
    .with_tool_nudge_policy(ToolNudgePolicy::Disabled);
```

### Implementation Details

**Outcome detection** (`src/hooks/spacebot.rs:on_tool_result`):
- After a successful `set_status` tool execution with `kind: "outcome"`, `outcome_signaled` is set to `true`
- The flag persists for the rest of the prompt request

**Nudge decision** (`src/hooks/spacebot.rs:should_nudge_tool_usage`):
- Returns `true` when: policy enabled, nudge active, no outcome signaled, response is text-only
- Returns `false` when: outcome signaled, response has tool calls, policy disabled

**Retry flow** (`prompt_with_tool_nudge_retry`):
1. Reset nudge state at start of prompt (clears `outcome_signaled`)
2. On text-only response without outcome: terminate with `TOOL_NUDGE_REASON`
3. Catch termination in retry loop, prune history, retry with nudge prompt
4. On success: prune the nudge prompt from history to keep context clean
5. After `TOOL_NUDGE_MAX_RETRIES` (2) exhausted: `PromptCancelled` propagates to worker → `WorkerState::Failed`

**History hygiene**:
- Synthetic nudge prompts are removed from history on both success and retry
- Failed assistant turns are pruned but user prompts are preserved
- Prevents accumulation of nudge noise in context

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
  - `nudges_on_every_text_only_response_without_outcome` — nudge fires on every text-only response
  - `nudges_after_tool_calls_without_outcome` — the exact bug case (read_skill + progress status + text exit)
  - `outcome_signal_allows_text_only_completion` — outcome signal unlocks exit
  - `progress_status_does_not_signal_outcome` — explicit progress kind doesn't unlock
  - `default_status_kind_does_not_signal_outcome` — omitted kind doesn't unlock
  - `does_not_nudge_when_completion_contains_tool_call`
  - `process_scoped_policy_*` variants for Branch/Channel/Worker
  - `tool_nudge_retry_history_hygiene_*` for history pruning

- **Integration tests** (`tests/tool_nudge.rs`):
  - Public API surface tests (constants, policy enum, hook creation)
  - Event emission verification
  - Process-type scoping

### Future Considerations

1. **Per-model tuning**: Some models may need more/fewer nudge retries
2. **Adaptive nudging**: Detect when nudging isn't working and escalate
3. **User-visible indicator**: Show in UI when a worker was nudged
4. **Metric**: Dedicated counter for nudge events
