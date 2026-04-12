# Slash Commands

Slash commands are the power-user interface to Spacebot — session control, memory, tasks, skills, and agent state, accessible from any platform without leaving the conversation.

Currently, Slack has config-driven slash commands that route raw text to an agent. That's not a command system — it's a message routing alias. This doc defines a real one: a central registry, a consistent command set, and support across Discord, Slack, Telegram, Portal, and text-based adapters.

---

## Central Registry

All commands are defined once. Every platform derives its command list, help text, and routing from the same source. No per-adapter command tables, no duplication.

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

    /// Usage hint shown in help (e.g. "[on|off]", "<query>")
    pub args_hint: &'static str,

    /// Tab-completable subcommand options for platforms that support it
    pub subcommands: &'static [&'static str],

    /// How this command is handled
    pub handler: CommandHandler,

    /// Which platforms this command is available on
    pub availability: CommandAvailability,
}

pub enum CommandHandler {
    /// Handled locally by the adapter or frontend — never sent to agent
    /// (e.g. /help, /status, /new)
    Local,

    /// Forwarded to the agent as a structured message
    /// The agent sees the command name and args, not raw text
    Agent,
}

pub struct CommandAvailability {
    pub portal: bool,
    pub discord: bool,
    pub slack: bool,
    pub telegram: bool,
    pub text_adapters: bool,  // Signal, Mattermost, Email, Webhook
}
```

### `CommandCategory`

```rust
pub enum CommandCategory {
    Session,
    Memory,
    Tasks,
    Skills,
    Info,
    Config,
}
```

---

## Command Set

### Session

| Command | Aliases | Description | Args | Handler |
|---------|---------|-------------|------|---------|
| `/new` | `reset` | Start a new conversation | — | Local |
| `/retry` | — | Resend the last message | — | Agent |
| `/undo` | — | Remove the last exchange | — | Agent |
| `/compress` | — | Manually trigger context compaction | — | Agent |
| `/stop` | — | Cancel all active workers and branches | — | Agent |
| `/background` | `bg` | Run a prompt without blocking the conversation | `<prompt>` | Agent |

### Memory

| Command | Aliases | Description | Args | Handler |
|---------|---------|-------------|------|---------|
| `/memory` | `memories` | Search or list memories | `[query]` | Agent |
| `/remember` | — | Save something to memory immediately | `<text>` | Agent |

### Tasks & Goals

| Command | Aliases | Description | Args | Handler |
|---------|---------|-------------|------|---------|
| `/tasks` | — | List tasks | `[status]` | Agent |
| `/goals` | — | List active goals | — | Agent |
| `/approve` | — | Approve a pending task | `[id]` | Agent |

### Skills

| Command | Aliases | Description | Args | Handler |
|---------|---------|-------------|------|---------|
| `/skills` | — | List installed skills | — | Agent |
| `/skill` | — | Invoke a skill by name | `<name>` | Agent |

### Info

| Command | Aliases | Description | Args | Handler |
|---------|---------|-------------|------|---------|
| `/help` | `commands` | Show available commands | — | Local |
| `/status` | — | Show conversation info: model, turn count, context usage | — | Local |
| `/workers` | — | List active workers and their status | — | Agent |
| `/usage` | — | Show token usage for this conversation | — | Local |

### Config

| Command | Aliases | Description | Args | Handler |
|---------|---------|-------------|------|---------|
| `/model` | — | Switch model for this conversation | `[model]` | Local |
| `/voice` | — | Toggle voice mode | `[on\|off\|status]` | Local |

---

## Platform Behaviour

### Portal

Portal has no slash commands today. Commands are parsed client-side when input starts with `/`. A command palette appears as the user types, showing matching commands with descriptions and args hints. Submit runs the command.

`Local` commands are handled in the frontend (no round trip). `Agent` commands are sent to `POST /api/channels/:id/command` with `{ name, args }`, which formats a structured inbound message and injects it into the channel.

### Discord

Native Discord slash command interactions via Serenity. Commands are registered with Discord's application commands API on startup (global or per-guild, configurable). Discord surfaces them in its built-in command palette with descriptions and argument hints.

`Local` commands return an ephemeral interaction response immediately. `Agent` commands ack the interaction within 3 seconds and defer — the real response comes through the normal message path.

### Slack

Replaces the current config-driven slash command system. Instead of per-command Slack app registrations pointing at different agents, a single `/spacebot` command with subcommands covers everything: `/spacebot retry`, `/spacebot tasks`, etc. Aliases work: `/spacebot bg do the thing`.

Slack is acked immediately; response delivered as a follow-up message. Removes `SlackCommandConfig` from config — no more per-command agent routing config required.

### Telegram

Telegram's `/command` syntax is native. The bot's command menu (set via `setMyCommands`) is generated from the registry at startup — description truncated to Telegram's 256-char limit, name to 32 chars. Commands are parsed from incoming messages that start with `/`.

`Local` commands reply with formatted text. `Agent` commands forward to the channel handler as a structured message.

### Text Adapters (Signal, Mattermost, Email, Webhook)

Messages starting with `/` are parsed as commands. If the first word matches a known command name or alias, it's dispatched as a command with the remainder as args. No native platform command UI — `/help` is the discovery mechanism.

---

## Routing

Command dispatch lives in the messaging layer, before the message reaches a channel. Each adapter calls `CommandRegistry::parse(text)` which returns `Option<ParsedCommand>`. If it's a `Local` command, the adapter handles it directly and no inbound message is created. If it's an `Agent` command, an `InboundMessage` is created with `MessageKind::Command { name, args }` instead of `MessageKind::Text`.

Channels receive the structured command, not raw text. The agent's system prompt includes a commands block explaining what commands exist and what they mean — the agent doesn't have to parse `/approve 3` from a text string.

```rust
pub struct ParsedCommand {
    pub def: &'static CommandDef,
    pub args: String,
}
```

`CommandRegistry::parse()` strips the leading slash, splits on the first whitespace, and does a case-insensitive lookup across names and aliases.

---

## `/help` Output

Grouped by category, adapts to platform:

**Portal / Discord (rich):**
```
Session
  /new          Start a new conversation
  /retry        Resend the last message
  /stop         Cancel active workers

