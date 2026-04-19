# User-Scoped Memories

Memories are currently global per agent. Every memory created by any user in any channel goes into the same pool. When two people are talking to the same agent in a Discord server, their memories mix — one person's website project pollutes recall results for the other. For a personal bot this is fine. For a community or enterprise system, it makes the memory system unusable at scale.

The fix: an optional `user_id` on memories. Memories about or from a specific person are scoped to that person. Shared facts, decisions, and identity memories remain global. Recall returns the queried user's memories plus globals. No AI system does this today.

## What Exists Today

The `Memory` struct has no user concept:

```rust
pub struct Memory {
    pub id: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub importance: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_accessed_at: DateTime<Utc>,
    pub access_count: i64,
    pub source: Option<String>,
    pub channel_id: Option<ChannelId>,
    pub forgotten: bool,
}
```

User identity does exist at the message level — `InboundMessage.sender_id` carries platform user IDs, and `conversation_messages` persists `sender_name` and `sender_id` per message. But none of this connects to memory. Branches see formatted history like `[JamiePine]: hello` but have no structured user data. The compactor actively strips attribution, rendering everything as `User: text`.

## Canonical User Identity

Rather than storing raw platform IDs (`discord:123456`) on memories, we introduce a canonical user identity. A `user_identifiers` table maps platform-specific IDs to a stable internal ID. This means:

- Memories reference one canonical ID regardless of which platform the user spoke on
- When the hosted platform adds spacebot.sh accounts, it's just another platform link
- Cross-platform identity linking (discord user X is slack user Y) is a row insert, not a data migration

### Schema

```sql
CREATE TABLE IF NOT EXISTS user_identifiers (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS user_platform_links (
    platform TEXT NOT NULL,
    platform_user_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES user_identifiers(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (platform, platform_user_id)
);

CREATE INDEX idx_user_links_user_id ON user_platform_links(user_id);
```

`user_identifiers.id` is a UUID. `display_name` is the last-known display name, updated on each message. `user_platform_links` maps platform-specific IDs to canonical IDs with a composite primary key.

### Resolution

On every inbound message, the routing layer resolves the sender to a canonical user:

```rust
let canonical_user_id = user_store
    .resolve_or_create(&message.source, &message.sender_id, &display_name)
    .await?;
```

This does a lookup on `(platform, platform_user_id)`. If found, returns the canonical ID and updates `display_name` if changed. If not found, creates a new `user_identifiers` row and a link row. One indexed query per message, fire-and-forget on the upsert path.

### UserStore

Small module, lives at `src/users.rs` with store logic in `src/users/store.rs`:

```rust
pub struct UserStore {
    pool: SqlitePool,
}

impl UserStore {
    pub async fn resolve_or_create(
        &self,
        platform: &str,
        platform_user_id: &str,
        display_name: &str,
    ) -> Result<String>;

    pub async fn get_by_id(&self, id: &str) -> Result<Option<UserIdentifier>>;

    pub async fn get_by_platform(
        &self,
        platform: &str,
        platform_user_id: &str,
    ) -> Result<Option<UserIdentifier>>;

    pub async fn link_platform(
        &self,
        canonical_id: &str,
        platform: &str,
        platform_user_id: &str,
    ) -> Result<()>;

    pub async fn list(&self, limit: i64, offset: i64) -> Result<Vec<UserIdentifier>>;
}
```

## Memory Schema Changes

Add `user_id` to the `memories` table:

```sql
ALTER TABLE memories ADD COLUMN user_id TEXT REFERENCES user_identifiers(id);
CREATE INDEX idx_memories_user_id ON memories(user_id);
CREATE INDEX idx_memories_user_type ON memories(user_id, memory_type);
```

Nullable. Existing memories keep `user_id = NULL` (global). No data migration needed.

The `Memory` struct gets:

```rust
pub struct Memory {
    // ... existing fields
    pub user_id: Option<String>,
}

impl Memory {
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }
}
```

### LanceDB

The `EmbeddingTable` schema adds a `user_id` column:

```
Table: memory_embeddings
  id:        Utf8 (NOT NULL)
  content:   Utf8 (NOT NULL)
  user_id:   Utf8 (NULLABLE)            ← new
  embedding: FixedSizeList<Float32>[384] (NOT NULL)
```

