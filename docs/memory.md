# Memory

Memories in SpaceBot are structured objects in a database. Not markdown files, not daily logs, not a manually curated MEMORY.md. Every memory is a row in SQLite with typed metadata and graph connections, paired with a vector embedding in LanceDB for search.

## Why Not Files

OpenClaw stores memories as markdown files on disk and indexes them into SQLite for search. This creates a whole category of problems: file watchers, hash-based change detection, stale indexes, async sync race conditions, and 43 source files just for the memory subsystem. SpaceBot uses embedded databases as the source of truth -- no file syncing, no server processes.

## Storage Split

Memories live across two embedded databases, each doing what it's best at:

- **SQLite** -- the memory graph. Rows with content, type, importance, timestamps, source. Association edges with weights and relation types. Relational queries for graph traversal, metadata filtering, and maintenance operations.
- **LanceDB** -- embeddings and search. Vector storage in Lance columnar format with HNSW indexing. Built-in full-text search via Tantivy. Hybrid search (vector + FTS) in one system.

The two are joined on memory ID. A recall worker queries LanceDB for semantic/keyword matches, then hits SQLite for graph traversal and metadata. No server processes -- both are embedded, everything is files in a data directory.

## Memory Structure

Every memory has:

- **Content** -- the actual information (a fact, a preference, a decision, an observation)
- **Type** -- what kind of memory this is (see below)
- **Embedding** -- vector representation for semantic search
- **Importance** -- a score that determines how likely it is to be surfaced
- **Timestamps** -- when it was created, when it was last accessed
- **Source** -- where this memory came from (which channel, which conversation, system-generated)
- **Associations** -- weighted edges to other memories in the graph

## Memory Types

Memories are typed. The type determines how the memory is stored, searched, and surfaced.

**Fact** -- Something that is true. "James's project SpaceDrive is a cross-platform file manager written in Rust." Facts can be updated or contradicted by newer facts.

**Preference** -- Something the user likes or dislikes. "James prefers Rust over TypeScript." Preferences influence behavior but don't change often.

**Decision** -- A choice that was made. "We decided to use PostgreSQL instead of SQLite for Spacebot." Decisions carry context about why they were made.

**Identity** -- Core information about who the user is or who the agent is. Always surfaced, never decayed. "James is a software engineer. My name is Spacebot."

**Event** -- Something that happened. "James deployed SpaceDrive v0.4 on Feb 10." Events are temporal and naturally decay in importance.

**Observation** -- Something the system noticed. "James tends to work late on Fridays." Observations are inferred, not stated.

## The Graph

Memories don't exist in isolation. They connect to each other through weighted associations.

When a new memory is created, the system searches for semantically similar memories and creates associations automatically. If a new fact is very similar to an existing one (>0.9 similarity), it's marked as an update -- the old memory's importance decays, and an `Updates` edge is created. If two memories contradict each other, a `Contradicts` edge is created.

Association types:

- **RelatedTo** -- general semantic connection
- **Updates** -- newer version of the same information
- **Contradicts** -- conflicting information
- **CausedBy / ResultOf** -- causal chain
- **PartOf** -- hierarchical relationship

The graph enables traversal during recall. When a recall worker finds a relevant memory, it can walk the graph to find connected context -- related facts, the history of how a decision evolved, contradictions that need resolution.

## How Memories Are Created

Three paths:

### 1. Branch-initiated (during conversation)

When a branch is processing a user message, it can save memories. This is the most common path. The branch has the conversation context, understands what the user said, and can decide what's worth remembering.

```
User: "Actually, let's switch to SQLite for the prototype. PostgreSQL is overkill for now."
    → Branch saves:
        Decision: "Switched to SQLite for the prototype phase"
        Association: Updates → previous decision to use PostgreSQL
```

Branches have a `memory_save` tool. They decide what to save, with what type and importance. The system handles embedding generation and auto-association.

### 2. Compactor-initiated (during compaction)

When the compactor triggers a compaction worker, that worker does two things in one pass: summarize the conversation and extract memories. This is where memories are harvested from conversation that's about to leave the context window.

This mirrors OpenClaw's memory flush pattern -- the best idea in OpenClaw's memory system. Before context is lost, an LLM pass pulls out anything worth keeping. But unlike OpenClaw, this happens in a background worker. The channel never blocks.

### 3. Cortex-initiated (system-level)

The cortex observes patterns across channels and can create memories at the system level. It consolidates related memories, creates observations ("James has been asking about authentication a lot this week"), and manages the graph.

## How Memories Are Recalled

Memory recall is always delegated to a worker. No LLM process ever queries the database directly and dumps raw results into its own context.

### Search Modes

The `memory_recall` tool supports four search modes, each suited to different retrieval needs:

**Hybrid** (default) -- Full pipeline: vector similarity (LanceDB HNSW) + full-text search (Tantivy) + graph traversal, merged via Reciprocal Rank Fusion (RRF). Requires a query string. Best when you have a specific topic to search for and conversation context to inform the query.

