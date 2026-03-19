# Working Memory

Replace the monolithic bulletin with a layered context assembly system. Each layer has its own data source, update trigger, and rendering strategy. The agent gets structured situational awareness -- what happened today, what is happening in other channels, who it is talking to -- without a single LLM-synthesized blob that tries to be everything for everyone.

This design supersedes the bulletin system. It complements (does not replace) the tiered memory design (`tiered-memory.md`), the participant awareness design (`participant-awareness.md`), and the user-scoped memories design (`user-scoped-memories.md`). Where those designs overlap with this one, the overlapping sections are noted and reconciled.

Prerequisite reading: `working-memory-problem-analysis.md`.

## The Five Layers

The system prompt is assembled from five independently managed layers. Each layer has a different volatility, a different update trigger, and a different rendering strategy. No layer depends on another for its content -- they are composed at prompt assembly time.

```
System Prompt Assembly
  1. Identity Context          [stable]        rendered on file change
  2. Working Memory Log        [volatile]      rendered on every turn from DB
  3. Channel Activity Map      [volatile]      rendered on every turn from DB
  4. Participant Context       [semi-volatile]  rendered on every turn from cached summaries
  5. Knowledge Synthesis       [semi-stable]   regenerated on dirty flag
  --- existing sections follow ---
  6. Channel instructions, delegation, tools, rules (unchanged)
  7. Status block (unchanged)
  8. Conversation history (unchanged)
```

Layers 1-5 replace `## Memory Context` (the bulletin) in the current `channel.md.j2` template. Everything after layer 5 is unchanged.

---

## Layer 1: Identity Context

**What it is:** The agent's stable identity -- personality, role, authority, product knowledge. What exists today as the Soul, Identity, and Role files loaded from disk.

**What changes:** Nothing structural. These files are already loaded at startup and hot-reloaded on change. The only change is that the bulletin no longer re-synthesizes information from these files. Identity facts that the bulletin used to repeat (product descriptions, star counts, role statements) are no longer duplicated in the synthesized output.

**Update trigger:** File change on disk (existing `notify` watcher).

**Rendering:** Programmatic. The raw markdown is injected directly. No LLM.

**Token budget:** Whatever the user wrote. Not our concern to optimize -- these are the user's own words.

---

## Layer 2: Working Memory Log

The core of this design. An append-only, structured event log scoped by day. Every significant thing that happens across the agent is recorded as a timestamped event. The channel gets a progressively compressed view: today in detail, yesterday as a summary, the week as a paragraph.

### Event Types

Events are structured records, not free-text memories. Each event has a type, a timestamp, a source channel, and a one-line description.

```rust
pub struct WorkingMemoryEvent {
    pub id: String,                          // UUID
    pub event_type: WorkingMemoryEventType,
    pub timestamp: DateTime<Utc>,
    pub channel_id: Option<ChannelId>,       // which channel this happened in
    pub user_id: Option<String>,             // canonical user ID if user-initiated
    pub summary: String,                     // one-line human-readable description
    pub detail: Option<String>,              // optional longer context (worker result, branch conclusion)
    pub importance: f32,                     // 0.0-1.0, used for filtering under token pressure
}

#[derive(Debug, Clone, Copy)]
pub enum WorkingMemoryEventType {
    /// A branch completed with a conclusion
    BranchCompleted,
    /// A worker was spawned
    WorkerSpawned,
    /// A worker completed (success or failure)
    WorkerCompleted,
    /// A cron job executed
    CronExecuted,
    /// A memory was saved (by any path)
    MemorySaved,
    /// A decision was made (extracted from conversation)
    Decision,
    /// An error or failure occurred
    Error,
    /// A task was created or updated
    TaskUpdate,
    /// Cross-agent communication
    AgentMessage,
    /// System event (startup, config change, maintenance)
    System,
    /// A graph memory was promoted to working tier (reserved for tiered memory integration)
    MemoryPromoted,
    /// A graph memory was demoted from working tier (reserved for tiered memory integration)
    MemoryDemoted,
}
```

### What Emits Events

Events are emitted **programmatically at the point they happen**. No LLM decides whether to emit them. The processes themselves write events as a side effect of doing their work.

| Source         | Event             | When                                                               |
| -------------- | ----------------- | ------------------------------------------------------------------ |
| Branch         | `BranchCompleted` | When a branch returns its conclusion                               |
| Worker         | `WorkerSpawned`   | When `spawn_worker` tool executes                                  |
| Worker         | `WorkerCompleted` | When a worker reaches terminal state                               |
| Cron           | `CronExecuted`    | When a cron job fires and completes                                |
| Memory tools   | `MemorySaved`     | When `memory_save` succeeds (already emitted on `memory_event_tx`) |
| Branch/Channel | `Decision`        | When the LLM explicitly flags a decision (see below)               |
| Any process    | `Error`           | On tool failure, worker failure, cancellation                      |
| Task tools     | `TaskUpdate`      | When task board is modified                                        |
| Link channels  | `AgentMessage`    | On cross-agent message send/receive                                |
| Cortex         | `System`          | On startup, config reload, maintenance run                         |

User messages and agent responses are **not** recorded as working memory events. They already live in the `conversation_messages` table with full attribution, timestamps, and channel context. The channel activity map (Layer 3) queries that table directly for recent per-channel activity. Duplicating messages into the working memory log would be redundant.

### Decision Extraction

The `Decision` event type deserves special attention. Decisions are the highest-value working memory entries -- "we decided to use JWT instead of sessions," "Jamie approved the PR," "sandbox disabled for gh cli."

Two extraction paths:

**Programmatic:** When the agent uses the `reply` tool with certain patterns (confirming an action, committing to a plan, changing course), the channel can tag the event as a decision. This is heuristic and imperfect.

**LLM-assisted:** The memory persistence branch (which already runs periodically) gains a new responsibility: in addition to saving graph memories, it emits `Decision` events into the working memory log for any decisions it identifies in the conversation. This costs nothing extra -- the persistence branch is already reading the conversation and making judgments about what matters.

### Storage

SQLite table, one row per event:

```sql
CREATE TABLE IF NOT EXISTS working_memory_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    timestamp TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    channel_id TEXT,
    user_id TEXT,
    summary TEXT NOT NULL,
    detail TEXT,
    importance REAL NOT NULL DEFAULT 0.5,
    day TEXT NOT NULL                        -- date string 'YYYY-MM-DD' for fast day-based queries
);

CREATE INDEX idx_wm_events_day ON working_memory_events(day, timestamp);
CREATE INDEX idx_wm_events_channel ON working_memory_events(channel_id, timestamp);
CREATE INDEX idx_wm_events_type ON working_memory_events(event_type, timestamp);
```

The `day` column is denormalized from `timestamp` for fast date-range queries without function calls in WHERE clauses. **The day is always computed in the agent's configured timezone at insert time**, not UTC. This ensures a 23:30 UTC event for a UTC+2 agent is correctly recorded as the next calendar day. The `WorkingMemoryStore` takes a timezone at construction and applies it consistently to all day computations (inserts, queries, day-rollover checks).

```rust
impl WorkingMemoryStore {
    pub fn new(pool: SqlitePool, timezone: chrono_tz::Tz) -> Arc<Self>;
}
```

### Intra-Day Synthesis

In a busy environment -- 10 engineers in a Slack, hundreds of workers per day, thousands of messages across channels -- raw events are useless. Fifty worker completions, thirty branch conclusions, and a dozen cron executions do not fit in a token budget, and even if they did, a scrolling log is not situational awareness.

**Today's section is always a synthesis, never a raw event list.** The cortex maintains a rolling narrative of the current day by synthesizing events in batches as they accumulate.

**Trigger:** Dual trigger — event-count or time-based, whichever fires first.

1. **Count-based:** When the number of new events since the last intra-day synthesis crosses a threshold (configurable, default 15), synthesize.
2. **Time-based fallback:** If any unsynthesized events exist and it has been more than `intraday_time_fallback_secs` (configurable, default 4 hours) since the last synthesis, synthesize regardless of count. This ensures quiet agents still get narrative blocks instead of raw event tails all day.

A quiet agent with 5-10 events per day gets 1-2 syntheses (time-triggered). A busy agent gets 10+ (count-triggered). Each synthesis is incremental -- it covers only the new batch, not the whole day.

**Storage:**

```sql
CREATE TABLE IF NOT EXISTS working_memory_intraday_syntheses (
    id TEXT PRIMARY KEY,
    day TEXT NOT NULL,
    time_range_start TIMESTAMP NOT NULL,     -- first event in this batch
    time_range_end TIMESTAMP NOT NULL,       -- last event in this batch
    summary TEXT NOT NULL,                   -- 50-100 word narrative of this batch
    event_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_wm_intraday_day ON working_memory_intraday_syntheses(day, time_range_start);
```

Each row is one synthesis batch -- a paragraph covering a time range. The `day` column groups them. The rows are append-only within a day.

**Synthesis prompt:**

```
Summarize the following {{ event_count }} events from {{ time_start }} to {{ time_end }}
into a concise 50-100 word narrative paragraph.

Focus on: what was accomplished, what decisions were made, what failed, what is in progress.
Be specific about who did what and in which channel. Use present/past tense naturally.
Do not list events mechanically -- write a narrative.

Events:
{{ events }}
```

### Daily Summaries

At day rollover, the cortex synthesizes the full day. Rather than re-processing raw events, it summarizes the intra-day synthesis paragraphs into a single cohesive narrative. This is cheap -- the input is a few paragraphs, not hundreds of events.

```sql
CREATE TABLE IF NOT EXISTS working_memory_daily_summaries (
    day TEXT PRIMARY KEY,                    -- 'YYYY-MM-DD'
    summary TEXT NOT NULL,                   -- LLM-synthesized narrative, 200-400 words
    event_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

The cortex checks on each tick: if the current day has changed since the last daily summary, synthesize yesterday. Input: all `working_memory_intraday_syntheses` rows for that day, plus any unsynthesized raw events that didn't hit the batch threshold. One LLM call per day.

Weekly and monthly summaries can be added later by summarizing daily summaries. For now, daily is sufficient.

### Context Injection

The working memory section is assembled from synthesis blocks + a raw event tail. Today's view is always narrative-first.

```
## Working Memory

### Today (Wednesday, March 18)
[morning] Shipped PR #447 fix to staging. Set up task board for 30 open PRs
after disabling sandbox for gh cli access. OAuth research confirmed device-code
flow works for ChatGPT. bergabman active in #talk-to-spacebot with OAuth
config questions.

[afternoon] Shifted to working memory implementation. Phase 1 complete --
event store, types, and all emission points wired. Decided to sunset
compaction-based memory extraction. Three users active in #talk-to-spacebot
discussing provider config.

**Since last synthesis (14:20):**
- Worker completed: Phase 2 context injection done
- Decision: Remove UserMessage from working memory events
- Branch completed: config validation for new WorkingMemoryConfig fields

### Yesterday (Tuesday, March 17)
Quiet day. Jamie tested voice integration in portal chat. bergabman submitted
PR #441 (opinionated Discord reply policy). Two cron jobs ran (inbox-check,
repo-monitor). No critical decisions.

