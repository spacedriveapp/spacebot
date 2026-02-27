# Named Messaging Adapters via Bindings

## Context

Messaging adapters are currently singleton per platform per instance (one Discord bot token, one Telegram bot token, one Slack app pair). Bindings route traffic by platform + location filters (guild/workspace/chat/channel), but they do not select which credential instance handles that traffic.

This blocks multi-bot scenarios on a single instance, such as:

- Two Telegram bots routing to different agents
- Two Discord bots with different server memberships and identities
- A default shared bot plus one specialized bot for a specific agent

The existing binding abstraction is still the right place to express routing. The missing piece is adapter instance selection.

## Proposal

Add support for multiple named adapter instances per platform, while keeping the existing nameless token fields as the default instance for backward compatibility.

- Existing config shape remains valid
- Named instances are optional and additive
- Bindings gain an optional `adapter` field
- Binding resolution chooses both agent and adapter instance

This keeps the simple path simple (paste one token) and unlocks advanced routing when needed.

## Goals

- Support multiple credential instances per messaging platform
- Keep old configs and API payloads working unchanged
- Keep bindings as the single routing abstraction
- Preserve first-match binding behavior
- Avoid introducing a separate per-agent messaging config system

## Non-Goals

- No backward-compat shim for legacy behavior beyond config/API compatibility
- No cross-platform global adapter namespace
- No automatic migration that rewrites user config files

## Config Shape

### Telegram example

```toml
[messaging.telegram]
enabled = true
token = "env:TELEGRAM_BOT_TOKEN" # default instance (legacy path)

[[messaging.telegram.instances]]
name = "support"
enabled = true
token = "env:TELEGRAM_BOT_TOKEN_SUPPORT"

[[messaging.telegram.instances]]
name = "sales"
enabled = true
token = "env:TELEGRAM_BOT_TOKEN_SALES"

[[bindings]]
agent_id = "main"
channel = "telegram"
chat_id = "-100111111111"
# adapter omitted => default instance

[[bindings]]
agent_id = "support-agent"
channel = "telegram"
chat_id = "-100222222222"
adapter = "support"
```

### Binding semantics

- `adapter` is platform-scoped (same name can exist under Telegram and Discord)
- `adapter` omitted means default instance for that platform
- If no matching binding exists, message still routes to default agent (current behavior)

## Data Model Changes

### `Binding`

Add:

- `adapter: Option<String>`

### Messaging config structs

For each platform (Discord, Telegram, Slack, Twitch, Email, Webhook):

- Keep existing singleton fields (`token`, `bot_token`, `app_token`, etc.)
- Add optional `instances: Vec<...InstanceConfig>` with:
  - `name: String`
  - `enabled: bool`
  - platform credential fields
  - optional platform-specific extras if needed later

## Runtime Model

## Adapter identity

Each running adapter gets a stable runtime key:

- Default instance: `<platform>` (example: `telegram`)
- Named instance: `<platform>:<name>` (example: `telegram:support`)

`MessagingManager` stores adapters by runtime key instead of platform name alone.

## Inbound routing

When an adapter emits an inbound message:

- Keep `message.source = <platform>` for existing platform semantics
- Add `adapter` metadata (or equivalent field) carrying runtime adapter key

Binding resolution matches on:

1. platform (`channel`)
2. adapter selector (`binding.adapter` vs inbound adapter identity)
3. existing platform filters (`guild_id`, `workspace_id`, `chat_id`, `channel_ids`, DMs)

Then first-match wins as before.

## Outbound routing

`respond`, `send_status`, and `fetch_history` use the adapter identity captured on inbound so replies stay on the same bot instance.

For proactive sends (`broadcast` and tools), the caller can target runtime adapter key explicitly when required.

## Permissions Model

Permission maps currently aggregate per platform. They become per adapter instance:

- Build permission set by filtering bindings on `(platform, adapter)`
- Default adapter uses bindings where `adapter` is absent or explicitly set to default selector
- Named adapter uses bindings with matching `adapter`

This allows independent scope per token instance without changing binding filter fields.

## API Changes

## Bindings API

`POST/PUT/DELETE /bindings` payloads add optional:

- `adapter?: string`

Old payloads remain valid.

## Messaging API

Status/toggle/disconnect move from platform-only targeting to adapter instance targeting:

- Platform + optional adapter name
- Platform-only continues to refer to default instance for compatibility

Response payloads should include adapter instance identity so UI can render multiple cards per platform.

## UI Changes

Two-column layout replacing the current single-column platform accordion.

### Layout