This allows filtering at the vector/FTS search level rather than post-filtering in SQLite, which would break RRF fusion scoring. Existing rows get `user_id = NULL` during the schema rebuild.

## Search Behavior

When `user_id` is set on a recall query, search returns **that user's memories + global memories** (where `user_id IS NULL`). Always. No flag to toggle this — you always want identity, shared decisions, and system observations in the results.

### SearchConfig

```rust
pub struct SearchConfig {
    // ... existing fields
    pub user_id: Option<String>,
}
```

### Filter Logic

All search paths apply the same rule:

```sql
-- SQLite queries
WHERE (user_id = ?1 OR user_id IS NULL) AND forgotten = 0

-- LanceDB vector/FTS
filter: "user_id = '{user_id}' OR user_id IS NULL"
```

When `user_id` is `None` on the query, no user filter is applied — all memories are returned (current behavior).

### Graph Traversal

Graph traversal respects user scope. When walking associations from a user-scoped memory, the traversal follows edges into:
- Other memories owned by the same user
- Global memories (user_id IS NULL)

It does not walk into memories owned by other users. This prevents one user's recall from surfacing another user's personal facts through association chains.

## Tool Changes

### memory_save

```rust
pub struct MemorySaveArgs {
    pub content: String,
    pub memory_type: String,
    pub importance: Option<f32>,
    pub source: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: Option<String>,        // ← new
    pub associations: Vec<AssociationInput>,
}
```

The LLM decides when to set `user_id`. The prompt guidance: "When saving a memory about or from a specific person, include their user_id. For shared decisions, system facts, or observations not tied to one person, omit user_id."

### memory_recall

```rust
pub struct MemoryRecallArgs {
    pub query: Option<String>,
    pub max_results: usize,
    pub memory_type: Option<String>,
    pub mode: Option<String>,
    pub sort_by: Option<String>,
    pub user_id: Option<String>,        // ← new
}
```

When set, filters results to that user + globals. Applies across all search modes (hybrid, recent, important, typed).

## Context Flow

### Channel → Branch

When a branch is spawned, the channel passes the current sender's canonical user ID. This gets injected into the branch's system prompt:

```
Current user: JamiePine (user_id: a1b2c3d4-...)
```

