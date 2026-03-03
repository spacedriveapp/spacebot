# Cortex Topic Synthesis

Replace the single-bulletin broadcast with targeted, living context documents. Each topic is an addressable unit of knowledge — synthesized from a memory subset, kept fresh by the cortex, and injectable into any process by ID.

## Why

The bulletin is a global broadcast. Every channel and every worker spawned from a channel gets the same ~1500 word context block regardless of what they're actually working on. A Discord moderation channel gets the same context as a coding channel. A worker spun up to refactor the auth module gets the same context as one checking the weather.

Topics fix this by making context **addressable** and **targeted**:

- A channel gets topics assigned to it — only the context it needs.
- A worker gets spawned with `topic_ids` — instant deep context on the specific project it's about to work on.
- The cortex maintains each topic independently — different refresh rates, different memory criteria, different scope.
- Humans can curate topics directly — pin specific memories, edit the synthesis, adjust criteria.

The bulletin becomes just another topic (a broad "General Context" topic) rather than a special system.

## What Exists Today

**The bulletin system** (`src/agent/cortex.rs`):
- `spawn_bulletin_loop()` runs on a timer (default 1 hour)
- `gather_bulletin_sections()` queries 8 hardcoded memory categories (Identity, Recent, Decisions, Important, Preferences, Goals, Events, Observations) plus active tasks
- `generate_bulletin()` feeds the raw sections through an LLM to synthesize a ~1500 word briefing
- Result is stored in `RuntimeConfig::memory_bulletin` as `ArcSwap<String>`
- Every channel reads the same bulletin via `rc.memory_bulletin.load()` in `build_system_prompt()`
- Workers receive no bulletin context — they only get the task description

**The `channels.bulletin` column** exists in the schema but is never written to. It was an early hook for per-channel bulletins.

**The signal buffer** in `Cortex` already collects `MemorySaved` signals from the event bus, which can drive topic staleness detection.

## Data Model

### `topics` table

```sql
CREATE TABLE IF NOT EXISTS topics (
    id              TEXT PRIMARY KEY,
    agent_id        TEXT NOT NULL,
    title           TEXT NOT NULL,
    content         TEXT NOT NULL DEFAULT '',
    criteria        TEXT NOT NULL,             -- JSON: TopicCriteria
    pin_ids         TEXT NOT NULL DEFAULT '[]', -- JSON: array of memory IDs always included
    channel_ids     TEXT NOT NULL DEFAULT '[]', -- JSON: array of channel_ids this routes to
    status          TEXT NOT NULL DEFAULT 'active',
    max_words       INTEGER NOT NULL DEFAULT 1500,
    last_memory_at  TEXT,                      -- newest memory timestamp used in last sync
    last_synced_at  TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_topics_agent ON topics(agent_id);
CREATE INDEX IF NOT EXISTS idx_topics_status ON topics(status);
```

### `topic_versions` table

```sql
CREATE TABLE IF NOT EXISTS topic_versions (
    id              TEXT PRIMARY KEY,
    topic_id        TEXT NOT NULL,
    content         TEXT NOT NULL,
    memory_count    INTEGER NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (topic_id) REFERENCES topics(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_topic_versions_topic ON topic_versions(topic_id);
```

### Topic status

```
active   — cortex syncs this topic on schedule
paused   — exists but skipped during sync
archived — hidden from UI, never synced, kept for history
```

### `TopicCriteria` (JSON in `criteria` column)

```rust
struct TopicCriteria {
    /// Semantic query for hybrid search (optional — omit for pure metadata queries)
    query: Option<String>,
    /// Filter to specific memory types (empty = all types)
    memory_types: Vec<MemoryType>,
    /// Minimum importance threshold
    min_importance: Option<f32>,
    /// Only memories from these channels (empty = all channels)
    channel_ids: Vec<ChannelId>,
    /// Only memories newer than this duration ago (e.g. "30d", "7d")
    max_age: Option<String>,
    /// Search mode override (defaults to Hybrid if query present, Recent otherwise)
    mode: Option<SearchMode>,
    /// Sort order for non-hybrid modes
    sort_by: Option<SearchSort>,
    /// Max memories to gather before synthesis (default 30)
    max_memories: usize,
}
```

This maps directly to the existing `SearchConfig` plus a few SQL-level filters (`channel_ids`, `max_age`) that need minor additions to `MemoryStore::get_sorted()`. No new query language — just structured parameters over existing search infrastructure.