```
┌───────────────────────┬────────────────────────────────────────┐
│  Available            │  Configured Instances                  │
│                       │                                        │
│  [Discord        ＋]  │  ┌─ discord ───────────────────────┐   │
│  [Slack          ＋]  │  │ Discord (default)       ● on    │   │
│  [Telegram       ＋]  │  │ Bindings: 2                     │   │
│  [Twitch         ＋]  │  │ [Edit] [Disable] [Remove]       │   │
│  [Email          ＋]  │  └──────────────────────────────────┘   │
│  [Webhook        ＋]  │  ┌─ telegram ──────────────────────┐   │
│                       │  │ Telegram (default)       ● on   │   │
│  Coming Soon          │  │ Bindings: 1                     │   │
│  WhatsApp             │  │ [Edit] [Disable] [Remove]       │   │
│  Matrix               │  └──────────────────────────────────┘   │
│  iMessage             │  ┌─ telegram:support ──────────────┐   │
│  IRC                  │  │ Telegram "support"       ● on   │   │
│  Lark                 │  │ Bindings: 1                     │   │
│  DingTalk             │  │ [Edit] [Disable] [Remove]       │   │
│                       │  └──────────────────────────────────┘   │
│                       │                                        │
│                       │  (empty state when nothing configured) │
└───────────────────────┴────────────────────────────────────────┘
```

- **Left column:** Platform catalog. Each platform has a "+" button to add an instance. Coming-soon platforms listed but disabled. Always visible regardless of configured count.
- **Right column:** Configured adapter instances as expandable summary cards. Compact view shows platform icon, instance name (or "default"), enabled status, binding count. Expands to show credentials, full binding list, and controls.

### Interaction model

- **Add instance:** Clicking "+" on a platform creates a new card inline in the right column in editing state. Name input (required for non-default), credential fields. Save creates the instance and collapses to summary.
- **First instance:** When a platform has no instances, the first "+" creates the default instance. No name input required — same single-token paste flow as today.
- **Subsequent instances:** "+" when a platform already has the default instance creates a named instance. Name input is required.
- **Instance cards:** Expandable accordions. Click to expand, showing credentials (masked), bindings section, enable/disable toggle, disconnect/remove button.
- **Bindings:** Edited per-instance inside each expanded card. Each instance card contains its own bindings section with add/edit/remove. Binding form auto-populates the `adapter` field.

The common single-token path remains one step.

## Validation Rules

- No duplicate instance names within a platform
- Binding `adapter` must reference an existing configured instance for that platform
- Reserved names: reject empty names and `default`
- Runtime key collisions are impossible under platform-scoped names but still validated

Config load should fail fast with clear messages when these constraints are violated.

## Backward Compatibility

- Existing `[messaging.<platform>]` blocks continue to create the default adapter
- Existing bindings with no `adapter` continue to work unchanged
- Existing API clients can omit `adapter`
- Existing docs/examples remain valid; new docs add advanced multi-instance examples

No migration required for existing users.

## Failure Modes and Handling

- Binding references missing adapter: reject config/API mutation
- Named adapter disabled/disconnected: bindings remain but produce clear routing/health errors
- Duplicate adapter name: reject config load and UI mutation
- Default adapter missing while bindings rely on default: validation error

## Ordered Implementation Phases

### Phase 1: Config and binding model

1. Add `adapter: Option<String>` to `Binding` (`config.rs:~1153`) and `TomlBinding` (`config.rs:~2302`)
2. Add per-platform instance config structs: `DiscordInstanceConfig`, `SlackInstanceConfig`, `TelegramInstanceConfig`, `TwitchInstanceConfig`, `EmailInstanceConfig`, `WebhookInstanceConfig` — each with `name: String`, `enabled: bool`, and the same credential fields as the parent platform config
3. Add `instances: Vec<XInstanceConfig>` to all platform configs: `DiscordConfig`, `SlackConfig`, `TelegramConfig`, `TwitchConfig`, `EmailConfig`, `WebhookConfig`
4. Add matching TOML deser structs (`TomlXInstanceConfig`) for `[[messaging.<platform>.instances]]` array-of-tables
5. Add validation: no duplicate instance names within a platform, no empty or `"default"` names, `binding.adapter` must reference an existing configured instance
6. Update `Binding::matches()` (`config.rs:~1170`) to accept adapter identity parameter

### Phase 2: Runtime adapter identity