### This Week
Focus: patch release stability + working memory design. 14 PRs reviewed across
3 days. Key decisions: sunset compaction-based memory saving, redesign bulletin
as layered context. Active contributors: Jamie, bergabman, vsumner, l33t0.
```

**Rendering logic:**

1. **Today:** Concatenate all `working_memory_intraday_syntheses` rows for today (time-labeled paragraphs). Append a "Since last synthesis" tail of raw events that haven't been synthesized yet (capped at `today_max_unsynthesized_events`, default 10).
2. **Yesterday:** The daily summary from `working_memory_daily_summaries`.
3. **This week:** Concatenated daily summaries for the past 5 days, truncated to fit budget.

All rendering is programmatic -- string concatenation from cached synthesis rows and SQL queries. No LLM call on the rendering path.

**Token budget:** Configurable, default 1500 tokens. Applied as follows:

1. Today's synthesis blocks + raw tail: up to 60% of budget.
2. Yesterday's summary: up to 80% consumed.
3. This week: remaining budget.
4. If today's synthesis blocks alone exceed budget: show only the most recent N blocks + the raw tail. Older blocks are dropped (they are still in the database for search).

**Events from the current channel get a boost.** When rendering for Channel B, events from Channel B are always included (up to a reasonable cap). Events from other channels are included by importance and recency.

### Relationship to Tiered Memory and the Graph

Working memory events are NOT memories in the graph. They are a separate data structure with a separate lifecycle. The [tiered memory system](tiered-memory.md) is a third concern that operates on the graph memory store itself.

|           | Working Memory Events             | Graph Memories                     | Tiered Memory                                  |
| --------- | --------------------------------- | ---------------------------------- | ---------------------------------------------- |
| Structure | Timestamped log entries           | Typed objects with associations    | `tier` column on graph memories                |
| Lifecycle | Day-scoped, synthesized, archived | Importance-based decay and pruning | 3-day working state → graph demotion           |
| Injection | Layers 2-5 of system prompt       | On-demand via branch recall        | Not injected — affects recall ranking          |
| Creation  | Automatic (event-driven)          | Intentional (branch/persistence)   | Automatic (new memories start in working tier) |
| Purpose   | Situational awareness (channels)  | Long-term knowledge                | Search quality (branches)                      |

All three systems are complementary:

- **Working memory events** tell the channel what happened today: "we decided X at 3pm." Synthesized into narrative blocks, always in context.
- **Graph memories** capture durable knowledge: the decision itself, with full context, associations, importance scoring, and long-term retrieval.
- **Tiered memory** ensures that when a branch calls `memory_recall`, the graph memory saved 2 minutes ago ranks higher than the one from 3 weeks ago. It is a search and lifecycle improvement on the graph, not a context injection mechanism.

The memory persistence branch feeds both systems: it saves graph memories (which start in the working tier) and emits working memory events for decisions and facts it identifies. Both systems get fed from the same observation pass.

---

## Layer 3: Channel Activity Map

A compact, programmatic summary of what is happening in all other channels right now. Gives the agent ambient awareness of activity elsewhere without injecting transcripts.

### What it contains

For each channel the agent has, render:

- Channel name
- Time since last activity
- Who is active (display names of recent senders)
- One-line topic (last branch conclusion or last significant message, truncated)

### Rendering

Fully programmatic. SQL queries against `conversation_messages` and the channel's in-memory state. No LLM.

```
## Other Channels
#development -- 12m ago, vsumner: discussing PR #447 fix
#talk-to-spacebot -- 3m ago, bergabman + okuna: ChatGPT OAuth config
#general -- 2h ago, release timeline discussion
#random -- 1d ago, inactive
```

**Update trigger:** Rendered fresh on every turn from the conversation database + channel registry. Cheap -- it is a bounded query (one row per channel, last message + sender).

**Token budget:** Configurable, default 300 tokens. Channels sorted by recency. Inactive channels (>24h) are collapsed to a single line or omitted entirely (configurable).

**What channels are included:** All channels for the same agent on the same messaging instance. Cross-platform channels (the same agent on Discord and Slack) are included with a platform prefix. Cross-agent channels are not included (that is the link-channels system's domain).

### Implementation

The channel registry (`ChannelStore` / in-memory channel map) already tracks active channels. We need to expose a method that returns the activity summary for all channels except the current one:

```rust
impl ChannelStore {
    /// Returns a compact activity summary for all channels except `exclude`.
    pub async fn get_activity_map(
        &self,
        exclude_channel_id: &str,
        max_channels: usize,
    ) -> Result<Vec<ChannelActivity>>;
}

pub struct ChannelActivity {
    pub channel_id: ChannelId,
    pub channel_name: String,
    pub platform: String,
    pub last_message_at: Option<DateTime<Utc>>,
    pub last_sender_name: Option<String>,
    pub recent_sender_names: Vec<String>,    // last 3 unique senders
    pub topic_hint: Option<String>,          // last branch conclusion or significant message
}
```

The `topic_hint` comes from the most recent `BranchCompleted` working memory event for that channel, or the last significant message if no branches ran. This is already in the working memory events table -- a single indexed query.

---

## Layer 4: Participant Context

Per-user context injected when specific users are active in the current channel. The agent knows who it is talking to before it starts thinking.

### Relationship to existing designs

This layer is the `participant-awareness.md` design, integrated into the layered context assembly. The design is unchanged -- `humans` table, cortex-generated summaries, cached and injected when `participants.len() >= min_participants`. The only change is its position in the prompt (it moves from "after Memory Context" to its own layer in the new assembly order).

If user-scoped memories lands first, the `humans` table merges with `user_identifiers` as described in that design doc. The working memory system does not depend on which identity table is canonical.

### Enhancement: Recent Activity Per User

The participant summary (2-3 sentences about who this person is) is augmented with a line of recent activity pulled from the working memory events table:

```
## Participants

**bergabman** -- Power user, runs modified Spacebot with reasoning effort support.
  Previously built ChatGPT OAuth integration. Prefers technical responses.
  Recent: asked about OAuth config in #talk-to-spacebot 2h ago, submitted PR #441 yesterday.

**okuna** -- New user, joined 3 days ago. Has been asking setup questions.
  Background in Docker and DevOps.
  Recent: asked about Docker runtime deps in #talk-to-spacebot 10m ago.
