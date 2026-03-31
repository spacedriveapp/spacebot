# Channel Settings Unification — Response Modes, Per-Channel Overrides, and Binding Defaults

**Status:** Draft
**Date:** 2026-03-28
**Context:** Conversation settings (model, memory, delegation, worker context) are implemented for portal conversations. Platform channels (Discord, Slack, Telegram, etc.) have no equivalent — their behavior is controlled by a fragmented mix of binding-level `require_mention`, agent-global `listen_only_mode`, per-channel key-value overrides in SettingsStore, and runtime slash commands (`/quiet`, `/active`). This doc proposes unifying all of it under ConversationSettings with a new `response_mode` field.

---

## Problem

### 1. Three systems, one concept

There are three independent mechanisms that all control "should the bot respond to this message":

| Mechanism            | Where configured                                   | Scope                                    | What it does                                                                                    |
| -------------------- | -------------------------------------------------- | ---------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `require_mention`    | `[[bindings]]` in TOML                             | Per-binding pattern                      | Drops messages at routing layer — channel never sees them                                       |
| `listen_only_mode`   | `[defaults.channel]` or `[agents.channel]` in TOML | Agent-global                             | Channel receives message, records history, runs memory persistence, but suppresses LLM response |
| `/quiet` / `/active` | Slash command in chat                              | Per-channel (persisted to SettingsStore) | Toggles listen_only at runtime for one channel                                                  |

Users encounter these in different places with different terminology and no explanation of how they interact. The global `listen_only_mode` toggle in the config UI is particularly confusing — it forces all channels into quiet mode with no per-channel escape (explicit config overrides per-channel DB values).

### 2. Platform channels have no ConversationSettings

Portal conversations got `ConversationSettings` (model, memory, delegation, worker context, model overrides) with a settings panel and persistence. Platform channels have none of this. You can't say "this Discord channel uses Haiku" or "this Slack channel doesn't need memory." The only per-channel state is `listen_only_mode` in the key-value SettingsStore.

### 3. Bindings are routing, not configuration

Bindings exist to route messages to agents. But `require_mention` is a behavioral setting masquerading as a routing field. It happens to be on bindings because channels are created lazily — you can't configure a channel that doesn't exist yet. Bindings are the only place where you can say "for messages matching this pattern, apply this behavior."

---

## What Exists Today

### Response suppression flow

```
Message arrives
  |
  v
resolve_agent_for_message()
  |-- binding.matches_route()  --> no match --> next binding / default agent
  |-- binding.passes_require_mention()  --> FAIL --> message DROPPED (never seen)
  |
  v
Channel created or reused
  |-- Channel.listen_only_mode checked
  |   |-- if quiet AND not (@mention | reply | command) --> message RECORDED but no LLM response
  |   |-- if active --> normal processing
  |
  v
LLM processes message, generates response
```

### listen_only resolution priority (current)

```
1. Session override (ephemeral, from /quiet or /active if persistence failed)
2. Explicit agent config ([agents.channel].listen_only_mode in TOML — LOCKS, blocks per-channel overrides)
3. Per-channel SettingsStore value (keyed by conversation_id)
4. Agent default from [defaults.channel]
```

Problem: Level 2 acts as a hard lock. If the agent config explicitly sets `listen_only_mode`, per-channel values are ignored. This is probably a bug, not a feature.

### ConversationSettings (portal only, current)

```rust
pub struct ConversationSettings {
    pub model: Option<String>,
    pub model_overrides: ModelOverrides,
    pub memory: MemoryMode,           // Full | Ambient | Off
    pub delegation: DelegationMode,    // Standard | Direct
    pub worker_context: WorkerContextMode,
}
```

Stored as JSON in `portal_conversations.settings` column. Loaded at channel creation for portal conversations. Not available for platform channels.

---

## Proposed Design

### Core idea: Response Mode

Replace `require_mention` (binding) and `listen_only_mode` (channel) with a single `response_mode` field on `ConversationSettings`:

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseMode {
    /// Respond to all messages normally.
    #[default]
    Active,
    /// Observe and learn (history + memory persistence) but only respond
    /// to @mentions, replies-to-bot, and slash commands.
    Quiet,
    /// Ignore messages entirely unless @mentioned or replied to.
    /// Messages that don't pass the mention check are still recorded in
    /// history but receive no processing (no memory persistence, no LLM).
    MentionOnly,
}
```

**Behavior comparison:**

| Mode        | Message recorded in history? | Memory persistence? | LLM response? | Trigger to respond       |
| ----------- | ---------------------------- | ------------------- | ------------- | ------------------------ |
| Active      | Yes                          | Yes                 | Yes           | Any message              |
| Quiet       | Yes                          | Yes                 | No            | @mention, reply, command |
| MentionOnly | Yes (lightweight)            | No                  | No            | @mention, reply          |

**Key decision:** `MentionOnly` keeps the routing-level drop. See "Why require_mention stays on bindings" below for the rationale.

### Updated ConversationSettings

```rust
pub struct ConversationSettings {
    pub model: Option<String>,
    pub model_overrides: ModelOverrides,
    pub memory: MemoryMode,
    pub delegation: DelegationMode,
    pub response_mode: ResponseMode,      // NEW
    pub worker_context: WorkerContextMode,
}
```

### Settings on bindings (TOML)

Bindings gain an optional `[bindings.settings]` table that provides **defaults** for any channel matched by that binding:

```toml
[[bindings]]
agent_id = "ops"
channel = "discord"
guild_id = "123456"
channel_ids = ["789"]

