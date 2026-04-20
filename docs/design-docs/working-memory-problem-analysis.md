# Working Memory: Problem Analysis

This document is a comprehensive analysis of why Spacebot's current memory and context assembly system fails to provide situational awareness, particularly in multi-user, multi-channel environments. It catalogs every failure mode with evidence from live system prompts and conversation transcripts. It is the prerequisite to a design document.

## Executive Summary

Users report Spacebot feels "stupid and forgetful." People are churning. The root cause is not the LLM -- it is that the context assembly system starves the LLM of the information it needs to be smart. The agent has no concept of what happened today, no awareness of activity in other channels, no per-user adaptation, and no proactive memory capture. The bulletin system -- the agent's primary knowledge injection mechanism -- is a single LLM-synthesized blob regenerated on a timer whether anything changed or not, identical for every channel and every user.

The problems fall into seven categories:

1. The bulletin is a monolithic, undifferentiated blob
2. There is zero cross-channel awareness
3. Memory creation is too passive
4. There is no temporal structure
5. The bulletin wastes tokens regenerating stable information
6. Per-user context does not exist
7. The system does not scale to multi-user environments

---

## Problem 1: The Bulletin Is a Monolithic Blob

### What happens today

The cortex generates a single text artifact called the "memory bulletin." It queries eight predefined sections from the memory store (Identity, Recent, Decisions, High-Importance, Preferences, Goals, Events, Observations), then sends all raw results to an LLM with a synthesis prompt that says: "Organize by what's most actionable or currently relevant, not by the raw section headers" and "Do NOT reproduce the section headers from the input."

The result is a narrative paragraph with no internal structure. It is stored in `RuntimeConfig::memory_bulletin` via `ArcSwap` and injected into every channel's system prompt on every turn. Every channel for the same agent sees the exact same bulletin.

### Why this is a problem

**No structure means no scanning.** The channel LLM cannot quickly find "what am I working on right now" vs "what do I know about user X" vs "what happened today." It must read the entire blob linearly. In a 500-word bulletin, the one relevant fact for the current conversation is buried in context the LLM has to wade through.

**No differentiation means noise for everyone.** In the Discord agent with 1000+ memories, the bulletin dedicates an entire paragraph to one user's consciousness research (SEAL, SDFT, neuromorphic computing), weather reports, per-user formatting quirks ("address DarkSkyFullOfStars as my lover"), and corporate strategy filler ("strong market positioning across three distinct technology verticals"). All of this is injected when any user in any channel sends any message.

**No priority means the important gets buried.** In the personal agent, a critical action item ("create task board entries for open PRs") is sandwiched between a Vancouver weather report and a paragraph about "operational readiness." The agent then proceeded to lie about completing that action item -- the bulletin's cheerful "task board shows no pending items" provided false confidence.

### Evidence from live systems

**Personal agent bulletin** (~550 tokens): Contains the agent's role (already stated in Soul + Identity + Role), product descriptions (repeated 4x across the prompt), weather, a raw worker UUID, and one buried action item.

**Discord agent bulletin** (~900 tokens): Contains bug status from unknown dates, 7 user profiles out of 13+ active users, one user's research interests consuming 25% of the space, per-user formatting constraints for 6 users, and architectural evolution notes with no temporal anchoring.

---

## Problem 2: Zero Cross-Channel Awareness

### What happens today

Each channel operates in complete isolation. The system prompt's Conversation Context section contains exactly:

```
Platform: discord
Server: Spacedrive / Spacebot / Voicebox
Channel: #talk-to-spacebot
Multiple users may be present.
```

No other channels are mentioned. No activity summaries. No awareness of what happened in `#development` or `#general` or any DM. The status block shows active workers and branches, but only those associated with the current channel.

The only mechanism for cross-channel awareness is explicit: a branch can use `channel_recall` to query the persisted message database. But this requires the agent to (a) know it should look, (b) know where to look, and (c) have a free branch slot to do so. None of these conditions are met by default.

### Why this is a problem

**Information told in one channel is invisible in another.** If a user tells the agent something important in Channel A, then immediately goes to Channel B and references it, the agent has no idea. This is the single most complained-about behavior. Users expect a team member to know what it was just told. The agent does not.

**Pattern detection across channels is impossible.** If three users in three channels are all hitting the same bug, the agent in each channel treats it as an isolated report. There is no mechanism to surface "this is a pattern -- multiple users are reporting Copilot provider errors."