Memory
  /memory       Search or list memories
  /remember     Save something to memory
...
```

**Telegram / text (compact):**
```
/new — new conversation
/retry — resend last message
/memory [query] — search memories
/tasks [status] — list tasks
/help — show this
```

---

## Implementation

**`src/commands/`** — new module

- `registry.rs` — `CommandDef`, `CommandRegistry`, static registry, `parse()`
- `handler.rs` — local command dispatch (help, status, usage, model, voice, new)

**Changes per adapter:**
- `portal.rs` — parse `/` prefix in inbound messages, route local commands before forwarding
- `discord.rs` — register application commands on startup, handle interaction events
- `slack.rs` — replace `SlackCommandConfig` dispatch with registry parse on all messages; single `/spacebot` app command
- `telegram.rs` — generate `setMyCommands` from registry, parse `/command` prefix
- text adapters — parse `/` prefix via shared utility

**API:**
```
POST /api/channels/:id/command
Body: { name: string, args: string }
```

Used by Portal frontend for `Agent` commands. `Local` commands never hit the API.

---

## What This Replaces

`SlackCommandConfig` and the current per-command agent routing config in `config.messaging.slack.commands` are removed. Slack slash commands go through the registry. The migration path: any existing `/command → agent_id` config becomes a binding.

---

## Non-Goals

- **Custom user-defined commands** — skills serve this purpose
- **Per-channel command availability** — registry is instance-wide
- **Command permissions** — commands respect existing channel permission rules; no separate command ACL
- **CLI/TUI** — out of scope
