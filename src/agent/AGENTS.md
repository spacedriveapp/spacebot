# src/agent/ — Agent Process Subsystem

Implements all five process types. Each file owns one concern. No `mod.rs` — `src/agent.rs` is the module root.

## File Table

| File | Lines | Purpose |
|------|-------|---------|
| `cortex.rs` | 4265 | Memory bulletin, process supervision, consolidation, interactive chat |
| `channel.rs` | 3149 | User-facing conversation, branch/worker spawning, status injection |
| `channel_dispatch.rs` | 1199 | Worker/branch result routing, completion handling, event processing |
| `channel_history.rs` | 1162 | Conversation persistence, context assembly, message formatting |
| `cortex_chat.rs` | 936 | Interactive admin chat with full tool access |
| `channel_attachments.rs` | 799 | File attachment download/upload/persistence |
| `worker.rs` | 1151 | Worker lifecycle, state machine, OpenCode integration |
| `branch.rs` | ~200 | Context fork, returns conclusion, then deleted |
| `compactor.rs` | ~300 | Threshold monitor, spawns compaction workers |
| `channel_prompt.rs` | ~200 | System prompt assembly (identity + memories + status) |
| `status.rs` | ~150 | StatusBlock snapshot injected each turn |
| `ingestion.rs` | ~300 | Memory ingestion from files (text, markdown, PDF) |
| `process_control.rs` | ~150 | Cancellation signals, process lifecycle |
| `invariant_harness.rs` | ~100 | `#[cfg(test)]` harness for agent invariants |

## Key Patterns (unique to this module)

**Non-blocking channel.** Channel never awaits branch or worker results. Results arrive as events and are incorporated on the next turn via `channel_dispatch.rs`. If you see a channel awaiting a branch, the design is wrong.

**External history.** `Vec<Message>` is not owned by the agent. Passed per-call via `.with_history(&mut history)`. Branching is `channel_history.clone()` — no special fork API.

**WorkerState machine.** Transitions validated by `can_transition_to()` using `matches!`. Illegal transitions are runtime errors. States: `Running -> WaitingForInput -> Done | Failed`. Interactive workers pause at `WaitingForInput` until the channel routes follow-up input.

**Per-turn tool registration.** Channel tools are not static. `add_channel_tools()` / `remove_channel_tools()` run each turn to reflect current state (e.g., cancel only appears when there's something to cancel).

**StatusBlock injection.** Every channel turn gets a fresh `StatusBlock` prepended to context. Workers set their own status via `set_status` tool. Short branches are invisible unless running >3s.

**AgentDeps bundle.** All constructors take `AgentDeps` (memory_store, llm_manager, tool_server, event_tx). Never pass individual deps.

## Complexity Notes

**`cortex.rs` (4265 lines)** is the largest file. It does three distinct jobs: bulletin generation (periodic), process supervision (kills hanging workers, cleans stale branches), and interactive cortex chat. These are separate logical flows sharing the same module. Read the top-level function list before diving in.

**`channel.rs` (3149 lines)** handles message coalescing (batches rapid incoming messages before processing), per-turn tool mutation, and the main prompt loop. The coalescing logic and tool registration are the two non-obvious parts.

**`channel_dispatch.rs`** is the event processor for channel. Branch conclusions and worker completions arrive here as `ProcessEvent`s and get injected into channel history. Keeps `channel.rs` from becoming unreadable.

**`worker.rs`** includes OpenCode subprocess integration alongside the native Rig worker path. The two paths share the `WorkerState` machine but diverge in how they communicate (events vs. subprocess stdio).
