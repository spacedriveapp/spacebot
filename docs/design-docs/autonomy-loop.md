# Autonomy Loop

Today Spacebot is reactive. It responds to messages, executes cron jobs from scratch, and runs four hardcoded cortex loops (warmup, bulletin, association, ready-task pickup). It has no mechanism for proactive, self-directed cognition — the kind where the agent works overnight tracking issues, processing emails, monitoring repos, and surfaces curated intelligence when you ask for it in the morning.

The [custom cortex loops](./custom-cortex-loops.md) design proposed solving this with user-created, per-topic cortex processes. This doc supersedes that approach. The core objection: the cortex should be something the user never thinks about. It is the system's internal monologue, not a user-configurable surface. Exposing "cortex loops" as a concept — with a creation tool, dashboard tab, and per-loop settings — turns internal cognition into just a fancier cron job.

This doc proposes two changes instead:

1. **The autonomy loop** — a single new built-in cortex process that gives the agent general proactive behavior, driven by its identity and role rather than user-enumerated monitoring tasks.
2. **Stateful cron jobs** — adding optional memory continuity to the existing cron system so scheduled jobs can accumulate knowledge across runs.

Together these cover the same use cases as custom cortex loops with far less conceptual overhead. The user tells the agent what it is (identity files) and what it should do on a schedule (cron jobs). The system figures out the rest.

## The Problem

When a user tells their agent "keep tabs on the Spacedrive issues" or "give me a morning briefing every day at 9am," there's no good mechanism for this today:

1. **Cron jobs** spin up a fresh channel each time. No cross-run context. No accumulated knowledge. The cron fires, the agent sees the prompt cold, does its thing, delivers the result. Each run is isolated — the agent doesn't build on what it learned last time.

2. **The existing cortex loops** are hardcoded system functions (bulletin, associations, task pickup). They don't support user-directed proactive work.

3. **The ready-task pickup loop** provides autonomous execution, but only for tasks that were explicitly created by a human conversation. The agent never generates its own work.

The gap: there's no way for the agent to think on its own about the things it's been told to care about.

## The Autonomy Loop

A new fifth built-in cortex loop, alongside warmup, bulletin, association, and ready-task pickup. One per agent. Not user-created — it's part of the system, like the bulletin.

### What It Does

Each cycle, the autonomy loop wakes up as a branch with memory tools and worker-spawning capability. It receives the agent's full identity context (soul, role) and the current memory bulletin. It recalls its own previous findings. Then it decides what to do — what to check on, what to investigate, what to follow up on — based on what it knows about its role, its recent history, and the tools available to it.

The key insight: the agent's identity files already define what it should care about. A ROLE.md that says "You manage the Spacedrive project" implies the agent should be tracking Spacedrive issues, PRs, and CI. A SOUL.md that says "You are Jamie's engineering assistant" implies it should be aware of Jamie's email, calendar, and active projects. The autonomy loop operationalizes the identity — it's the system asking itself "given who I am and what I know, what should I be doing right now?"

### What It Doesn't Do

- It doesn't deliver results to the user. It saves memories and creates tasks. The bulletin and cron jobs handle delivery.
- It doesn't have its own personality or conversation context. It's a cortex process — internal cognition.
- It doesn't replace cron jobs. Cron jobs are "do X at time Y and tell the user." The autonomy loop is "think about what I should be aware of."
- It doesn't require user configuration beyond identity files. The agent figures out what to monitor.

### Execution Model

The autonomy loop runs on a configurable interval (default: 30 minutes). Each cycle:

```
1. CONTEXT ASSEMBLY
   ├─ Load identity context (soul.md, role.md, identity.md, user.md)
   ├─ Load current memory bulletin
   ├─ Load active tasks from the task board
   └─ Load recent cortex event log (what happened since last cycle)

2. RECALL PHASE
   ├─ Recall memories with source "cortex:autonomy" (previous cycle findings)
   ├─ Recall recent high-importance memories (what's changed in the world)
   └─ Build a context summary of current state + recent changes

3. EXECUTION PHASE (branch with worker-spawning capability)
   ├─ System prompt: autonomy_loop.md.j2
   ├─ Identity context + bulletin + recalled context injected
   ├─ Branch decides what to investigate this cycle:
   │   ├─ Check integrations relevant to its role (GitHub, email, etc.)
   │   ├─ Follow up on unresolved items from previous cycles
   │   ├─ Evaluate task board for stale or blocked items
   │   └─ Look for patterns or emerging issues across recent activity
   ├─ Spawns workers for actual work (API calls, shell commands)
   ├─ Saves findings as memories (source: "cortex:autonomy")
   ├─ Creates tasks for actionable items (status: pending_approval or backlog)
   └─ Max turns from config (default: 15)

4. SAVE PHASE
   ├─ Force-save a cycle summary memory (source: "cortex:autonomy")
   │   covering: what was checked, what was found, what was deferred
   ├─ Log execution to cortex_events
   └─ Update last_executed_at
```

### Why One Loop, Not Many

The custom cortex loops design had separate loops for each concern ("track-spacedrive-issues", "monitor-email", "weekly-project-review"). The autonomy loop is a single process that handles all proactive concerns. The trade-offs:

**What you lose:**
- Per-topic interval control ("check issues every 5 min, email every hour")
- Per-topic circuit breakers and failure isolation
- Clear separation of concerns in execution logs

**What you gain:**
- The agent prioritizes dynamically. If there's a CI failure, it focuses on that instead of dutifully running all N loops regardless of urgency.
- No user-facing complexity. No "cortex loop" concept to explain, no creation tool, no dashboard tab.
- Natural cross-concern reasoning. The agent can connect "Alice emailed about the contract" with "there's a PR pending review from Alice" in a single thought process, rather than having two separate loops that don't talk to each other.
- Bounded resource consumption. One branch + N workers per cycle, not N branches each spawning their own workers.
- The agent gets better over time. As it accumulates memories about what's relevant, it naturally spends more cycles on what matters and less on what doesn't.

**The prioritization concern is key.** With separate loops, each topic gets equal treatment regardless of urgency. With a single autonomy loop, the agent can look at everything it knows and decide "the CI failure is the most important thing right now, I'll check email later." This is closer to how human awareness actually works.

### Source-Tagged Memory Accumulation

The autonomy loop uses the source tag `cortex:autonomy` on all memories it creates. This requires making source tags functional in the memory system — today they're stored but never filtered on.

**Required changes to the memory system:**

1. Add `source: Option<String>` to `SearchConfig`
2. Add a `WHERE source = ?` clause to `MemoryStore::get_sorted()` when source is set
3. Add source post-filtering in `MemorySearch::hybrid_search()` (alongside the existing `memory_type` filter)
4. Add a `source` parameter to `MemoryRecallArgs` in the recall tool
5. Add a SQLite index on the `source` column: `CREATE INDEX idx_memories_source ON memories(source)`
6. Expose `source` in the API query structs (`MemoriesListQuery`, `MemoriesSearchQuery`)

The autonomy loop's recall phase uses source filtering to pull memories from previous cycles. But the memories also surface naturally through semantic recall — when a user asks "what's going on with Spacedrive issues?", autonomy loop memories about Spacedrive surface by relevance alongside memories from conversations.

### Interaction with Existing Systems

**Bulletin.** No changes needed. The bulletin already queries memories across all sources. Autonomy loop memories appear in the "Recent Memories," "Observations," "Events," and other bulletin sections based on type and recency. The bulletin is how the agent's autonomous findings reach every conversation.

**Ready-task pickup.** The autonomy loop creates tasks. The ready-task loop executes them. This is the natural flow: **autonomy loop monitors and discovers → creates task → task system executes**. Tasks created by the autonomy loop should default to `pending_approval` unless the agent's config allows auto-approval for autonomy-generated tasks.

**Cron jobs.** The autonomy loop builds awareness. Cron jobs deliver it on schedule. A morning briefing cron fires, branches to recall, and autonomy loop memories surface. No direct integration needed — the memory system bridges them.

**Email adapter.** Email channels already handle immediate triage (save memories about important emails, skip spam). The autonomy loop synthesizes across email memories: "5 emails today, 1 urgent from Alice about a contract deadline." The channel handles triage; the autonomy loop handles synthesis.