## Rust Types

```
src/topics/mod.rs         — Topic, TopicStatus, TopicCriteria, TopicVersion types + re-exports
src/topics/store.rs       — TopicStore CRUD (SQLite)
```

### TopicStore API

```rust
impl TopicStore {
    pub async fn create(&self, topic: &Topic) -> Result<()>;
    pub async fn get(&self, id: &str) -> Result<Option<Topic>>;
    pub async fn list(&self, agent_id: &str) -> Result<Vec<Topic>>;
    pub async fn list_active(&self, agent_id: &str) -> Result<Vec<Topic>>;
    pub async fn update(&self, topic: &Topic) -> Result<()>;
    pub async fn delete(&self, id: &str) -> Result<()>;
    pub async fn get_for_channel(&self, agent_id: &str, channel_id: &str) -> Result<Vec<Topic>>;
    pub async fn save_version(&self, version: &TopicVersion) -> Result<()>;
    pub async fn get_versions(&self, topic_id: &str, limit: i64) -> Result<Vec<TopicVersion>>;
}
```

`TopicStore` goes into `AgentDeps` alongside `MemorySearch`, `TaskStore`, etc.

## Synthesis

### Topic sync loop

New cortex loop: `spawn_topic_sync_loop()`. Runs alongside the existing bulletin and association loops.

```
spawn_topic_sync_loop(deps, logger)
  │
  └─ Startup: sync all active topics (same as bulletin does initial generation)
  │
  └─ Every topic_sync_interval_secs (default 600):
      │
      ├─ Load all active topics for this agent
      │
      └─ For each topic:
          ├─ Staleness check:
          │   Query newest memory matching criteria
          │   Stale if newest_memory.created_at > topic.last_memory_at
          │   Stale if topic.last_synced_at is None (never synced)
          │
          ├─ If not stale → skip
          │
          └─ If stale → synthesize:
              ├─ gather_topic_memories(criteria, pin_ids)
              │   Uses existing MemorySearch with SearchConfig built from criteria
              │   Loads pinned memories by ID, prepends to results
              │   Deduplicates
              │
              ├─ Format as raw sections (markdown, same style as bulletin)
              │
              ├─ LLM synthesis via topic_synthesis.md.j2
              │   System: "Synthesize a topic document about: {title}"
              │   User: "Synthesize into {max_words} words:\n{raw_sections}"
              │
              ├─ Update topic.content, last_synced_at, last_memory_at
              │
              ├─ Save TopicVersion snapshot
              │
              └─ Log cortex_event: topic_synced
```

The LLM call per topic is the same weight as the bulletin call — one synthesis pass, no tools, branch-tier model. With 5-10 active topics on a 10-minute interval, that's 5-10 cheap LLM calls every 10 minutes, but only for stale topics. In practice most ticks sync 0-2 topics.

### Prompt template

New file: `prompts/en/cortex_topic_synthesis.md.j2`

System prompt tells the LLM to synthesize a focused context document about the topic title. Same constraints as the bulletin synthesis (no raw IDs, no section headers from input, prioritize recent and high-importance, keep it scannable) but scoped to the topic.

The synthesis fragment can reuse `fragments/system/cortex_synthesis.md.j2` since it's the same structure — max_words + raw_sections.

## Delivery

### Workers (the primary use case)

Workers spawned with topic IDs get the synthesized content injected above their task prompt.

**Changes to `SpawnWorkerArgs`:**
```rust
pub struct SpawnWorkerArgs {
    pub task: String,
    pub interactive: bool,
    pub suggested_skills: Vec<String>,
    pub worker_type: Option<String>,
    pub directory: Option<String>,
    pub topic_ids: Vec<String>,         // new
}
```

**In `spawn_worker_from_state()`:**

After building the worker task with temporal context, before creating the Worker:

```rust
if !topic_ids.is_empty() {
    let mut topic_context = String::from("## Topic Context\n\n");
    for id in &topic_ids {
        if let Ok(Some(topic)) = topic_store.get(id).await {
            topic_context.push_str(&format!("### {}\n\n{}\n\n", topic.title, topic.content));
        }
    }
    worker_task = format!("{topic_context}---\n\n{worker_task}");
}
```

