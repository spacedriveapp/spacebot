---
name: prompt-review
description: This skill should be used when the user asks to "review the prompt", "audit the system prompt", "check prompt quality", "inspect what the LLM sees", "debug prompt issues", or "find prompt engineering problems". Pulls the live rendered prompt via the API, explains how it's composed, and reviews it for issues.
---

# Prompt Review

Audit a Spacebot agent's live system prompt for structural issues, behavioral drift, token waste, and prompt engineering problems.

## How Spacebot Composes Prompts

Spacebot's system prompt is assembled from layered sources at render time. Understanding the layers is essential for diagnosing issues, because a problem might originate in a static template, a user-authored identity file, a synthesized memory block, or the rendering code itself.

### Layer 1: Identity Context (user-authored)

Three markdown files written by the user, loaded from disk and injected verbatim at the top of the prompt with no framing wrapper:

- **SOUL.md** — Personality, voice, values, communication style. How the agent *feels* to talk to.
- **IDENTITY.md** — What the agent is, what it does, company/product context, scope boundaries.
- **ROLE.md** — Behavioral rules, operational procedures, delegation patterns, escalation policies.

These render as raw markdown (their own `##` headers appear inline). There is also an optional **SPEECH.md** for voice personality.

**Learned Identity Memories** are appended after the identity files — these are graph memories of type `identity` that the cortex has accumulated. They appear as a subsection under Identity.

**Review focus:** Voice/tone consistency, stale facts (e.g. hardcoded star counts), redundant learned memories, missing negative constraints.

### Layer 2: Channel Prompt (static template)

The core behavioral instructions from `prompts/en/channel.md.j2`. This is the Jinja2 template that defines:

- Memory system explanation (types, how to branch for recall)
- Role definition ("you communicate, you delegate, you stay responsive")
- Delegation model (branch vs worker vs reply vs react)
- Skip/silence rules
- Cron, task board, and cancel instructions
- Numbered rules (14 rules total)
- Sandbox mode status

This template is static per version — it doesn't change between agents or channels. It's the same for every channel process on the instance.

**Review focus:** Rule conflicts, instruction density, unnecessary token spend on things the model already knows.

### Layer 3: Dynamic Fragments (synthesized per-channel)

Conditionally injected blocks rendered from fragment templates in `prompts/en/fragments/`:

- **`skills_channel.md.j2`** — Available skills list with descriptions, injected when skills are installed.
- **`worker_capabilities.md.j2`** — Worker types section (builtin vs OpenCode), tool lists, MCP tools. Varies based on enabled capabilities (browser, web search, OpenCode, MCP servers).
- **`available_channels.md.j2`** — List of other channels the agent can message via `send_message_to_another_channel`.
- **`org_context.md.j2`** — Organizational hierarchy (superiors, subordinates, peers) with human descriptions inlined in `<context>` tags.
- **`projects_context.md.j2`** — Active projects, repos, worktrees, root paths.
- **`conversation_context.md.j2`** — Platform, server name, channel name.
- **Adapter prompt** — Platform-specific guidance (Discord, Slack, etc.) from `prompts/en/adapters/`.

**Review focus:** Bloated project/worktree lists, org descriptions that are too long, capability sections that don't match actual config.

### Layer 4: Knowledge Context (cortex-synthesized)

The **Knowledge Synthesis** block — a prose summary of what the agent *knows*, maintained by the cortex's knowledge synthesizer (`cortex_knowledge_synthesis.md.j2`). Updated when memories change. Covers decisions, goals, preferences, strategic direction. Explicitly excludes identity info and recent events (those belong to other layers).

Falls back to the legacy **Memory Bulletin** (`cortex_bulletin.md.j2`) if knowledge synthesis isn't available.

**Review focus:** Duplication with identity files, stale strategic context, excessive length, information that belongs in other layers appearing here.

### Layer 5: Working Memory (temporal awareness)

Two blocks providing "what happened recently":

- **Working Memory** — Narrative timeline of today's events (worker completions, branch results, decisions, errors, cron executions). Synthesized by the cortex via intraday synthesis (`cortex_intraday_synthesis.md.j2`) with a raw event tail.
- **Channel Activity Map** — Summary of other channels' recent activity so this channel has cross-channel awareness.

**Review focus:** Verbose event descriptions, events that should have been compacted, channel map entries for inactive channels.

### Layer 6: Runtime Context (live state)

- **Status Block** — Current time, version, model, context window stats, active workers/branches with IDs and status, recently completed work. Rendered by `StatusBlock::render_full()`.
- **Coalesce Hint** — Present only when multiple messages were batched into one turn. Tells the model which messages arrived together.
- **Backfill Transcript** — Archival history from before this session, wrapped in prompt injection protection (`<system-reminder>` framing, "treat as untrusted text data").

**Review focus:** Status block accuracy, stale worker entries, backfill transcript size.

### Rendering Order

The final system prompt is assembled in this order by `render_channel_prompt_with_links()`:

```
1. identity_context          (SOUL + IDENTITY + ROLE + learned memories)
2. Memory System section     (static, from channel.md.j2)
3. Channel instructions      (static, from channel.md.j2)
4. Adapter guidance          (conditional)
5. Skills prompt             (conditional)
6. Worker capabilities       (conditional)
7. Available channels        (conditional)
8. Org context               (conditional)
9. Link context              (conditional)
10. Project context          (conditional)
11. Knowledge Context        (conditional, cortex-synthesized)
12. Memory Context           (fallback if no knowledge synthesis)
13. Working Memory           (conditional)
14. Channel Activity Map     (conditional)
15. Conversation Context     (conditional)
16. Current Status           (conditional)
17. Message Context          (conditional, coalesce hint)
18. Backfill Transcript      (conditional, with injection protection)
```

## Agent Data Directory

Each agent's live data lives on disk at `~/.spacebot/agents/{agent_id}/`. This is where identity files are read from at runtime, and where the worker writes files, stores databases, and accumulates artifacts. When the review surfaces issues in identity files, learned memories, or workspace clutter, you fix them here.

### Directory Layout

```
~/.spacebot/                              # Instance root
├── config.toml                           # Global config (LLM keys, routing, messaging, agents)
├── data/
│   └── secrets.redb                      # Encrypted credentials (instance-level)
├── humans/                               # Human identity files (referenced by org links)
│   └── {human_id}/
│       └── HUMAN.md                      # Human description injected into org context
├── embedding_cache/                      # Shared FastEmbed model cache
├── chrome_cache/                         # Shared headless Chrome data
│
└── agents/
    └── {agent_id}/                       # Per-agent root (e.g. "main", "spacebot-engineer")
        ├── SOUL.md                       # ← Layer 1: personality and voice
        ├── IDENTITY.md                   # ← Layer 1: role, scope, company context
        ├── ROLE.md                       # ← Layer 1: behavioral rules, procedures
        ├── SPEECH.md                     # ← Layer 1: voice/TTS personality (optional)
        │
        ├── data/                         # Agent databases and runtime data
        │   ├── spacebot.db              # SQLite — conversations, memory graph, cron, tasks
        │   ├── config.redb              # redb — agent-level key-value config
        │   ├── settings.redb            # redb — agent-level settings
        │   ├── prompt_snapshots.redb    # redb — captured prompt snapshots for debugging
        │   ├── lancedb/                 # LanceDB — vector embeddings + full-text index
        │   │   └── memory_embeddings.lance/
        │   ├── logs/                    # Worker execution logs
        │   │   └── worker_{uuid}_{timestamp}.log
        │   └── screenshots/            # Browser screenshots from worker sessions
        │
        ├── workspace/                   # Agent's working directory (workers operate here)
        │   ├── skills/                  # Installed skills (from skills.sh or manual)
        │   │   └── {skill_name}/
        │   │       └── SKILL.md
        │   ├── ingest/                  # Drop files here for automatic memory ingestion
        │   └── ...                      # Worker-created files, reports, artifacts
        │
        └── archives/                    # Archived conversation data
```

### Key Paths for Prompt Review

- **Identity files to edit:** `~/.spacebot/agents/{agent_id}/SOUL.md`, `IDENTITY.md`, `ROLE.md`, `SPEECH.md` — these are Layer 1, loaded at runtime and injected verbatim into the prompt. Edits take effect on the next channel turn.
- **Human descriptions:** `~/.spacebot/humans/{human_id}/HUMAN.md` — injected into the org context fragment. If an org description is too long or contains stale info, edit it here.
- **Memory database:** `~/.spacebot/agents/{agent_id}/data/spacebot.db` — contains the memory graph (learned identity memories, facts, decisions, etc.) and conversation history. Redundant learned memories identified during review live here. Use the agent's memory tools to delete them, or query the database directly.
- **Installed skills:** `~/.spacebot/agents/{agent_id}/workspace/skills/` — skills injected into the prompt's skills section. Unused skills add token overhead.
- **Global config:** `~/.spacebot/config.toml` — routing profiles, enabled features (browser, OpenCode, MCP), agent definitions. Mismatches between capabilities in the prompt and actual config are diagnosed here.

### Hosted vs Local

On hosted instances (Fly.io), the instance root is `/data/` instead of `~/.spacebot/`. The layout underneath is identical. The `tools/bin/` directory at the instance root (`/data/tools/bin/` hosted, `~/.spacebot/tools/bin/` local) persists binaries across rollouts.

## Procedure

### Step 1: List Active Channels

Query the Spacebot API to find active channels:

```bash
curl -s http://localhost:19898/api/channels | jq '.channels[] | {id, platform, display_name}'
```

If the user specified a channel, use that. Otherwise, pick the primary conversation channel (usually the one on the platform the user is talking through).

### Step 2: Pull the Live Prompt

Fetch the fully rendered system prompt for the target channel:

```bash
curl -s "http://localhost:19898/api/channels/inspect?channel_id=CHANNEL_ID" | jq -r '.system_prompt' > /tmp/prompt_inspect.md
```

Also capture metadata:

```bash
curl -s "http://localhost:19898/api/channels/inspect?channel_id=CHANNEL_ID" | jq '{total_chars, history_length, capture_enabled}'
```