**Association loop.** Continues to run independently. Autonomy loop memories get the same auto-association treatment as all other memories.

### Configuration

Add to `CortexConfig`:

```rust
pub struct CortexConfig {
    // ... existing fields ...

    /// Whether the autonomy loop is enabled. Default: true.
    pub autonomy_enabled: bool,
    /// Interval between autonomy loop cycles in seconds. Default: 1800 (30 min).
    /// Minimum: 300 (5 min).
    pub autonomy_interval_secs: u64,
    /// Max LLM turns per autonomy cycle. Default: 15.
    pub autonomy_max_turns: usize,
    /// Max concurrent workers the autonomy loop can spawn per cycle. Default: 3.
    pub autonomy_max_workers: usize,
    /// Whether autonomy-created tasks require approval. Default: true.
    pub autonomy_tasks_require_approval: bool,
}
```

These are system settings, not user-facing configuration. They're tunable in `config.toml` or via the API but don't require a dedicated UI.

### System Prompt

New template: `prompts/en/autonomy_loop.md.j2`

```jinja
You are the autonomy process for {{ agent_name }}. You are the agent's proactive awareness — the part that thinks about what's going on without being asked.

This is not a conversation. There is no user present. You are background cognition, waking up periodically to stay informed, identify emerging issues, and prepare knowledge that will be useful when the user next interacts.

{% if identity_context %}
{{ identity_context }}
{% endif %}

{% if memory_bulletin %}
## Current Awareness
{{ memory_bulletin }}
{% endif %}

## Previous Cycle Findings
{{ recalled_context }}

## Active Tasks
{{ active_tasks }}

## Recent Activity
{{ recent_events }}

## Instructions

Based on your identity, role, and current awareness:

1. **Assess the current state.** What do you know? What has changed since your last cycle? What are the most important things happening right now?

2. **Decide what to investigate.** You have access to workers that can execute shell commands, check APIs, read files, browse the web. What would be most valuable to check right now? Prioritize by urgency and relevance to your role.

3. **Do the work.** Spawn workers to gather information. Review what they find. Connect it to what you already know.

4. **Save what you learn.** Every finding, observation, status change, or emerging pattern should be saved as a memory. Use appropriate memory types:
   - **event** — something that happened (CI failed, PR merged, email received)
   - **observation** — a pattern or trend you notice (build times increasing, certain contributor very active)
   - **fact** — a concrete piece of information (current issue count, deployment status)
   - **decision** — something you decided or concluded (this issue is urgent, this can wait)

5. **Create tasks for actionable items.** If you discover something that needs human attention or automated action, create a task on the board. Be specific about what needs to happen and why.

6. **Decide what to defer.** You can't check everything every cycle. Note what you're deferring and why, so you can pick it up next time.

Tag all memories with source "cortex:autonomy". Be efficient with your turn budget — spawn workers for the actual work, use your turns for reasoning and synthesis.
```

### Implementation

The autonomy loop is implemented as `spawn_autonomy_loop` alongside the existing four cortex loops. It follows the same pattern: a `tokio::spawn` that runs for the lifetime of the process.

```
spawn_autonomy_loop(deps, logger)
  └─ loop:
       1. Sleep for autonomy_interval_secs
       2. Check autonomy_enabled (hot-reload via ArcSwap)
       3. Acquire execution lock (prevent overlap)
       4. Assemble context:
          a. Load identity (from RuntimeConfig)
          b. Load bulletin (from RuntimeConfig)
          c. Load active tasks (from TaskStore)
          d. Query recent cortex events
          e. Recall memories with source "cortex:autonomy" (latest N)
       5. Create a branch:
          - System prompt: autonomy_loop.md.j2 with assembled context
          - Tools: memory_save, memory_recall, spawn_worker, task_create, task_list
          - Max turns: autonomy_max_turns
          - Worker limit: autonomy_max_workers
       6. Run the branch
       7. Force-save cycle summary memory (source: "cortex:autonomy")
       8. Log to cortex_events
       9. Circuit breaker: disable after N consecutive failures
```

