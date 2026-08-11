# Skill Lifecycle: Self-Improvement for Spacebot

Supersedes `skill-authoring.md`. That doc proposed the first slice (a `write_skill`
tool plus a reflection branch); this one covers the full loop it was reaching for:
**outcome → candidate procedure → authored or patched skill → provenance and usage
tracking → curation and pruning.** The reference implementation studied is
Hermes (`nousresearch/hermes-agent` at 2026-08-08 HEAD), which runs this loop in
production, plus a 126-skill live corpus from a heavily-used Hermes instance.

## Why skills and not more memory

Memory answers "who is the user and what is going on." Skills answer "how do we
do this class of task here." Hermes draws this line explicitly in its review
prompts, and it's the right line: a correction like "stop posting walls of text
in Discord" is not a fact about the user — it's a standing procedure change, and
it belongs in the skill governing that task class. Spacebot has real memory
(cortex, LanceDB) and a real skill loader, but nothing that turns experience
into skills. That's the gap.

## What exists today (audited 2026-08-08)

- `SkillSet` with three sources, precedence Builtin < Instance < Workspace
  (`src/skills.rs:66-103`). Loaded via `ArcSwap` on `RuntimeConfig`, hot-reloaded
  by the file watcher.
- Prompt injection is index-only (name + description): channels get
  `fragments/skills_channel.md.j2`, workers get `skills_worker.md.j2` with
  suggested flags. **Branches get no skill index at all** (`branch.md.j2:8`).
- Tools: `read_skill` (workers + Direct-mode channels), `skills_search` and
  `install_skill` **only in the deprecated cortex-chat toolset**
  (`src/tools.rs:1068-1075` has the TODO to port them).
- No create, no patch, no delete tool. `SkillSet::remove` exists but is only
  reachable over HTTP/CLI. No provenance beyond `source_repo` (repo name only).
  No usage tracking. `CreateSkill.tsx` is a stub.
- Known defects to fix in passing: hand-rolled frontmatter parser drops YAML
  lists and multiline scalars (`src/skills.rs:353-410`); API skill mutations
  never call `reload_skills` and rely on the watcher (`src/api/skills.rs:258`);
  the watcher only watches skill dirs that existed at startup
  (`src/config/watcher.rs:93-110`); `skills_search` formats the HTTP status
  slot with `body.len()` (`src/tools/skills_search.rs:215-219`); `read_skill`
  output is unbounded.

## What Hermes proves works

Distilled from source, not docs. The load-bearing rules, with where they live
in Hermes:

1. **Progressive disclosure with a paid-for index.** Name + ≤60-char description
   in every system prompt; full body only via explicit load. The budget is
   enforced at *create* time, not truncated at read time
   (`skill_manager_tool.py:604-617`).
2. **Provenance is ambient, never model-supplied.** A context-bound write origin
   (foreground vs. autonomous review) decides `created_by`; the model cannot
   claim authorship semantics because it never passes the field
   (`skill_provenance.py`).
3. **Only agent-created skills are auto-curated.** A skill created by the user,
   installed from a registry, or authored in a foreground conversation is
   outside curator jurisdiction unless the user explicitly adopts it into
   management. Provenance is a user declaration.
4. **Autonomy narrows the blast radius.** Same verb, different semantics by
   origin: foreground delete removes; autonomous delete archives (recoverable).
   Pin blocks deletion for a present user but blocks *all writes* for an
   autonomous actor — consent is the axis, not the operation.
5. **Read-before-write for autonomous editors.** The review fork must have
   loaded the exact target file in the current pass before it may patch it —
   a mechanical rail against patching imagined content
   (`skill_manager_tool.py:424`).
6. **Fail closed on unverified destruction.** Consolidation deletes require a
   declared `absorbed_into` target that exists on disk. This fixed a real
   incident where an LLM consolidation pass archived active skills with zero
   verified merges (`skill_manager_tool.py:463`).