[bindings.settings]
response_mode = "quiet"
model = "anthropic/claude-haiku-4.5"
memory = "ambient"
```

Backwards compatibility: `require_mention = true` on a binding is equivalent to `settings.response_mode = "mention_only"`. If both are set, `settings.response_mode` takes priority.

### Why require_mention stays on bindings

There are three levels of "should the bot respond?" and they operate at different layers:

| Level | Setting | Where it lives | Why it lives there |
|-------|---------|---------------|-------------------|
| **Routing** | `require_mention` | Binding | Must be decided **before a channel exists**. Drops messages at the router — the channel is never created, no resources allocated, no history recorded. This is the right place for noisy servers where creating a channel per Discord channel would be wasteful. |
| **Binding defaults** | `[bindings.settings]` | Binding | Sets the **default behavior** for any channel that IS created by this binding. A policy applied to a pattern of channels. |
| **Per-channel override** | ConversationSettings | DB (per conversation_id) | Runtime overrides set by `/quiet`, `/active`, or the settings UI. An exception to the binding policy. |

The key insight: **bindings are policies, channels are exceptions.**

A binding like `channel = "discord", guild_id = "123456", require_mention = true` says: "For every channel in this server, don't even create a channel unless the bot is mentioned." That's a routing decision about resource allocation, not a channel behavior setting.

Once the bot IS mentioned and a channel is created, that channel gets `response_mode = MentionOnly` as its default from the binding. The user can then `/active` it — that per-channel override persists and the binding default no longer applies to that specific channel. But the binding still controls whether NEW channels are created for other Discord channels in the same server.

`listen_only` (Quiet mode) does NOT need to be on bindings as a routing gate because it doesn't affect channel creation — the channel is created either way, messages flow through, the bot just doesn't respond. It lives entirely in ConversationSettings:
- Set as a binding default via `[bindings.settings].response_mode = "quiet"`
- Toggled at runtime via `/quiet` and `/active`
- Visible in the settings panel

```
Message arrives
  │
  ▼
resolve_agent_for_message()
  ├── binding.matches_route() ──→ no match → next binding / default agent
  ├── binding.require_mention? ──→ not mentioned → MESSAGE DROPPED (routing gate)
  │
  ▼
Channel created or reused
  ├── Load ConversationSettings:
  │     per-channel DB > binding defaults > agent defaults > system defaults
  ├── response_mode == Active? → normal processing
  ├── response_mode == Quiet? → record + memory, suppress LLM unless @mention/reply/command
  ├── response_mode == MentionOnly? → record only, suppress everything unless @mention/reply
  │
  ▼
