# Slash Commands

Slash commands are the power-user interface to Spacebot — session control, memory, tasks, skills, and agent state, accessible from any platform without leaving the conversation.

Today there is no command system. There are 11 command strings and 9 behaviors spread across three dispatch sites in `src/agent/channel.rs`: an inline full-string `match` (`/status`, `/quiet`, `/observe`, `/active`, `/mention-only`, `/help`), a special-cased `/agent-id` check, and three prompt rewrites (`/tasks`, `/today`, `/digest`). `/help` is a hand-maintained string array that has already drifted from the match arms. Slack's "slash commands" are config-driven routing aliases, and the `slack_command_agent_id` metadata they attach is written but never read. Portal isn't in the supported-source list, so commands typed in the web UI silently fall through to the LLM as plain text. No command takes arguments. No command checks who sent it.

This doc defines the real system: one typed registry, declarative access control, per-command busy policy, and native support across Discord, Slack, Telegram, Portal, and text adapters.

---

## Central Registry

All commands are defined once, in a static table. Every surface — adapter registration, parsing, dispatch, help, access checks, busy handling, the agent's prompt block — derives from the same source. Adding a command is one table entry; nothing else to keep in sync.

```rust
pub struct CommandDef {
    /// Canonical name without slash (e.g. "retry")
    pub name: &'static str,

    /// Short description shown in platform menus and /help
    pub description: &'static str,

    /// Grouping for /help display
    pub category: CommandCategory,

    /// Alternative names that resolve to this command
    pub aliases: &'static [&'static str],

    /// Argument shape — drives validation, help hints, and native
    /// platform option types
    pub args: ArgSpec,

    /// How this command executes
    pub handler: CommandHandler,

    /// Which platforms this command is available on
    pub availability: CommandAvailability,

    /// Who may run it
    pub access: CommandAccess,

    /// What happens when it arrives while a turn is in flight
    /// (Agent commands only — Control commands never wait)
    pub busy: BusyPolicy,
}
```

### `ArgSpec` — arguments as data, not doc strings

A free-text `args_hint` forces every handler to re-parse its own arguments and gives native platform registration nothing to generate typed options from. A small enum covers every command we have:

```rust
pub enum ArgSpec {
    None,
    /// Optional free text, named for the hint: "[query]"
    Optional(&'static str),
    /// Required free text: "<prompt>"
    Required(&'static str),
    /// Closed set, tab-completable, validated centrally: "[on|off|status]"
    Choice(&'static [&'static str]),
}
```

The registry validates before dispatch: a `Required` command with no args gets a usage reply without touching its handler; a `Choice` command with an unknown value gets the valid set. Discord option types, Telegram hints, and Portal palette completion all generate from this.

### `CommandHandler` — two execution planes

```rust
pub enum CommandHandler {
    /// Executes on the channel control plane — settings store,
    /// ChannelControlHandle, ProcessControl. Never consumes an agent
    /// turn, never enters the channel message queue. Works mid-turn
    /// by construction.
    Control(fn(&CommandContext, &str) -> ControlOutcome),

    /// Forwarded to the agent as a structured message. The agent sees
    /// the command name and args, not raw text.
    Agent,
}
```

The `Control` plane is the important design move. The channel is a serial actor: `run` awaits each turn inline, so anything routed through the message queue sits behind the current turn — today a `/status` sent mid-task waits minutes for an answer that reads two `ArcSwap` fields. Control commands bypass the queue entirely and execute against the handles that already exist outside the turn (`ChannelControlHandle`, `ProcessControl`, `ChannelSettingsStore`). That is why `/stop` can cancel a running turn and `/status` answers instantly while one is in flight — not because of a priority queue, but because they never enter the queue at all.

`ControlOutcome` covers the few control commands with side effects on the running turn:

```rust
pub enum ControlOutcome {
    /// Reply, nothing else
    Reply(CommandReply),
    /// Cancel active work on this channel, then reply
    CancelThenReply(CommandReply),
}

pub struct CommandReply {
    /// Canonical, adapter-independent core text
    pub text: String,
    /// Structured values behind the text, for surfaces that render
    /// natively (Portal tables, Discord embeds) without recomputing
    pub data: Option<serde_json::Value>,
}
```

Two contracts on Control output:

- **Surface independence.** A Control handler's output depends only on its args and channel state — never on which adapter invoked it. The core text is identical everywhere; adapters apply only their own decoration (markdown flavor, ephemeral delivery, entity escaping). A registry test pins this by running each Control command against a fixed context across every adapter surface.
- **Data beside text.** `/status`, `/workers`, and `/usage` derive structured values to build their text; `data` carries them so Portal renders a real table and the API returns machine-readable output, without a second code path computing the same numbers.

### `CommandAccess` — declarative access control

> **Superseded:** `human-scoped-turn-authority.md` replaces the binary
> platform-ID `Everyone`/`Authority` model below with Human-scoped capabilities.
> The typed command registry remains; commands declare required capabilities and
> use the same `TurnAuthority` as model tools.

```rust
pub enum CommandAccess {
    Everyone,
    /// Requires the sender to be in the authority list for this scope
    Authority,
}
```

Commands that mutate channel or agent state — `/quiet`, `/active`, `/mention-only`, `/new`, `/stop`, `/model` — are `Authority`. Read-only commands are `Everyone`. Anyone who can post in a bound channel today can flip the agent to observe mode permanently; that ends here.

Authority is configured on bindings (and as an adapter-instance default):

```toml
[[bindings]]
agent_id = "orion"
channel = "discord"
guild_id = 123456
authority = ["91827364"]        # platform user ids
```

Semantics:

- **Opt-in by absence.** No `authority` list configured → every command is open to everyone the binding already admits. Zero-migration: existing configs behave exactly as before.
- **Scope-local.** Authority on a guild binding does not grant authority in DMs or another guild. Lists never cross scopes.
- **Discovery floor.** `/help` and `/status` are always allowed regardless of access config, so a denied user can see what they *can* do. Denials name the commands available to that user rather than a bare "no".
- Authorization (who may talk to the agent) stays where it is — bindings and adapter permission snapshots. `CommandAccess` layers *authority* (who may change state) on top; it never widens admission.

### `BusyPolicy` — Agent commands mid-turn

Control commands are busy-immune by construction, so this applies only to `Agent` commands:

```rust
pub enum BusyPolicy {
    /// Wait for the current turn, then run as a normal turn (default)
    Queue,
    /// Refuse mid-turn with a pointer to /stop
    Reject,
}
```

The invariant: **a recognized command is never silently swallowed.** It is validated, queued with an acknowledgment, rejected with a reason, or executed — but the sender always learns what happened. Unknown `/words` are not commands; they flow to the model as ordinary text, preserving current behavior for messages that merely start with a slash.

### `CommandCategory` and `CommandAvailability`

```rust
pub enum CommandCategory {
    Session,
    Response,   // response-mode controls
    Memory,
    Tasks,
    Skills,
    Info,
    Config,
}

pub struct CommandAvailability {
    pub portal: bool,
    pub discord: bool,
    pub slack: bool,
    pub telegram: bool,
    pub text_adapters: bool,  // Signal, Mattermost, Twitch, Email, Webhook
}
```

---

## Command Set

Every command that exists today keeps a home. `Ctl` = Control handler, `Agt` = Agent handler.

### Session

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/new` | `reset` | — | Ctl | — | Authority | Start a new conversation (cancels active work) |
| `/stop` | `cancel` | — | Ctl | — | Authority | Cancel active workers, branches, and the current turn |
| `/retry` | — | — | Agt | Reject | Everyone | Resend the last message |
| `/undo` | — | — | Agt | Reject | Authority | Remove the last exchange |
| `/compress` | `compact` | — | Agt | Reject | Authority | Manually trigger context compaction |
| `/background` | `bg` | `<prompt>` | Agt | Queue | Everyone | Run a prompt in a branch without blocking the conversation |

### Response mode

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/active` | — | — | Ctl | — | Authority | Respond to all messages |
| `/mention-only` | — | — | Ctl | — | Authority | Respond only when mentioned |
| `/quiet` | `observe` | — | Ctl | — | Authority | Observe without responding |

`/quiet` absorbs today's `/observe` as an alias. Mode commands work in every mode — `/active` must be able to rescue an observing channel, so control dispatch runs before response-mode suppression, as the current code already orders it.

### Memory

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/memory` | `memories` | `[query]` | Agt | Queue | Everyone | Search or list memories |
| `/remember` | — | `<text>` | Agt | Queue | Everyone | Save something to memory immediately |

### Tasks & Goals

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/tasks` | — | `[status]` | Agt | Queue | Everyone | List tasks |
| `/today` | — | — | Agt | Queue | Everyone | Tasks snapshot: in progress and up next |
| `/digest` | — | — | Agt | Queue | Everyone | Day digest: decisions, themes, open loops |
| `/goals` | — | — | Agt | Queue | Everyone | List active goals |
| `/approve` | — | `[id]` | Agt | Queue | Authority | Approve a pending task |