1. Define runtime key format — `"telegram"` for default, `"telegram:support"` for named — as a type alias or newtype
2. Refactor `MessagingManager` `HashMap<String, Arc<dyn MessagingDyn>>` (`manager.rs:~17`) to key by runtime key instead of `adapter.name()`. Update `register()`, `register_and_start()`, `remove_adapter()`, `respond()`, `has_adapter()`
3. Make adapter constructors accept an optional instance name so `name()` / `runtime_key()` returns the full key. Or add `runtime_key()` to the `Messaging` trait
4. Add `adapter: String` field to `InboundMessage` carrying the runtime adapter key
5. Update startup to instantiate default adapter from root config + one adapter per `instances` entry, all registered with the manager
6. Update `respond()` (`manager.rs:~203`) to route outbound by adapter runtime key captured on inbound, not `message.source`

### Phase 3: Binding resolution and permissions

1. Extend `Binding::matches()` to check `binding.adapter` against inbound adapter identity. `None` matches default adapter only. `Some("x")` matches named adapter `"x"` only
2. Build per-adapter permission sets by filtering bindings on `(platform, adapter)` when constructing each platform's permission struct
3. Update hot-reload to rebuild and `ArcSwap` permissions per adapter instance independently

### Phase 4: API

1. Refactor `GET /api/messaging/status` (`api/messaging.rs:~16-23`) from hardcoded per-platform struct to `Vec<AdapterInstanceStatus>` with `{platform, name, configured, enabled}`
2. Add optional `adapter: Option<String>` to `TogglePlatformRequest` and `DisconnectPlatformRequest` (`api/messaging.rs`). Omitted = default instance
3. Add adapter instance CRUD: `POST /api/messaging/instances` (create named instance with credentials), `DELETE /api/messaging/instances` (remove named instance + associated bindings)
4. Add optional `adapter` to bindings CRUD payloads (`api/bindings.rs`): `CreateBindingRequest`, `UpdateBindingRequest`, `DeleteBindingRequest`. Old payloads without `adapter` remain valid

### Phase 5: UI

1. Update TypeScript types (`interface/src/api/client.ts`): new `AdapterInstance` type, update `MessagingStatusResponse` to return instance list, add `adapter` to `BindingInfo` and binding request types
2. Replace `ChannelsSection` (`interface/src/routes/Settings.tsx:~785`) single-column layout with two-column: left platform catalog, right configured instances
3. Build platform catalog (left column): platform list with "+" buttons, coming-soon platforms grayed out
4. Refactor `ChannelSettingCard` (`interface/src/components/ChannelSettingCard.tsx`) into expandable instance summary cards: platform icon, instance name, status badge, binding count. Expand for credentials, bindings, controls
5. Build add-instance flow: "+" creates inline card in editing state, name input for non-default instances, credential fields, save calls instance API
6. Scope binding editor per-instance: existing `BindingsSection` logic scoped to the instance's adapter, binding form auto-populates `adapter` field
7. Default instance UX: first instance of any platform = default, no name required, same single-token paste experience as today. Only second+ instances require a name

### Phase 6: Tests

1. Config parsing/validation tests: valid instance arrays, empty names, duplicate names, `"default"` name rejection
2. Binding match tests: default adapter match, named adapter match, mismatch rejection, adapter-less binding matches default only
3. API backward compat tests: payloads without `adapter` field work unchanged
4. Permission filtering tests: per-adapter permission sets built correctly from filtered bindings

## Risk Notes

**Phase 2 is the most invasive.** Changing how `MessagingManager` keys adapters touches every adapter, startup, hot-reload, and outbound routing. Most bugs will surface here. Consider landing Phase 2 as its own PR with focused review.

**`ChannelEditModal` duplication.** The current UI has duplicated adapter logic between `ChannelSettingCard` and `ChannelEditModal`. The Phase 5 refactor is a good opportunity to consolidate, but optional — updating `ChannelSettingCard` alone is sufficient if the modal is used elsewhere.

**Build order is backend-first.** Phases 1-4 are Rust. Phase 5 is React. This avoids building UI against speculative API contracts.

## Resolved Questions

- **Adapter identity on `InboundMessage`:** Yes, first-class `adapter: String` field, not metadata. Binding resolution and outbound routing both depend on it directly.
- **Slack app-level settings beyond tokens:** No, not in this phase. Named Slack instances share the same config shape (bot_token + app_token). Independent app-level settings can be added later if needed.
- **Proactive broadcast with multiple instances:** Keep default fallback. Broadcast targets the default adapter unless the caller explicitly specifies a runtime key. This matches existing behavior and avoids breaking proactive sends for users who add named instances alongside their default.