The branch uses this when calling `memory_save` (to scope new memories) and `memory_recall` (to search for that user's context). The LLM still makes the judgment call — not every memory in a conversation with user X should be scoped to user X.

### Channel to Compactor

Historical note: the old compactor path rendered messages as `User: {text}`, stripping all sender attribution. That made compaction-extracted memories impossible to scope to a user because the compaction LLM did not know who said what.

Fix: `render_messages_as_transcript()` preserves sender names:

```
JamiePine: I'm switching the auth system to JWT
JamiePine: session cookies don't work well with the mobile app
```

The compaction worker also receives a mapping of display names to canonical user IDs, injected into its system prompt, so it can set `user_id` on extracted memories.

### Context Injection

`build_channel_context()` currently injects identity + high-importance memories globally. With user scoping:

1. Identity memories — always global, no change
2. High-importance memories — when a channel has a current sender, filter to that user + globals
3. Memory bulletin — remains global (the cortex synthesizes across all users)

This means when JamiePine sends a message, the channel's context includes JamiePine's high-importance memories alongside global ones. When someone else messages, they get their own context.

## API Changes

Existing endpoints get a `user_id` query parameter:

```
GET /api/agents/memories?agent_id=X&user_id=Y&limit=50
GET /api/agents/memories/search?agent_id=X&user_id=Y&q=auth+system
```

New endpoint for listing known users:

```
GET /api/agents/users?agent_id=X&limit=50&offset=0
```

Returns `UserIdentifier` objects with `id`, `display_name`, `created_at`, `platform_links[]`, and a `memory_count`.

## Prompt Updates

### memory_save_description.md.j2

Add documentation for the `user_id` parameter. Explain the distinction: user-scoped memories are about or from a specific person, global memories are shared context.

### memory_recall_description.md.j2

Add documentation for the `user_id` filter. Explain that filtering returns user memories + globals.

### branch.md.j2

When a branch is spawned with user context, the prompt includes structured user info and instructions to use it with memory tools.

## Files Changed

| File | Change |
|------|--------|
| New migration SQL | `user_identifiers`, `user_platform_links` tables; `user_id` column on `memories` |
| `src/users.rs` (new) | Module root, re-exports |
| `src/users/store.rs` (new) | `UserStore` — resolve, create, link, list |
| `src/memory/types.rs` | `user_id: Option<String>` on `Memory`, builder method |
| `src/memory/store.rs` | User-filtered queries on all read paths |
| `src/memory/lance.rs` | Schema change + filtered vector/FTS search |
| `src/memory/search.rs` | `SearchConfig.user_id`, filter propagation |
| `src/tools/memory_save.rs` | `user_id` on `MemorySaveArgs`, pass through to `Memory` |
| `src/tools/memory_recall.rs` | `user_id` on `MemoryRecallArgs`, pass to `SearchConfig` |
| `src/agent/channel.rs` | Resolve canonical user_id, pass to branch/compactor |
| `src/agent/branch.rs` | Accept user context, inject into prompt |
| `src/agent/compactor.rs` | Preserve sender attribution in transcripts, pass user mapping |
| `src/conversation/context.rs` | User-aware memory injection |
| `src/main.rs` | Initialize `UserStore`, resolve on inbound messages |
| `src/api/server.rs` | `user_id` filter params, `/api/agents/users` endpoint |
| `src/lib.rs` | Re-export user types |
| `prompts/en/tools/memory_save_description.md.j2` | Document `user_id` |
| `prompts/en/tools/memory_recall_description.md.j2` | Document `user_id` filter |
| `prompts/en/branch.md.j2` | User context injection |
| `prompts/en/compactor.md.j2` | User mapping for memory extraction |

## Phases

### Phase 1: User Identity

- Migration for `user_identifiers` and `user_platform_links`
- `UserStore` with `resolve_or_create`
- Wire into `main.rs` message routing to resolve canonical IDs on every inbound message
- Attach canonical user ID to `InboundMessage` or pass alongside it

### Phase 2: Memory Schema

- Migration adding `user_id` to `memories`
- Update `Memory` struct and builder
- Update `MemoryStore` queries to accept optional `user_id` filter
- LanceDB schema update (rebuild embeddings table with `user_id` column)

### Phase 3: Search Pipeline

- `SearchConfig` gets `user_id`
- SQLite queries apply `(user_id = ? OR user_id IS NULL)` filter
- LanceDB vector search applies user_id filter predicate
- LanceDB FTS search applies user_id filter predicate
- Graph traversal respects user scope boundaries

### Phase 4: Tools + Context

- `MemorySaveArgs` and `MemoryRecallArgs` get `user_id`
- Channel passes canonical user_id to branches
- Branch prompt injection with current user context
- Compactor attribution fix (`render_messages_as_transcript`)
- `build_channel_context` user-aware memory injection
- Prompt template updates

### Phase 5: API

- `user_id` filter on existing memory endpoints
- `/api/agents/users` endpoint
- Wire `UserStore` into `ApiState`

## Migration Path

Fully backward compatible. All existing memories retain `user_id = NULL` (global) and appear in every search. The LanceDB embeddings table needs a one-time schema rebuild to add the `user_id` column — existing entries get NULL. No reindexing needed for SQLite since the new column is nullable with no default constraint.

The feature is opt-in from the LLM's perspective. If prompts aren't updated to mention `user_id`, the LLM never sets it and behavior is identical to today. This means the rollout can be incremental — ship the schema and plumbing first, then update prompts to activate it.

## What This Enables

**Community Discord servers:** An agent running in a 500-person server remembers each person individually. When Alice asks about her project, the agent recalls Alice's context without surfacing Bob's unrelated work.

**Enterprise teams:** A team Spacebot remembers each engineer's preferences, current projects, and decisions. Shared decisions are global. Individual context is scoped.

**Hosted platform (spacebot.sh):** When users have spacebot.sh accounts, a `platform: "spacebot"` link connects their canonical ID to their platform account. The dashboard can show per-user memory views, and team features can share agents across users with proper memory isolation.

**Dynamic user recall:** Agents can proactively recall a user's context when they join a conversation. "Oh, JamiePine — last time we talked you were debugging that auth migration. How'd that go?" No AI system does this at the memory layer today.
