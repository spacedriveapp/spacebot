# Conversation & Channel Settings — Unifying Configuration and Sunsetting Cortex Chat

**Status:** Draft
**Date:** 2026-03-25
**Context:** The Spacedrive Spacebot interface is gaining multi-conversation support. The existing split between channels (no tools, has personality) and cortex chat (all tools, no personality) creates UX confusion. This doc proposes a unified per-conversation settings model that absorbs cortex chat's capabilities into normal conversations. It also defines worker context settings and renames "webchat" to "portal" throughout the codebase.

---

## Problem

There are four problems converging:

### 1. Cortex Chat is a UX Dead End

Cortex chat exists because channels deliberately lack tools — they delegate to branches and workers. When a user wants to configure something, query memories, or run shell commands directly, they go to cortex chat. But from a user's perspective, "cortex" is an unexplained internal concept. Why is there a separate chat? Why does it behave differently? Why can't I just tell my agent to do this in our normal conversation?

Cortex chat is one session per agent. It has no personality. It gets a different system prompt. It persists to a separate table (`cortex_chat_messages`). None of this is visible or meaningful to the user — it's implementation leaking into product.

### 2. No Per-Conversation Configuration

The portal conversation feature just landed. Users can now create multiple conversations. But every conversation behaves identically — same model, same tools, same memory injection. There is no way to say "this conversation uses Opus" or "this conversation doesn't need memory context" or "let me use tools directly here."

The model selector in the Spacedrive UI is decorative — Spacebot ignores it because models are configured per process type in TOML, not per conversation.

### 3. Channel Configuration is Agent-Global

Channels (Discord, Slack, etc.) have `require_mention` on bindings and `listen_only_mode` in config. But the model, memory injection, and tool access are all agent-level. You can't say "this Discord channel uses Haiku" or "this Slack channel doesn't need working memory." All channels for an agent share the same routing config.

### 4. Workers Are Context-Starved

Workers receive only a task description and a static system prompt. No conversation history, no knowledge synthesis, no working memory. The channel must pack everything the worker needs into the task string. This is limiting — a worker can't understand the broader conversation context, can't leverage memory the agent has built up, and can't see what led to the task being created.

Branches, by contrast, get the full channel history clone plus memory tools. The gap between branch context richness and worker context poverty is entirely hardcoded. There's no way to say "this worker should get conversation history" or "this worker should get the agent's memory context."

---

## What Exists Today

### Configuration Hierarchy (Current)

```
Agent Config (TOML)
├── routing: { channel, branch, worker, compactor, cortex }
│   ├── task_overrides: { "coding" → model }
│   └── fallbacks: { model → [fallback_models] }
├── memory_persistence: { enabled, message_interval }
├── working_memory: { enabled, context_token_budget, ... }
├── cortex: { knowledge_synthesis (change-driven), maintenance, ... }
├── channel_config: { listen_only_mode, save_attachments }
└── bindings[]: { channel, require_mention, ... }
```

### Tool Access (Current)

| Context | Memory Tools | Execution Tools | Delegation Tools | Platform Tools |
|---------|-------------|-----------------|------------------|----------------|
| Channel | No | No | Yes (branch, spawn_worker, route) | Yes (reply, react, skip) |
| Branch | Yes (recall, save, delete) | No | Yes (spawn_worker) | No |
| Worker | No | Yes (shell, file_*, browser) | No | No |
| Cortex Chat | Yes | Yes | Yes (spawn_worker) | No |

Cortex chat is the only context that combines memory tools + execution tools + worker spawning. That's why it exists — it's the "power mode."

### Memory Injection (Current)

The memory system has three layers that get injected into prompts:

1. **Knowledge Synthesis** (replaced the old "bulletin") — LLM-curated briefing synthesized from the memory graph (decisions, preferences, goals, high-importance facts, active tasks). Change-driven regeneration via dirty-flag + debounce. Primary memory context.
2. **Working Memory** — Temporal event log. Today's events in detail, yesterday compressed to summary, earlier this week as a paragraph. Real-time, rendered on every prompt.
3. **Channel Activity Map** — Cross-channel visibility. What's happening in other channels. Real-time.

The old bulletin is kept only as a startup fallback if knowledge synthesis fails. The prompt template prefers `knowledge_synthesis`, falls back to `memory_bulletin`, and renders `working_memory` separately.

| Context | Knowledge Synthesis | Working Memory (Temporal) | Channel Activity Map | Tool-Based Recall |
|---------|-------------------|--------------------------|---------------------|-------------------|
| Channel | Yes | Yes (if enabled) | Yes (if enabled) | No (delegates to branch) |
| Branch | Yes | No | No | Yes |
| Worker | No | No | No | No |
| Cortex Chat | Yes | No | No | Yes |

### Worker Context (Current)

When a worker is spawned today:

1. **System prompt** — Rendered from `worker.md.j2` with filesystem paths, sandbox config, tool secrets, available skills. No memory, no conversation context.
2. **Task description** — The only user-facing input. Becomes the first prompt. Channel must front-load all necessary context into this string.
3. **Skills** — Available skills listed with suggested skills flagged by the channel.
4. **Tools** — shell, file_read/write/edit/list, set_status, read_skill, task_update, optionally browser/web_search/MCP.
5. **No history** — Fire-and-forget workers start with empty history. Interactive workers only get history from prior turns in their own session.
6. **No memory** — No knowledge synthesis, no working memory, no memory tools.

Compare to branches, which get a **full clone of the channel history** plus memory tools (recall, save, delete) and knowledge synthesis via system prompt.

---

## Proposed Design

### Core Idea: Conversation Modes

