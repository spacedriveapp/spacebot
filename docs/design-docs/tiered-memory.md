# Tiered Memory: Search & Lifecycle Improvement

> Revised 2026-03-18. Original scope included context injection of working-state memories into the system prompt. That responsibility is now handled by the [working memory system](working-memory.md), which provides temporal situational awareness through an event log and intra-day synthesis. This doc is scoped to **search quality and memory lifecycle** only.

Memories today are a single pool. Every memory — whether it was created 30 seconds ago or 30 days ago — lives in the same SQLite table and LanceDB index, searched with the same priority, decayed with the same formula. A memory saved 2 minutes ago about the task you're working on right now has no retrieval advantage over a stale observation from last month.

The fix: two tiers with distinct lifecycles and retrieval semantics. Recent memories get a search boost and skip decay. Old memories follow the existing decay/prune/merge pipeline.

## Relationship to Working Memory

The [working memory system](working-memory.md) and tiered memory solve different problems:

| Concern | Working Memory | Tiered Memory |
|---------|---------------|---------------|
| **What it improves** | Situational awareness (what happened today) | Search quality (what matters when recalling) |
| **Data structure** | Append-only event log + synthesis | Tier column on existing `memories` table |
| **Injection point** | Layers 2-5 of the system prompt | `memory_recall` search pipeline |
| **Update mechanism** | Processes emit events automatically | Branch calls `memory_save` / `memory_promote` |
| **Who sees it** | Channel (via context assembly) | Branch (via recall results) |

They are complementary. Working memory gives channels "what's happening now." Tiered memory gives branches "find what matters first."

---

## The Two Tiers

### Working State (hot, 3-day window)

Recently created or recently accessed memories. These get priority in search and are exempt from decay.

**Lifecycle:**
- New memories start in working state (unless explicitly saved as `tier: "graph"`)
- 3-day TTL from last access (not creation — accessing a memory resets its clock)
- Bounded size: configurable max (default 64 memories per agent)
- When the cap is hit, least-recently-accessed memories are demoted early
- Identity memories never enter working state — they are already permanent

**Retrieval:**
- Searched first in `memory_recall`, before graph tier
- 1.5x score boost in merged ranking (configurable)
- No decay while in working state

### Graph (warm, 30-day retention with decay)

The current system. Long-term associative memory with typed relationships, hybrid search, and gradual decay. Memories demoted from working state land here.

**Lifecycle:**
- Memories arrive via demotion from working state (TTL expired or LRU evicted)
- Existing decay mechanics apply (importance * age_decay * access_boost)
- Pruned after 30 days if importance drops below threshold
- Graph associations drive retrieval — related memories surface together
- Identity memories remain exempt from decay (unchanged)

**Retrieval:**
- Searched when working state doesn't satisfy the query
- Hybrid search (vector + FTS + RRF + graph traversal) — unchanged
- Results are curated by branches before reaching channels — unchanged

## What Exists Today

The `Memory` struct has no tier concept:

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

All 8 memory types share the same storage, search, and decay behavior. The only differentiation is that Identity memories skip decay. Maintenance runs hourly: decay all non-Identity memories (linear, 0.05/day), prune below 0.1 importance after 30 days, merge above 0.95 similarity. No TTL. No tiered retrieval.

## Schema Changes

### SQLite Migration

```sql
ALTER TABLE memories ADD COLUMN tier TEXT NOT NULL DEFAULT 'graph';
ALTER TABLE memories ADD COLUMN demoted_at TIMESTAMP;
CREATE INDEX idx_memories_tier ON memories(tier, forgotten);
CREATE INDEX idx_memories_tier_access ON memories(tier, last_accessed_at);
```

`tier` is `'working'` or `'graph'`. Existing memories default to `'graph'` — they've already survived past whatever working state window would have applied. `demoted_at` records when a memory transitioned from working to graph, used for the 30-day graph retention clock.

### Memory Struct

```rust
pub struct Memory {
    // ... existing fields
    pub tier: MemoryTier,
    pub demoted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum MemoryTier {
    Working,
    Graph,
}
```

### LanceDB

No schema change needed. Working state memories are a small set queried from SQLite directly — they don't need vector search to be found. When a working state memory is demoted to graph tier, its embedding already exists in LanceDB. When searching, the tier filter is applied at the SQLite level after RRF fusion.

## Promotion and Demotion

### On Creation (Promotion to Working State)

When `memory_save` creates a new memory:

1. If `memory_type == Identity` → goes directly to graph tier (Identity is already permanent)
2. Otherwise → enters working state with `tier = 'working'`, `last_accessed_at = now`
3. If working state is at capacity (64 memories) → demote the least-recently-accessed working memory to graph before inserting

The LLM can override this with an explicit `tier: "graph"` parameter on `memory_save` for memories it knows are long-term reference material (e.g., "Jamie's email is X" is a Fact that should go straight to graph).