**Worker and branch activity in other channels is invisible.** A long-running coding worker in Channel A is invisible from Channel B. If a user in Channel B asks "what are you working on?" the agent can only see its local status block.

### Evidence from live systems

The Discord agent's conversation history shows users referencing prior conversations that happened in other channels. The agent consistently fails to connect these references because it has no ambient signal that activity occurred elsewhere. The `channel_recall` tool exists but is never used proactively -- the agent does not know there is something to recall.

---

## Problem 3: Memory Creation Is Too Passive

### Original baseline

At the time of this analysis, memories were created through three paths:

1. **Branch-initiated:** A branch decides to call `memory_save` during conversation processing. This depends on the LLM's judgment -- it may or may not decide something is worth saving.

2. **Compactor-initiated, now removed:** When context hit >80% capacity, the compactor spawned a worker that summarized old context AND extracted memories via `memory_save`. This only fired when the context window was nearly full.

3. **Periodic memory persistence branch:** Every 50 user messages (configurable), a special branch runs with the full conversation history and a prompt to extract important memories. It recalls first to avoid duplicates, then saves selectively.

### Why this was a problem

**Compaction is the wrong trigger for memory creation.** Compaction fires at >80% context capacity. For light conversations (a few messages per day), the agent may never hit this threshold. Days or weeks of conversation can pass with zero compaction-initiated memories. Memory creation should not depend on context pressure.

**50 messages is too high a threshold for persistence.** In a multi-user Discord, 50 messages might take 10 minutes during an active conversation. In a quiet DM, 50 messages might take a week. In neither case is the threshold appropriate. By the time the persistence branch runs, key context from early in the conversation may have been lost to compaction or simply too far back in the history for the LLM to give proper attention.

**Branch-initiated saving depends on LLM judgment.** The branch has to decide that something is worth remembering. In practice, branches focus on answering the user's question -- memory saving is a secondary concern. Many significant pieces of information are never explicitly saved because the branch is optimizing for response quality, not memory coverage.

**No event-driven capture exists.** When a worker completes a task, when a cron job fires, when a user makes an explicit decision ("let's go with option B"), when the agent makes an error and gets corrected -- none of these events trigger automatic memory creation. They are captured only if a branch happens to be running and decides to save them, or if they survive long enough in the conversation history to be caught by the periodic persistence branch.

### Evidence from live systems

The personal agent had a ~25-message interaction where it lied about creating tasks, got caught, retried with sandbox issues, gathered PR data, and still had not finished. Zero memories were saved from this exchange. Context was not at 80%, the persistence threshold of 50 messages was not hit, and no branch decided to save anything.

The Discord agent shows a user (bergabman) saying: "it seems you got your memory wiped, multiple wrong statements in that message while we talked about this before." The agent gave fundamentally wrong information about ChatGPT OAuth because prior interactions were not captured as memories.

---

## Problem 4: No Temporal Structure

### What happens today

The agent knows the current date and time from the status block:

```
Time: 2026-03-18 15:50:49 +00:00
```

That is the entirety of its temporal awareness. There is no concept of:

- What happened today
- What happened yesterday
- What the agent was doing an hour ago
- What the daily rhythm looks like (cron execution history, conversation volume)
- What conversations have been active today vs dormant

The bulletin mentions events with no dates. "v0.3.3 is live" -- when? "A formatting bug has emerged" -- when? "Fleet operations reports from March 10th and 13th" -- in the personal agent, this is the only temporal reference, and it is five and eight days stale.

### Why this is a problem

**No daily narrative means no situational awareness.** A real team member starts their day knowing what happened yesterday and what is on the agenda today. This agent starts every conversation from a timeless pool of memories. It cannot answer "what have we been working on today?" without branching to search.

**No temporal decay in context injection.** A memory from 30 seconds ago and a memory from 30 days ago are treated identically in the bulletin. The tiered memory design doc (working state + graph tiers) attempts to fix this with a 3-day working state window, but it operates at the individual memory level -- it does not provide a structured daily narrative.

**No session continuity.** When a user starts a new conversation, the agent has no awareness of "the last time we spoke, you were working on X." The `backfill_transcript` mechanism exists (loading recent history from the database on first message), but it provides raw messages without any narrative framing.