The branch creation reuses the existing `Branch` infrastructure. The only new code is the runner function, the prompt template, and the context assembly.

## Stateful Cron Jobs

The second half of the design. Today, cron jobs are stateless — each run spins up a fresh channel with no memory of previous runs. This is fine for simple reminders ("remind me at 9am") but insufficient for recurring analytical tasks ("give me a morning briefing of what changed overnight").

### The Change

Add an optional `source_tag` field to `CronConfig`. When set, the cron job's channel automatically:

1. **Recalls memories** tagged with this source before executing the prompt
2. **Saves relevant findings** tagged with this source after execution
3. **Accumulates knowledge** across runs — each execution builds on the previous

```rust
pub struct CronConfig {
    // ... existing fields ...

    /// Optional source tag for memory continuity across runs.
    /// When set, the cron channel recalls memories with this source
    /// before execution and saves findings with this source after.
    /// Format: "cron:{id}" (auto-set from job ID if enabled).
    pub stateful: bool,  // default: false
}
```

When `stateful` is true, the source tag is automatically derived as `cron:{id}` (e.g., `cron:morning-briefing`). No user-specified tags — keep it simple.

### How It Works

When a stateful cron job fires, the existing `run_cron_job` flow is modified:

```
1. Create fresh channel (same as today)
2. NEW: If stateful, inject a recall preamble:
   "You are running a recurring scheduled task. Here is context
    from previous executions:"
   + recalled memories filtered by source "cron:{id}"
3. Send the cron prompt as a synthetic message (same as today)
4. Collect responses and deliver (same as today)
5. NEW: If stateful, spawn a brief memory-persistence branch
   that saves relevant findings with source "cron:{id}"
```

The recall happens before the prompt, injected as part of the system context. The save happens after delivery, as a fire-and-forget branch (same pattern as the existing `memory_persistence` branches that run after conversations). The cron job itself doesn't need to know about memory mechanics — it's handled by the runtime.

### Cron Tool Update

The `CronTool` gains a `stateful` parameter on the `create` action:

```
Arguments (create):
  id: String (required)
  prompt: String (required)
  interval_secs: u64 (optional, default 3600)
  delivery_target: String (required)
  active_hours: String (optional)
  run_once: bool (optional, default false)
  stateful: bool (optional, default false)    // NEW
```

The LLM decides whether a cron job should be stateful based on the user's request:

- "Remind me at 9am to check email" → stateful: false (one-shot reminder)
- "Give me a morning briefing every day" → stateful: true (needs to accumulate overnight context)
- "Check the build status every hour and ping me if it fails" → stateful: true (needs to know previous state to detect changes)

### Migration

```sql
ALTER TABLE cron_jobs ADD COLUMN stateful INTEGER NOT NULL DEFAULT 0;
```

### What This Replaces

In the custom cortex loops design, the suggested approach for a "morning briefing" was:

1. Create cortex loop `track-spacebot-repo` (monitors repo every 30 min)
2. Create cortex loop `monitor-email` (monitors email every 15 min)
3. Create cron job `morning-briefing` (delivers at 9am, recalls loop memories)

With the autonomy loop + stateful cron approach:

1. The autonomy loop already monitors things relevant to the agent's role (repo, email, etc.) — no user action needed
2. Create stateful cron job `morning-briefing` (delivers at 9am, recalls previous briefing memories + autonomy loop memories surface naturally via the bulletin)

The user creates one thing (the cron job) instead of three. The system handles the rest.

## How It All Fits Together

```
Identity files (SOUL.md, ROLE.md, etc.)
  │
  │ define what the agent cares about
  ▼
Autonomy Loop (runs every 30 min)
  │ proactively monitors relevant integrations
  │ saves findings as memories (source: "cortex:autonomy")
  │ creates tasks for actionable items
  │
  ├──► Memory system ◄── Email channels (triage incoming mail)
  │         │              Conversations (save user context)
  │         │              Compactor (extract from history)
  │         │
  │         ▼
  │    Bulletin (synthesizes all memories hourly)
  │         │
  │         ▼
  │    Every channel reads the bulletin
  │         │
  │         ▼
  │    User conversations have ambient awareness
  │
  ├──► Task board
  │         │
  │         ▼
  │    Ready-task pickup loop (executes autonomy-created tasks)
  │
  └──► Stateful cron jobs
          │ recall previous run memories + autonomy memories surface via bulletin
          │ deliver accumulated knowledge on schedule
          ▼
       User gets a morning briefing with overnight findings
```