Every conversation (portal or platform channel) gets a **mode** that determines its behavior. The mode is a small set of flags. Agent config provides defaults. Per-channel and per-conversation overrides are optional.

### The Settings

```
ConversationSettings {
    model: Option<String>,              // LLM override for this conversation's channel process
    memory: MemoryMode,                 // How memory is used
    delegation: DelegationMode,         // How tools work
    worker_context: WorkerContextMode,  // What context workers receive
}
```

#### `model: Option<String>`

When set, overrides `routing.channel` for the channel process in this conversation. Branches and workers spawned from this conversation also inherit the override (overriding `routing.branch` and `routing.worker` respectively).

When `None`, falls through to agent-level routing config as today.

This replaces the non-functional model selector in the UI with something that actually works.

**Implementation:** `routing.resolve()` gains an optional `override_model` parameter. When present, it returns the override instead of the process-type default. Task-type overrides (`task_overrides`) still take priority over per-conversation overrides for workers/branches, because task-type routing is a quality-of-output concern, not a user preference.

#### `memory: MemoryMode`

```rust
enum MemoryMode {
    Full,       // Knowledge synthesis + working memory + channel activity map + auto-persistence (current default)
    Ambient,    // All memory context injected, but no auto-persistence and no memory tools
    Off,        // No memory context injected, no memory tools, no persistence
}
```

- **Full** — The agent knows everything it normally knows. All three memory layers injected (knowledge synthesis, working memory, channel activity map). Memory persistence branches fire. This is today's default.
- **Ambient** — The agent gets all memory context (knowledge synthesis, working memory, channel activity map) but doesn't write new memories from this conversation. No auto-persistence branches. Useful for throwaway chats or sensitive topics.
- **Off** — Raw mode. No memory context injected — no knowledge synthesis, no working memory, no channel activity map. The conversation is stateless relative to the agent's memory. System prompt still includes identity/personality. This is the spirit of what cortex chat provides today.

#### `delegation: DelegationMode`

```rust
enum DelegationMode {
    Standard,   // Current channel behavior: must delegate via branch/worker
    Direct,     // Channel gets all tools: memory, shell, file, browser, + delegation tools
}
```

- **Standard** — The channel has `reply`, `branch`, `spawn_worker`, `route`, `cancel`, `skip`, `react`. It delegates heavy work. It stays responsive. This is today's default and the right mode for ongoing async conversations.
- **Direct** — The channel gets the full tool set: memory tools, shell, file operations, browser, web search, **plus** delegation tools. The agent can choose to do things directly or spawn workers. This is what cortex chat provides today — the "power user" mode.

**Why not just "tools on/off"?** Because turning tools "off" for a channel is meaningless — it already barely has tools. The meaningful toggle is whether the channel can act directly or must delegate. "Direct" mode is strictly additive.

#### `worker_context: WorkerContextMode`

```rust
struct WorkerContextMode {
    history: WorkerHistoryMode,     // What conversation context the worker sees
    memory: WorkerMemoryMode,      // What memory context the worker gets
}

enum WorkerHistoryMode {
    None,       // No conversation history (current default)
    Summary,    // LLM-generated summary of recent conversation context
    Recent(u32), // Last N messages from the parent conversation, injected into system prompt
    Full,       // Full conversation history clone (branch-style)
}

enum WorkerMemoryMode {
    None,       // No memory context (current default)
    Ambient,    // Knowledge synthesis + working memory injected into system prompt (read-only)
    Tools,      // Ambient context + memory_recall tool (can search but not write)
    Full,       // Ambient context + full memory tools (recall, save, delete) — branch-level access
}
```

This is the most important new axis. Today, the gap between "worker" and "branch" is a cliff — branches see everything, workers see nothing. Worker context settings turn this into a spectrum.

**`WorkerHistoryMode` options:**

- **None** — Current behavior. Worker sees only the task description. Channel must front-load all context into the task string. Cheapest, most isolated, fastest to start.
- **Summary** — Before spawning the worker, the channel generates a brief summary of the relevant conversation context and prepends it to the worker's system prompt. Cost: one extra LLM call at spawn time (can use a cheap model). Benefit: worker understands why the task exists without seeing the full transcript.
- **Recent(N)** — Last N conversation messages injected into the worker's system prompt as context (similar to how cortex chat injects channel context). No extra LLM call. Worker sees recent exchanges but not the full history. Good middle ground.
- **Full** — Worker receives a clone of the full channel history, same as a branch. Most expensive in tokens, but gives the worker complete conversational context.

**`WorkerMemoryMode` options:**

- **None** — Current behavior. No memory context, no memory tools. Worker is a pure executor.
- **Ambient** — Knowledge synthesis + working memory injected into the worker's system prompt. Worker has ambient awareness of what the agent knows — identity, facts, preferences, decisions, recent activity — but can't search or write. Read-only, no extra tools, minimal cost.
- **Tools** — Ambient context + `memory_recall` tool. Worker can actively search the agent's memory when it needs context. Cannot save new memories (that's still a branch concern). This is the "informed executor" mode.
- **Full** — Ambient context + full memory tools (recall, save, delete). Worker operates at branch-level memory access. Use sparingly — this blurs the worker/branch boundary.

**Why this matters for the Spacedrive interface:** When a user starts a "Hands-on" conversation in Spacedrive (Direct delegation, memory off), the workers spawned from that conversation should probably get more context than workers spawned from a Discord channel with 50 participants. The user is sitting right there, interacting directly — the workers are extensions of that focused session.

**Implementation:** Worker context settings are resolved at spawn time in `spawn_worker_from_state()`:

```rust
fn spawn_worker_with_context(
    state: &ChannelState,
    task: &str,
    settings: &ResolvedConversationSettings,
) {
    let mut system_prompt = render_worker_prompt(...);

    // Inject memory context
    match settings.worker_context.memory {
        WorkerMemoryMode::None => { /* current behavior */ }
        WorkerMemoryMode::Ambient | WorkerMemoryMode::Tools | WorkerMemoryMode::Full => {
            // Inject knowledge synthesis (primary memory context)
            let knowledge = state.deps.runtime_config.knowledge_synthesis();
            system_prompt.push_str(&format!("\n\n## Knowledge Context\n{knowledge}"));
            // Inject working memory (temporal context)
            if let Some(wm) = render_working_memory(&state.deps.working_memory_store, &config).await {
                system_prompt.push_str(&format!("\n\n{wm}"));
            }
        }
    }

    // Build history
    let history = match settings.worker_context.history {
        WorkerHistoryMode::None => vec![],
        WorkerHistoryMode::Summary => {
            let summary = generate_context_summary(state, task).await;
            // Inject as a system message at the start
            vec![Message::system(format!("[Conversation context]: {summary}"))]
        }
        WorkerHistoryMode::Recent(n) => {
            let h = state.history.read().await;
            h.iter().rev().take(n as usize).rev().cloned().collect()
        }
        WorkerHistoryMode::Full => {
            let h = state.history.read().await;
            h.clone()
        }
    };

    // Build tool server
    let mut tool_server = create_worker_tool_server(...);
    match settings.worker_context.memory {
        WorkerMemoryMode::Tools => {
            tool_server.add(MemoryRecallTool::new(...));
        }
        WorkerMemoryMode::Full => {
            tool_server.add(MemoryRecallTool::new(...));
            tool_server.add(MemorySaveTool::new(...));
            tool_server.add(MemoryDeleteTool::new(...));
        }
        _ => {}
    }

    Worker::new(..., task, system_prompt, history, tool_server)
}
```

### Configuration Hierarchy (Proposed)

```
Agent Config (TOML) — defaults for all conversations
├── defaults.conversation_settings:
│   ├── model: None (use routing config)
│   ├── memory: Full
│   ├── delegation: Standard
│   └── worker_context: { history: None, memory: None }
│
├── Per-Channel Override (TOML or runtime API)
│   └── channels["discord:guild:channel"].settings:
│       ├── model: "anthropic/claude-haiku-4.5"
│       ├── memory: Ambient
│       ├── delegation: Standard
│       └── worker_context: { history: None, memory: Ambient }
│
└── Per-Conversation Override (runtime, stored in DB)
    └── portal_conversations.settings:
        ├── model: "anthropic/claude-opus-4"
        ├── memory: Off
        ├── delegation: Direct
        └── worker_context: { history: Recent(20), memory: Tools }
```

**Resolution order:** Per-conversation > Per-channel > Agent default > System default

For portal conversations, "per-channel" doesn't apply (there's no binding). For platform channels, "per-conversation" doesn't apply (Discord threads aren't separate conversations in this model — they share the channel's settings).

### Sunsetting Cortex Chat

With these settings, cortex chat becomes a conversation preset, not a separate system:

**"Cortex mode" conversation** = `{ model: None, memory: Off, delegation: Direct, worker_context: { history: Recent(20), memory: Tools } }`

The user starts a new conversation, toggles delegation to Direct, turns memory off, and they have cortex chat. No separate concept, no separate table, no separate API.

**Migration path:**

1. Add `ConversationSettings` to `portal_conversations` table (JSON column).
2. Add `ConversationSettings` to channel config (TOML + runtime).
3. Wire settings into channel creation — read settings when spawning a channel process.
4. In `Direct` mode, use `create_cortex_chat_tool_server()` (or a new unified factory) instead of `add_channel_tools()`.
5. In `Off` memory mode, skip knowledge synthesis, working memory, and channel activity map injection in the system prompt.
6. Deprecate `/api/cortex-chat/*` endpoints. Keep them working but add a sunset header.
7. Remove cortex chat from the UI. Replace with conversation presets.

**What about cortex chat's channel context injection?** Today, opening cortex chat on a channel page injects the last 50 messages from that channel. This is useful. In the new model, a "Direct" conversation can have a `channel_context` field — "I'm looking at Discord #general" — and the system prompt injects that context. This is an optional enhancement, not a blocker.

**What about cortex chat's admin-only access?** Cortex chat is implicitly admin-only because it's in the dashboard. In the portal/Spacedrive context, all users are the owner. If multi-user access control becomes needed, it should be a separate authorization layer, not a property of conversation mode.

---

## Schema Changes

### `portal_conversations` table (renamed from `webchat_conversations`)

Add a `settings` JSON column:

```sql
ALTER TABLE portal_conversations ADD COLUMN settings TEXT;
-- JSON: {"model": "...", "memory": "full|ambient|off", "delegation": "standard|direct",
--        "worker_context": {"history": "none|summary|recent:20|full", "memory": "none|ambient|tools|full"}}
-- NULL means "use defaults"
```

### `channels` table

Add a `settings` JSON column:

```sql
ALTER TABLE channels ADD COLUMN settings TEXT;
-- Same schema as above. NULL means "use agent defaults"
```

### Config TOML

```toml
[defaults.conversation_settings]
memory = "full"           # "full", "ambient", "off"
delegation = "standard"   # "standard", "direct"
# model omitted = use routing config

[defaults.conversation_settings.worker_context]
history = "none"          # "none", "summary", "recent:20", "full"
memory = "none"           # "none", "ambient", "tools", "full"
```

Per-channel overrides in bindings:

```toml
[[bindings]]
agent_id = "star"
channel = "discord"
guild_id = "123456"
channel_ids = ["789"]

[bindings.settings]
model = "anthropic/claude-haiku-4.5"
memory = "ambient"
delegation = "standard"

[bindings.settings.worker_context]
history = "none"
memory = "ambient"
```

---

## API Changes

### Portal Endpoints (renamed from `/webchat/*`)

**POST /portal/conversations** — Add optional `settings` field:

```json
{
    "agent_id": "star",
    "title": "Quick coding help",
    "settings": {
        "model": "anthropic/claude-opus-4",
        "memory": "off",
        "delegation": "direct",
        "worker_context": {
            "history": "recent:20",
            "memory": "tools"
        }
    }
}
```

**PUT /portal/conversations/{id}** — Allow updating `settings`:

```json
{
    "agent_id": "star",
    "settings": {
        "memory": "ambient"
    }
}
```

Settings changes take effect on the **next message** in the conversation. In-flight channel processes are not interrupted.

**GET /portal/conversations** — Return `settings` in response (null = defaults).

**Full endpoint rename:**

| Old | New |
|-----|-----|
| `POST /webchat/send` | `POST /portal/send` |
| `GET /webchat/history` | `GET /portal/history` |
| `GET /webchat/conversations` | `GET /portal/conversations` |
| `POST /webchat/conversations` | `POST /portal/conversations` |
| `PUT /webchat/conversations/{id}` | `PUT /portal/conversations/{id}` |
| `DELETE /webchat/conversations/{id}` | `DELETE /portal/conversations/{id}` |

### New Endpoint: GET /api/conversation-defaults

Returns the resolved default settings for new conversations:

```json
{
    "model": "anthropic/claude-sonnet-4",
    "memory": "full",
    "delegation": "standard",
    "worker_context": {
        "history": "none",
        "memory": "none"
    },
    "available_models": ["anthropic/claude-sonnet-4", "anthropic/claude-opus-4", "anthropic/claude-haiku-4.5"],
    "memory_modes": ["full", "ambient", "off"],
    "delegation_modes": ["standard", "direct"],
    "worker_history_modes": ["none", "summary", "recent", "full"],
    "worker_memory_modes": ["none", "ambient", "tools", "full"]
}
```

This replaces the hardcoded model list in the Spacedrive UI.

### Channel Settings Endpoint

**PUT /api/channels/{id}/settings** — Update per-channel settings:

```json
{
    "agent_id": "star",
    "settings": {
        "model": "anthropic/claude-haiku-4.5",
        "memory": "ambient",
        "worker_context": {
            "memory": "ambient"
        }
    }
}
```

**GET /api/channels/{id}** — Return `settings` in channel info.

---

## Implementation in the Channel Process

### Channel Creation Changes

When a channel is created (`main.rs` routing logic), resolve settings:

```rust
fn resolve_conversation_settings(
    agent_config: &AgentConfig,
    channel_settings: Option<&ConversationSettings>,
    conversation_settings: Option<&ConversationSettings>,
) -> ResolvedConversationSettings {
    let defaults = &agent_config.conversation_settings;

    // Per-conversation > Per-channel > Agent default
    ResolvedConversationSettings {
        model: conversation_settings.and_then(|s| s.model.clone())
            .or_else(|| channel_settings.and_then(|s| s.model.clone()))
            .or_else(|| defaults.model.clone()),
        memory: conversation_settings.map(|s| s.memory)
            .or_else(|| channel_settings.map(|s| s.memory))
            .unwrap_or(defaults.memory),
        delegation: conversation_settings.map(|s| s.delegation)
            .or_else(|| channel_settings.map(|s| s.delegation))
            .unwrap_or(defaults.delegation),
        worker_context: conversation_settings.and_then(|s| s.worker_context.clone())
            .or_else(|| channel_settings.and_then(|s| s.worker_context.clone()))
            .unwrap_or_else(|| defaults.worker_context.clone()),
    }
}
```

### Tool Server Selection

In `run_agent_turn()`, check delegation mode:

```rust
match self.resolved_settings.delegation {
    DelegationMode::Standard => {
        // Current behavior: add_channel_tools()
        self.tool_server.add_channel_tools(...);
    }
    DelegationMode::Direct => {
        // Full tool access: memory + execution + delegation
        // Use a new factory or merge channel + cortex tool sets
        self.tool_server.add_direct_mode_tools(...);
    }
}
```

### Memory Injection

In `build_system_prompt()`, check memory mode:

```rust
match self.resolved_settings.memory {
    MemoryMode::Full => {
        // Inject all memory layers + enable persistence branches
        prompt.set("knowledge_synthesis", &knowledge_synthesis);
        prompt.set("working_memory", &working_memory_context);
        prompt.set("channel_activity_map", &channel_activity_map);
    }
    MemoryMode::Ambient => {
        // Inject all memory layers, but no persistence
        prompt.set("knowledge_synthesis", &knowledge_synthesis);
        prompt.set("working_memory", &working_memory_context);
        prompt.set("channel_activity_map", &channel_activity_map);
        // Skip scheduling persistence branches
    }
    MemoryMode::Off => {
        // No memory context at all
        prompt.set("knowledge_synthesis", "");
        prompt.set("working_memory", "");
        prompt.set("channel_activity_map", "");
    }
}
```

### Model Override

In the LLM call, check for model override:

```rust
let model_name = match &self.resolved_settings.model {
    Some(override_model) => override_model.clone(),
    None => routing.resolve(ProcessType::Channel, None).to_string(),
};
```

For spawned workers and branches, propagate the override:

```rust
// In spawn_worker
let worker_model = match &self.resolved_settings.model {
    Some(override_model) => override_model.clone(),
    None => routing.resolve(ProcessType::Worker, task_type).to_string(),
};
```