```

The "Recent:" line is programmatic -- a query against `working_memory_events WHERE user_id = X ORDER BY timestamp DESC LIMIT 3`. The summary paragraph is the cached cortex-generated profile from the participant-awareness design. No additional LLM call.

**Token budget:** Configurable, default 400 tokens. Max 5 participants rendered. In a 50-person channel, only the 5 most recently active participants get profiles.

---

## Layer 5: Knowledge Synthesis

The remnant of the bulletin. Long-term knowledge that does not fit into the temporal working memory log or the identity files. Strategic context, ongoing themes, accumulated observations.

### What it contains

- Cross-cutting themes from graph memories (not tied to a specific day or channel)
- Active goals and strategic direction
- Known unknowns ("detailed intelligence on Spacedrive requires updating")
- Accumulated observations from the cortex

### What it does NOT contain

- Identity/role information (Layer 1)
- Recent events (Layer 2)
- Channel activity (Layer 3)
- User profiles (Layer 4)
- Bug reports, weather, per-user formatting preferences (these are graph memories, recalled on demand by branches)

### Rendering

LLM-synthesized, but only when the underlying data changes.

**Dirty flag mechanism:** A counter `knowledge_synthesis_version` in `RuntimeConfig` (atomic u64). Incremented when a memory's content is created, updated, or deleted. **Not** incremented on importance-only changes (decay, access count updates) — those shift ranking but don't change what the agent knows. The cortex checks on each tick: if the counter has changed since the last synthesis, regenerate. If not, skip.

This replaces the timer-based bulletin. An idle agent with no new memories generates zero synthesis calls. A busy agent with 50 new memories in an hour triggers one synthesis after activity settles (debounced -- wait 60 seconds after the last memory change before regenerating, to avoid regenerating mid-burst).

**Scope reduction:** The synthesis prompt is narrower than the current bulletin prompt. It does not ask for identity context, recent events, or user profiles -- those are handled by other layers. It asks only for:

```
Synthesize the agent's long-term knowledge into a concise briefing.
Focus on:
- Active goals and strategic direction
- Cross-cutting themes and patterns
- Known gaps in knowledge
- Accumulated observations

Do not include: identity/role information, recent events, channel activity,
or user profiles. Those are provided separately.

Maximum {{ max_words }} words.
```

**Token budget:** Configurable, default 500 tokens. Substantially smaller than the current bulletin (which was 500-1500 words carrying all concerns).

### Interaction with Topics (PR #287)

The cortex topic synthesis system (PR #287) produces living documents on specific themes -- detailed, searchable, pulled into workers on demand. Knowledge synthesis (Layer 5) is the broad overview; topics are the deep dives.

When topics are available, the knowledge synthesis can reference them: "See topic 'OAuth Integration' for detailed provider status." This keeps the synthesis concise while pointing to deeper context that branches and workers can access.

Topics are NOT injected into the channel system prompt by default. They are pulled in by branches and workers when relevant. This is a key difference from working memory layers 1-5, which are always present.

---

## Memory Creation Overhaul

### Sunset Compaction-Based Memory Extraction

The compactor's job is context management -- summarizing old context to free up space. Memory extraction is a secondary concern that was bolted on because there was no better place for it.

**Change:** The compactor no longer calls `memory_save`. Its sole output is a compaction summary that replaces the compacted messages in the conversation history. Memories are extracted through other paths.

**Why this is safe:** The memory persistence branch already exists and runs periodically. It has better context (full conversation history, not just the messages being compacted) and better attribution (it can set `user_id` and `channel_id`). The compactor was a safety net for conversations that ran long enough to hit compaction but not long enough to trigger persistence. That safety net is replaced by event-driven memory capture (see below).

### Memory Persistence Branch: Smarter Triggers

The periodic memory persistence branch currently runs every 50 user messages. Replace with signal-based triggers:

1. **Message count trigger (reduced):** Every 20 user messages instead of 50. Lower threshold means less information lost between persistence runs.

2. **Time-based trigger:** If a conversation has been active for 15 minutes since the last persistence run, trigger regardless of message count. Catches slow-but-important conversations.

3. **Event-density trigger:** If the working memory log has recorded 5+ events from this channel since the last persistence run, trigger. Catches conversations with high signal (multiple decisions, worker completions, etc.) even if the raw message count is low.

4. **Explicit trigger:** The LLM can call a new `persist_memories` channel tool to force a persistence run. For moments where the agent recognizes something important just happened. (This is optional -- the automatic triggers should be sufficient in most cases.)

### Persistence Branch Dual Output

The memory persistence branch gains a second responsibility: in addition to saving graph memories, it emits `Decision` and `MemorySaved` events into the working memory log. This connects the two systems:

```
Persistence branch runs:
  1. Recalls existing graph memories (avoid duplicates)
  2. Reads conversation history since last run
  3. Saves new graph memories via memory_save (as today)
  4. Identifies key decisions and events
  5. Emits working memory events for each decision identified
  6. Calls memory_persistence_complete
```

Step 5 is new. The persistence branch prompt is updated to instruct it to identify decisions explicitly. The `memory_persistence_complete` tool gains an optional `events` field:

```rust
pub struct MemoryPersistenceCompleteArgs {
    pub outcome: String,
    pub memory_ids: Vec<String>,
    pub events: Option<Vec<WorkingMemoryEventInput>>,  // new
}