### On Access (TTL Reset)

When `memory_recall` returns a working state memory, its `last_accessed_at` is updated (this already happens via `record_access`). This resets the 3-day demotion clock. Memories that are actively being used stay hot.

When `memory_recall` returns a graph tier memory, it can be **re-promoted** to working state if the agent is actively working with it. The branch can call `memory_promote` to explicitly move a graph memory back to working state.

### On Expiry (Demotion to Graph)

The cortex maintenance loop checks working state memories on each tick:

```rust
async fn demote_expired_working_memories(&self) -> Result<usize> {
    let cutoff = Utc::now() - Duration::days(self.config.working_state_ttl_days);
    let expired = self.store.get_working_memories_before(cutoff).await?;

    for memory in &expired {
        self.store.demote_to_graph(memory.id.clone()).await?;
    }

    Ok(expired.len())
}
```

`demote_to_graph` sets `tier = 'graph'`, `demoted_at = now`. The memory retains all its metadata, associations, and embedding. It just moves from the hot path to the searchable archive.

### On Capacity (LRU Eviction)

When working state hits the configured cap:

```rust
async fn ensure_working_capacity(&self, max: usize) -> Result<usize> {
    let count = self.store.count_working_memories().await?;
    if count < max {
        return Ok(0);
    }

    let excess = count - max + 1; // make room for the new one
    let to_demote = self.store
        .get_working_memories_lru(excess as i64)
        .await?;

    for memory in &to_demote {
        self.store.demote_to_graph(memory.id.clone()).await?;
    }

    Ok(to_demote.len())
}
```

LRU is based on `last_accessed_at`. The oldest-accessed working memories get demoted first.

## Retrieval Changes

### Search Priority

When `memory_recall` runs a hybrid search:

1. Search working state memories first (simple SQLite query + text matching, no vector search needed for a 64-item set)
2. Search graph tier via existing hybrid pipeline (vector + FTS + RRF + graph traversal)
3. Working state results get a retrieval boost in the merged ranking (configurable, default 1.5x score multiplier)
4. Deduplicate (a memory can't appear from both tiers)
5. Return merged results

This means working state memories naturally rank higher without requiring the LLM to filter them. The agent sees its recent context first.

## Maintenance Changes

### Working State Maintenance (Every Tick)

Added to the cortex tick loop (runs on `tick_interval_secs`, default 60s):

1. **Demote expired:** Find working memories where `last_accessed_at < now - working_state_ttl_days`. Set `tier = 'graph'`, `demoted_at = now`.
2. **Enforce capacity:** If working state count > max, demote LRU excess.

No decay is applied to working state memories. Their importance stays fixed while they're hot.

### Graph Maintenance (Every Maintenance Interval)

Existing maintenance runs unchanged, but only on graph tier memories:

1. **Decay** — Apply to `tier = 'graph'` memories only (working state memories skip decay)
2. **Prune** — Apply to `tier = 'graph'` memories where `importance < threshold` AND either:
   - `created_at < now - 30 days` (for memories that were never in working state), OR
   - `demoted_at < now - 30 days` (for memories that graduated from working state)
3. **Merge** — Apply to `tier = 'graph'` memories only. Working state memories are too fresh to merge — they may still be evolving.

The 30-day retention clock starts from `demoted_at` for memories that passed through working state, and from `created_at` for memories that went directly to graph (Identity memories, explicit `tier: "graph"` saves).

## Tool Changes

### memory_save

```rust
pub struct MemorySaveArgs {
    pub content: String,
    pub memory_type: String,
    pub importance: Option<f32>,
    pub source: Option<String>,
    pub channel_id: Option<String>,
    pub associations: Vec<AssociationInput>,
    pub tier: Option<String>,           // ← new: "working" (default) or "graph"
}
```

Default behavior: new memories enter working state. The LLM can explicitly set `tier: "graph"` for reference material that should skip the hot path.

### memory_recall

No schema change needed. The recall tool already returns all non-forgotten memories. The tiered retrieval (working state first, then graph) is handled in the search layer, transparent to the tool interface.

Add a `tier` filter option for explicit queries:

```rust
pub struct MemoryRecallArgs {
    // ... existing fields
    pub tier: Option<String>,           // ← new: filter to "working" or "graph"
}
```

### memory_promote (new tool, branch only)

Re-promotes a graph memory to working state:

```rust
pub struct MemoryPromoteArgs {
    pub memory_id: String,
}
```

Sets `tier = 'working'`, `last_accessed_at = now`, `demoted_at = None`. Enforces working state capacity (demotes LRU if needed). Returns confirmation with the promoted memory content.

Use case: a branch recalls a graph memory that's relevant to the current task and wants to keep it in the hot path. "I found this decision from last week — promoting it to working state so I don't lose track of it."

## Configuration

New fields on `CortexConfig`:

```rust
pub struct CortexConfig {
    // ... existing fields

    /// Maximum number of memories in working state (default: 64)
    pub working_state_max: usize,

    /// Days before a working state memory is demoted to graph (default: 3)
    pub working_state_ttl_days: i64,

    /// Score multiplier for working state memories in search results (default: 1.5)
    pub working_state_search_boost: f32,

    /// Days in graph tier before prune-eligible (default: 30)
    /// Replaces maintenance_min_age_days for demoted memories
    pub graph_retention_days: i64,
}
```

All hot-reloadable via `ArcSwap` (follows existing config pattern).

## Files Changed

| File | Change |
|------|--------|
| New migration SQL | `tier` column, `demoted_at` column, indexes |
| `src/memory/types.rs` | `MemoryTier` enum, `tier` + `demoted_at` on `Memory` |
| `src/memory/store.rs` | Tier-filtered queries, `demote_to_graph()`, `promote_to_working()`, `count_working_memories()`, `get_working_memories_lru()`, `get_working_memories_before()` |
| `src/memory/maintenance.rs` | Scope decay/prune/merge to graph tier, add working state demotion |
| `src/memory/search.rs` | Tiered retrieval (working first, then graph), score boost |
| `src/tools/memory_save.rs` | `tier` parameter, default to working state |
| `src/tools/memory_recall.rs` | `tier` filter option |
| `src/tools/memory_promote.rs` (new) | Re-promote graph memory to working state |
| `src/agent/cortex.rs` | Working state demotion in tick loop |
| `src/config/types.rs` | `working_state_*` and `graph_retention_days` config fields |
| `prompts/en/branch.md.j2` | `memory_promote` documentation |
| `prompts/en/tools/memory_save_description.md.j2` | Document `tier` parameter |
| `prompts/en/tools/memory_promote_description.md.j2` (new) | Promote tool description |

## Phases

### Phase 1: Schema + Storage

- Migration adding `tier` and `demoted_at` columns
- `MemoryTier` enum and updated `Memory` struct
- `MemoryStore` methods for tier transitions and working state queries
- All existing memories default to `tier = 'graph'`

### Phase 2: Maintenance Integration

- Scope existing decay/prune/merge to `tier = 'graph'`
- Add working state demotion to cortex tick loop (TTL expiry + LRU eviction)
- Config fields for working state tuning

### Phase 3: Retrieval

- Search pipeline queries working state first, applies score boost
- `memory_recall` gets `tier` filter option

### Phase 4: Tools + Prompts

- `memory_save` gets `tier` parameter (default: working)
- `memory_promote` tool for re-promoting graph memories
- Prompt updates explaining tier semantics to the LLM

## Migration Path

Fully backward compatible. All existing memories get `tier = 'graph'` (they've already aged past any working state window). New memories created after the migration enter working state by default. No reindexing needed — LanceDB schema is unchanged.

The feature activates immediately on deploy. The LLM doesn't need prompt changes to benefit — default behavior (new memories → working state → auto-demote after 3 days) works without any LLM awareness. Prompt updates in Phase 4 let the LLM make smarter tier decisions, but the system works without them.

## What This Enables

**Better recall quality:** A memory saved 2 minutes ago about the auth migration ranks higher than a stale observation from last month. The branch doesn't have to wade through irrelevant results to find what matters.

**Natural forgetting:** Memories that aren't accessed for 3 days fade from the hot search path into the standard archive. After 30 more days in the graph without access, they're pruned. This mirrors how human working memory operates.

**Active context management:** The LLM can promote and demote memories explicitly. "This old decision is relevant again" → promote to working state. "This is reference material, not active work" → save directly to graph.

**Bounded hot path:** Working state has a hard cap (64 memories). The search boost only applies to a small, bounded set of recent memories. This prevents recall results from being dominated by volume.

## Resolved Questions

> Previously open questions, now answered by the [working memory design](working-memory.md):

**Should the working state summary replace part of the bulletin?** — The bulletin is replaced entirely by the [layered context assembly](working-memory.md#the-five-layers). Tiered memory no longer injects into the system prompt. It only affects `memory_recall` search results.

**Should working state be per-channel or per-agent?** — Per-agent. The working memory log (separate system) handles per-channel temporal awareness. Tiered memory operates on the shared graph — a memory's tier applies across all channels.

**What about the compactor?** — The compactor [no longer creates memories](working-memory.md#sunset-compaction-based-memory-extraction). This question is moot.

**Should auto-promotion exist?** — No, for now. Explicit promotion via `memory_promote` is sufficient. Auto-promotion risks filling working state with tangentially related memories from aggressive recall queries. Can be revisited after the system is running and we have data on promotion patterns.