---

## Interface Changes — Separation of Concerns

There are three interface surfaces that interact with this feature. The critical constraint is: **do not break the legacy Spacebot dashboard while building the Spacedrive experience.**

### The Three Surfaces

```
┌─────────────────────────────────────────────────────────────────┐
│  Spacebot Server (Rust)                                         │
│  ├── /api/portal/*        — portal conversation endpoints       │
│  ├── /api/channels/*      — channel endpoints                   │
│  ├── /api/conversation-defaults — new settings metadata         │
│  └── /api/cortex-chat/*   — cortex chat (deprecated later)      │
├─────────────────────────────────────────────────────────────────┤
│  @spacebot/api-client (shared TS package)                       │
│  ├── portalSend(), portalHistory(), etc.                        │
│  ├── createPortalConversation({ settings? })  ← new field       │
│  ├── updatePortalConversation({ settings? })  ← new field       │
│  ├── getConversationDefaults()                ← new method       │
│  └── All existing methods remain unchanged                      │
├──────────────────────┬──────────────────────────────────────────┤
│  Spacebot Dashboard  │  Spacedrive Interface                    │
│  (spacebot/interface)│  (spacedrive/.../Spacebot/)              │
│                      │                                          │
│  ✗ NO changes needed │  ✓ All new settings UI goes here        │
│  - AgentConfig keeps │  - SpacebotContext gets settings state   │
│    its model routing │  - ChatComposer gets settings controls   │
│  - CortexChatPanel   │  - ConversationScreen shows active mode │
│    keeps working     │  - New conversation gets settings picker │
│  - ChannelDetail     │  - Presets for common configurations     │
│    unchanged         │                                          │
│  - Settings page     │                                          │
│    unchanged         │                                          │
└──────────────────────┴──────────────────────────────────────────┘
```

### Why the Dashboard Doesn't Change

The Spacebot dashboard (`spacebot/interface/`) is a developer-facing admin control plane. It already has:

- **Agent-level model routing** in `AgentConfig.tsx` — 6 model slots (channel, branch, worker, compactor, cortex, voice) with `ModelSelect` component and adaptive thinking controls. These are agent-wide defaults. They keep working. The new per-conversation settings are *overrides* on top of these defaults.
- **Cortex chat** in `CortexChatPanel.tsx` — used in the Cortex route and Channel Detail views. This keeps working until Phase 4 sunset. No dashboard changes needed for deprecation — we just stop rendering it later.
- **Channel management** in `ChannelDetail.tsx` — read-only channel timeline with cortex panel. No per-channel settings UI exists today. Adding per-channel settings to the dashboard is a *future* enhancement, not a blocker.
- **Global settings** in `Settings.tsx` (3000+ lines) — provider credentials, channel bindings, server config. None of this changes.

The dashboard consumes the same `@spacebot/api-client` package, but it never calls `createPortalConversation()` with settings because its `WebChatPanel` component creates conversations without settings (defaulting to `null`). The new `settings` field is optional — `null` means "use agent defaults." Existing code that doesn't pass settings continues working identically.

### What Changes in @spacebot/api-client

All changes are **additive**. No breaking changes to existing methods.

**Renamed methods (Phase 0 — portal rename):**

```typescript
// Old (removed after rename)          // New
apiClient.webchatSend(...)           → apiClient.portalSend(...)
apiClient.webchatHistory(...)        → apiClient.portalHistory(...)
apiClient.listWebchatConversations() → apiClient.listPortalConversations()
apiClient.createWebchatConversation()→ apiClient.createPortalConversation()
apiClient.updateWebchatConversation()→ apiClient.updatePortalConversation()
apiClient.deleteWebchatConversation()→ apiClient.deletePortalConversation()
```

The rename touches both surfaces simultaneously — the Spacebot dashboard's `WebChatPanel` and `AgentChat.tsx` import from api-client too. This is a coordinated rename across both consumers. It's mechanical but must be done in one pass.

**New methods (Phase 1):**

```typescript
// Fetch resolved defaults + available options for settings UI
apiClient.getConversationDefaults(agentId: string): Promise<ConversationDefaultsResponse>

// Types
interface ConversationDefaultsResponse {
    model: string;                      // Current default model name
    memory: MemoryMode;                 // Current default memory mode
    delegation: DelegationMode;         // Current default delegation mode
    worker_context: WorkerContextMode;  // Current default worker context
    available_models: ModelOption[];     // All available models with metadata
    memory_modes: MemoryMode[];
    delegation_modes: DelegationMode[];
    worker_history_modes: string[];
    worker_memory_modes: string[];
}

interface ModelOption {
    id: string;                         // e.g. "anthropic/claude-sonnet-4"
    name: string;                       // e.g. "Claude Sonnet 4"
    provider: string;                   // e.g. "anthropic"
    context_window: number;
    supports_tools: boolean;
    supports_thinking: boolean;
}

type MemoryMode = 'full' | 'ambient' | 'off';
type DelegationMode = 'standard' | 'direct';

interface WorkerContextMode {
    history: string;                    // "none" | "summary" | "recent:N" | "full"
    memory: string;                     // "none" | "ambient" | "tools" | "full"
}

interface ConversationSettings {
    model?: string | null;
    memory?: MemoryMode;
    delegation?: DelegationMode;
    worker_context?: WorkerContextMode;
}
```

**Updated method signatures (additive):**

