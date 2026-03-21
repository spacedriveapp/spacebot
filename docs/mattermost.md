# Mattermost

Spacebot connects to Mattermost via a bot account using the Mattermost REST API and WebSocket event stream. The integration can be configured in the web UI or via `config.toml`.

## Features

### Supported
- Receive messages from channels and direct messages
- Send text replies (long messages are automatically split into multiple posts)
- Streaming replies with live edit-in-place updates and typing indicator
- Thread-aware replies (replies stay in the originating thread)
- File/image attachments (up to a configurable size limit)
- Emoji reactions
- Fetch channel history (used for context window)
- Multiple named instances (connect to more than one Mattermost server)
- Per-team and per-channel allowlists
- DM allowlist (fail-closed: DMs are blocked unless the sender is explicitly listed)
- `require_mention` routing (only respond when the bot is @-mentioned)

### Not supported (compared to Slack/Discord)
- Slash commands — Mattermost slash commands are not handled; use @-mentions instead
- Ephemeral (private) messages
- Message threading via `parent_id` lookup — threads are followed when the inbound message carries a root ID, but the bot cannot independently look up thread context
- User/channel autocomplete
- Presence or status events
- App marketplace / interactive components (buttons, modals)

## Setup

### 1. Create a bot account

In Mattermost: **System Console → Integrations → Bot Accounts → Add Bot Account**

- Give it a username (e.g. `spacebot`)
- Copy the generated access token

The bot must be added to any team and channel it should respond in. Bot accounts in Mattermost are not automatically visible in channels.

### 2. Configure in config.toml

```toml
[messaging.mattermost]
enabled = true
base_url = "https://mattermost.example.com"   # origin only, no path
token = "your-bot-access-token"
team_id = "team_id_here"                      # optional: default team for events without one
max_attachment_bytes = 52428800               # optional: default 50 MB
dm_allowed_users = []                         # optional: user IDs allowed to DM the bot
```

The token can also be supplied via the `MATTERMOST_TOKEN` environment variable and `base_url` via `MATTERMOST_BASE_URL`.

> **Security**: `base_url` must use `https` for non-localhost hosts. Plain `http` is only accepted for `localhost` / `127.0.0.1` / `::1`.

### 3. Wire up a binding

Bindings connect Mattermost channels to agents:

```toml
[[bindings]]
agent_id = "my-agent"
channel = "mattermost"
channel_ids = ["channel_id_here"]   # leave empty to match all channels
require_mention = false
```

To scope a binding to a specific Mattermost team, add `team_id`:

```toml
[[bindings]]
agent_id = "my-agent"
channel = "mattermost"
team_id = "team_id_here"
channel_ids = ["channel_id_here"]
```

## Multiple servers (named instances)

```toml
[[messaging.mattermost.instances]]
name = "corp"
enabled = true
base_url = "https://mattermost.corp.example.com"
token = "corp-bot-token"
team_id = "corp_team_id"

[[messaging.mattermost.instances]]
name = "community"
enabled = true
base_url = "https://community.example.com"
token = "community-bot-token"
```

Named instance tokens can be supplied via `MATTERMOST_CORP_TOKEN` / `MATTERMOST_COMMUNITY_TOKEN` etc.

In bindings, reference a named instance with the `adapter` field:

```toml
[[bindings]]
agent_id = "my-agent"
channel = "mattermost"
adapter = "corp"
channel_ids = ["channel_id_here"]
```

## Web UI

All of the above can be configured in the Spacebot web interface under **Settings → Messaging → Mattermost**. The UI supports adding credentials, enabling/disabling the adapter, and managing bindings with team and channel scoping.

## Finding IDs

Mattermost does not display IDs in the UI by default. The easiest ways to retrieve them:

- **Team ID**: `GET /api/v4/teams/name/{team_name}` → `.id`
- **Channel ID**: `GET /api/v4/teams/{team_id}/channels/name/{channel_name}` → `.id`
- **User ID**: `GET /api/v4/users/username/{username}` → `.id`

Alternatively, enable **Account Settings → Advanced → Enable post formatting** and inspect the network tab when loading a channel — team and channel IDs appear in the request URLs.