pub struct WorkingMemoryEventInput {
    pub event_type: String,       // "decision", "error", etc.
    pub summary: String,
    pub importance: Option<f32>,
}
```

### Structured Event Capture (Zero LLM Cost)

The majority of working memory events are captured programmatically, with zero LLM involvement:

| Event            | Emitter                                      | How                                                                  |
| ---------------- | -------------------------------------------- | -------------------------------------------------------------------- |
| Worker spawned   | `spawn_worker` tool handler                  | After successful spawn, write event with task description as summary |
| Worker completed | Worker state machine terminal transition     | Write event with worker result summary (truncated to 200 chars)      |
| Branch completed | Branch return path in channel                | Write event with branch conclusion (truncated to 200 chars)          |
| Cron executed    | Cron scheduler after job completes           | Write event with cron name + outcome                                 |
| Memory saved     | `memory_save` tool handler                   | Write event with memory type + content preview                       |
| Task updated     | Task tool handlers                           | Write event with task title + new status                             |
| Error            | SpacebotHook on tool failure, worker failure | Write event with error description                                   |
| System           | Startup, config change, maintenance          | Write event with description                                         |

Each emitter calls `working_memory_store.record_event()` as a fire-and-forget `tokio::spawn`. The message processing pipeline never waits on event recording.

```rust
impl WorkingMemoryStore {
    /// Fire-and-forget event recording. Never blocks the caller.
    pub fn record(&self, event: WorkingMemoryEvent) {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            if let Err(error) = insert_event(&pool, &event).await {
                tracing::warn!(%error, "failed to record working memory event");
            }
        });
    }
}
```

---

## Cortex Role Changes

The cortex shifts from "generate a blob every 15 minutes" to four focused responsibilities:

### 1. Intra-Day Synthesis (Event-Count Triggered)

On each tick, check if the number of unsynthesized events for today exceeds the batch threshold (configurable, default 15). If so, synthesize the batch into a paragraph and store it in `working_memory_intraday_syntheses`. One LLM call per batch.

```rust
async fn maybe_synthesize_intraday_batch(&self) -> Result<()> {
    let today = current_day(&self.config);
    let last_synthesis_end = self.working_memory
        .get_last_intraday_synthesis_end(&today)
        .await?;

    let unsynthesized = self.working_memory
        .get_events_after(&today, last_synthesis_end)
        .await?;

    let threshold_met = unsynthesized.len() >= self.config.working_memory.intraday_batch_threshold;
    let time_fallback = !unsynthesized.is_empty()
        && last_synthesis_end
            .map(|t| Utc::now() - t > Duration::seconds(self.config.working_memory.intraday_time_fallback_secs as i64))
            .unwrap_or(true); // no previous synthesis today → fallback fires

    if !threshold_met && !time_fallback {
        return Ok(());
    }

    let time_start = unsynthesized.first().unwrap().timestamp;
    let time_end = unsynthesized.last().unwrap().timestamp;
    let summary = self.synthesize_intraday_batch(&unsynthesized).await?;

    self.working_memory.save_intraday_synthesis(
        &today, time_start, time_end, &summary, unsynthesized.len()
    ).await?;

    Ok(())
}
```

On a quiet day this fires 1-2 times. On a busy day with hundreds of workers it fires every few minutes, each time digesting the latest batch into a readable paragraph. The LLM cost scales linearly with activity, not with time.

### 2. Daily Summary Synthesis (Day Rollover)

On each tick, check if the day has rolled over. If yesterday has no daily summary yet, synthesize one from yesterday's intra-day synthesis paragraphs (not raw events -- the paragraphs are already digested). One LLM call per day.

```rust
async fn maybe_synthesize_daily_summary(&self) -> Result<()> {
    let today = current_day(&self.config);
    let yesterday = today - Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();

    if self.working_memory.has_daily_summary(&yesterday_str).await? {
        return Ok(());
    }

    let intraday_blocks = self.working_memory
        .get_intraday_syntheses(&yesterday_str).await?;

    if intraday_blocks.is_empty() {
        self.working_memory.save_daily_summary(&yesterday_str, "No activity.", 0).await?;
        return Ok(());
    }

    // Summarize the intra-day paragraphs, not raw events
    let summary = self.synthesize_daily_summary(&yesterday_str, &intraday_blocks).await?;
    let event_count = intraday_blocks.iter().map(|b| b.event_count).sum();
    self.working_memory.save_daily_summary(&yesterday_str, &summary, event_count).await?;
    Ok(())
}
```

### 3. Knowledge Synthesis (On Dirty Flag)

On each tick, check if `knowledge_synthesis_version` has changed. If so, debounce (wait 60s after last change), then regenerate the knowledge synthesis (Layer 5). Store in `RuntimeConfig::knowledge_synthesis` via `ArcSwap`.

This replaces the bulletin generation loop entirely. The warmup loop still runs at startup to generate the initial synthesis, but the recurring generation is change-driven, not timer-driven.

### 4. Participant Summary Generation

Unchanged from the `participant-awareness.md` design. The cortex checks for stale human summaries and regenerates them. This already runs on a 5-minute tick -- it is cheap and targeted.

### What the Cortex No Longer Does

- **Bulletin generation on a timer.** Replaced by change-driven knowledge synthesis.
- **Full 8-section memory retrieval every 15 minutes.** Only Layer 5 queries the memory store, and only when dirty.
- **Warmup-driven bulletin regeneration every 15 minutes.** The warmup loop ensures Layer 5 exists on startup. After that, it is change-driven.

### Warmup Changes

The warmup loop still runs to ensure the agent is ready before accepting traffic. But its scope shrinks:

1. **Embedding warmup** -- unchanged, ensures FastEmbed is loaded.
2. **Knowledge synthesis** -- generate Layer 5 if it does not exist. After initial generation, this is change-driven.
3. **Working memory events** -- no warmup needed, they are always available (SQL queries).
4. **Channel activity map** -- no warmup needed, it is always available (SQL queries).
5. **Participant summaries** -- no warmup needed for the summaries to exist (they generate on their own schedule). The readiness contract may relax to not require participant summaries before accepting traffic.

The `ready_for_work` contract changes from "warm state + embedding ready + fresh bulletin" to "embedding ready + knowledge synthesis exists." The working memory log and channel activity map are always ready (they are database queries, not LLM outputs).

---

## Working Memory Event Lifecycle

### Today's Events (0-24h)

Raw events accumulate in `working_memory_events`. As they accumulate, the cortex synthesizes them in batches into `working_memory_intraday_syntheses` rows (see "Intra-Day Synthesis" above). The channel's context shows today's synthesis paragraphs plus a raw tail of unsynthesized recent events. Raw events are never shown directly once synthesized -- the synthesis replaces them in the context view.

### Yesterday (24-48h)

At day rollover, the cortex synthesizes the day's intra-day synthesis paragraphs (plus any remaining unsynthesized events) into a single `working_memory_daily_summaries` row. The raw events and intra-day syntheses are retained for search but no longer injected into context -- only the daily summary is.

### Older Days (48h+)

Daily summaries are the canonical representation. Raw events are retained for search (the `working_memory_events` table is append-only and queryable) but are not rendered into context.

### Weekly Summaries (optional, future)

After 7 daily summaries exist for a week, the cortex can synthesize them into a weekly summary. One LLM call per week. This provides the "this week" paragraph in the context injection. For the initial implementation, "this week" is rendered by concatenating the last 5-7 daily summaries and truncating to fit the token budget. LLM-synthesized weekly summaries can be added later.

### Pruning

Raw events older than 30 days are pruned. Intra-day syntheses older than 30 days are pruned (the daily summary subsumes them). Daily summaries are retained indefinitely (they are small -- one row per day). This gives the agent permanent access to "what happened on March 18" through the daily summaries, and 30 days of granular event-level and intra-day synthesis search.

Pruning runs as part of the existing cortex maintenance loop, alongside memory decay and graph pruning.

---

## Schema Summary

### New Tables

```sql
-- Working memory events: the append-only log
CREATE TABLE IF NOT EXISTS working_memory_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    timestamp TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    channel_id TEXT,
    user_id TEXT,
    summary TEXT NOT NULL,
    detail TEXT,
    importance REAL NOT NULL DEFAULT 0.5,
    day TEXT NOT NULL
);