## Example Scenarios

### Scenario 1: Overnight Monitoring + Morning Briefing

**Setup:**
- ROLE.md says: "You are the engineering assistant for the Spacebot project. You monitor the spacebot repo, track CI status, and keep the team informed."
- User creates a stateful cron job: "Every morning at 9am, give me a briefing of what happened overnight"

**Overnight:**
- Autonomy loop runs every 30 min:
  - Cycle 1: Recalls no previous findings (first run). Reads role.md — should monitor spacebot repo. Spawns worker to check GitHub. Finds 2 new issues, 1 merged PR. Saves as memories.
  - Cycle 2: Recalls cycle 1 findings. Spawns worker to check CI — main build passing. Checks issues again — 1 new comment on issue #423. Saves findings.
  - Cycle 3: Recalls cycles 1-2. CI still passing. No new issues. Defers to next cycle.
  - Cycle 4: Recalls previous cycles. New email from Alice about contract renewal (surfaced via email channel memories in bulletin). Notes this is important. Saves observation: "Alice's contract renewal needs attention by Friday."

**9am:**
- Morning briefing cron fires (stateful)
- Recalls its own previous briefing memories (context from last briefing)
- Bulletin has been refreshed with autonomy loop findings
- Channel branches to recall — autonomy memories surface by relevance
- Delivers: "Good morning. Overnight: 2 new issues on spacebot (#423 memory leak, #424 docs update), PR #420 merged (email adapter). CI is green. Alice emailed about her contract renewal — deadline is Friday."

### Scenario 2: Proactive Task Creation

**Setup:**
- Agent has been told it's responsible for CI health

**Autonomy cycle:**
- Recalls previous findings: CI was passing last cycle
- Spawns worker to check GitHub Actions — build failing for 2 hours
- Saves event memory: "CI failure on main: test_memory_search failing since commit abc123"
- Creates task: "Investigate CI failure: test_memory_search on main" (status: pending_approval, priority: high)
- Saves observation: "CI has been unstable this week — 3 failures in 5 days"

**Next bulletin refresh:**
- Bulletin picks up the CI failure event and the instability observation
- Every channel now has ambient awareness of the CI problem

**User opens chat:**
- Agent's channel prompt includes the bulletin with CI failure info
- User asks "what's going on?" — agent already knows, no recall needed

### Scenario 3: Email Synthesis

**Ongoing:**
- Email channels process incoming mail (already implemented on `feat/email-adapter`)
- Each email is triaged: save memories for important ones, skip spam
- Email memories accumulate: sender, subject, urgency, key content

**Autonomy cycle:**
- Recalls recent email-related memories (they surface by relevance alongside cortex:autonomy memories)
- Synthesizes: "5 emails today. 1 urgent from Alice (contract renewal, Friday deadline). 2 newsletters (skipped). 1 from Bob about the API redesign meeting next Tuesday. 1 GitHub notification (PR review requested)."
- Saves consolidated observation

**User asks "any important emails?":**
- Branch recalls autonomy observations about email
- Can also use `email_search` tool to read specific emails on demand
- Delivers curated summary without the user needing to check their inbox

## Phases

### Phase 1: Source Filter Infrastructure

- Add `source: Option<String>` to `SearchConfig`
- Add source filtering to `MemoryStore::get_sorted()` SQL queries
- Add source post-filtering in `MemorySearch::hybrid_search()`
- Add `source` parameter to `MemoryRecallArgs` and the API query structs
- Add SQLite index on `memories.source`
- This is prerequisite for both the autonomy loop and stateful cron

### Phase 2: Autonomy Loop

