# Participant Awareness

The agent currently has no idea who it's talking to until it branches to recall memories. In a group channel with five people, every message is just `[SomeName]: text` — the agent has to explicitly think about who that person is every time. There's no ambient awareness of the humans in the conversation.

The fix: a `humans` table that caches what the agent knows about each person, populated by a cortex loop that periodically recalls memories per-human and generates short summaries. Active channels get a `## Participant Info` section in the system prompt with a paragraph about each active participant. The agent knows who it's talking to before it even starts thinking.

This builds on the user identity system from the [user-scoped memories](user-scoped-memories.md) design. If user-scoped memories land first, this uses `user_identifiers` as the canonical identity. If not, this introduces its own lightweight identity table and the two get merged later.

## What Exists Today

**User tracking:** None at the database level. Users exist only as `sender_id` + `sender_name` on individual `conversation_messages` rows. No table of known humans. No way to query "who has this agent talked to" without scanning the entire message log.

**Participant awareness in channels:** The channel tracks unique senders within a coalesced message batch for the coalesce hint (`"3 messages from 2 people arrived in 4.2s"`). This is ephemeral — it's a `HashSet` that lives for one batch and is thrown away.

**Memory about humans:** Memories can contain information about users (a Fact memory might say "Jamie prefers dark mode"), but there's no structured link between a memory and a user identity. Recalling "what do I know about Jamie" requires a full hybrid search with the person's name as the query — which only works if the branch thinks to do it.

**The bulletin:** The cortex generates a global memory bulletin every hour and injects it into every channel's system prompt. This is the agent's ambient awareness layer. But it's about the agent itself — its identity, recent events, decisions, goals. It says nothing about who's in the current conversation.

## The Humans Table

New SQLite table for tracking known humans:

```sql
CREATE TABLE IF NOT EXISTS humans (
    id TEXT PRIMARY KEY,                  -- UUID
    display_name TEXT NOT NULL,           -- best-known name, updated on each message
    platform TEXT NOT NULL,               -- "discord", "slack", "telegram"
    platform_user_id TEXT NOT NULL,       -- raw platform ID
    summary TEXT,                         -- cortex-generated 2-3 sentence bio
    last_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_summary_at TIMESTAMP,           -- when summary was last regenerated
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(platform, platform_user_id)
);

CREATE INDEX idx_humans_last_seen ON humans(last_seen_at);
CREATE INDEX idx_humans_platform ON humans(platform, platform_user_id);
```

The `UNIQUE(platform, platform_user_id)` constraint gives one row per platform identity. Cross-platform linking (same human on Discord and Slack) is deferred — if user-scoped memories lands first with `user_identifiers` + `user_platform_links`, this table becomes a `summary` + `last_summary_at` extension on `user_identifiers` rather than a standalone table.

### HumanStore

```rust
pub struct HumanStore {
    pool: SqlitePool,
}

impl HumanStore {
    /// Upsert a human from an inbound message. Fire-and-forget.
    pub fn upsert(&self, platform: &str, platform_user_id: &str, display_name: &str);

    /// Look up a human by platform identity.
    pub async fn get_by_platform_id(
        &self,
        platform: &str,
        platform_user_id: &str,
    ) -> Result<Option<Human>>;

    /// Batch lookup by IDs.
    pub async fn get_by_ids(&self, ids: &[String]) -> Result<Vec<Human>>;

    /// Update a human's summary.
    pub async fn update_summary(&self, id: &str, summary: &str) -> Result<()>;

    /// Humans whose summary is stale (no summary, or last_summary_at < last_seen_at - threshold).
    pub async fn get_stale_summaries(&self, threshold_secs: u64) -> Result<Vec<Human>>;
}
```

Lives at `src/conversation/humans.rs`. The `upsert` is fire-and-forget (`tokio::spawn`) like `ConversationLogger::log_user_message` and `ChannelStore::upsert` — the message pipeline never waits on it.

## Message Pipeline Integration

Every inbound message already carries `sender_id` (platform user ID) and `source` (platform). We add a fire-and-forget `human_store.upsert()` call right next to the existing `channel_store.upsert()`:

```rust
// Already exists
self.state.channel_store.upsert(&message.conversation_id, &message.metadata);

// New
self.state.human_store.upsert(&message.source, &message.sender_id, display_name);
```

The `humans` table fills organically as people talk to the bot. No backfill needed.

## Channel Participant Tracking

Each `Channel` gets a new field:

```rust
pub struct Channel {
    // ... existing fields
    participants: HashMap<String, String>,  // human_id -> display_name
}
```

On each inbound message:

1. Look up `human_store.get_by_platform_id(source, sender_id)` — should always hit since we just upserted
2. Insert into `participants` map
3. If `participants.len() >= min_participants` (configurable, default 2), include the `## Participant Info` section in the next system prompt build

The participant map is per-channel-session (resets when the channel process is dropped from memory). It tracks who's active in this conversation right now, not historically.

### Why a HashMap and not a HashSet

We need the display name for the prompt rendering without an extra DB round-trip on every turn. The map also lets us update display names mid-conversation if a user changes their nickname.

## Participant Summary Generation

New cortex loop: `spawn_participant_loop()`. Sits alongside `spawn_bulletin_loop()` and `spawn_association_loop()` in `cortex.rs`.

### The Loop

Runs on `ParticipantConfig::summary_interval_secs` (default: 300s / 5 minutes).

Each tick:
1. Query `humans` for stale summaries — where `summary IS NULL` or `last_summary_at` is older than `last_seen_at` by `summary_stale_after_secs` (default: 3600s)
2. For each stale human (capped at `max_summaries_per_pass`, default: 10):
   a. Search memories with the human's `display_name` as query via `MemorySearch::search()` (hybrid mode, limit 15)
   b. Query `conversation_messages` for recent messages from this `sender_id` (limit 20)
   c. Feed both to an LLM with the `cortex_participant` system prompt
   d. Store the resulting summary in `humans.summary`, update `last_summary_at`
3. Log the pass via `CortexLogger`

### The Prompt

New template at `prompts/en/cortex_participant.md.j2`:

```
You are generating a brief participant profile for a person the agent interacts with.

Given the memory recall results and recent messages below, write a 2-3 sentence summary of this person. Focus on:
- Who they are and their role (if known)
- What they're currently working on or interested in
- Communication style or preferences (if obvious)

Be factual. Don't speculate beyond what the data shows. If there's very little information, say so briefly.

Write in third person. No headers, no bullet points — just a short paragraph.
```

The user prompt sent to the LLM:

```
Generate a participant summary for: {{ display_name }}

## Memory Recall Results
{{ memory_results }}

## Recent Messages
{{ recent_messages }}
```

### Cost Considerations

The summary is cached — it only regenerates when the human has been active since the last summary. For a server with 100 users where 10 are active daily, the loop generates ~10 summaries per day. Each summary is a single short LLM call (small context, short output). This is negligible compared to the bulletin generation, which runs hourly with much more context.

## Prompt Integration

### Template Changes

`channel.md.j2` gets a new optional section:

```jinja2
{%- if participant_info %}
## Participant Info

{{ participant_info }}
{%- endif %}
```

Positioned after `## Memory Context` and before `## Memory System`. The agent sees who it's talking to before it sees the rules about how memory works.

### `render_channel_prompt` Signature

```rust
pub fn render_channel_prompt(
    &self,
    identity_context: Option<String>,
    memory_bulletin: Option<String>,
    participant_info: Option<String>,        // ← new
    skills_prompt: Option<String>,
    worker_capabilities: String,
    conversation_context: Option<String>,
    status_text: Option<String>,
    coalesce_hint: Option<String>,
) -> Result<String>
```

### System Prompt Assembly

In `Channel::build_system_prompt()`:

```rust
let participant_info = if self.participants.len() >= min_participants {
    let human_ids: Vec<String> = self.participants.keys().cloned().collect();
    let humans = self.state.human_store.get_by_ids(&human_ids).await.unwrap_or_default();

    let sections: Vec<String> = humans
        .iter()
        .filter(|h| h.summary.is_some())
        .map(|h| format!(
            "**{}** — {}",
            h.display_name,
            h.summary.as_deref().unwrap_or("No information available yet.")
        ))
        .collect();

    if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
} else {
    None
};
```

### Example Output

In a Discord server with three active participants:

```markdown
## Participant Info

**Jamie** — Lead developer on the project. Currently deep in an auth system rewrite, switching from session cookies to JWT for mobile app compatibility. Prefers concise responses and works late nights.

**Alex** — Backend engineer focused on database performance. Previously built the migration system. Tends to ask detailed questions about query optimization and indexing.

**Sam** — New to the project, onboarding this week. Has been asking setup questions and reading through the codebase. Background in frontend React development.
```

## The User ID → Human Connection

The mapping chain is straightforward because all the data already flows through the message pipeline:

```
InboundMessage.source ("discord")
    + InboundMessage.sender_id ("123456789")
    → humans table UNIQUE(platform, platform_user_id)
    → Human.id (UUID)
    → Channel.participants HashMap
    → System prompt
```

No ambiguity. The platform adapter already produces both values on every message. We're persisting what's already flowing through the system.