**Cron execution history is invisible.** The agent might have run 10 scheduled tasks today -- checking email, monitoring repos, running health checks. None of this activity is surfaced in context. The status block shows cron count but not what they did or when they last ran.

### What users expect

Users coming from tools like the triage document (`2026-03-18-SPACEBOT-TRIAGE.md`) expect the agent to have equivalent awareness. That document is structured temporally: here is what matters today, here are the immediate priorities, here is the backlog. The agent should be able to maintain and surface this kind of temporal structure autonomously.

---

## Problem 5: Token Waste on Stable Information

### What happens today

The bulletin is fully regenerated by an LLM on every cycle. The cycle runs on two overlapping timers:

- **Warmup loop:** Every `warmup.refresh_secs` (default 900s / 15 minutes)
- **Cortex bulletin loop:** Every `bulletin_interval_secs` (default 3600s / 1 hour), acts as fallback

There is no change detection. The cortex queries all eight memory sections, sends them to an LLM, and gets back a synthesized blob -- regardless of whether any memories have been created, updated, or accessed since the last generation.

Additionally, the bulletin re-synthesizes information that is already present elsewhere in the system prompt:

**Overlap between Soul/Identity/Role and Bulletin:**

| Information | Where it appears |
|------------|-----------------|
| Agent's role (COO, community ambassador) | Soul, Identity, Role, Bulletin |
| Product descriptions (Spacebot, Spacedrive, Voicebox) | Soul, Identity, Bulletin |
| GitHub star counts | Soul, Identity, Bulletin |
| Operational philosophy | Soul, Identity, Role, Bulletin |
| Authority/escalation rules | Identity, Role |

In the personal agent prompt, the same "I'm the COO of Spacedrive Technology Inc. with three products" information appears four times in four slightly different phrasings.

### Why this is a problem

**LLM cost scales with memory count, not with change rate.** An agent with 1000 memories pays the same synthesis cost whether 100 new memories were created since the last bulletin or zero were. The 8-section query pulls up to 85 memories per cycle (15+15+10+10+10+10+10+5), plus tasks. The synthesis call processes all of them every time.

**Stable information dominates the bulletin.** Identity facts, user preferences, product descriptions, and long-standing decisions change on a scale of weeks or months. Yet they are re-synthesized alongside genuinely volatile information (recent events, active tasks) every 15 minutes. The stable content pushes out volatile content because the word limit (default 500-1500 words) is shared.

**The agent has been idle? Regenerate anyway.** If the agent has not been used in 3 hours, the warmup loop still regenerates the bulletin every 15 minutes. Six unnecessary LLM calls burning tokens on identical output. The cortex has no concept of "nothing has changed."

### Quantifying the waste

At the default cadence of 15-minute warmup refreshes: **96 bulletin generations per day**. If each synthesis call uses ~2000 input tokens (raw sections) + ~500 output tokens, that is ~240,000 tokens per day per agent. For an agent that is only active for 4 hours a day, roughly 80% of those generations are wasted on periods of zero activity.

---

## Problem 6: Per-User Context Does Not Exist

### What happens today

The bulletin is global and agent-centric. It says nothing about who the agent is currently talking to. Every user in every channel sees the same bulletin.

In the Discord agent, 7 out of 13+ active users get profile mentions in the bulletin. The rest are invisible. When RAKU asks "do you know me?" the answer is no -- despite having had prior conversations.

The only per-user adaptation mechanism is the `channel_recall` tool (available to branches), which can search the conversation database for messages from a specific user. But this requires branching, which adds latency and consumes a limited branch slot.

Two design docs address this:

- **`participant-awareness.md`:** Proposes a `humans` table with cortex-generated per-user summaries, injected into channels with multiple users. Not implemented.
- **`user-scoped-memories.md`:** Proposes a `user_id` column on memories and scoped search. Not implemented.

Neither design integrates with the bulletin. Both add parallel injection points into the system prompt, creating a layered context assembly that could become unwieldy.

### Why this is a problem

**The agent cannot adapt to who it is talking to.** When bergabman (a power user debugging OAuth) sends a message, the agent gets the same context as when a new user asks "what is Spacebot?" There is no mechanism to surface bergabman's prior conversations, his technical level, his specific configuration, or his recent activity.