- `spawn_autonomy_loop` function in `cortex.rs`
- `autonomy_loop.md.j2` system prompt template
- Context assembly: identity + bulletin + tasks + recent events + recalled memories
- Branch creation with memory tools + worker spawning + task tools
- Forced cycle summary save (source: "cortex:autonomy")
- Execution logging to `cortex_events`
- Circuit breaker (disable after N consecutive failures)
- `CortexConfig` extensions (autonomy_enabled, autonomy_interval_secs, etc.)
- Wire into `main.rs` alongside existing cortex loops

### Phase 3: Stateful Cron Jobs

- Add `stateful` column to `cron_jobs` table (migration)
- Add `stateful` field to `CronConfig` and `CronJob`
- Modify `run_cron_job` to inject recalled memories when stateful
- Add post-execution memory persistence branch for stateful crons
- Update `CronTool` to accept `stateful` parameter on create
- Update cron tool description prompt to explain when to use stateful

### Phase 4: Polish

- Bulletin section showing autonomy loop status and recent findings
- SSE events for autonomy cycle state changes
- Smart model routing — use cheaper model for recall/save phases
- Concurrency limiting for autonomy-spawned workers
- Dashboard visibility: autonomy loop status in the cortex section (read-only, not configurable)

### Phase Ordering

```
Phase 1 (source filters)  — standalone, unblocks 2 and 3
Phase 2 (autonomy loop)   — depends on Phase 1
Phase 3 (stateful cron)   — depends on Phase 1, independent of Phase 2
Phase 4 (polish)           — depends on Phases 2 + 3
```

Phases 2 and 3 can run in parallel after Phase 1.

## Open Questions

**Autonomy loop prioritization.** How does the autonomy loop decide what to check each cycle? The prompt instructs it to prioritize by urgency and role relevance, but in practice the LLM might fall into a rut (always checking the same things). Options: (a) trust the LLM and let memory accumulation guide it naturally, (b) inject a "what haven't you checked recently?" nudge using the cycle summary from the previous run, (c) rotate through a checklist derived from the role. Starting with (a) + (b) seems right — the cycle summary already captures what was deferred.

**Autonomy loop depth vs. breadth.** Should the loop try to check everything each cycle (broad, shallow) or focus on one area per cycle (narrow, deep)? The prompt says "decide what to investigate" — leaving it to the LLM. A 30-minute interval with 15 max turns is enough for 2-3 worker spawns with reasoning around them. If the loop is too shallow, increase max_turns or decrease interval. This is a tuning problem, not an architectural one.

**Task approval for autonomy-generated tasks.** The design defaults to `pending_approval` for tasks created by the autonomy loop. This prevents the agent from auto-executing potentially expensive or destructive work. But it means someone has to approve tasks — which defeats the purpose of autonomous operation for trusted agents. The `autonomy_tasks_require_approval` config flag allows disabling this per-agent. High-trust deployments (personal assistants) might auto-approve; team deployments might require approval.

**Stateful cron recall budget.** How many memories should a stateful cron recall from previous runs? Too few and it loses continuity; too many and the channel's context fills up before the prompt even runs. A default of 10-15 most recent memories from `cron:{id}` is probably right, with a configurable cap.

**Autonomy loop and multi-agent.** In a multi-agent setup, each agent has its own autonomy loop. Should they coordinate? The multi-agent communication system (send_agent_message) handles cross-agent messaging, but the autonomy loops run independently. If two agents both monitor the same repo, they'll duplicate work. This is acceptable for now — the memory system is per-agent, and each agent needs its own awareness. Cross-agent deduplication is a future concern.

**Warmup gating.** The autonomy loop should respect the same `WorkReadiness` gates as branches and workers. It should not run until the warmup state is `Warm` and the first bulletin has been generated. This ensures it has a bulletin to reference on its first cycle.

**Cycle budget allocation.** With 15 max turns per cycle: how many should go to reasoning vs. worker spawns? Workers run asynchronously — the branch spawns them and they report back. If the branch spawns 3 workers at turn 2 and they all complete by turn 5, it has 10 turns to reason about results and save memories. If workers are slow, the branch might hit its turn limit before results arrive. The branch-worker interaction pattern in the existing codebase handles this (branches wait for workers or move on), but the autonomy loop prompt should instruct the LLM to spawn workers early in the turn budget.