`/tasks`, `/today`, `/digest` stop being raw-text prompt rewrites; they arrive as structured commands and the prompt template renders the instruction, so the wording lives in Jinja with the rest of the prompts instead of Rust string literals.

### Skills

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/skills` | — | — | Agt | Queue | Everyone | List installed skills |
| `/skill` | — | `<name>` | Agt | Queue | Everyone | Invoke a skill by name |

### Info

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/help` | `commands` | — | Ctl | — | Everyone | Show available commands |
| `/status` | — | — | Ctl | — | Everyone | Conversation info: model, mode, context usage, active work |
| `/agent-id` | — | — | Ctl | — | Everyone | Print the agent id (deterministic, for identity checks) |
| `/workers` | — | — | Ctl | — | Everyone | List active workers and branches with status |
| `/usage` | — | — | Ctl | — | Everyone | Token usage for this conversation |

`/workers` moves from Agent to Control: worker state is readable from `ProcessControl` without an LLM turn, and its main use is checking on work that is currently running — exactly when an Agent command would be stuck in the queue.

### Config

| Command | Aliases | Args | Handler | Busy | Access | Description |
|---------|---------|------|---------|------|--------|-------------|
| `/model` | — | `[model]` | Ctl | — | Authority | Show or switch the model for this conversation |
| `/voice` | — | `[on\|off\|status]` | Ctl | — | Authority | Toggle voice mode |

---

## Parsing

One shared parser in the registry, used by every adapter:

```rust
pub struct ParsedCommand {
    pub def: &'static CommandDef,
    pub args: String,
}

impl CommandRegistry {
    pub fn parse(&self, text: &str) -> ParseResult;
}

pub enum ParseResult {
    /// Recognized command, args validated against ArgSpec
    Command(ParsedCommand),
    /// Recognized command, invalid args — reply with usage, no dispatch
    Usage(&'static CommandDef, String),
    /// Leading slash but no matching name — ordinary text
    NotACommand,
}
```

Normalization rules, applied in `parse`:

- strip the leading slash; split on first whitespace; case-insensitive lookup across names and aliases;
- strip a `@botname` suffix from the command token (Telegram sends `/status@spacebot` in groups);
- reject tokens containing `/` so file paths pasted at line start never parse as commands;
- normalize smart dashes in args — iOS autocorrects `--` to `—` and `-` to `–`, which silently breaks any flag-style argument typed from a phone.

Coalescing interaction: command messages are already exempt from coalescing, but the current flush ordering runs a full LLM turn on the buffered batch *before* handling the command. Control commands dispatch immediately without flushing; the buffer keeps its own debounce clock. Agent commands with `Queue` flush first — they're joining the conversation, so order matters.

---

## Dispatch

Parsing happens in the messaging layer, before the message reaches a channel. The flow per inbound message starting with `/`:

1. `CommandRegistry::parse(text)`.
2. `NotACommand` → normal message path, untouched.
3. `Usage` → reply (ephemeral where supported), done.
4. Access check against the binding's authority list. Denied → reply naming the commands the sender *can* run, done.
5. `Control` → execute on the control plane, reply, done. No inbound message is created.
6. `Agent` → construct `MessageContent::Command { name: &str, args: String }` (a new variant beside `Interaction`) and inject into the channel. Busy policy applies at the channel boundary: `Reject` while a turn is in flight replies immediately; `Queue` enqueues normally.

Channels receive the structured command, not raw text. The system prompt includes a commands block generated from the registry — the agent doesn't parse `/approve 3` out of a string, and the block stays current by construction.

Replies from Control commands and rejections use `OutboundResponse::Ephemeral` — already implemented and correctly degraded to a plain message by every adapter that lacks ephemeral support.

---

## Platform Behaviour

### Portal

Commands are parsed client-side as the user types `/` — a command palette shows matching commands, descriptions, and arg hints, generated from the registry (served at `GET /api/commands`). Control commands round-trip through `POST /api/channels/:id/command` `{ name, args }`; Agent commands go through the same endpoint and inject as structured messages. Portal joins the supported sources — the current silent no-op in the web UI is a bug, not a policy.

### Discord

Native application commands via Serenity, registered on startup (global or per-guild, configurable). Discord's constraints need explicit handling:

- **100-command cap, all-or-nothing.** One over-limit command makes Discord reject the entire batch. Registration is cap-aware: core commands register first in table order, overflow is dropped with one actionable log line counting what was cut.
- **Diff-only sync.** Fetch live commands, key by name, delete obsolete entries *first* (to free cap headroom), then create/update changed ones. Re-registering an identical set is a no-op — no churn on every restart.
- `ArgSpec` maps to typed options: `Choice` becomes a native choice list, `Required`/`Optional` become string options with the hint as description.
- `Control` commands answer with an ephemeral interaction response. `Agent` commands defer the interaction within the 3-second window; the real response arrives through the normal message path.

### Slack

Replaces the config-driven alias system. One `/spacebot` app command with subcommands covers everything: `/spacebot status`, `/spacebot bg do the thing`. Aliases resolve normally. Acked immediately; response delivered as a follow-up, ephemeral for Control replies. `SlackCommandConfig`, its loader, and the never-read `slack_command_agent_id` metadata are deleted; any existing `/command → agent_id` config maps to a binding.

### Telegram

`/command` syntax is native. The bot's menu is generated from the registry at startup via `setMyCommands` — descriptions truncated to Telegram's 256-char limit, names to 32 chars, `Choice` values appended to the description as hints. Group-chat `/cmd@botname` addressing is handled in the shared parser.

### Text adapters (Signal, Mattermost, Twitch, Email, Webhook)

Messages starting with `/` go through the shared parser. No native command UI — `/help` is the discovery mechanism. Twitch and Signal keep the behavior they have today, minus the inline match.

### Parity enforcement

A registry test walks every command × platform and fails when a command is available on one platform but silently missing from another without an explicit, named exemption (e.g. Slack's reserved slash names). Platform caps become compile-time decisions instead of silent drops.

---

## `/help`

Generated from the registry, grouped by category, filtered by the caller's availability and access — a user never sees a command they can't run. Rich format for Portal/Discord, compact for Telegram/text:

```text
/new — new conversation
/status — conversation info
/memory [query] — search memories
/help — show this
```

---

## Implementation

**`src/commands/`** — new module

- `registry.rs` — `CommandDef`, `ArgSpec`, `CommandRegistry`, the static table, `parse()`
- `control.rs` — Control handler implementations against `CommandContext` (channel handle, settings store, process control, runtime config)
- `access.rs` — authority resolution from bindings

**Core changes:**

- `MessageContent::Command { name, args }` variant; arms in the handful of existing matches
- binding config gains `authority`; adapter instances gain a default list; hot-reload through the existing permission-snapshot path
- `channel.rs` loses `try_handle_builtin_ops_commands`, the `/agent-id` special case, `rewrite_tool_routed_command_prompt`, and the hand-written help array
- prompt template gains a commands block rendered from the registry

**API:**

```text
GET  /api/commands                      registry projection for Portal palette
POST /api/channels/:id/command          { name, args } → { text, data }
```

Control commands return their `CommandReply` directly in the response body; Portal renders `data` natively when present and falls back to `text`.

### Phase 1 — Registry and port

Registry, parser, `MessageContent::Command`, control plane. Port all 11 existing command strings behavior-preserving (same replies, same ordering guarantees), delete the three dispatch sites. Portal joins supported sources. `/help` generates.

### Phase 2 — Access and busy policy

Binding `authority` config, access checks with the discovery floor, denial messages. Busy handling: Control bypass, `Reject` replies, queue acknowledgments. Coalesce-flush ordering fix.

### Phase 3 — Native registration

Discord application commands with cap-aware diff sync. Telegram `setMyCommands`. Slack `/spacebot` umbrella; delete `SlackCommandConfig`. Parity test.

### Phase 4 — Command set completion

New commands from the tables above (`/new`, `/stop`, `/retry`, `/memory`, `/remember`, `/approve`, `/workers`, `/usage`, `/model`, `/voice`, …), prompt commands block, structured `/tasks`/`/today`/`/digest` through templates.

---

## Non-Goals

- **Custom user-defined commands** — skills serve this purpose; the registry stays static
- **Per-channel command availability** — availability is per-platform, access is per-binding; no per-channel toggles
- **CLI flag parity** — the clap CLI is a separate surface (see `cli-coverage.md`); the registry is a plain static table precisely so a future `spacebot chat` REPL can consume it, but nothing in these phases depends on that
- **Role systems** — authority is a flat id list per scope; roles, groups, and wildcards wait for a real multi-tenant story