CREATE INDEX idx_wm_events_day ON working_memory_events(day, timestamp);
CREATE INDEX idx_wm_events_channel ON working_memory_events(channel_id, timestamp);
CREATE INDEX idx_wm_events_type ON working_memory_events(event_type, timestamp);

-- Intra-day synthesis: rolling narrative blocks within a day
CREATE TABLE IF NOT EXISTS working_memory_intraday_syntheses (
    id TEXT PRIMARY KEY,
    day TEXT NOT NULL,
    time_range_start TIMESTAMP NOT NULL,
    time_range_end TIMESTAMP NOT NULL,
    summary TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_wm_intraday_day ON working_memory_intraday_syntheses(day, time_range_start);

-- Daily summaries: synthesized narratives per day
CREATE TABLE IF NOT EXISTS working_memory_daily_summaries (
    day TEXT PRIMARY KEY,
    summary TEXT NOT NULL,
    event_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

### No Changes to Existing Tables

The `memories` table is unchanged. Working memory events are a separate system that complements graph memories, not a modification to them. The tiered memory design (`tier` column, `demoted_at` column) proceeds independently.

### New Rust Types

```rust
// src/memory/working.rs (new module)

pub struct WorkingMemoryStore {
    pool: SqlitePool,
}

pub struct WorkingMemoryEvent { ... }      // as defined above
pub enum WorkingMemoryEventType { ... }    // as defined above

pub struct IntradaySynthesis {
    pub id: String,
    pub day: String,
    pub time_range_start: DateTime<Utc>,
    pub time_range_end: DateTime<Utc>,
    pub summary: String,
    pub event_count: i64,
    pub created_at: DateTime<Utc>,
}

pub struct DailySummary {
    pub day: String,
    pub summary: String,
    pub event_count: i64,
    pub created_at: DateTime<Utc>,
}

pub struct WorkingMemoryContext {
    pub today_syntheses: Vec<IntradaySynthesis>,  // intra-day narrative blocks
    pub today_unsynthesized: Vec<WorkingMemoryEvent>,  // raw tail
    pub yesterday_summary: Option<String>,
    pub week_summary: Option<String>,
}
```

Each agent has its own SQLite database, so no `agent_id` column is needed on any table. The database file itself is the agent scope.

---

## Configuration

New fields on `CortexConfig`:

```rust
pub struct WorkingMemoryConfig {
    /// Whether working memory is enabled (default: true)
    pub enabled: bool,

    /// Events before an intra-day synthesis batch is triggered (default: 15)
    pub intraday_batch_threshold: usize,

    /// Seconds before time-based fallback triggers intra-day synthesis (default: 14400 / 4 hours)
    /// Ensures quiet agents still get narrative blocks even with few events
    pub intraday_time_fallback_secs: u64,

    /// Maximum unsynthesized recent events to show in the raw tail (default: 10)
    pub today_max_unsynthesized_events: usize,

    /// Token budget for the entire working memory section (default: 1500)
    pub context_token_budget: usize,

    /// Token budget for the channel activity map (default: 300)
    pub channel_map_token_budget: usize,

    /// Maximum channels to show in the activity map (default: 10)
    pub channel_map_max_channels: usize,

    /// Hide inactive channels after this many hours (default: 24)
    pub channel_map_inactive_hours: u64,

    /// Minimum importance for events to be included under token pressure (default: 0.5)
    pub min_importance_under_pressure: f32,

    /// Days to retain raw events before pruning (default: 30)
    pub event_retention_days: i64,

    /// Daily summary max words (default: 300)
    pub daily_summary_max_words: usize,

    /// Persistence branch trigger: message count threshold (default: 20)
    pub persistence_message_threshold: usize,

    /// Persistence branch trigger: time threshold in seconds (default: 900 / 15 min)
    pub persistence_time_threshold_secs: u64,

    /// Persistence branch trigger: event density threshold (default: 5)
    pub persistence_event_density_threshold: usize,
}
```

On `CortexConfig`, replace `bulletin_interval_secs` and `bulletin_max_words` with:

```rust
pub struct CortexConfig {
    // ... existing fields (tick_interval_secs, worker_timeout_secs, etc.)

    /// Knowledge synthesis max words (default: 500). Replaces bulletin_max_words.
    pub knowledge_synthesis_max_words: usize,

    /// Debounce seconds after last memory change before regenerating knowledge synthesis (default: 60)
    pub knowledge_synthesis_debounce_secs: u64,

    /// Working memory configuration
    pub working_memory: WorkingMemoryConfig,
}
```

`bulletin_interval_secs` and `bulletin_max_words` are deprecated. They continue to work as aliases for backward compatibility (mapping to `knowledge_synthesis_debounce_secs` and `knowledge_synthesis_max_words` respectively) but are removed from documentation and examples.

All config is hot-reloadable via `ArcSwap`.

---

## Context Assembly: The New `build_system_prompt`

The channel's `build_system_prompt()` method changes from:

```
identity_context + memory_bulletin + channel_prompt + status_block
```

To:

```
identity_context                    // Layer 1 (unchanged)
+ working_memory_section            // Layer 2 (new)
+ channel_activity_map              // Layer 3 (new)
+ participant_context               // Layer 4 (from participant-awareness design)
+ knowledge_synthesis               // Layer 5 (replaces bulletin)
+ channel_prompt                    // unchanged
+ status_block                      // unchanged
```

Each layer is rendered independently before assembly. The rendering functions:

```rust
/// Layer 2: Working memory log, rendered from DB.
async fn render_working_memory(
    store: &WorkingMemoryStore,
    channel_id: &str,
    config: &WorkingMemoryConfig,
    timezone: &str,
) -> Result<String>;

/// Layer 3: Channel activity map, rendered from DB + channel registry.
async fn render_channel_activity_map(
    channel_store: &ChannelStore,
    exclude_channel_id: &str,
    config: &WorkingMemoryConfig,
) -> Result<String>;

/// Layer 4: Participant context, rendered from cached summaries + working memory events.
async fn render_participant_context(
    human_store: &HumanStore,
    working_memory_store: &WorkingMemoryStore,
    participants: &HashMap<String, String>,
    config: &ParticipantConfig,
) -> Result<String>;

/// Layer 5: Knowledge synthesis, read from ArcSwap cache.
fn render_knowledge_synthesis(
    runtime_config: &RuntimeConfig,
) -> Option<String>;
```

Layers 2 and 3 are SQL queries -- they add ~2-5ms per turn. Layer 4 is a cache read + one small SQL query. Layer 5 is an `ArcSwap` load. Total overhead per turn: negligible.

---

## Prompt Template Changes

### `channel.md.j2`

Replace:

```jinja2
{%- if memory_bulletin %}
## Memory Context

{{ memory_bulletin }}
{%- endif %}
```

With:

```jinja2
{%- if working_memory %}
{{ working_memory }}
{%- endif %}

{%- if channel_activity_map %}
{{ channel_activity_map }}
{%- endif %}

{%- if participant_context %}
{{ participant_context }}
{%- endif %}

{%- if knowledge_synthesis %}
## Knowledge Context

{{ knowledge_synthesis }}
{%- endif %}
```

The section headers (`## Working Memory`, `## Other Channels`, `## Participants`) are rendered by the respective functions, not the template. This allows each function to omit the header entirely when there is no content to show.

### `memory_persistence.md.j2`

Add instruction to emit working memory events:

```
In addition to saving graph memories, identify key decisions and important events
from the conversation. For each, include it in the `events` field of
memory_persistence_complete. Events should be one-line summaries of decisions,
actions, or noteworthy moments.
```

### `cortex_bulletin.md.j2`

Renamed to `cortex_knowledge_synthesis.md.j2`. Narrowed scope:

```
Synthesize the agent's long-term knowledge into a concise briefing.
Focus on:
- Active goals and strategic direction
- Cross-cutting themes and patterns
- Known gaps in knowledge ("I don't have current information on X")
- Accumulated observations

Do NOT include: identity/role information, recent events or activity,
channel-specific context, or user profiles. Those are provided by other
context layers.

Maximum {{ max_words }} words.
```

### `compactor.md.j2`

Remove `memory_save` from the compactor's tool set. The compactor's only output is a compaction summary.

---

## Files Changed

| File                                                | Change                                                                                                                 |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| New migration SQL                                   | `working_memory_events` and `working_memory_daily_summaries` tables                                                    |
| `src/memory/working.rs` (new)                       | `WorkingMemoryStore`, event types, rendering functions                                                                 |
| `src/memory.rs`                                     | Add `mod working` + re-exports                                                                                         |
| `src/agent/channel.rs`                              | New `build_system_prompt` assembly, event emission on reply/branch                                                     |
| `src/agent/branch.rs`                               | Emit `BranchCompleted` event on return                                                                                 |
| `src/agent/worker.rs`                               | Emit `WorkerSpawned` and `WorkerCompleted` events                                                                      |
| `src/agent/cortex.rs`                               | Replace bulletin loop with daily summary + knowledge synthesis (dirty flag). Remove warmup bulletin regeneration loop. |
| `src/agent/compactor.rs`                            | Remove `memory_save` from compactor tool server                                                                        |
| `src/agent/channel_dispatch.rs`                     | Smarter persistence branch triggers (message count, time, event density)                                               |
| `src/config/types.rs`                               | `WorkingMemoryConfig`, deprecate `bulletin_interval_secs` / `bulletin_max_words`                                       |
| `src/config/runtime.rs`                             | `knowledge_synthesis` replaces `memory_bulletin` in `RuntimeConfig`. Add `knowledge_synthesis_version` atomic counter. |
| `src/conversation/humans.rs`                        | Augment participant rendering with recent working memory events                                                        |
| `src/tools/memory_save.rs`                          | Emit `MemorySaved` working memory event after successful save                                                          |
| `src/tools/spawn_worker.rs`                         | Emit `WorkerSpawned` working memory event                                                                              |
| `src/tools/memory_persistence_complete.rs`          | Accept optional `events` field, write to working memory                                                                |
| `src/cron/scheduler.rs`                             | Emit `CronExecuted` working memory event after job completes                                                           |
| `src/hooks/spacebot.rs`                             | Emit `Error` working memory event on tool failure                                                                      |
| `src/main.rs`                                       | Initialize `WorkingMemoryStore`, wire into `AgentDeps`                                                                 |
| `src/lib.rs`                                        | Re-export working memory types                                                                                         |
| `prompts/en/channel.md.j2`                          | Replace `memory_bulletin` with layered sections                                                                        |
| `prompts/en/cortex_knowledge_synthesis.md.j2` (new) | Narrowed synthesis prompt                                                                                              |
| `prompts/en/cortex_daily_summary.md.j2` (new)       | Daily summary synthesis prompt                                                                                         |
| `prompts/en/memory_persistence.md.j2`               | Add working memory event emission instructions                                                                         |
| `prompts/en/compactor.md.j2`                        | Remove memory_save instructions                                                                                        |

---

## Phases

### Phase 1: Working Memory Store + Event Emission

**Goal:** Events start flowing into the database.

- Migration for `working_memory_events` and `working_memory_daily_summaries`
- `WorkingMemoryStore` with `record()`, `get_events_for_day()`, `get_events_for_channel()`, `get_recent_events()`, `has_daily_summary()`, `save_daily_summary()`
- Wire `WorkingMemoryStore` into `AgentDeps`
- Emit events from: worker state machine (spawned, completed), branch return path, cron scheduler, `memory_save` tool, `SpacebotHook` (errors), startup/config change
- Fire-and-forget on all emission paths

**Verification:** Events accumulate in the database. `SELECT count(*) FROM working_memory_events` grows during normal operation. No performance regression on message processing (all writes are fire-and-forget).

### Phase 2: Context Injection (Working Memory + Channel Map)

**Goal:** Channels see working memory and cross-channel activity.

- `render_working_memory()` function with token budgeting
- `render_channel_activity_map()` function
- Update `build_system_prompt()` to include Layers 2 and 3
- Update `channel.md.j2` template
- Message filtering for `UserMessage` events (rate limiting in multi-user channels)

**Verification:** The system prompt now contains `## Working Memory` and `## Other Channels` sections. Events from other channels appear in the channel activity map. Token budgets are respected.

### Phase 3: Knowledge Synthesis (Replace Bulletin)

**Goal:** The bulletin is replaced with change-driven knowledge synthesis.

- `knowledge_synthesis_version` counter in `RuntimeConfig`
- Dirty flag incremented on `memory_save` / `memory_delete` / `memory_update`
- Cortex tick loop checks dirty flag, debounces, regenerates Layer 5
- `cortex_knowledge_synthesis.md.j2` prompt
- Deprecate `bulletin_interval_secs` and `bulletin_max_words` (keep as aliases)
- Update warmup loop to generate knowledge synthesis instead of bulletin

**Verification:** Knowledge synthesis regenerates only when memories change. Idle agents produce zero synthesis calls. The system prompt contains `## Knowledge Context` instead of `## Memory Context`.

### Phase 4: Daily Summaries + Cortex Overhaul

**Goal:** The cortex synthesizes daily narratives and the working memory log gains progressive compression.

- `cortex_daily_summary.md.j2` prompt
- `maybe_synthesize_daily_summary()` in cortex tick loop
- Context injection renders yesterday's summary instead of raw events
- Event pruning (>30 days) added to maintenance loop
- Remove bulletin generation loop entirely

**Verification:** At day rollover, a daily summary is generated. Yesterday's section in the working memory shows the narrative summary, not raw events. Raw events older than 30 days are pruned.

### Phase 5: Memory Creation Overhaul

**Goal:** Memory persistence is smarter and compaction no longer extracts memories.

- Smarter persistence branch triggers (message count reduced to 20, time-based 15min, event-density 5+)
- Persistence branch dual output (graph memories + working memory events)
- Update `memory_persistence.md.j2` and `memory_persistence_complete` tool
- Remove `memory_save` from compactor tool server
- Update `compactor.md.j2`

**Verification:** Memory persistence fires more frequently and at the right times. The compactor produces summaries only, no memories. The working memory log contains `Decision` events emitted by the persistence branch.

### Phase 6: Participant Context Integration

**Goal:** Per-user context with recent activity from working memory.

- Depends on `participant-awareness.md` Phase 1-3 being implemented
- Augment participant rendering with recent working memory events per user
- Update `build_system_prompt()` to include Layer 4

**Verification:** When bergabman sends a message, the channel sees his profile summary plus "Recent: asked about OAuth config 2h ago." The recent activity line comes from the working memory events table.

---

## Migration Path

Fully backward compatible. The bulletin continues to work until Phase 3 replaces it. Phases 1-2 can ship independently -- working memory and channel activity map appear alongside the existing bulletin. Phase 3 replaces the bulletin. Phase 5 changes memory creation behavior.

For existing agents with `bulletin_interval_secs` in config, the value is mapped to `knowledge_synthesis_debounce_secs` automatically. The log warning suggests updating to the new config keys.

The working memory events table starts empty. There is no backfill -- the system captures events going forward. Historical context comes from existing graph memories and conversation history, which remain unchanged.

---

## Token Budget Breakdown (Default Configuration)

For a typical channel turn:

| Layer                        | Budget         | Source                                      |
| ---------------------------- | -------------- | ------------------------------------------- |
| 1. Identity Context          | ~600-2000      | User-defined (Soul + Identity + Role files) |
| 2. Working Memory            | 1500           | Today events + yesterday summary + week     |
| 3. Channel Activity Map      | 300            | Other channels summary                      |
| 4. Participant Context       | 400            | Active user profiles + recent activity      |
| 5. Knowledge Synthesis       | 500            | Long-term knowledge                         |
| **Total context layers**     | **~3300-4700** |                                             |
| Channel instructions + rules | ~2000          | Unchanged                                   |
| Status block                 | ~500-1500      | Unchanged                                   |
| **Total system prompt**      | **~5800-8200** |                                             |

Compare to today: ~6300+ tokens for system prompt, with the bulletin consuming 550-900 tokens of undifferentiated content. The new system uses a similar total budget but the content is structured, relevant, and independently managed.

---

## What This Enables

**"What happened today?"** The agent knows. The working memory log gives it a beat-by-beat narrative of the day's events without branching or searching. If Jamie asks at 4pm "what have we done today?" the answer is in the context.

**Cross-channel awareness.** Channel B knows that Channel A had a conversation about OAuth config 10 minutes ago. When bergabman references that conversation in Channel B, the agent can connect the dots without branching.

**Event-driven, not timer-driven.** The cortex only synthesizes when something changes. Idle agents produce zero LLM calls. Busy agents synthesize efficiently -- one daily summary per day, knowledge synthesis only on memory changes.

**Proactive memory capture.** Working memory events are recorded automatically as things happen. Decisions, worker completions, and cron executions are captured without relying on LLM judgment or compaction pressure.

**Progressive temporal compression.** Today is detailed. Yesterday is a summary. Last week is a paragraph. The agent has a sense of time passing and can tell you what last Tuesday looked like.

**Per-user awareness.** When a returning user sends a message, the agent sees their profile and recent activity across channels. It does not treat them as a stranger.

**Token efficiency.** Stable identity content is rendered once. Working memory rendering is programmatic (concatenating cached synthesis rows). Channel maps are programmatic SQL queries. Knowledge synthesis only regenerates on change. Intra-day synthesis scales with activity, not time -- a quiet day gets 1-2 LLM calls, a busy day gets 10-15, each one small (50-100 words). Daily summaries are one LLM call per day from already-digested paragraphs. The total LLM cost for context assembly drops from ~96 calls/day (bulletin, regardless of activity) to ~5-20 calls/day (proportional to actual activity).