```typescript
// createPortalConversation gains optional settings
apiClient.createPortalConversation(input: {
    agentId: string;
    title?: string | null;
    settings?: ConversationSettings | null;  // ← new, optional
}): Promise<PortalConversationResponse>

// updatePortalConversation gains optional settings
apiClient.updatePortalConversation(input: {
    agentId: string;
    sessionId: string;
    title?: string | null;
    archived?: boolean;
    settings?: ConversationSettings | null;  // ← new, optional
}): Promise<PortalConversationResponse>

// PortalConversationResponse and PortalConversationSummary gain settings field
interface PortalConversationResponse {
    // ...existing fields...
    settings: ConversationSettings | null;   // ← new, nullable
}

interface PortalConversationSummary {
    // ...existing fields...
    settings: ConversationSettings | null;   // ← new, nullable
}
```

**Dashboard impact:** The dashboard's `WebChatPanel` calls `createWebchatConversation({ agentId, title })` (soon `createPortalConversation`). After the type update, `settings` is optional, so this call continues to work — settings defaults to `null`, which means "use agent defaults." The dashboard never needs to pass settings unless it wants to. The `settings` field appears in the response type but the dashboard's list rendering doesn't use it — it only shows title, preview, and timestamp.

### What Changes in Spacedrive Interface

All new settings UI lives in `spacedrive/packages/interface/src/Spacebot/`. Here's the file-by-file breakdown.

#### `SpacebotContext.tsx` — State & Data

**Remove hardcoded data:**
```typescript
// DELETE these static arrays
export const modelOptions = ['Claude 3.7 Sonnet', 'GPT-5', 'Qwen 2.5 72B'];
export const projectOptions = ['Spacedrive v3', 'Spacebot Runtime', 'Hosted Platform'];
```

**Add settings state:**
```typescript
// New state in SpacebotContext
conversationDefaults: ConversationDefaultsResponse | null;  // from API
conversationSettings: ConversationSettings;                  // current selection
setConversationSettings: (s: Partial<ConversationSettings>) => void;
```

**Add defaults query:**
```typescript
// New TanStack Query — fetches available models, modes, defaults
const defaultsQuery = useQuery({
    queryKey: ['spacebot', 'conversation-defaults', selectedAgent],
    queryFn: () => apiClient.getConversationDefaults(selectedAgent),
    staleTime: 60_000, // Defaults don't change often
});
```

**Update conversation creation:**
```typescript
// handleSendMessage — when creating a new conversation, pass settings
const conversation = await apiClient.createPortalConversation({
    agentId: selectedAgent,
    title: null,
    settings: hasNonDefaultSettings(conversationSettings)
        ? conversationSettings
        : null,  // Don't store if all defaults
});
```

**Add settings from conversation response:**
```typescript
// When loading a conversation, read its settings
const activeConversationSettings: ConversationSettings | null =
    activeConversation?.settings ?? null;

// Resolved settings: conversation-specific if set, else defaults
const resolvedSettings: ConversationSettings = {
    model: activeConversationSettings?.model ?? conversationDefaults?.model ?? null,
    memory: activeConversationSettings?.memory ?? conversationDefaults?.memory ?? 'full',
    delegation: activeConversationSettings?.delegation ?? conversationDefaults?.delegation ?? 'standard',
    worker_context: activeConversationSettings?.worker_context ?? conversationDefaults?.worker_context ?? { history: 'none', memory: 'none' },
};
```

**Expose through context:**
```typescript
// Added to SpacebotContextType
conversationDefaults: ConversationDefaultsResponse | null;
resolvedSettings: ConversationSettings;
updateConversationSettings: (patch: Partial<ConversationSettings>) => Promise<void>;
```

The `updateConversationSettings` mutation calls `apiClient.updatePortalConversation()` with the new settings and invalidates the conversation query.

#### `ChatComposer.tsx` — Settings Controls

The composer currently has project and model selector popovers that are non-functional. Replace them with real settings controls.

**Replace model selector:**
```
Before: Static dropdown cycling through ['Claude 3.7 Sonnet', 'GPT-5', 'Qwen 2.5 72B']
After:  Real model dropdown populated from conversationDefaults.available_models
        Selecting a model calls updateConversationSettings({ model: selectedId })
```

**Replace project selector with memory/delegation toggles:**
```
Before: Static dropdown cycling through project names (unused)
After:  Two compact selectors:
        Memory: [On ▾] / [Context Only ▾] / [Off ▾]
        Mode: [Standard ▾] / [Direct ▾]
```

The composer bottom bar becomes:

```
┌──────────────────────────────────────────────────────┐
│ [textarea input area]                                │
│                                                      │
│ [Model: Sonnet 4 ▾]  [Memory: On ▾]  [Mode ▾]  [⎈] │
└──────────────────────────────────────────────────────┘
                                                    ^
                                            settings gear → opens
                                            full settings panel
                                            with worker context
```

The `[⎈]` gear icon opens a popover/panel for advanced settings (worker context, presets). The three inline selectors cover the most common toggles without requiring the panel.

**New component: `ConversationSettingsPanel.tsx`**

```
spacedrive/packages/interface/src/Spacebot/ConversationSettingsPanel.tsx
```

A panel (popover or slide-out) with:

```
Model
[Sonnet 4 ▾] — searchable dropdown from available_models

Memory
( ) On — Full memory context, auto-persistence
( ) Context Only — Memory context visible, no writes
( ) Off — No memory context

Mode
( ) Standard — Delegates to workers and branches
( ) Direct — Full tool access (shell, files, memory)

Worker Context (collapsed by default)
  History: [None ▾]  — None / Summary / Recent (20) / Full
  Memory:  [None ▾]  — None / Ambient / Tools / Full

Presets
[Chat]  [Focus]  [Hands-on]  [Quick]  [Deep Work]
```

Presets are pill buttons that set all fields at once. Selecting one updates the form. User can then tweak individual fields.

#### `ConversationScreen.tsx` — Active Settings Display