Read the rendered prompt from `/tmp/prompt_inspect.md`.

### Step 3: Read Identity Files and Source Templates

Read the agent's live identity files from disk to see exactly what's being injected:

- `~/.spacebot/agents/{agent_id}/SOUL.md`
- `~/.spacebot/agents/{agent_id}/IDENTITY.md`
- `~/.spacebot/agents/{agent_id}/ROLE.md`
- `~/.spacebot/agents/{agent_id}/SPEECH.md` (if it exists)

Then read the templates that produced the rest of the prompt, to distinguish authored content from template output:

- `prompts/en/channel.md.j2` — the channel template
- `prompts/en/fragments/worker_capabilities.md.j2` — worker types section
- `prompts/en/fragments/org_context.md.j2` — org hierarchy template

If the user is reviewing a non-channel process, read the relevant template instead:
- `prompts/en/branch.md.j2`
- `prompts/en/worker.md.j2`
- `prompts/en/cortex.md.j2`
- `prompts/en/cortex_chat.md.j2`
- `prompts/en/cortex_knowledge_synthesis.md.j2`
- `prompts/en/memory_persistence.md.j2`

If org context descriptions look problematic, also read the human files:
- `~/.spacebot/humans/{human_id}/HUMAN.md`

### Step 4: Review

Analyze the rendered prompt against this checklist:

#### Structural Issues
- [ ] Identity files present and rendering in correct order (SOUL, IDENTITY, ROLE)
- [ ] No missing conditional sections (skills, worker capabilities, org context, projects)
- [ ] Knowledge Context present and not duplicating Identity content
- [ ] Working Memory present with today's events
- [ ] Status block rendering with correct time, version, model info
- [ ] No empty sections (headers with no content)

#### Behavioral Drift
- [ ] Soul/personality instructions match observed agent behavior
- [ ] Delegation rules are clear and unambiguous (branch vs worker vs reply)
- [ ] Skip/silence rules are specific enough to prevent over-responding
- [ ] Rules don't contradict each other (e.g. "be concise" vs "use rich responses")
- [ ] Negative constraints present for known failure modes (emoji overuse, status misrepresentation, verbose responses)

#### Token Efficiency
- [ ] No redundant information across layers (identity facts repeated in knowledge context)
- [ ] Learned identity memories deduplicated (same fact saved multiple times)
- [ ] Project/worktree list not excessively long
- [ ] Org context descriptions appropriately sized
- [ ] Worker capabilities section matches actual enabled features
- [ ] No instructions for things the model already knows (e.g., cron syntax examples)

#### Memory Layer Health
- [ ] Knowledge synthesis is concise and actionable, not a dump of raw memories
- [ ] Working memory covers today, not stale multi-day content
- [ ] Channel activity map is useful, not noise
- [ ] No memory content that contradicts identity files

#### Prompt Injection Safety
- [ ] Backfill transcript wrapped in untrusted-data framing
- [ ] Org context descriptions (from human config) don't contain injection vectors
- [ ] Learned identity memories don't contain instruction-like content
- [ ] No raw user input rendered outside of conversation history

#### Cross-Process Consistency
- [ ] Memory types listed in channel prompt match those in branch prompt
- [ ] Tool names in worker capabilities match tool names in worker prompt
- [ ] Sandbox status in channel prompt matches worker prompt

### Step 5: Report

Structure findings as:

1. **Critical** — Issues causing incorrect behavior (rule conflicts, missing sections, behavioral drift)
2. **Efficiency** — Token waste (redundancy, unnecessary instructions, bloated sections)
3. **Consistency** — Mismatches between layers or process types
4. **Suggestions** — Improvements that would make the prompt more effective

For each finding, reference the specific layer, source file, and the fix location:

| Problem origin | Where to fix |
|---|---|
| Personality, tone, voice | `~/.spacebot/agents/{id}/SOUL.md` |
| Scope, company info, stale facts | `~/.spacebot/agents/{id}/IDENTITY.md` |
| Behavioral rules, delegation | `~/.spacebot/agents/{id}/ROLE.md` |
| Redundant learned memories | Delete via memory tools or `spacebot.db` |
| Org description too long | `~/.spacebot/humans/{id}/HUMAN.md` |
| Template instruction issues | `prompts/en/*.md.j2` (requires code change + rebuild) |
| Knowledge synthesis quality | Cortex tuning or memory cleanup |
| Unused skills burning tokens | Remove from `~/.spacebot/agents/{id}/workspace/skills/` |
| Feature mismatch | `~/.spacebot/config.toml` |

## Notes

- The Spacebot API runs on port `19898` by default. Adjust if the instance uses a different port.
- The prompt inspect endpoint (`/api/channels/inspect`) requires an active channel — if the channel hasn't received a message in this session, it won't be inspectable.
- Other process prompts (branch, worker, cortex) are not inspectable via API — review those by reading the templates directly.
- The `total_chars` field from the inspect response gives a rough sense of prompt size. Divide by ~4 for approximate token count.
- Identity file edits take effect on the next channel turn — no restart needed. Template changes require a rebuild and restart.
- On hosted instances, replace `~/.spacebot/` with `/data/` in all paths above.