### Future: User-Scoped Memories Integration

When user-scoped memories lands with `user_identifiers` + `user_platform_links`, the `humans` table either:
- Merges into `user_identifiers` (add `summary`, `last_summary_at` columns), or
- Becomes a foreign-key extension (`humans.user_id REFERENCES user_identifiers(id)`)

The participant summary loop would then use the canonical user ID for memory recall with `SearchConfig.user_id`, making recall results dramatically more relevant — only that user's memories, not a fuzzy name match.

## Configuration

The current codebase already has a smaller `ParticipantContextConfig` runtime surface for prompt-time rendering and a per-channel active participant map. The config below is still the intended full-pipeline target once summary generation and persistence land.

```rust
pub struct ParticipantConfig {
    pub enabled: bool,                      // default: true
    pub summary_interval_secs: u64,         // default: 300
    pub summary_max_words: usize,           // default: 100
    pub summary_stale_after_secs: u64,      // default: 3600
    pub min_participants: usize,            // default: 2
    pub max_summaries_per_pass: usize,      // default: 10
}
```

Stored in `RuntimeConfig` as `ArcSwap<ParticipantConfig>`, hot-reloadable like `CortexConfig`.

### min_participants

Controls when the `## Participant Info` section appears. Default is 2 — skip in DMs where there's only one human. Set to 1 to include participant info even in DMs (useful if the agent talks to many people and benefits from ambient per-person context).

## Files Changed

| File | Change |
|------|--------|
| New migration | `humans` table |
| `src/conversation/humans.rs` (new) | `HumanStore` struct — upsert, lookup, summary management |
| `src/conversation.rs` | Add `mod humans` + re-export |
| `src/agent/cortex.rs` | Add `spawn_participant_loop()`, summary generation logic |
| `src/agent/channel.rs` | Add `participants` field, upsert on message, build participant info section |
| `src/config.rs` | Add `ParticipantConfig` |
| `src/main.rs` | Wire `HumanStore` into deps, spawn participant loop |
| `src/lib.rs` | Add `HumanStore` to `AgentDeps` (or channel state) |
| `prompts/en/channel.md.j2` | Add `participant_info` section |
| `prompts/en/cortex_participant.md.j2` (new) | Summary generation prompt |
| `prompts/en/fragments/system/participant_synthesis.md.j2` (new) | User prompt template for summary generation |
| `src/prompts/engine.rs` | Register templates, add render methods |
| `src/prompts/text.rs` | Register new template files |

## Phases

### Phase 1: Humans Table + Store

- Migration for `humans` table
- `HumanStore` with `upsert`, `get_by_platform_id`, `get_by_ids`, `update_summary`, `get_stale_summaries`
- Wire `upsert` into the message pipeline (fire-and-forget, next to `channel_store.upsert`)
- Add `HumanStore` to `AgentDeps` or `ChannelState`

### Phase 2: Channel Participant Tracking

- Add `participants: HashMap<String, String>` to `Channel`
- On each inbound message, look up human and add to participants
- Add `ParticipantConfig` to `config.rs` and `RuntimeConfig`

### Phase 3: Participant Summary Cortex Loop

- `spawn_participant_loop()` in `cortex.rs`
- `cortex_participant.md.j2` and `participant_synthesis.md.j2` prompts
- Register templates in `PromptEngine`
- Summary generation: memory recall + recent messages → LLM → cached summary
- Log via `CortexLogger`

### Phase 4: Prompt Integration

- Add `participant_info` to `channel.md.j2`
- Update `render_channel_prompt()` signature
- Build participant info section in `Channel::build_system_prompt()`
- Update `build_system_prompt_with_coalesce()` to include participant info

Phase 1 and 2 are tightly coupled and should ship together. Phase 3 can ship independently — channels work without summaries (the section just won't appear until the cortex generates them). Phase 4 depends on Phase 2.

## What This Enables

**Contextual awareness in group channels.** The agent walks into a conversation knowing who everyone is. No branching to recall, no "remind me what you're working on" — it already knows.

**Proactive relevance.** When Jamie messages about auth, the agent already knows Jamie is the one doing the auth rewrite. It can connect dots immediately instead of guessing or branching to check.

**Natural conversation in communities.** In a 50-person Discord server, the agent doesn't treat every message as coming from a stranger. Regular participants get recognized. The agent's responses feel like talking to someone who actually remembers you.

**Foundation for user-scoped memories.** The `humans` table is a stepping stone. Once user-scoped memories lands, the participant summary loop becomes dramatically more accurate — it recalls that specific user's memories instead of doing a fuzzy name search across all memories.