Show the active settings inline below the conversation header:

```
Conversation Title
Sonnet 4 · Memory On · Standard                   [⎈]
─────────────────────────────────────────────────────
[messages...]
```

This is a single line of muted text showing the resolved settings. Clicking `[⎈]` opens the settings panel for editing. If all settings are defaults, this line can be hidden or show just the model name.

When settings are `Direct` delegation, show a visual indicator (e.g., a subtle border color change or badge) so the user knows the conversation has tool access.

#### `EmptyChatHero.tsx` — Preset Selection

The empty chat hero currently says "Let's get to work, James." Update it to:

1. Replace "James" with the actual user name (from library context or platform).
2. Show preset cards below the greeting:

```
Let's get to work, Jamie

How would you like to work?

[💬 Chat]          [🔧 Hands-on]        [⚡ Quick]
Normal              Direct tools          Fast & cheap
conversation        replaces cortex       throwaway
```

Selecting a preset sets `conversationSettings` in context and focuses the composer. The user's first message creates the conversation with those settings.

#### `SpacebotLayout.tsx` — No Structural Changes

The layout (sidebar + topbar + content area) doesn't change structurally. The sidebar conversation list could optionally show a small icon or badge indicating non-default settings (e.g., a wrench icon for Direct mode conversations), but this is polish, not required.

#### New File: `ConversationSettingsPanel.tsx`

```
spacedrive/packages/interface/src/Spacebot/ConversationSettingsPanel.tsx
```

This is the only new file. It's a self-contained panel component that:
- Reads `conversationDefaults` and `resolvedSettings` from context
- Renders model dropdown, memory radio group, delegation radio group, worker context dropdowns
- Renders preset buttons
- Calls `updateConversationSettings()` on change
- Uses @sd/ui primitives (DropdownMenu, RadioGroup, Button) and semantic colors

#### Routes — No New Routes

No new routes needed. Settings are accessed from within existing conversation views via the settings panel, not from a separate page. The stub routes (Tasks, Memories, Autonomy, Schedule) are unrelated and don't change.

### Conversation Presets

Common configurations get named presets:

| Preset | Model | Memory | Delegation | Worker History | Worker Memory | Use Case |
|--------|-------|--------|------------|----------------|---------------|----------|
| Chat | Default | On | Standard | None | None | Normal conversation |
| Focus | Default | Context Only | Standard | None | None | Sensitive topic, no memory writes |
| Hands-on | Default | Off | Direct | Recent(20) | Tools | Direct tool use, replaces cortex |
| Quick | Haiku | Off | Standard | None | None | Fast, cheap, throwaway |
| Deep Work | Opus | On | Standard | Summary | Ambient | Long-running complex tasks with rich worker context |

Presets are UI sugar — they set the fields. Users can customize after selecting a preset. Presets are defined client-side in the Spacedrive interface, not stored on the server.

### Channel Settings (Spacebot Dashboard — Future)

For platform channels (Discord, Slack), per-channel settings can eventually be added to the Spacebot dashboard's `ChannelDetail.tsx`. This is **not part of the initial implementation** — it requires changes to the dashboard, which we're avoiding. The backend API (`PUT /api/channels/{id}/settings`) supports it, but the dashboard UI can be added later when the feature is proven in the Spacedrive interface.

```
Future: #general (Discord)
├── Model: [Sonnet 4 ▾]  (override / use default)
├── Memory: [On ▾]
├── Mode: [Standard ▾]
├── Mention Only: [Yes/No]
└── [Advanced: Worker Settings]
    ├── History: [None ▾]
    └── Memory: [None ▾]
```

---

## Webchat → Portal Rename

### Motivation

"Webchat" and "portal" have been two names for the same thing since the beginning. The conversation ID format already uses `portal:chat:{agent_id}:{uuid}`. The adapter registers as `"webchat"` but the platform is extracted as `"portal"`. This inconsistency has been called out in `multi-agent-communication-graph.md` as needing resolution.

The name **"portal"** wins because:
- It's already the canonical ID prefix
- It's product-facing (webchat is implementation)
- It describes what it is — a portal into the agent, usable from any surface (browser, Spacedrive, mobile)
- "Webchat" implies a web-only chat widget, which undersells what this is

### Scope

This rename touches:

**Rust source (30 files):**
- 3 module files: `src/api/webchat.rs` → `src/api/portal.rs`, `src/messaging/webchat.rs` → `src/messaging/portal.rs`, `src/conversation/webchat.rs` → `src/conversation/portal.rs`
- 18 struct/type names: `WebChat*` → `Portal*` (e.g., `WebChatConversation` → `PortalConversation`, `WebChatAdapter` → `PortalAdapter`)
- 24 function names: `webchat_*` → `portal_*`
- 8 string literals: `"webchat"` → `"portal"` (adapter name, message source)
- Module declarations in `src/api.rs`, `src/conversation.rs`, `src/messaging.rs`
- References in `src/main.rs`, `src/api/state.rs`, `src/api/server.rs`, `src/tools.rs`, `src/config/types.rs`

**SQL migration:**
- New migration: `ALTER TABLE webchat_conversations RENAME TO portal_conversations`
- Rename index: `idx_webchat_conversations_agent_updated` → `idx_portal_conversations_agent_updated`

**TypeScript/React:**
- `interface/src/components/WebChatPanel.tsx` → `PortalChatPanel.tsx`
- `interface/src/hooks/useWebChat.ts` → `usePortal.ts`
- `packages/api-client/src/client.ts`: all `webchat*` methods → `portal*`
- Generated types in `interface/src/api/schema.d.ts` and `types.ts`
- Import references in `interface/src/routes/AgentChat.tsx`