**Recent** -- Returns the most recent memories ordered by `created_at`. No query needed, no vector/FTS overhead. Pure SQLite. Best for temporal awareness -- "what just happened?"

**Important** -- Returns highest-importance memories ordered by importance score. No query needed. Best for surfacing critical context that should always be in mind.

**Typed** -- Filters by `MemoryType` (fact, preference, decision, identity, event, observation, goal, todo) with a configurable sort order. Requires `memory_type`. Best for structured retrieval -- "give me all decisions" or "show me identity memories."

Non-hybrid modes bypass the vector/FTS/RRF pipeline entirely and query SQLite directly. They're fast and don't require an embedding model or FTS index.

All modes support an optional `memory_type` filter. In hybrid mode, results are post-filtered after RRF fusion. In non-hybrid modes, the filter is applied at the SQL level.

Sort options for non-hybrid modes: `recent` (created_at DESC), `importance` (importance DESC), `most_accessed` (access_count DESC).

### The Recall Flow

```
Channel receives: "What do we know about the auth system?"
    → Branch created with channel's context
        → Branch calls memory_recall tool (mode: hybrid, query: "auth system")
            Tool searches: vector similarity + full-text + graph traversal
            Tool gets 50 results
            Branch curates: filters noise, ranks by relevance
            Branch selects 5 clean memories
        → Branch returns conclusion to channel
    → Channel responds with clean, relevant information
```

The branch is the recall intermediary. It has the channel's full context (so it knows what's relevant), and it picks the appropriate search mode. Hybrid for topic-specific queries, recent for temporal context, typed for structured retrieval.

RRF works on ranks rather than scores, which handles the different scales of vector and keyword results better than a weighted sum. After finding initial results, the branch can walk the memory graph in SQLite to pull in connected context. If the top result is "we decided to use JWT for auth tokens", the graph might surface "we considered session cookies but rejected them because of the mobile app" through a `ResultOf` edge.

The branch curates. 50 raw results become 5 relevant, contextualized memories. The channel never sees the noise -- it only gets the branch's conclusion.

### Why Not Search Directly?

In OpenClaw, the LLM calls `memory_search`, gets raw results in its context, and has to make sense of them. This pollutes the context with irrelevant matches, partial chunks, and search metadata. In Spacebot, the branch absorbs all that noise and returns only what matters. The branch is disposable -- its context gets thrown away after it returns. The channel stays clean.

This costs an extra LLM call. But it keeps the channel's context clean, which means fewer compactions, which means fewer memory extractions, which means the system runs more efficiently overall.

## Importance and Decay

Every memory has an importance score between 0 and 1. This score determines how likely a memory is to be surfaced during recall and how long it survives before pruning.

Importance is influenced by:

- **Explicit importance** -- set at creation time (identity memories start high, casual observations start low)
- **Access frequency** -- memories that get recalled often are more important
- **Recency** -- recent memories score higher; old memories decay
- **Graph centrality** -- memories with many strong connections to other memories are more important

A background maintenance process runs periodically to decay old memories, prune memories that have fallen below a threshold, merge near-duplicates, and recompute graph centrality scores.

Identity and permanent-tagged memories are exempt from decay and pruning. They always survive.

The specific decay rates, scoring weights, and thresholds are implementation details that will be tuned with real data. The mechanisms matter; the numbers don't yet.

## Identity Files

Not everything is a graph memory. Some context is stable, foundational, and user-editable:

- **SOUL.md** -- core values, personality, tone
- **IDENTITY.md** -- agent name, nature
- **USER.md** -- user context

These are loaded into channel system prompts every time. They're files on disk, not database rows, because they change rarely and users should be able to edit them in a text editor.

What's gone:

- **MEMORY.md** -- replaced by dynamic memory selection from the graph
- **daily/YYYY-MM-DD.md** -- replaced by typed memories with timestamps
- **HEARTBEAT.md** -- replaced by database-stored heartbeat definitions

## Context Injection

When a channel starts or a heartbeat fires, memories are injected into the system prompt. This isn't a raw dump -- it's a curated selection:

1. Identity-tagged memories are always included
2. High-importance memories above a threshold are included
3. Recently accessed memories get a boost
4. The rest depends on what the conversation is about (populated by recall workers during the session)

The injected memories are grouped by type and formatted cleanly. The channel sees something like:

```
## Identity
- My name is Spacebot
- James is a software engineer who works on SpaceDrive

## Recent Context
- We decided to use SQLite for the prototype (2 days ago)
- James prefers short, direct responses (preference)
```

Not a wall of raw search results. Not everything in the database. Just what matters right now.

## Maintenance

A periodic background process handles graph hygiene:

- **Decay** -- reduce importance of old, unaccessed memories
- **Prune** -- delete memories below an importance floor (identity/permanent exempt)
- **Merge** -- combine near-duplicate memories (>0.95 similarity)
- **Reindex** -- recompute graph centrality scores

This is a heartbeat-style job managed by the cortex. It runs in workers, doesn't block anything, and keeps the graph healthy over time.