LLM processes message (if not suppressed)
```

### Resolution order

For any channel (portal or platform):

```
1. Per-channel ConversationSettings (persisted in DB, set via API/commands)
2. Binding defaults (from [bindings.settings] in TOML, matched at message routing time)
3. Agent defaults (from agent config)
4. System defaults (Active, Full memory, Standard delegation, etc.)
```

The current "explicit config locks everything" behavior for `listen_only_mode` goes away. Per-channel overrides always win.

### Where per-channel settings are stored

**Portal conversations:** Already in `portal_conversations.settings` (JSON column). No change.

**Platform channels:** New `channel_settings` SQLite table (see "Decisions made" #5). Same JSON-in-TEXT pattern as `portal_conversations`. The existing per-channel `listen_only_mode` keys in SettingsStore (redb) are migrated out and removed in Phase 3.

### Slash command changes

- `/quiet` → sets `response_mode = Quiet` on the current channel's ConversationSettings
- `/active` → sets `response_mode = Active`
- `/mention-only` → sets `response_mode = MentionOnly` (new)
- `/status` → shows current response mode alongside other settings

### How binding settings flow to channels

When a message arrives and matches a binding, the binding's `settings` block provides defaults. The binding does NOT need to know about per-channel overrides — it just provides the baseline:

```
1. Message arrives, matches binding
2. require_mention gate passes (or message is dropped)
3. Channel created or reused
4. Load per-channel ConversationSettings from DB
5. If found → use per-channel settings (override)
6. If not found → use binding.settings as defaults
7. Resolve against agent defaults and system defaults
```

The binding's settings are NOT persisted to the DB automatically — they're just defaults. If a user runs `/quiet` on a channel that was `Active` via binding defaults, the per-channel override (`Quiet`) is persisted and takes priority from then on. The binding default still applies to any other channel matched by the same binding that hasn't been overridden.

---

## Migration path

### Phase 1: Add ResponseMode to ConversationSettings

- Add `response_mode: ResponseMode` field (default: Active)
- Keep existing `listen_only_mode` working in parallel
- `/quiet` and `/active` write to both old and new systems

### Phase 2: Add settings to bindings

- Parse `[bindings.settings]` from TOML
- Store parsed `ConversationSettings` on `Binding` struct
- Pass binding settings through to channel creation

### Phase 3: Unify listen_only into response_mode

- Channel's `listen_only_mode` field replaced by `resolved_settings.response_mode`
- Remove `listen_only_mode` from `ChannelConfig`
- Remove per-channel listen_only keys from SettingsStore
- `require_mention` on bindings becomes sugar for `settings.response_mode = "mention_only"`

### Phase 4: Platform channel settings persistence

- New `channel_settings` table (or reuse SettingsStore — TBD)
- Load per-channel settings at channel creation for all channel types
- API endpoints for get/set channel settings

---

## Decisions made

1. **MentionOnly drops at routing.** `require_mention` stays on bindings as a routing gate. No channel created, no resources wasted. Once mentioned, the channel is created with `response_mode = MentionOnly` as its default.

2. **require_mention stays on bindings, not in ConversationSettings.** It's a routing decision, not a channel behavior. See "Why require_mention stays on bindings" above.

3. **Bindings provide setting defaults via `[bindings.settings]`.** These are policies applied to a pattern of channels. Per-channel overrides (via commands or UI) are exceptions that always win.

4. **The global `listen_only_mode` agent config becomes a default, not a lock.** Per-channel overrides always take priority.

5. **Platform channel settings stored in a new SQLite table.** SettingsStore (redb) is a simple KV store for scalar runtime toggles — not the right place for structured JSON settings. A `channel_settings` table in SQLite matches the existing `portal_conversations` pattern, is queryable for the Channels UI, and keeps everything in one database. The per-channel `listen_only_mode` keys in redb migrate out as part of the cleanup.

```sql
CREATE TABLE channel_settings (
    agent_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    settings TEXT NOT NULL DEFAULT '{}',
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (agent_id, conversation_id)
);
```

6. **`listen_only_mode` in agent config becomes `response_mode`.** Boolean `listen_only_mode = true/false` replaced by `response_mode = "active" | "quiet" | "mention_only"` in `[defaults.channel]` and `[agents.channel]`. Backwards compat: `listen_only_mode = true` accepted as alias for `response_mode = "quiet"` with a deprecation warning.

```toml
# Before
[defaults.channel]
listen_only_mode = true

# After
[defaults.channel]
response_mode = "quiet"
```

7. **Platform channel settings in the UI.** The Channels page gets a gear icon per channel row that opens the existing `ConversationSettingsPanel` as a popover — same component used for portal conversations. Wired to `GET/PUT /channel-settings/{conversation_id}` API endpoints backed by the new `channel_settings` table.

8. **`save_attachments` moves into ConversationSettings.** It's a per-channel behavior toggle, same as response_mode or memory. Added as a boolean field on `ConversationSettings` and a toggle in the settings panel.

## Future: `/new` command for platform channels

Portal conversations support creating new conversations (new session ID, fresh history). Platform channels are tied to a fixed `conversation_id` (e.g., `discord:guild:channel`) — you can't create a new Discord channel from a slash command.

For platform channels, `/new` means **"clear context and start fresh"**:

1. Flush the channel's in-memory history vec
2. Insert a marker message in `conversation_messages` so the DB history shows where the reset happened
3. The channel continues with the same `conversation_id` — no new rows in `channel_settings` or `portal_conversations`
4. Old messages stay in the DB for reference, but the LLM starts with a clean slate

This is simpler than portal's new-conversation flow — no new metadata rows, no session ID changes. Just a context reset.

**Implementation:** Add `/new` to the channel's built-in command handler (alongside `/quiet`, `/active`, `/status`). Clear `self.state.history`, optionally reset `self.message_count` and `self.last_persistence_at`, log a system marker.

**Separate from the settings unification work** — this is a standalone feature that can be built independently.
