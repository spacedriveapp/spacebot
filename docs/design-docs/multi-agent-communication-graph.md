# Multi-Agent Communication Graph

Agents on a single Spacebot instance are completely isolated. They share an LLM provider pool and a messaging pipeline, but have no way to talk to each other. The `send_message_to_another_channel` tool sends messages to platform channels (Discord, Slack, etc.), not to other agents. Two agents watching the same Discord server can't coordinate, delegate, escalate, or share context.

The fix: an explicit communication graph between agents. Directed edges define who can message whom, with policy flags controlling the relationship. Messages flow through a shared internal channel visible to both agents and to humans in the dashboard. The graph models organizational hierarchy — superiors, subordinates, peers — so agents can be wired into company-like structures with clear chains of delegation and reporting.

The existing `send_message_to_another_channel` tool is unrelated — it's scoped to a single agent's known platform channels and routes through `MessagingManager::broadcast()`. Agent-to-agent communication is a new mechanism, but not a new transport. Messages are constructed as `InboundMessage` with `source: "internal"` and injected into the existing `MessagingManager` fan-in via `inject_message()`. The main loop already routes by `agent_id` and `conversation_id`, so internal messages flow through the same pipeline as platform messages — with link policy enforcement, shared conversation history, and UI visibility.

## What Exists Today

**Agent isolation:** Each agent has its own SQLite database, memory store, LanceDB instance, and set of channels. Agents share only the `LlmManager`, `MessagingManager`, and instance-level config. There is no data path between agents.

**Cross-channel messaging:** The `send_message_to_another_channel` tool lets a channel send a message to another platform channel via `MessagingManager::broadcast()`. It resolves targets through `ChannelStore::find_by_name()`, which searches the `channels` table for display name matches. This is platform-level routing — it delivers to a Discord channel or Telegram chat, not to another agent's processing pipeline.

**Available channels context:** The `available_channels.md.j2` prompt fragment lists channels the agent knows about. This gives the LLM awareness of where it can send messages, but the list is platform channels, not agent peers.

**Bindings:** `config.rs` defines `Binding` structs that route inbound platform messages to agents. Bindings are one-directional (platform → agent) and have no concept of agent-to-agent routing.

**Event bus:** `ProcessEvent` is a broadcast channel per agent. Events are typed (branch started, worker complete, tool started, etc.) and feed the API's SSE pipeline. There is no cross-agent event bus.

## The Communication Graph

The graph is a set of directed edges between agents. Each edge is a **link** — a persistent, policy-governed communication channel. When agent A has a link to agent B, agent A can send messages to agent B. The link carries policy flags that define the nature of the relationship.

### Why Agent-Level, Not Channel-Level

Links connect agents, not channels. An agent may have dozens of active channels (one per Discord thread, Telegram chat, etc.), and those channels come and go. The communication graph operates at the organizational level — "the support agent can escalate to the engineering agent" — not at the conversation level.

When agent A sends a message to agent B through a link, the message lands in a dedicated internal channel between them. This channel persists across platform channel lifecycles.

### Link Model

```rust
pub struct AgentLink {
    pub id: String,                    // UUID
    pub from_agent_id: String,         // source agent
    pub to_agent_id: String,           // target agent
    pub direction: LinkDirection,      // one_way, two_way
    pub relationship: LinkRelationship, // peer, superior, subordinate
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum LinkDirection {
    /// from_agent can message to_agent, but not vice versa.
    OneWay,
    /// Both agents can message each other through this link.
    TwoWay,
}

pub enum LinkRelationship {
    /// Equal peers — neither agent has authority over the other.
    Peer,
    /// from_agent is superior to to_agent. Can delegate tasks,
    /// request status, and override decisions.
    Superior,
    /// from_agent is subordinate to to_agent. Reports status,
    /// escalates issues, requests approval.
    Subordinate,
}
```

A two-way link creates a single internal channel shared by both agents. A one-way link creates the same channel but only the `from_agent` can initiate messages — the `to_agent` can read and respond within an existing thread but cannot start new conversations.

### Internal Channel

When a link is created, a dedicated internal channel is spawned for that link. The channel ID follows the pattern `link:{link_id}`. This channel:

- Has its own conversation history in `conversation_messages`, just like platform channels
- Appears in both agents' channel lists
- Is visible in the dashboard under a dedicated "Agent Links" section
- Supports the same coalescing, branching, and worker spawning as platform channels