Topic content goes into the task message (the first user message), not the system prompt. The worker template already says "Your task is provided in the first message. It contains everything you need to know." Topics are just more of "everything you need to know."

The same injection applies to cortex-spawned task workers in `pickup_one_ready_task()`. Task metadata can carry `topic_ids`:

```json
{
    "topic_ids": ["uuid-1", "uuid-2"],
    "worker_type": "opencode",
    "directory": "/path/to/project"
}
```

### Channels

Channels with topic assignments receive composed topic content instead of (or alongside) the global bulletin.

**In `build_system_prompt()`:**

```rust
let channel_topics = topic_store.get_for_channel(&agent_id, &channel_id).await?;
let memory_context = if channel_topics.is_empty() {
    // Fall back to global bulletin
    rc.memory_bulletin.load().to_string()
} else {
    channel_topics.iter()
        .map(|t| format!("### {}\n\n{}", t.title, t.content))
        .collect::<Vec<_>>()
        .join("\n\n")
};
```

The template slot stays the same — `{{ memory_bulletin }}` in `channel.md.j2` just receives different content. No template changes needed.

Channels with no topic assignments continue to receive the global bulletin. The bulletin loop doesn't need to change or stop — it serves as the default.

### Cortex chat

Cortex chat currently receives the global bulletin. It should receive all active topics composed together, since the cortex chat is the system-wide reasoning interface.

## Topic-Aware Spawning (LLM Side)

The channel's `spawn_worker` tool description gets updated to include `topic_ids` as an optional parameter. The channel prompt should mention available topics so the LLM can reference them when spawning workers.

**In the status block or a new section:**
```
## Available Topics
- "Platform Refactor" (id: abc123) — last synced 5m ago
- "Auth System" (id: def456) — last synced 12m ago
```

This lets the channel LLM make informed decisions about which topics to attach when delegating work.

## API Endpoints

```
GET    /api/agents/topics              — list all topics for agent
POST   /api/agents/topics              — create topic
GET    /api/agents/topics/:id          — get topic with current content
PUT    /api/agents/topics/:id          — update topic (title, criteria, channels, status, etc.)
DELETE /api/agents/topics/:id          — delete topic
POST   /api/agents/topics/:id/sync     — trigger manual re-sync
GET    /api/agents/topics/:id/versions — get version history
```

Handler file: `src/api/topics.rs`. Follows the same patterns as `src/api/tasks.rs` and `src/api/cron.rs`.

## Interface

New tab: **Topics** (between Memories and Workers in the tab order).

### Topic browser

Card grid showing all topics. Each card displays:
- Title
- Status badge (active/paused/archived)
- Last synced timestamp (relative, e.g. "5m ago")
- Channel count (how many channels assigned)
- Word count of current content
- Memory count from last sync

### Topic detail / editor

Two-column layout:

**Left — Content:**
- Rendered markdown of the current synthesis
- Word count
- "Last synced" timestamp with manual sync button

**Right — Configuration:**
- Title (editable)
- Status toggle (active/paused/archived)
- Criteria editor — form fields mapping to `TopicCriteria`:
  - Semantic query (text input)
  - Memory types (multi-select pills)
  - Min importance (slider, 0.0-1.0)
  - Channel filter (multi-select from known channels)
  - Max age (dropdown: 7d, 30d, 90d, all)
  - Max memories (number input)
- Channel assignments (checkbox list of all channels)
- Pinned memories (search + add interface — search memories, click to pin)

### Version history / diff

Below the content panel: a version timeline. Click any two versions to see a diff. Standard inline diff rendering (insertions green, deletions red). Versions are timestamped and show memory count.

### Topic creation

"New Topic" button opens a form with title, criteria, and channel assignments. Content starts empty — first sync populates it.

## Config

```rust
pub struct CortexConfig {
    // ... existing fields ...

    /// Interval between topic sync passes (default: 600s = 10 min)
    pub topic_sync_interval_secs: u64,
}
```

Per-topic `max_words` is stored on the topic itself (default 1500), not in the global config.

## Bulletin Deprecation Path

The bulletin doesn't need to die immediately.

1. **Coexistence.** Topics exist alongside the bulletin. Channels with topic assignments use topics. Channels without use the bulletin. Workers optionally receive topics.

2. **Bulletin becomes a topic.** Once topics are working, create a "General Context" topic with broad criteria (the same 8 categories the bulletin currently uses). Assign it to channels that don't need specific topics. The bulletin loop continues to run in parallel for backward compatibility.

