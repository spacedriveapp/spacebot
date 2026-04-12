# Skill Authoring

Spacebot already has a skills system: agents load skills from disk, inject a summary into the channel prompt, and workers receive full skill content to follow. Skills can be installed from the skills.sh registry or uploaded as zip files. What's missing is any path for an agent to **write a skill** — capturing reusable approaches as they emerge.

Skills are structured markdown files. An agent writing one is no different from a user installing one or uploading a zip. Same format, same load path, same effect. The only thing missing is the write path and the trigger.

This doc covers three additions:

1. **`write_skill` tool** — a Branch-accessible tool to create or update workspace skills
2. **Post-conversation reflection** — a background Branch that fires after conversations to decide what (if anything) to save
3. **`CreateSkill` UI** — an AI-assisted skill creation surface in the interface

---

## What Already Exists

The skills infrastructure is largely in place:

- `src/skills.rs` — `SkillSet` loads from `{instance_dir}/skills/` and `{workspace}/skills/`, parses YAML frontmatter, resolves `{baseDir}` references
- `src/tools/read_skill.rs` — workers call this to get full skill content by name
- `src/tools/install_skill.rs` — installs from GitHub spec, reloads `RuntimeConfig.skills` immediately
- `src/api/skills.rs` — list, content, install, remove, upload, registry browse/search endpoints
- `interface/src/components/skills/` — full UI: sidebar, directory, inspector, bundled skills, rows
- `interface/src/routes/AgentSkills.tsx` — route container wiring mutations and queries

What doesn't exist: any path for an agent to **write** a skill to its workspace.

---

## 1. `write_skill` Tool

A new tool available to **branches only** (same access level as `memory_save`). Channels delegate to a branch when they want to save a skill; branches call the tool directly.

### Input

```rust
struct WriteSkillInput {
    /// Skill name. Becomes the directory name (slugified). Used as the skill's
    /// display name in the index. Must be unique within the workspace — if a
    /// skill with this name exists, it is overwritten.
    name: String,

    /// One-line description injected into the channel prompt index.
    description: String,

    /// Full skill content (markdown body, no frontmatter — the tool writes
    /// frontmatter). Should follow the standard skill format: When to Use,
    /// Procedure, Pitfalls, Verification sections.
    content: String,

    /// Optional list of tags for future search/filtering.
    #[serde(default)]
    tags: Vec<String>,
}
```

### Behavior

1. Slugify `name` → `skill_name` (lowercase, spaces to hyphens)
2. Resolve the agent's skills dir: `{instance_dir}/skills/{skill_name}/`
3. Create or overwrite `SKILL.md` with YAML frontmatter + content body:

```markdown
---
name: {name}
description: {description}
tags: [{tags}]
---

{content}
```

4. Reload the agent's `SkillSet` in `RuntimeConfig` (same reload path as `install_skill`)
5. Return the skill name and file path on success

**Constraints:**
- Writes to the agent's own skills directory. Cross-agent skill sharing is out of scope.
- Create and overwrite are the same operation — no separate patch. The agent rewrites the skill with improved content.
- The tool does not delete skills. Deletion is user-initiated via the API or UI.

### Tool Registration

`write_skill` is added to the branch `ToolServer` alongside `memory_save`. It is not available to channels, workers, or cortex.

---

## 2. Post-Conversation Reflection

After a conversation ends — or goes idle for a configurable window — a background Branch runs to review the conversation and decide whether to save skills or memories.

### Trigger Conditions

A reflection branch is eligible to fire when:

- The channel receives no new message for `reflection_idle_secs` (default: 300 seconds / 5 minutes)
- **And** at least `reflection_min_turns` user turns occurred in the conversation (default: 6)
- **And** no reflection has run for this conversation in the last `reflection_cooldown_secs` (default: 3600 / 1 hour)

All three thresholds are configurable in `config.toml` under `[reflection]`. Setting `reflection_idle_secs = 0` disables reflection entirely.

The cooldown prevents multiple reflections per conversation for long-running interactive sessions. The minimum turn count prevents reflection on short exchanges that are unlikely to contain reusable knowledge.

### What the Reflection Branch Does