Messages in this channel carry metadata identifying the sending agent:

```rust
InboundMessage {
    id: uuid::Uuid::new_v4().to_string(),
    source: "internal".into(),
    conversation_id: format!("link:{link_id}"),
    sender_id: sending_agent_id.to_string(),
    agent_id: Some(receiving_agent_id.clone()),
    content: MessageContent::Text(message),
    timestamp: Utc::now(),
    metadata: HashMap::from([
        ("link_id".into(), json!(link_id)),
        ("from_agent_id".into(), json!(sending_agent_id)),
        ("relationship".into(), json!("peer")), // or "superior" / "subordinate"
    ]),
    formatted_author: Some(format!("[{}]", sending_agent_name)),
}
```

### Relationship Semantics

The `LinkRelationship` affects the receiving agent's system prompt context and available actions:

**Peer:** Both agents are equals. Messages are informational — "here's what I found", "can you check this", "FYI the deploy failed". Neither agent has authority to assign tasks to the other.

**Superior → Subordinate:** The superior can delegate tasks (which spawn workers on the subordinate), request status reports, and send directives. The subordinate's prompt context includes awareness that messages from this agent carry authority. The subordinate can escalate back — "I need help with X" or "this is beyond my scope."

**Subordinate → Superior:** The subordinate can report status, escalate issues, and request decisions. The superior's prompt context frames these as reports from a direct report. The superior can respond with instructions.

The relationship doesn't restrict message delivery — it frames context. A subordinate can still message its superior freely. The relationship metadata shapes how the LLM interprets and responds to messages.

## Schema

New migration:

```sql
CREATE TABLE IF NOT EXISTS agent_links (
    id TEXT PRIMARY KEY,
    from_agent_id TEXT NOT NULL,
    to_agent_id TEXT NOT NULL,
    direction TEXT NOT NULL DEFAULT 'two_way',   -- 'one_way' or 'two_way'
    relationship TEXT NOT NULL DEFAULT 'peer',    -- 'peer', 'superior', 'subordinate'
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(from_agent_id, to_agent_id)
);

CREATE INDEX IF NOT EXISTS idx_agent_links_from ON agent_links(from_agent_id);
CREATE INDEX IF NOT EXISTS idx_agent_links_to ON agent_links(to_agent_id);
```

This table lives in a shared instance-level SQLite database, not per-agent. Agent links span agents, so they can't live in either agent's isolated database. The instance already has a config loading path — this adds a small shared database alongside it.

The `UNIQUE(from_agent_id, to_agent_id)` constraint means at most one link per direction between two agents. A two-way link between A and B is a single row with `direction = 'two_way'`. For asymmetric relationships (A is superior to B), there's one row with `from_agent_id = A, to_agent_id = B, relationship = 'superior'`.

### Instance Database

New shared SQLite database at `{instance_dir}/instance.db`. Contains only cross-agent data:

```
{instance_dir}/
    instance.db              ← new: agent_links, shared_notes (future)
    agents/
        agent-a/
            data/spacebot.db ← existing per-agent database
        agent-b/
            data/spacebot.db
```

This keeps per-agent data isolated while giving links a home that both agents can reference.

Instance-level migrations live in a separate `migrations_instance/` directory. The existing `sqlx::migrate!("./migrations")` is compiled into the binary targeting per-agent databases — instance.db needs its own `sqlx::migrate!("./migrations_instance/")` call during startup.

## LinkStore

```rust
pub struct LinkStore {
    pool: SqlitePool,  // instance.db pool
}

impl LinkStore {
    /// Get all links involving this agent (as source or target).
    pub async fn get_links_for_agent(&self, agent_id: &str) -> Result<Vec<AgentLink>>;

    /// Get a specific link by ID.
    pub async fn get(&self, link_id: &str) -> Result<Option<AgentLink>>;

    /// Get the link between two specific agents (if any).
    pub async fn get_between(
        &self,
        from_agent_id: &str,
        to_agent_id: &str,
    ) -> Result<Option<AgentLink>>;

    /// Create a new link. Returns error if a link already exists between these agents.
    pub async fn create(&self, link: &AgentLink) -> Result<()>;

    /// Update link properties (direction, relationship, enabled).
    pub async fn update(&self, link: &AgentLink) -> Result<()>;

    /// Delete a link and its associated internal channel history.
    pub async fn delete(&self, link_id: &str) -> Result<()>;
}
```

