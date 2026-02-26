# Link Channels as Task Delegation (v3)

Replaces the LLM-to-LLM conversational model from link-channels-v2 with deterministic task-based delegation. Agents don't talk to each other — they assign tasks. Link channels become audit logs of delegated work, not conversation threads.

## Why

The v2 design had agents exchange messages through mirrored link channels, with each side running its own LLM to process and respond. This was fundamentally brittle:

- **Recursive loops**: Agents ping-pong conclusions, replies, or re-delegations endlessly.
- **Context loss**: When a link channel re-opens after concluding, the LLM has no memory of prior work and re-sends the original task.
- **Turn count gaming**: Safety caps fire, force-conclude, then the next message resets the budget.
- **Conclusion non-compliance**: Agents ignore `conclude_link` instructions and chat until the safety cap.
- **Result routing corruption**: `initiated_from` metadata gets overwritten by subsequent messages, causing results to bridge to the wrong channel.

Every fix added more special-case logic (drop guards, peer-initiated flags, history seeding, mechanical passthroughs). The system was getting more complex with each bug, not simpler. The core problem is irreducible: two LLMs having a conversation is non-deterministic and uncontrollable.

## The New Model

Instead of conversations, agents delegate through **tasks**. The existing task tracking system (Phase 1 already implemented on `task-tracking` branch) provides the structured, deterministic substrate.

```
User asks Agent A to do something
  -> Agent A decides it should be delegated to Agent B
    -> Agent A calls send_agent_message (modified)
      -> A task is created in Agent B's task store
      -> A record is logged in the link channel between A and B
      -> Agent A's turn ends (skip flag)
    -> Agent B's cortex ready-task loop picks up the task
      -> Worker executes the task
      -> Task moves to done
      -> Completion record logged in link channel
      -> Agent A is notified (retrigger on originating channel)
```

No LLM-to-LLM conversation. No reply relay. No conclusion handshake. No turn counting. The delegation is a database write; the execution is a worker; the result is a task status change.

## What Link Channels Become

Link channels shift from conversation threads to **audit logs**. They record:

1. **Task created**: "Agent A assigned task #42 to Agent B: [title]"
2. **Task completed**: "Agent B completed task #42: [summary]"
3. **Task failed**: "Agent B's worker failed on task #42: [error]"
4. **Task requeued**: "Task #42 returned to ready after worker failure"

These are **system messages** — not LLM-generated text. The link channel is a historical record of delegation activity between two agents. When a human opens a link channel in the dashboard, they see a timeline of tasks assigned and results returned.

Link channels are no longer processed by the LLM. There is no `handle_message()` call, no branching, no worker spawning from link channels. They are write-only logs read by humans through the UI.

### Channel ID Convention

Keep `link:{agent_a}:{agent_b}` as the channel ID format. The `ChannelStore` records these for the UI to discover. Messages are persisted via `ConversationLogger` with `source: "system"` so they're never fed into an LLM context window.

## Modified `send_agent_message` Tool

The tool's external interface stays the same — the LLM calls it with a target agent and a message. The implementation changes completely:

```
Before (v2):
  1. Construct InboundMessage
  2. Inject into target agent's message pipeline
  3. Target agent's link channel processes it with LLM
  4. Reply routed back through outbound handler
  5. Source agent processes reply with LLM
  6. Back and forth until conclude_link

After (v3):
  1. Validate link exists and permits this direction
  2. Create task in target agent's task store
     - title: extracted from message (first sentence or explicit title)
     - description: full message content
     - status: ready (skip pending_approval for agent-delegated tasks)
     - priority: inferred or default medium
     - created_by: "agent:{source_agent_id}"
     - metadata: { delegated_by, originating_channel, link_id }
  3. Log delegation record in link channel (system message)
  4. Set skip flag (end source agent's turn)
  5. Return { success: true, task_number }
```

The tool needs access to the **target agent's** `TaskStore`, not just the source agent's. This means `send_agent_message` needs a way to resolve task stores across agents.

### Cross-Agent Task Store Access

Currently, `TaskStore` instances are per-agent and stored in `ApiState::task_stores`. The tool runs inside a specific agent's process and only has access to that agent's `AgentDeps`.