**API endpoints:**
- `/webchat/send` → `/portal/send`
- `/webchat/history` → `/portal/history`
- `/webchat/conversations` → `/portal/conversations` (CRUD)

**OpenAPI tags:** `"webchat"` → `"portal"`

**Documentation:** References in 7 design docs and README.

### Migration Strategy

1. **Rename Rust modules and types** — Mechanical rename. `WebChat` → `Portal` everywhere.
2. **Rename SQL table** — Single migration: `ALTER TABLE webchat_conversations RENAME TO portal_conversations`. SQLite supports this natively.
3. **Rename API routes** — Update route registration in `server.rs`. Add temporary redirects from old paths for any external consumers.
4. **Rename adapter** — `PortalAdapter::name()` returns `"portal"`. Update source literals. Remove the `"portal" => "webchat:{id}"` remapping hack in `tools.rs` since the names now match.
5. **Rename TypeScript** — Rename files, update imports, regenerate OpenAPI types.
6. **Conversation ID prefix stays `portal:chat:`** — Already correct. No data migration needed.
7. **Update design docs** — Search-and-replace in markdown files.

This rename is safe to do in a single commit. The "webchat" name is internal — no external API consumers depend on it (the Spacedrive interface uses the `@spacebot/api-client` package which abstracts the endpoints).

---

## What This Replaces

| Today | After |
|-------|-------|
| Cortex chat (separate system) | "Direct" mode conversation |
| Cortex chat API (`/api/cortex-chat/*`) | Deprecated, then removed |
| `cortex_chat_messages` table | No longer written to |
| `CortexChatSession` struct | Removed |
| Hardcoded model selector (non-functional) | Per-conversation model override (functional) |
| Agent-global memory settings | Per-conversation memory mode |
| Agent-global tool access | Per-conversation delegation mode |
| Context-starved workers | Configurable worker context (history + memory) |
| "webchat" naming confusion | Unified "portal" naming |
| `webchat_conversations` table | `portal_conversations` table |

---

## Migration

### Phase 0 — Portal Rename

Rename webchat → portal throughout the codebase. This is a prerequisite because it's purely mechanical and removes naming confusion before the settings work begins.

1. Rename Rust modules, structs, functions, string literals.
2. Add SQL migration to rename table and index.
3. Rename TypeScript files, components, hooks, API methods.
4. Update API route paths.
5. Remove `portal → webchat` remapping hack in tools.rs.
6. Update documentation.

### Phase 1 — Add Settings Infrastructure

1. Add `settings` column to `portal_conversations` and `channels` tables.
2. Add `ConversationSettings` struct to config types (with `WorkerContextMode`).
3. Add `conversation_settings` defaults to agent config.
4. Add `resolve_conversation_settings()` function.
5. Wire settings into conversation create/update API.
6. Add `/api/conversation-defaults` endpoint.

### Phase 2 — Wire Into Channel Process

1. Pass resolved settings into `Channel::new()`.
2. Branch tool server selection on `delegation` mode.
3. Branch memory injection on `memory` mode.
4. Branch model resolution on `model` override.
5. Wire worker context settings into `spawn_worker_from_state()`.
6. Add knowledge synthesis + working memory injection and history cloning to worker creation based on settings.
7. Add memory tools to worker tool server when `worker_context.memory` is `Tools` or `Full`.

### Phase 3 — Wire Into UI

1. Add settings controls to new conversation dialog in Spacedrive.
2. Add settings display/edit to conversation header.
3. Add presets.
4. Replace non-functional model selector.
5. Add advanced worker context settings (collapsed by default).

### Phase 4 — Sunset Cortex Chat

1. Stop showing cortex chat in dashboard UI.
2. Add deprecation notice to cortex chat API endpoints.
3. Offer migration: convert existing cortex chat threads to portal conversations with `Direct` mode.
4. Eventually remove `CortexChatSession`, cortex chat API routes, and `cortex_chat_messages` table.

---

## Open Questions

1. **Should model override propagate to workers unconditionally?** If a user picks Opus for a conversation and the agent spawns 5 workers, that's expensive. Option: override only applies to the channel process, workers still use routing defaults. Or: show estimated cost in the UI.

2. **Should "Direct" mode keep personality?** Cortex chat strips personality. But a "Direct" conversation might feel better with personality intact — you're still talking to your agent, just with more power. Proposal: keep personality, add a "technical mode" flag separately if needed.

3. **Per-channel settings for Discord threads?** Discord threads inherit their parent channel's settings. Should a user be able to override per-thread? Probably not in v1 — threads are ephemeral.

4. **Settings changes mid-conversation?** Proposed: take effect on next message. But what if the user switches from Standard to Direct mid-conversation — does the history context change? The channel process would need to reload its tool server. This is doable (tools are per-turn already) but needs care.

5. **Should `Ambient` mode still allow explicit memory tool use in Direct mode?** If delegation is Direct and memory is Ambient, the agent has memory tools but we said "no writes." Option: Ambient removes `memory_save` tool but keeps `memory_recall`. Off removes both.

6. **Channel-level settings via TOML vs API?** TOML is static and requires restart. API is runtime. Proposal: support both. TOML provides initial defaults, API allows runtime changes that persist to DB. DB overrides TOML.

7. **Worker context cost guardrails?** `WorkerHistoryMode::Full` with a long conversation could inject thousands of tokens into every worker. Should there be a token budget cap? A max message count even in Full mode? Or is that the user's problem — they opted in.

8. **Summary mode implementation?** `WorkerHistoryMode::Summary` requires an extra LLM call at spawn time. Which model? The compactor model (cheap, fast)? The conversation's override model? A hardcoded summarizer? This adds latency to worker spawning.