## Message Routing

### Sending

New tool: `send_agent_message`. Available to channels that have at least one active link.

```rust
pub struct SendAgentMessageArgs {
    /// Target agent ID or name.
    pub target: String,
    /// The message content.
    pub message: String,
}
```

The tool:

1. Resolves the target agent by ID or name
2. Looks up the link between the sending agent and the target agent via `LinkStore`
3. Validates the link exists, is enabled, and permits messaging in this direction
4. Constructs an `InboundMessage` with `source: "internal"`, the target `agent_id`, and `conversation_id: "link:{link_id}"`
5. Calls `MessagingManager::inject_message()` to push it into the existing fan-in

No new transport is needed. `inject_message()` already exists on `MessagingManager` — it pushes an `InboundMessage` into the same `mpsc` channel that platform adapters use. The main loop already routes by `agent_id` and `conversation_id`, so internal messages get routed to the correct agent and land in the correct link channel automatically.

### Receiving

The receiving agent processes internal messages the same way it processes platform messages. The message arrives through the existing `InboundMessage` pipeline, gets assigned to the `link:{link_id}` channel, and triggers the standard channel runtime (coalescing, system prompt build, LLM call, branching if needed).

The channel's system prompt includes context about who the message is from:

```jinja2
{%- if link_context %}
## Agent Communication

This is an internal channel with **{{ link_context.agent_name }}** ({{ link_context.relationship }}).
{% if link_context.relationship == "superior" -%}
Messages from this agent carry organizational authority. Treat directives as assignments.
{%- elif link_context.relationship == "subordinate" -%}
This is a report from a subordinate agent. They may be escalating, reporting status, or requesting guidance.
{%- else -%}
This is a peer agent. Communication is collaborative and informational.
{%- endif %}
{%- endif %}
```

## Prompt Integration

### ROLE.md

New identity file alongside `SOUL.md`, `IDENTITY.md`, and `USER.md`. Defines what the agent is supposed to *do* — responsibilities, scope, what to handle vs what to escalate, what success looks like.

`SOUL.md` is personality. `IDENTITY.md` is who the agent is. `USER.md` is context about the human. `ROLE.md` is the job: "you handle tier 1 support tickets, escalate billing issues to the finance agent, never touch production infrastructure."

In single-agent setups, `ROLE.md` separates identity from operational responsibilities. In multi-agent setups, it's what differentiates agents operationally — each agent sees its position in the hierarchy via org context, and `ROLE.md` tells it what to actually do in that position. Structure vs scope.

Loaded the same way as the other identity files — from the agent's workspace directory, injected into the system prompt by `identity/files.rs`.

### Organizational Awareness

The core prompt addition is not a list of sendable targets — it's structural awareness. The agent needs to understand where it sits in the org, who's above it, who's below it, and who its peers are. Without this, link channels are just more inboxes. With it, the agent can reason about delegation, escalation, and collaboration.

New prompt fragment `fragments/org_context.md.j2`:

```jinja2
{%- if org_context %}
## Organization

You are part of a multi-agent system. Here is your position:

{% if org_context.superiors -%}
**Reports to:**
{% for agent in org_context.superiors -%}
- **{{ agent.name }}** — your superior. Messages from this agent carry organizational authority.
{% endfor %}
{%- endif %}

{% if org_context.subordinates -%}
**Direct reports:**
{% for agent in org_context.subordinates -%}
- **{{ agent.name }}** — reports to you. You can delegate tasks, request status, and send directives.
{% endfor %}
{%- endif %}

{% if org_context.peers -%}
**Peers:**
{% for agent in org_context.peers -%}
- **{{ agent.name }}** — equal peer. Communication is collaborative and informational.
{% endfor %}
{%- endif %}

Use the `send_agent_message` tool to communicate with these agents. Each link has a dedicated internal channel with full conversation history.
{%- endif %}
```

This is structured by relationship, not as a flat list. The agent sees the hierarchy, not just who it can message. The template groups agents by superiors/subordinates/peers so the LLM can reason about appropriate behavior — escalate up, delegate down, collaborate across.

This fragment is reusable across process types. Channels get it on every turn. The cortex gets it when running autonomous behaviors (future work — the template is ready, injection into cortex prompts is a separate PR). The data source is the same: `LinkStore::get_links_for_agent()` resolved against agent configs for display names.