**In multi-user channels, the agent cross-contaminates context.** When Kael, bergabman, and okuna are all talking in `#talk-to-spacebot` simultaneously, the agent is tracking three interleaved conversations with no separation. It may reference Kael's consciousness research when replying to okuna's Docker question.

**The bulletin's user profile section scales terribly.** With 7 profiles at ~50 words each, that is 350 words -- 70% of a 500-word bulletin -- dedicated to user descriptions that are only relevant when that specific user is talking. In a server with 100 users, this section would need to be 50x larger to cover everyone, which is obviously impossible within token budgets.

---

## Problem 7: Multi-User/Multi-Channel Scaling Failure

### What breaks under load

Consider a realistic scenario: a Slack workspace with 10 engineers, 5 channels, and moderate activity (each channel gets 20-30 messages per day).

**Token budget collapses.** The system prompt (Soul + Identity + Role + Bulletin + Channel instructions + Status block + Skills + Worker types) is ~6,300+ tokens before conversation history. In a fast-moving channel, history fills the remaining context rapidly. Older messages -- including those containing important context -- are pushed out.

**Worker/branch contention.** Max 3 workers and max 3 branches are shared across the entire agent, not per-channel. When User A's coding worker in Channel 1 is running and User B's research branch in Channel 2 is active, User C in Channel 3 has reduced capacity. Users experience delayed or missing responses with no explanation.

**Bulletin staleness scales with activity.** The bulletin regenerates on a timer, not on events. In a 5-channel setup with continuous activity, the bulletin is stale the moment it is created. By the next regeneration (15 minutes later), dozens of events have occurred that are invisible to channels that did not directly participate.

**Memory recall contention.** Every `memory_recall` call goes through the same search pipeline (SQLite + LanceDB). With 1000+ memories, search is fast but not free. Multiple concurrent branches all recalling simultaneously creates IO contention on the embedded databases.

**The compactor compounds the problem.** Active channels hit the compaction threshold faster. Compaction extracts memories and summarizes context, but the extracted memories are global (no user_id), the summaries are channel-specific, and the bulletin does not immediately reflect the extracted memories (it waits for the next regeneration cycle).

### Evidence from live systems

The Discord agent's conversation transcript shows:

- The agent retrying failed tool calls (spawn_worker) five consecutive times for the same user
- Users being told "I don't know you" despite prior interactions
- Users receiving wrong information because prior context was not surfaced
- The agent struggling with interleaved multi-user conversations

---

## The Compaction Memory Path Was Sunset

The original compactor had two jobs: (1) summarize old context to free up space, and (2) extract memories from the context being compacted. Job 1 is fine and remains. Job 2 was the wrong place for memory creation and has since been removed, for several reasons:

**It only fires under context pressure.** If the agent has a light conversation (20 messages), compaction never triggers. Those 20 messages may contain critical information that is never persisted.

**It fires too late.** By the time context is 80% full, the oldest messages being compacted are already stale. The most important information -- decisions, corrections, commitments made early in the conversation -- may have been made dozens of messages ago.

**The compactor LLM has the wrong incentive.** Its primary job is to produce a compact summary. Memory extraction is a secondary objective bolted onto the compaction prompt. The LLM optimizes for summary quality, not memory coverage.

**Compactor-extracted memories have no user attribution.** The compactor strips sender information from the transcript before processing. Memories extracted during compaction are always global, with no `user_id` or `channel_id` context.

Memory creation should be proactive, event-driven, and happen at the point of creation -- not as a side effect of context pressure management.

---

## The Tiered Memory Design Doc: Right Idea, Wrong Scope

The existing `tiered-memory.md` design doc introduces two tiers:

- **Working state** (hot, 3-day TTL, max 64 memories, injected into context without search)
- **Graph** (warm, 30-day retention with decay, hybrid search)

This is the right instinct -- recent memories should be immediately available without search. But it solves a different problem than what users are asking for:

**Tiered memory is about individual memory lifecycle.** A memory enters working state, stays hot for 3 days while accessed, then demotes to graph tier. This improves recall for recently-saved memories.

**Working memory (what users want) is about temporal narrative.** Users want the agent to know "what happened today." Not just "here are 64 recent memory objects sorted by access time," but a coherent narrative: "Today, Jamie asked me to create task board entries for open PRs. I failed the first attempt. We discussed sandbox issues. I gathered PR data via GitHub CLI. Meanwhile, in #development, vsumner submitted a fix PR. In #general, there was a discussion about the next release." That is a journal, not a memory tier.