3. **Bulletin loop removed.** Once all channels have topic assignments and the "General Context" topic is proven stable, the bulletin loop can be removed. `RuntimeConfig::memory_bulletin` stays as a read path that loads the "General Context" topic content.

No rush on steps 2 and 3. The bulletin works fine as a default.

## Phases

### Phase 1: Data Layer

- Migration for `topics` and `topic_versions` tables
- `Topic`, `TopicVersion`, `TopicStatus`, `TopicCriteria` types
- `TopicStore` with full CRUD + version history
- Add `TopicStore` to `AgentDeps`
- Add `channel_ids` and `max_age` filter support to `MemoryStore::get_sorted()` (minor SQL additions)

### Phase 2: Synthesis + Sync Loop

- `gather_topic_memories()` — build `SearchConfig` from `TopicCriteria`, query, merge pinned, dedup
- `synthesize_topic()` — format raw sections, LLM synthesis, save content + version
- `spawn_topic_sync_loop()` — startup sync + periodic staleness check
- `cortex_topic_synthesis.md.j2` prompt template
- Cortex event logging for `topic_synced` / `topic_sync_failed`

### Phase 3: Delivery — Workers

- Add `topic_ids` to `SpawnWorkerArgs` and the `spawn_worker` tool JSON schema
- Inject topic content in `spawn_worker_from_state()`
- Inject topic content in `pickup_one_ready_task()` (read from task metadata)
- Update `spawn_worker` tool description to mention topic IDs

### Phase 4: Delivery — Channels

- `get_for_channel()` query in topic store
- Modify `build_system_prompt()` to compose channel topics, fall back to bulletin
- Inject available topics list into status block or a new prompt section
- Update cortex chat to receive all active topics

### Phase 5: API

- `src/api/topics.rs` handlers (list, create, get, update, delete, sync, versions)
- Register routes in `src/api/server.rs`
- TypeScript client types and methods in `interface/src/api/client.ts`

### Phase 6: Interface

- Topic browser page (`AgentTopics.tsx`)
- Topic detail/editor with criteria form and channel assignments
- Pin memory search UI
- Version history with diff view
- Tab in `AgentTabs.tsx`
- Route in `router.tsx`

### Phase 7: Signal-Driven Refresh

- On `MemorySaved` signal in cortex, evaluate which active topics match the new memory
- Mark matching topics as stale (bump priority in next sync pass)
- Optionally trigger immediate sync for topics whose channels are currently active

## Phase Ordering

```
Phase 1 (data)          — standalone
Phase 2 (synthesis)     — depends on Phase 1
Phase 3 (workers)       — depends on Phase 1 (reads from store), independent of Phase 2 (content may be empty)
Phase 4 (channels)      — depends on Phase 1, benefits from Phase 2
Phase 5 (API)           — depends on Phase 1
Phase 6 (interface)     — depends on Phase 5
Phase 7 (signals)       — depends on Phase 2
```

Phases 2, 3, 4, and 5 can run in parallel after Phase 1. Phase 6 needs Phase 5. Phase 7 needs Phase 2.

## Open Questions

**Topic creation by cortex.** Should the cortex be able to create topics automatically? For example, when it detects a recurring cluster of related memories across channels, it could propose a topic with pre-filled criteria. This mirrors how cortex promotes todos into tasks — it could promote memory patterns into topics. Probably a Phase 7+ feature, after the manual creation path is proven.

**Topic size budget.** Multiple topics assigned to a channel all consume context window space. If a channel has 5 topics at 1500 words each, that's 7500 words of topic context — potentially crowding out conversation history. Options: enforce a per-channel total word budget, let the channel LLM request specific topics instead of receiving all assigned ones, or trust the operator to assign topics sensibly. Starting with trust and adding a budget later if it becomes a problem.

**Topic sharing across agents.** In multi-agent setups, should topics be shareable? A "Codebase Architecture" topic might be useful to multiple agents. The current design scopes topics to `agent_id`. Cross-agent sharing could be added later by introducing a `shared` flag or a separate `org_topics` table.

**Memory pinning UX.** The pin_ids field lets humans force specific memories into a topic regardless of criteria match. The UI needs a way to search memories and add them as pins. This should use the existing memory search API (`/api/agents/memories/search`) with a "pin to topic" action.