Platform channel awareness (`available_channels.md.j2`) remains separate — it lists Discord/Slack/Telegram channels for `send_message_to_another_channel`. Org context lists agents for `send_agent_message`. Different tools, different context.

### Link Channel Prompt

When a channel is a link channel (`link:{link_id}`), the system prompt includes the `link_context` section described in the Receiving section above. This is injected during `build_system_prompt()` by checking if the channel ID starts with `link:` and looking up the link metadata. This is in addition to the org context — the agent knows both its overall position and who it's currently talking to in this specific channel.

## API Surface

### Link CRUD

```
GET    /api/links                    — list all links
GET    /api/links/:id                — get link details
POST   /api/links                    — create a link
PUT    /api/links/:id                — update link properties
DELETE /api/links/:id                — delete a link

GET    /api/agents/:id/links         — get links for a specific agent
```

### Link Messages

```
GET    /api/links/:id/messages       — get conversation history for a link channel
```

This reuses the existing conversation history infrastructure — the link channel ID is just another channel ID in the `conversation_messages` table.

### Topology Snapshot

```
GET    /api/topology                 — full agent graph for UI rendering
```

Returns:

```json
{
    "agents": [
        { "id": "support", "name": "Support Agent" },
        { "id": "engineering", "name": "Engineering Agent" }
    ],
    "links": [
        {
            "id": "uuid",
            "from": "support",
            "to": "engineering",
            "direction": "two_way",
            "relationship": "subordinate",
            "enabled": true
        }
    ]
}
```

This is the payload the React Flow graph editor consumes to render the topology.

## Config Integration

Links can be defined in TOML config alongside agents:

```toml
[[links]]
from = "support"
to = "engineering"
direction = "two_way"
relationship = "subordinate"

[[links]]
from = "manager"
to = "support"
direction = "two_way"
relationship = "superior"

[[links]]
from = "manager"
to = "engineering"
direction = "two_way"
relationship = "superior"
```

Config-defined links are synced to the database on startup. The API can also create links at runtime. Config-defined links take precedence — if a link exists in both config and DB with different properties, the config version wins on next reload.

## Event Pipeline

Link messages emit `ProcessEvent` variants so the dashboard can track inter-agent communication:

```rust
ProcessEvent::AgentMessageSent {
    from_agent_id: AgentId,
    to_agent_id: AgentId,
    link_id: String,
    channel_id: ChannelId,
}

ProcessEvent::AgentMessageReceived {
    from_agent_id: AgentId,
    to_agent_id: AgentId,
    link_id: String,
    channel_id: ChannelId,
}
```

These feed into the existing SSE pipeline. The `ProcessEvent` → `ApiEvent` forwarding in `api/state.rs` has a catch-all that drops unknown variants, so these need explicit `ApiEvent` counterparts and match arms to reach the dashboard. The dashboard can then render a live activity view of inter-agent communication overlaid on the topology graph.

## Shared Notes (v2)

Deferred to a second phase. The concept: named knowledge nodes that multiple agents can read from and write to, with per-agent permissions (read-only, read-write).

```sql
CREATE TABLE IF NOT EXISTS shared_notes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    content TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS shared_note_permissions (
    note_id TEXT NOT NULL REFERENCES shared_notes(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    access TEXT NOT NULL DEFAULT 'read',  -- 'read' or 'read_write'
    PRIMARY KEY (note_id, agent_id)
);
```

Shared notes would give agents a persistent scratchpad — the engineering agent writes deployment status, the support agent reads it to answer customer questions. Tools: `read_shared_note`, `write_shared_note`. But this is a separate design problem with its own conflict resolution and versioning concerns.

## Files Changed

