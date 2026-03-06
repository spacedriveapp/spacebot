# Cortex Chat

A global admin chat panel that provides a direct interactive line to the cortex. One conversation per agent, accessible from any channel page or the agent-level cortex tab. When opened on a channel, the last 50 channel messages are injected as context into the system prompt.

## Concept

The cortex chat is not a branch of a channel — it's a persistent, agent-scoped conversation with the cortex in interactive mode. It has the full toolset (memory, shell, file, exec, browser, web search, spawn worker) and uses SSE for response delivery and tool activity events.

Key properties:
- **One conversation per agent** — same thread regardless of which channel page you're viewing
- **Channel context is ephemeral** — injected into the system prompt, not the chat history. Switching channels changes the context for the next message
- **Persistent history** — stored in SQLite, survives refresh and navigation
- **Current behavior** — SSE emits tool activity and a final `done` message with full response text
- **Target behavior** — character-by-character text delta streaming via SSE
- **Full tool access** — everything except reply/react/skip (those are channel-only platform tools)

## Data Model

Messages stored in a new SQLite table per agent:

```sql
CREATE TABLE cortex_chat_messages (
    id TEXT PRIMARY KEY,
    role TEXT NOT NULL,            -- 'user' | 'assistant'
    content TEXT NOT NULL,
    channel_context TEXT,          -- which channel_id was active when sent (nullable)
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

`channel_context` records which channel the admin was viewing when they sent the message. It's metadata for audit — the conversation itself is global.

## Phase 1: LLM Streaming Infrastructure

`SpacebotModel::stream()` was stubbed when this design was drafted; the LLM streaming layer now exists. Remaining work is adopting stream-native Rig loops in cortex chat (`stream_prompt`) so SSE can emit text deltas instead of only final responses.

### 1a. Anthropic SSE Streaming

Implement `stream_anthropic()` in `src/llm/model.rs`:
- Same request as `call_anthropic()` but with `"stream": true`
- Parse Anthropic SSE format: `message_start`, `content_block_start`, `content_block_delta` (text + tool input deltas), `content_block_stop`, `message_delta` (stop reason + usage), `message_stop`
- Return Rig's `StreamingCompletionResponse` wrapping a `Pin<Box<dyn Stream<Item = StreamingChoice>>>`

### 1b. OpenAI/OpenRouter SSE Streaming

Implement `stream_openai()` / `stream_openrouter()` in `src/llm/model.rs`:
- Same request but with `"stream": true, "stream_options": { "include_usage": true }`
- Parse OpenAI SSE format: `choices[].delta.content` for text, `choices[].delta.tool_calls[]` for tool calls, final chunk has `usage`
- OpenRouter uses identical format to OpenAI

### 1c. `SpacebotModel::stream()` Implementation

Wire the stream method to dispatch to the right provider, same as `completion()`. No fallback chain on streaming initially — just the primary model.

### 1d. `RawStreamingResponse` Type

The current placeholder struct needs to implement Rig's streaming response trait, mapping provider SSE events into Rig's `StreamingCompletionResponse` which yields `StreamingChoice` items for multi-turn tool calling.

### 1e. `StreamingPromptHook` for `SpacebotHook`

Implement the streaming variant of the hook in `src/hooks/spacebot.rs`. The key new method is `on_text_delta(&self, delta: &str)` which fires on each text chunk — the cortex chat handler uses this to forward deltas to the client SSE stream.

## Phase 2: Cortex Chat Backend

### 2a. Migration

New migration adding the `cortex_chat_messages` table to each agent's SQLite database.

### 2b. `CortexChatStore`

Simple SQLite CRUD in `src/agent/cortex_chat.rs`:
- `load_history(limit)` -> `Vec<CortexChatMessage>`
- `save_message(role, content, channel_context)` -> message ID
- `clear()` -> delete all

### 2c. `CortexChatSession`

Core struct holding: agent deps, tool server, store.

`send_message(user_text, channel_context_id) -> impl Stream<Item = CortexChatEvent>`:

1. Load cortex chat history from DB
2. If `channel_context_id` is provided, fetch the last 50 messages from that channel
3. Render `cortex_chat.md.j2` with: identity context, memory bulletin, channel transcript (if any), worker capabilities
4. Current implementation: `agent.prompt(user_text).with_history(&mut history)` with SSE tool events + final response
5. Target implementation: `agent.stream_prompt(user_text).with_history(...).multi_turn(50)` with text-delta forwarding
6. Save both user message and assistant response to DB

### 2d. Tool Server Factory

New `create_cortex_chat_tool_server()` in `src/tools.rs`:
- Memory tools: `MemorySaveTool`, `MemoryRecallTool`, `MemoryDeleteTool`
- `ChannelRecallTool`
- Worker execution tools: `ShellTool`, `FileTool`, `ExecTool`
- `BrowserTool` (if enabled)
- `WebSearchTool` (if configured)
- `SpawnWorkerTool` (when opened on a channel, uses that channel's `ChannelState`; otherwise spawns standalone workers)

### 2e. System Prompt

New file `prompts/en/cortex_chat.md.j2`:

The prompt identifies the cortex in interactive mode talking to an admin. It has full tool access, references channel context when available, and is direct/technical. Template variables: `identity_context`, `memory_bulletin`, `channel_transcript` (optional), `worker_capabilities`.

### 2f. API State Extensions

Add to `ApiState` in `src/api/state.rs`:
- `cortex_chat_sessions: ArcSwap<HashMap<String, Arc<CortexChatSession>>>` per agent
- `ChannelState` registry (extends beyond current `channel_status_blocks` to hold full `ChannelState` refs for history cloning and `SpawnWorkerTool`)

### 2g. API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/cortex-chat/messages?agent_id=...&limit=50` | Load persisted history |
| `POST` | `/api/cortex-chat/send` | Send message, returns SSE stream (`409` when a send is already in flight) |
| `DELETE` | `/api/cortex-chat/messages?agent_id=...` | Clear history |