The two systems are complementary, not competing:

| Concern | Tiered Memory | Working Memory |
|---------|--------------|----------------|
| Unit | Individual memory objects | Daily narrative entries, channel summaries |
| Structure | Flat set with LRU eviction | Append-only temporal log |
| Lifecycle | 3-day TTL, access-based | Day-bounded, with progressive rollups |
| Injection | Programmatic rendering of top-N | Structured sections in context |
| Creation | Same as today (branch, persistence) | Event-driven + cortex synthesis |
| Purpose | Better recall of recent facts | Situational awareness of what is happening |

The tiered memory design should proceed as planned -- it improves memory retrieval. But it does not replace the need for a structured temporal working memory system.

---

## The Bulletin System: Right Idea, Wrong Execution

The bulletin was designed to give channels ambient knowledge without requiring them to branch and search. That is the right idea. The execution fails because:

1. **One artifact for all channels, all users, all contexts.** The bulletin cannot be relevant to everyone. In a multi-user Discord, it tries to serve Kael's consciousness research and okuna's Docker questions with the same 500 words. It serves neither well.

2. **Timer-based regeneration with no change detection.** Regenerates every 15 minutes whether anything changed or not. Wastes LLM tokens on identical output during idle periods.

3. **Full LLM synthesis every cycle.** Stable information (identity, long-standing facts, user preferences) is re-synthesized alongside volatile information (recent events, active tasks). The stable parts should be programmatically rendered and cached.

4. **No temporal framing.** The bulletin has no concept of "today" vs "this week" vs "historical." Events from 5 days ago and events from 5 minutes ago occupy the same undifferentiated narrative.

5. **No per-channel activity awareness.** The bulletin says nothing about what is happening in specific channels. Cross-channel awareness requires explicit branching and searching.

6. **Word limit forces competition.** At 500-1500 words, stable identity content competes with volatile events for space. A weather report displaces a key decision. A user's formatting preference displaces a bug pattern.

### What the bulletin should become

The bulletin should be decomposed into independent, structured sections with different update cadences and different rendering strategies:

| Section | Update Trigger | Rendering |
|---------|---------------|-----------|
| Identity/long-term context | Config change, identity file edit | Programmatic (no LLM) |
| Today's working memory | Event-driven (new memories, worker completions, key decisions) | LLM synthesis of recent events only |
| Channel activity summaries | Event-driven (new messages in other channels) | Programmatic (last N messages + topic) |
| Per-user context | When a specific user sends a message | Programmatic (recall user-scoped memories) |

Each section is independently cached and only regenerated when its inputs change. The system prompt assembles them from cached sections, not from a single monolithic generation.

---

## Summary of All Failure Modes

| # | Problem | Impact | Severity |
|---|---------|--------|----------|
| 1 | Monolithic bulletin blob | Important info buried in noise | High |
| 2 | Zero cross-channel awareness | Info told in one channel invisible in another | Critical |
| 3 | Passive memory creation | Significant conversations produce zero memories | Critical |
| 4 | No temporal structure | Agent cannot answer "what happened today" | High |
| 5 | Token waste on stable info | 80%+ of bulletin generations are redundant | Medium |
| 6 | No per-user context | Agent cannot adapt to who it is talking to | High |
| 7 | Multi-user scaling failure | Worker/branch contention, context collapse | High |
| 8 | Compaction as memory path | Wrong trigger, wrong timing, no attribution | Medium |
| 9 | Bulletin covers all concerns in one blob | Stable and volatile content compete for space | High |
| 10 | No change detection before regeneration | Idle agents waste LLM tokens continuously | Medium |

---

## What This Analysis Does NOT Cover

This document is a problem analysis, not a design. The following questions are deferred to the design document:

- What is the schema for working memory entries?
- How does the cortex decide what goes into working memory vs the existing memory graph?
- What is the exact context assembly layout (section order, token budgets)?
- How do topics (PR #287, cortex topic synthesis) interact with working memory?
- What are the right defaults for update cadences, detail levels, and rollup intervals?
- How does per-user context scale in a 500-person server?
- What is the migration path from the current bulletin to the new system?

These are design decisions that should be made after this problem analysis is reviewed and agreed upon.