| File | Change |
|------|--------|
| `migrations_instance/` (new dir) | Separate migration directory for instance.db |
| New migration | `agent_links` table in instance.db |
| `src/links.rs` (new) | Module root, re-exports |
| `src/links/store.rs` (new) | `LinkStore` — CRUD for agent links |
| `src/links/types.rs` (new) | `AgentLink`, `LinkDirection`, `LinkRelationship` |
| `src/tools/send_agent_message.rs` (new) | `SendAgentMessageTool` — agent-to-agent messaging |
| `src/tools.rs` | Register `send_agent_message` tool for linked agents |
| `src/config.rs` | Add `links: Vec<LinkDef>` to `Config`, TOML parsing |
| `src/lib.rs` | Add `mod links`, new `ProcessEvent` variants |
| `src/main.rs` | Initialize instance.db, `LinkStore`, sync config links |
| `src/identity/files.rs` | Load `ROLE.md` alongside SOUL/IDENTITY/USER |
| `src/agent/channel.rs` | Inject link context + org context into system prompt, handle internal source messages |
| `src/api/server.rs` | Mount link CRUD and topology routes |
| `src/api/links.rs` (new) | API handlers for link CRUD + topology |
| `src/api/state.rs` | Add `LinkStore` to `ApiState` |
| `prompts/en/fragments/org_context.md.j2` (new) | Organizational hierarchy prompt section |
| `prompts/en/fragments/link_context.md.j2` (new) | Link channel context section |
| `prompts/en/tools/send_agent_message_description.md.j2` (new) | Tool description |
| `src/prompts/engine.rs` | Register new templates |
| `src/prompts/text.rs` | Register new text templates |
| `src/db.rs` | Add instance.db connection setup with separate migration path |

## Phases

### Phase 1: Instance Database + Link Model

- Set up `instance.db` at `{instance_dir}/instance.db` with separate migration directory
- Migration for `agent_links` table
- `LinkStore` with full CRUD
- `AgentLink`, `LinkDirection`, `LinkRelationship` types
- Config parsing for `[[links]]` sections
- Sync config links to database on startup

### Phase 2: Send Tool + Prompt Context

- `ROLE.md` identity file — loaded by `identity/files.rs`, injected into system prompt
- `SendAgentMessageTool` — resolve target, validate link, construct `InboundMessage`, deliver via `inject_message()`
- `org_context.md.j2` prompt fragment — organizational hierarchy, injected into channel system prompt when agent has links
- `link_context.md.j2` prompt fragment — inject when channel is a link channel
- Tool description prompt for `send_agent_message`
- Register tool conditionally (only when agent has active links)

### Phase 3: Channel Runtime Integration

- Handle `source: "internal"` messages in the channel runtime
- Link channels get their own conversation history (same `conversation_messages` table)
- Coalescing, branching, and workers work on link channels the same as platform channels
- `ProcessEvent::AgentMessageSent` and `AgentMessageReceived` events
- Corresponding `ApiEvent` variants in `api/state.rs` for SSE forwarding

### Phase 4: API + UI Foundation

- Link CRUD API endpoints
- Topology snapshot endpoint
- Wire `LinkStore` into `ApiState`
- SSE events for inter-agent message activity

Phase 1 is the data foundation. Phase 2 gives agents the ability to send messages — no new transport, just a tool that constructs an `InboundMessage` and pushes it through the existing `MessagingManager::inject_message()` fan-in. Phase 3 makes the receiving side work end-to-end. Phase 4 is the API layer that the dashboard will consume for the React Flow editor.

### Future: React Flow Topology Editor

Not part of this design but the intended consumer of the topology API. The Overview page in the embedded dashboard would be replaced (or extended) with a React Flow graph showing agents as nodes and links as directed edges. Users drag to create links, click edges to configure direction and relationship. Live activity indicators show messages flowing between agents in real time.

### Future: Shared Notes (v2)

Persistent knowledge nodes with per-agent read/write permissions. Separate design doc when the link system is stable.

## What This Enables

**Organizational hierarchy.** Wire agents into manager/report structures. A manager agent delegates to specialists, specialists report back. The communication is explicit, auditable, and visible to humans.

**Cross-agent coordination.** A support agent detects a bug and escalates to the engineering agent with full context. The engineering agent investigates, spawns workers, and reports findings back through the link channel. Humans can observe the entire exchange in the dashboard.

**Separation of concerns.** Instead of one omniscient agent, split responsibilities across specialized agents that communicate through defined interfaces. A sales agent handles leads, a support agent handles tickets, an engineering agent handles technical work. Each has its own memory, identity, and personality.

**Auditable communication.** Every inter-agent message is persisted in `conversation_messages` with full metadata. The dashboard shows the communication graph with live activity. There are no hidden side channels — everything flows through the link system.

**Foundation for agent teams.** Once links and the topology API exist, the React Flow editor turns agent wiring into a visual, drag-and-drop experience. Non-technical users can design agent organizations without editing config files.