7. **Deterministic maintenance and LLM consolidation are separate passes.**
   Stale/archive transitions are pure code over usage counters, always on.
   The opinionated LLM rewrite pass is off by default.
8. **The review prompt is the policy surface**, and its sharpest rules are
   negative: never persist environment-dependent failures, negative tool
   claims ("browser tools don't work" hardens into a refusal cited for months),
   transient errors, or one-off task narratives. Prefer patching an existing
   umbrella skill over creating a new one; new-skill names must describe the
   task class, not the incident.
9. **Telemetry in a sidecar, not frontmatter** — usage data never creates merge
   pressure on content. (Spacebot does one better: per-agent SQLite.)
10. **Demote, never hide.** When the index needs compacting, drop descriptions
    but keep every name visible — models don't rediscover what vanishes.

Corpus evidence backs the shape: 126 skills, directory-per-skill with support
files, categories with `DESCRIPTION.md`, `related_skills` cross-links, and
skills genuinely evolving (one skill edited four times in four days with
version bumps).

## Architecture mapping

Hermes fakes process separation with forked Python agents. Spacebot already has
the real thing, so each loop lands on the process built for it:

| Loop | Hermes | Spacebot |
|---|---|---|
| Capability index + explicit load | system prompt + `skill_view` | existing index fragments + `read_skill` (unchanged shape) |
| Outcome → skill pump | post-turn forked agent replaying the conversation | **reflection branch** — branches already clone channel history at spawn; this is exactly the replay-fork, natively |
| Slow curation | idle-triggered curator fork | **cortex maintenance** — cortex already owns background cognition and has a maintenance cadence |
| Mutation surface | `skill_manage` tool | new `skill_manage` tool, origin-scoped |
| Usage/provenance store | JSON sidecar with file locks | table in the agent's SQLite |
| Backups | tar.gz snapshots | tar.gz snapshots (same; skills dirs aren't git repos) |

## Design

### 1. Format and parsing

Keep directory-per-skill `SKILL.md`. Replace the hand-rolled frontmatter parser
with `serde_yaml` into a typed struct:

```rust
#[derive(Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,          // falls back to directory name
    description: String,
    platforms: Option<Vec<Platform>>, // hard gate vs. host OS; absent = all
    tags: Option<Vec<String>>,
    related_skills: Option<Vec<String>>, // advisory, surfaced by read_skill
    source_repo: Option<String>,   // kept for installer compatibility
}
```

Unknown fields ignored (Hermes corpus carries `version`, `author`, `license` —
decorative there too; tolerate, don't require). Support subdirectories inside a
skill: `references/`, `templates/`, `scripts/`, `assets/` — excluded from skill
discovery, listed by `read_skill` as `linked_files` so a skill body can point
at deeper material without inflating the index. Keep `{baseDir}` substitution.
No inline shell expansion — Hermes ships it off by default and it's a prompt
injection surface we don't need.

Description budget: 80 chars, enforced on create/edit through `skill_manage`
and the write API only — pre-existing and installed skills render truncated
with an ellipsis rather than failing to load.

### 2. Provenance and usage: `skill_usage` table

Per-agent SQLite (new migration — schema is append-only per repo policy):

```sql
CREATE TABLE skill_usage (
    skill_name      TEXT PRIMARY KEY,   -- lowercased canonical name
    created_by      TEXT NOT NULL,      -- 'user' | 'agent' | 'installed'
    origin_conversation_id TEXT,        -- set when created_by = 'agent'
    state           TEXT NOT NULL DEFAULT 'active', -- 'active'|'stale'|'archived'
    pinned          INTEGER NOT NULL DEFAULT 0,
    read_count      INTEGER NOT NULL DEFAULT 0,
    patch_count     INTEGER NOT NULL DEFAULT 0,
    last_read_at    TEXT,
    last_patched_at TEXT,
    created_at      TEXT NOT NULL,
    archived_at     TEXT
);
```

`read_skill` bumps `read_count`; `skill_manage` bumps `patch_count`; the
installer inserts `created_by = 'installed'`. Skills present on disk with no
row get one seeded on first sight with `created_at = now` — a newly noticed
skill's staleness clock starts now, not at epoch (Hermes does this; it prevents
mass-archiving a fresh install).

`WriteOrigin` rides the tool deps, set by the process constructing the tool
server — `User` for channel/API/CLI paths, `Agent` for the reflection branch
and cortex curation. The model never supplies it.

### 3. `skill_manage` tool

One tool, action-dispatched, mirroring the shape that works in Hermes:

```
skill_manage(action, name, ...)
  create   { content, category? }        -- full SKILL.md text
  patch    { old_string, new_string, replace_all? }
  edit     { content }                   -- full rewrite
  delete   { absorbed_into? }
  write_file  { file_path, file_content } -- under references|templates|scripts|assets
  remove_file { file_path }
```

Writes always target the agent's workspace skills dir (autonomous writes never
touch instance-level or builtin skills). Validation, all origins:

- name: `^[a-z0-9][a-z0-9._-]*$`, ≤64 chars; category a single path segment.
- frontmatter parses, has description; body non-empty; description ≤80 on
  create/edit.
- size caps: SKILL.md 100 KB, support files 1 MiB.
- path rails: reject `..` before allow-listing, canonicalize and verify the
  resolved path stays inside the skill dir, refuse targets reached via symlink.
- delete refuses builtin (already true), anything outside a skills root, and
  a skills root itself.

Origin-scoped rails (the Hermes rules, ported as code not prompt):

- `WriteOrigin::Agent` may not modify installed (`created_by = 'installed'`),
  pinned, or instance-level skills.
- `WriteOrigin::Agent` must have `read_skill`'d the exact target this session
  before patch/edit (tracked in the tool server's session state).
- `WriteOrigin::Agent` delete → archive: move the directory to
  `{workspace}/skills/.archive/{name}/`, set `state = 'archived'`. `User`
  delete removes the directory. `.archive` is excluded from discovery.
- delete with `absorbed_into` requires the named skill to exist on disk and
  differ from the target.
- Pin: blocks delete for `User`, blocks every mutation for `Agent`.

Every successful mutation calls `reload_skills` directly (the deterministic
path the `install_skill` tool already uses) — no reliance on the watcher.

Tool placement: branches and cortex get `skill_manage`; workers keep
`read_skill` only (workers execute tasks, they don't legislate procedure);
channels don't get it — a channel that wants to save a procedure spawns the
reflection branch with focus text, which keeps the conversational loop clean
and gives every skill write the same restricted, auditable surface. While in
here, complete the `src/tools.rs:1068` TODO: `skills_search`/`install_skill`
move to the channel toolset and the deprecated cortex-chat server drops.

### 4. Reflection branch — the pump

A silent branch spawned after work that likely produced a reusable lesson.
Branches already clone channel history and system prompt at spawn, which is
precisely Hermes's replay-fork, minus the Python.

**Trigger** (channel-side, after the turn's response is delivered):
tool iterations this turn ≥ `reflection_min_tool_iterations` (default 10),
or a worker attached to the conversation finished with `Success`/`Partial`
after ≥ that many iterations. Gated by `reflection_cooldown_secs` (default
3600) per conversation, and skipped entirely for cron-originated turns.
Counter-based, not idle-based: the signal that something was learned is that
real work happened, not that the user went quiet. (This replaces
skill-authoring.md's idle/turn-count gates.)

**Constraints on the branch:**
- toolset: `skill_manage`, `read_skill`, `skills_list` (new: name/desc/state
  listing backed by the usage table), plus the memory-save tools. Nothing else
  — no shell, no messaging, no spawn.
- it cannot message the user; its outcome is a one-line summary logged and
  surfaced as a low-priority status event (interface can render "learned:
  patched *discord-rendering*" the way Hermes prints its 💾 line).
- `WriteOrigin::Agent`, so every rail in §3 applies mechanically.
- capped at 6 LLM turns (matches the prior doc's budget).

**Prompt policy** (new `prompts/en/reflection.md.j2`; the prompt *is* the
product here, port Hermes's semantics not its text):
- decide first whether anything is worth keeping; ending with no writes is
  acceptable, but treat a session where the user corrected the agent's
  procedure as a strong write signal.
- preference ladder: patch the skill that was loaded this session → patch an
  existing related skill → add a `references/` file to one → only then create
  a new skill, named for the task class, never the incident.
- user frustration and corrections are skill signals, not just memory signals.
- the negative-capture list, verbatim in spirit: no environment-dependent
  failures, no negative capability claims about tools, no transient errors
  that resolved, no one-off narratives, no unresolved failure logs dressed as
  procedure.

### 5. Curation — cortex maintenance

Two passes, run from cortex's existing maintenance cadence (weekly default,
config-gated), only over skills with `created_by = 'agent'` or explicitly
adopted ones:

**Deterministic (always on when curation is enabled):** pure code over the
usage table. `active → stale` after `stale_after_days` (default 30) without a
read; `stale → archived` after `archive_after_days` (default 90); any read
reactivates. Skips pinned skills and any skill referenced by a cron job or
routine. Before any pass that will mutate, snapshot the workspace skills tree
(tar.gz + manifest under `{workspace}/skills/.snapshots/`, retention 5), and
write a run report row so the interface can show what happened.

**Consolidation (off by default):** an LLM pass in a cortex-spawned worker
with the same restricted toolset, allowed to merge overlapping skills — every
delete requires `absorbed_into`, enforced by §3's rail, so it can only archive
what it demonstrably merged. This stays off until the deterministic pass has
proven boring.

**User controls** (CLI + API, interface later): `pin`/`unpin`, `adopt` (flip
`created_by` to `'agent'` — the explicit act of handing a skill to curation),
`archive`/`restore`, `snapshots`/`rollback`. Rollback snapshots before
restoring, so it's undoable.

### 6. Surfaces

- `POST /agents/skills/write` — backs `CreateSkill.tsx` (currently a stub);
  same validation as `skill_manage(create)` with `WriteOrigin::User`. While
  in the API: make all mutating skill endpoints call `reload_skills`
  deterministically, fixing the existing reload gap.
- `SkillInspector` gains the usage row: created-by, state, counts, pin toggle,
  origin conversation link when agent-created.
- Watcher fixes ride along: watch skills roots even when created after
  startup (create-then-watch), and replace the `contains("skills")` substring
  classification with prefix matching against the actual watched roots.
- `read_skill`: apply the standard 50 KB tool-output cap, return
  `linked_files` and `related_skills`.
- Branches get the skill index fragment they currently lack.

### 7. Config

```toml
[skills]
write_approval = false          # stage agent writes for user approval instead of committing

[skills.reflection]
enabled = true
min_tool_iterations = 10
cooldown_secs = 3600
max_turns = 6

[skills.curation]
enabled = true
interval_days = 7
stale_after_days = 30
archive_after_days = 90
consolidation = false
snapshot_retention = 5
```

All hot-reloadable via the existing `RuntimeConfig` pattern. `write_approval`
staging (pending records + approve/reject over API, diff rendering in the
interface) is designed in but built last — Hermes ships it off by default and
the origin rails carry the real safety load.

## Phases

**Phase 1 — foundations.** serde_yaml frontmatter with the typed struct and
new fields; support-subdir handling + `linked_files`; `skill_usage` migration
and read-count plumbing; `WriteOrigin` on tool deps; fix the API reload gap,
watcher gaps, `skills_search` status-format bug; cap `read_skill`; port
`skills_search`/`install_skill` out of the deprecated cortex-chat toolset;
give branches the skill index.

**Phase 2 — mutation.** `skill_manage` with the full validation and
origin-rail set; archive semantics; `reload_skills` on every mutation;
`skills_list` tool; CLI parity (`spacebot skill pin|adopt|archive|restore`).

**Phase 3 — the pump.** Reflection branch: trigger plumbing in the channel
turn finalizer and worker-outcome path, restricted tool server, reflection
prompt, cooldown state, status-event surfacing.

**Phase 4 — curation.** Deterministic pass in cortex maintenance with
snapshots, run reports, and rollback; pin/adopt/cron-reference protections;
consolidation pass behind its default-off flag.

**Phase 5 — surfaces.** `POST /agents/skills/write` + CreateSkill UI; usage
and provenance in SkillInspector; reflection/curation activity in the
interface; `write_approval` staging mode; user docs.

Each phase lands as its own PR and is independently shippable; Phases 3 and 4
are prompt-heavy and should expect iteration after real transcripts.

## Non-goals and deliberate divergences from Hermes

- **No inline shell expansion in skill bodies** — injection surface, off by
  default even in Hermes.
- **No skill bundles, org overlay, or trust-matrix hub** for now. The existing
  skills.sh search + GitHub installer stay as-is; a content-hash install lock
  can come with a future registry pass.
- **Dry-run, if added, is capability-level** (a tool server that refuses
  mutations), not prompt-advisory. Hermes's prompt-banner dry-run is its one
  rail we should not copy.
- **No static security scanner for agent-created skills.** Spacebot skills are
  markdown injected into prompts; scripts they reference execute through the
  existing sandboxed shell tools, which is where that enforcement belongs.
- **No semantic versioning or content history** — `patch_count` +
  curation snapshots + (optional) user git on the skills dir cover it, same
  posture as the prior doc.
- **No cross-agent skill sharing** yet; workspace scoping stands.

## Shipped status (2026-08-11)

### Shipped

**Phase 1 — foundations** (PR #621): serde_yaml frontmatter, `skill_usage` table,
`WriteOrigin`, API reload fix, watcher fixes, `skills_search` status-format fix,
`read_skill` cap, branch skill index, `skills_search`/`install_skill` moved to
channel toolset.

**Phase 2 — mutation** (PR #622): `skill_manage` tool with full validation and
origin-scoped rails, archive semantics, deterministic `reload_skills` on every
mutation, `skills_list` tool.

**Phase 3 — the pump** (PR #624, #633): Reflection riding the memory-persistence
branch — turn-work and worker-completion triggers, restricted tool server
(`read_skill`, `skill_manage`, `skills_list` + memory-save, no shell/no
messaging/no spawn), reflection prompt section with decide-first and
negative-capture bans, cooldown state, reflection signal with worker-ID
tracking, worker transcript feeding for reflection passes.

**Reflection run record** (this PR): Durable `reflection_runs` table with
agent/channel identity, trigger source, referenced worker IDs, start/end
timestamps, terminal status (`success`/`no_op`/`error`/`cancelled`), declared
rationale (separate from observed actions), outcome summary, affected skill
identifiers, and token usage slot. Fire-and-forget persistence via
`ReflectionRunLogger` matching the existing `ProcessRunLogger` pattern.
`ReflectionRunCompleted` event on the shared `ProcessEvent` bus, piped through
the existing `ApiEvent` SSE pipeline. Minimal timeline surface in the portal UI
rendering reflection outcomes inline (same pattern as chronicle checkpoints).

### Deferred to Phase 4 (curation) and Phase 5 (surfaces)

- Deterministic stale/archive pass in cortex maintenance
- LLM consolidation pass (default-off)
- Snapshots + rollback
- `POST /agents/skills/write` + `CreateSkill.tsx`
- Full `SkillInspector` usage row (provenance, state, counts, pin toggle)
- `write_approval` staging mode
- CLI `pin`/`adopt`/`archive`/`restore` commands