POST accepts `{ agent_id, message, channel_id? }` and returns `Content-Type: text/event-stream`.
If a prior send is still running for the same agent session, the endpoint now returns `409 CONFLICT` with no stream; clients should wait briefly and retry:

```
event: text_delta
data: {"text": "Let me check"}

event: tool_started
data: {"tool": "memory_recall", "args_preview": "searching for..."}

event: tool_completed
data: {"tool": "memory_recall", "result_preview": "Found 3 memories..."}

event: done
data: {"full_text": "Based on the memories I found..."}

event: error
data: {"message": "Model returned an error..."}
```

## Phase 3: Frontend

### 3a. API Client Extensions

New types and methods in `interface/src/api/client.ts`:
- `CortexChatMessage` type (id, role, content, channel_context, created_at)
- `CortexChatEvent` union type (text_delta, tool_started, tool_completed, done, error)
- `cortexChatMessages(agentId, limit?)` — GET
- `cortexChatSend(agentId, message, channelId?)` — POST, returns ReadableStream
- `cortexChatClear(agentId)` — DELETE

### 3b. `useCortexChat` Hook

New hook in `interface/src/hooks/useCortexChat.ts`:

State: `messages`, `streamingText`, `activeTools`, `isStreaming`

`sendMessage(text, channelId?)`:
1. Optimistically add user message to local state
2. POST to `/api/cortex-chat/send` with fetch streaming
3. Parse SSE from response body using `ReadableStream` + `TextDecoder`
4. Forward `text_delta` -> accumulate `streamingText`
5. Forward `tool_started`/`tool_completed` -> update `activeTools`
6. On `done` -> finalize assistant message, clear streaming state

### 3c. `CortexChatPanel` Component

New component in `interface/src/components/CortexChatPanel.tsx`:

- Fixed-width panel (400px), right side of content area
- Header: "Cortex" label + channel context indicator + clear button + close button
- Message list: scrollable, auto-scroll, markdown rendering
- Tool activity: inline indicators when tools are running
- Streaming text: renders incrementally with blinking cursor
- Input area: text input + send button, disabled while streaming
- framer-motion slide-in animation from the right
- Distinct background shade (`bg-app-darkBox/30`) to differentiate from channel timeline

### 3d. Channel Detail Integration

Modify `interface/src/routes/ChannelDetail.tsx` to a two-panel layout:

```tsx
<div className="flex h-full">
  <div className="flex-1 flex flex-col overflow-hidden">
    {/* existing channel timeline */}
  </div>
  <AnimatePresence>
    {cortexOpen && (
      <motion.div initial={{ width: 0 }} animate={{ width: 400 }} exit={{ width: 0 }}>
        <CortexChatPanel agentId={agentId} channelId={channelId} onClose={...} />
      </motion.div>
    )}
  </AnimatePresence>
</div>
```

Toggle button (brain icon) in the channel sub-header.

### 3e. Agent-Level Cortex Tab

Replace the cortex tab placeholder with the `CortexChatPanel` in full-page mode (no channel context). Same component, `channelId` is `null`.

## Build Order

```
Phase 1a-1e  LLM Streaming         foundation, no UI dependency
Phase 2a     Migration              standalone
Phase 2b-2c  CortexChatSession      depends on Phase 1 + 2a
Phase 2d     Tool server factory    standalone, parallel with Phase 1
Phase 2e     System prompt          standalone, parallel with Phase 1
Phase 2f-2g  API endpoints          depends on 2b-2c
Phase 3a-3b  Client + hook          depends on 2g
Phase 3c-3e  UI components          depends on 3a-3b
```

Phases 2d and 2e can run in parallel with Phase 1. Phase 3 is entirely frontend.

## File Changes

**New files:**
- `src/agent/cortex_chat.rs` — session struct, store, streaming logic
- `prompts/en/cortex_chat.md.j2` — interactive cortex prompt
- `interface/src/components/CortexChatPanel.tsx` — panel UI
- `interface/src/hooks/useCortexChat.ts` — streaming + state management

**Modified files:**
- `src/llm/model.rs` — `stream()` impl, provider streaming methods, `RawStreamingResponse`
- `src/hooks/spacebot.rs` — `StreamingPromptHook` impl
- `src/api/server.rs` — three new endpoints
- `src/api/state.rs` — cortex chat sessions, channel state registry
- `src/tools.rs` — `create_cortex_chat_tool_server()` factory
- `src/agent.rs` — add `cortex_chat` submodule
- `src/db/migrations.rs` — new migration
- `src/main.rs` — register cortex chat sessions at startup
- `interface/src/api/client.ts` — new types + API methods
- `interface/src/routes/ChannelDetail.tsx` — two-panel layout + toggle
- `interface/src/router.tsx` — cortex tab wiring

## Notes

- The streaming infrastructure is reusable — once it works for cortex chat, it can be wired into the channel's `OutboundResponse::StreamChunk` path for platform streaming
- The cortex chat's channel history injection is read-only — it never writes to the channel's history or triggers the channel's LLM
- Rate limiting on the POST endpoint (one concurrent request per agent) prevents double-sends while streaming
- `SpawnWorkerTool` spawns standalone workers (channel_id: None) when no channel context is active