**Decision: Per-agent task stores.** Each agent owns its own `TaskStore` backed by its own SQLite database. A superior agent instructs task creation via the link channel system and can query/read a subordinate agent's tasks (read-only cross-agent access). The task store registry (`HashMap<AgentId, Arc<TaskStore>>`) is passed to tools that need cross-agent visibility, but task *creation* on another agent goes through the link channel mechanism, not direct writes.

This preserves agent isolation — each agent's task board is its own — while giving the hierarchy the ability to observe and manage work across the org.

## Task Completion Notification

When a delegated task completes, the delegating agent needs to know. Two mechanisms:

### 1. Link Channel Record

When a worker completes a task that has `metadata.delegated_by`, the cortex (or the task completion handler in `cortex.rs`) logs a completion message in the link channel:

```
[System] Task #42 completed by community-manager: "Published 3 posts to Discord announcements channel. Links: [...]"
```

This is a passive record — it doesn't trigger the delegating agent's LLM.

### 2. Originating Channel Retrigger

The task metadata includes `originating_channel` (the channel where the user originally asked for the work). When the task completes, a system message is injected into that channel:

```
[System] Delegated task completed by community-manager: "Published 3 posts..."
```

This **does** retrigger the channel's LLM, which can then relay the result to the user naturally. The message has `source: "system"` and no `formatted_author`, so it renders as a plain system notification.

This replaces the old `bridge_to_initiator` mechanism but is much simpler — it's a single message injection on task completion, not a recursive conclusion chain.

### 3. Task Status Polling (Optional, Future)

The delegating agent's cortex could periodically check on delegated tasks via `task_list` filtered by `metadata.delegated_by`. This is a pull model that doesn't require any special routing — the cortex just queries its own task store for tasks it created on other agents.

Not needed for v1 since the push notification (retrigger) handles the common case.

## What Gets Removed

### Files to Delete

| File | Reason |
|------|--------|
| `src/tools/conclude_link.rs` | Conversational conclusion mechanism |
| `prompts/en/fragments/link_context.md.j2` | "You're in a conversation with agent X" prompt |
| `prompts/en/tools/conclude_link_description.md.j2` | Tool description for deleted tool |

### Code to Remove from `src/agent/channel.rs`

All link-conversation handling logic:

- `link_concluded` field and all checks against it
- `link_turn_count` field and safety cap logic
- `peer_initiated_conclusion` field
- `initiated_from` field and capture logic
- `originating_channel` / `originating_source` fields
- `build_link_context()` method (~40 lines)
- `route_link_conclusion()` / `handle_link_conclusion()` / `bridge_to_initiator()` methods (~160 lines)
- History seeding for link channels (original_sent_message replay)
- Coalesce bypass for link channels
- Drop guard for concluded link channels
- `conclude_link` tool registration in `run_agent_turn()`
- `ConcludeLinkFlag` / `ConcludeLinkSummary` return values from `run_agent_turn()`
- `is_link_channel` checks throughout

### Code to Remove from `src/main.rs`

- Outbound reply relay for `source == "internal"` channels (~90 lines, ~lines 981-1072)
- This is the code that intercepts Agent B's reply on `link:B:A` and injects it into `link:A:B`

### Code to Remove from `src/tools.rs`

- `pub mod conclude_link` and re-exports
- `conclude_link` parameter in `add_channel_tools()` / `remove_channel_tools()`
- `link_counterparty_for_agent()` helper
- `has_other_delegation_targets` complexity (simplify to basic "has any link targets")

### Prompt Changes

- Remove `conclude_link` text entry from `src/prompts/text.rs`
- Update `org_context.md.j2` wording — replace conversation language with task delegation language
- Update `send_agent_message` tool description — "assigns a task" not "sends a message"

## What Gets Kept

### Link Infrastructure (Unchanged)

- `src/links.rs` + `src/links/types.rs` — `AgentLink`, `LinkDirection`, `LinkKind`, store utilities
- `src/config.rs` — `[[links]]` TOML parsing, `LinkDef`
- `src/main.rs` — Link initialization, `ArcSwap` plumbing, `AgentDeps.links`
- `ProcessEvent::AgentMessageSent` / `AgentMessageReceived`
- `src/api/links.rs` — Link CRUD API
- Topology API

### Link Prompt Context (Modified)

- `org_context.md.j2` — Keep the hierarchy rendering (superiors/subordinates/peers). Update the instruction text from "send a message" to "assign a task".
- `build_org_context()` in `channel.rs` — Keep as-is. It reads link topology and renders the org hierarchy.