The branch receives the full conversation history (identical to how a normal branch works — it's a clone of channel history at trigger time). It runs with a capped turn budget (max 6 turns) and a focused system prompt:

```
Review the conversation above. Consider two things:

**Skills**: Was a non-trivial workflow used that required multiple steps,
trial and error, or problem-solving? If so, and if that workflow would be
reusable in future sessions, save it as a skill using write_skill. Only
save skills that represent genuinely reusable procedures — not one-off
tasks or simple requests.

**Memories**: Did the user reveal preferences, constraints, or facts about
themselves or their environment that would reduce the need for future
clarification? If so, save them using memory_save.

If nothing is worth saving, output only: "Nothing to save." and stop.
Do not explain your reasoning unless you take an action.
```

**Tools available:** `write_skill`, `memory_save`, `memory_recall` (to avoid duplicating existing memories).

**Silent by default.** The branch result is not injected into the channel's history and produces no user-visible output. The agent runtime logs the reflection outcome (skill/memory saved, or nothing) at `DEBUG` level.

### Implementation Notes

The idle timer lives on the channel. When the channel's message loop has been waiting longer than `reflection_idle_secs` without receiving a message, it checks the cooldown and minimum turn count, then spawns the reflection branch if eligible. This is structurally similar to how the compactor monitors context size — it's a threshold check in the channel's tick loop, not a separate process.

The branch runs on the same concurrency budget as normal branches (`max_concurrent_branches`). If the channel is already at its branch limit, the reflection is skipped for this idle window and retried on the next.

---

## 3. `CreateSkill` UI

The `CreateSkill` component is currently a "coming soon" stub. It becomes an AI-assisted skill authoring surface.

### User Flow

The user describes a skill in plain language. The agent generates the skill content, shows a preview, and the user can edit before saving.

```
┌──────────────────────────────────────────────┐
│ Create Skill                                  │
│ Describe a skill and let the agent generate  │
│ it for you.                                  │
├──────────────────────────────────────────────┤
│ ┌────────────────────────────────────────┐   │
│ │ What should this skill do?             │   │
│ │                                        │   │
│ │ e.g. "Deploy a Next.js app to Fly.io"  │   │
│ └────────────────────────────────────────┘   │
│                          [Generate Skill →]  │
├──────────────────────────────────────────────┤
│ Preview (editable)                           │
│ ┌────────────────────────────────────────┐   │
│ │ ---                                    │   │
│ │ name: deploy-nextjs-fly                │   │
│ │ description: Deploy Next.js to Fly.io  │   │
│ │ ---                                    │   │
│ │                                        │   │
│ │ # Deploy Next.js to Fly.io             │   │
│ │ ...                                    │   │
│ └────────────────────────────────────────┘   │
│                                              │
│                      [Cancel]  [Save Skill]  │
└──────────────────────────────────────────────┘
```

### States

- **Empty** — prompt input, generate button
- **Generating** — spinner, streaming skill content into preview pane
- **Preview** — editable textarea showing generated SKILL.md, save/cancel buttons
- **Saving** — calls `POST /agents/skills/write`, shows loading state
- **Success** — dismisses to sidebar with new skill selected; query invalidation refreshes installed list

### New Backend Endpoint

```
POST /agents/skills/write
Body: { agent_id, name, description, content }
Response: { name, description, file_path, base_dir, source, source_repo }
```

This is the same logic as `write_skill` tool but exposed over HTTP for the UI. It writes to `{instance_dir}/skills/{slug}/SKILL.md` and reloads the skill set.

The generate step calls an existing agent endpoint (e.g., a one-shot branch or the cortex chat endpoint) with the skill generation prompt. The exact routing is an implementation decision — the UI just needs a streaming text response to populate the preview pane.

---

## Skill Authoring Guidance in System Prompt

The channel system prompt gets a brief `SKILL_GUIDANCE` block when skills are loaded:

```
When you encounter a non-trivial workflow — something that required multiple
steps, problem-solving, or domain knowledge — consider whether it would be
worth saving as a skill for future reuse. If so, delegate to a branch to
capture it with write_skill. Don't save obvious or one-off tasks.
```

This nudges the agent toward proactive skill capture without over-instructing it. It only appears when `skill_set.len() > 0` or when `write_skill` is in scope — no guidance is shown to agents that haven't opted into the skills system.

---

## What This Enables

With these three pieces, skills accumulate from experience:

```
User conversation
  → Channel identifies reusable workflow, delegates to branch
  → Branch writes skill via write_skill
  → Skill on disk, loaded into next session

OR

Conversation ends (idle 5 min, ≥6 turns)
  → Reflection branch reviews history
  → Saves skills and/or memories silently
  → Next conversation benefits from captured knowledge
```

The memory side already works (branches write memories, cortex maintains them). Skills are the missing procedural half — and they're just files.

---

## Non-Goals

- **Skill evaluation** — no metrics on whether saved skills actually improve outcomes. That's a future concern.
- **Cross-agent skill sharing** — skills are workspace-scoped per agent. Instance-level sharing remains admin-managed.
- **Skill versioning** — overwrite is the update mechanism. Git history covers version tracking if needed.
- **Structured skill templates** — the agent writes free-form markdown. Format guidance is in the skill authoring prompt, not enforced by schema.