### Task System (From `task-tracking` Branch)

- `migrations/20260219000001_tasks.sql` — Schema
- `src/tasks.rs` + `src/tasks/store.rs` — `TaskStore`, CRUD, status transitions
- `src/api/tasks.rs` — REST API
- `src/tools/task_create.rs`, `task_list.rs`, `task_update.rs` — LLM tools
- `src/agent/cortex.rs` — `spawn_ready_task_loop`, `pickup_one_ready_task`

## New Code Needed

### 1. Cross-Agent Task Creation in `send_agent_message.rs`

Rewrite the tool's `call()` method to create a task instead of injecting a message. Needs a `task_stores: Arc<HashMap<AgentId, Arc<TaskStore>>>` field.

### 2. Task Completion Callback in `cortex.rs`

After `pickup_one_ready_task` marks a task as `Done`, check if `metadata.delegated_by` exists. If so:

1. Log completion in the link channel via `ConversationLogger`
2. Inject a system message into `metadata.originating_channel` to retrigger the delegating agent

This replaces the entire `bridge_to_initiator` mechanism with ~20 lines of straightforward code.

### 3. Link Channel System Message Logging

A small helper that writes system messages to link channels:

```rust
fn log_link_event(
    conversation_logger: &ConversationLogger,
    link_channel_id: &str,
    message: &str,
) {
    // Persist as a system message (source: "system", role: "system")
    // Not fed to any LLM — purely for UI display
}
```

Called from `send_agent_message` (task created) and from the cortex completion handler (task done/failed).

### 4. `send_agent_message` Tool Description Update

Rewrite `prompts/en/tools/send_agent_message_description.md.j2` to describe task delegation:

> Assign a task to another agent. The target agent's cortex will pick it up and execute it autonomously. Use this when work falls outside your scope or belongs to a subordinate. Your turn ends after delegation — the result will be delivered when the task completes.

## Implementation Order

### Phase 1: Tear Out LLM Conversations

1. Delete `conclude_link.rs`, `link_context.md.j2`, conclude_link prompt text
2. Remove all link-conversation logic from `channel.rs` (fields, methods, guards)
3. Remove outbound reply relay from `main.rs`
4. Remove conclude_link from `tools.rs` registration
5. Simplify `add_channel_tools()` — drop conclude_link param, simplify delegation target logic
6. Verify compilation, run tests

### Phase 2: Wire Task Delegation into `send_agent_message`

1. Add `task_stores` registry to `SendAgentMessageTool`
2. Rewrite `call()` to create a task in the target agent's store
3. Add link channel system message logging on task creation
4. Update tool description prompt
5. Update `org_context.md.j2` wording

### Phase 3: Task Completion Notifications

1. Add `delegated_by` / `originating_channel` metadata checks to cortex task completion handler
2. Log completion in link channel
3. Inject retrigger system message into originating channel
4. Test full delegation round-trip

### Phase 4: UI + Polish

1. Link channel UI shows task timeline instead of conversation
2. Task board shows delegated tasks with source agent badge
3. SSE events for delegation activity
4. Dashboard topology graph shows task flow between agents

## Open Questions

**Per-agent vs instance-level task store**: Should delegated tasks live in the target agent's per-agent SQLite database, or should all tasks move to `instance.db`? Per-agent is cleaner for isolation but requires cross-agent store access. Instance-level is simpler but changes the storage model.

**Task approval for delegated tasks**: Should agent-delegated tasks skip `pending_approval` and go straight to `ready`? The design assumes yes — if Agent A trusts Agent B enough to have a link, the task should execute without human approval. But some deployments might want human-in-the-loop for all delegated work.

**Bidirectional task results**: When Agent B completes a task delegated by Agent A, should A get the full worker output or just a summary? Full output could be large (coding task transcripts). A summary is more practical but loses detail. Could store the full result in task metadata and show a summary in the retrigger message.

**Multi-hop delegation**: Agent A delegates to Agent B, who delegates to Agent C. The completion notification needs to bubble up through all hops. Task metadata can track `delegation_chain: [A, B]` so C's completion notifies B, which notifies A. But this adds complexity — start with single-hop and extend later.

**Task priority inheritance**: Should delegated tasks inherit priority from the delegating agent's context? If the user marked something urgent, the delegated task should probably be `high` priority. The LLM could set this explicitly in the `send_agent_message` call, or it could be inferred.
