# Wiki

A shared, versioned knowledge base for the entire Spacebot instance. Multiple agents and multiple users can read and write. Every edit is recorded permanently — like Wikipedia, nothing is ever destroyed, and the full history of how the wiki evolved is always traceable.

The wiki is not per-agent. It belongs to the instance. A company running Spacebot has one wiki. All agents contribute to it. All users can browse and edit it.

---

## What the Wiki Is Not

- **Not a replacement for the memory graph.** Atomic facts, preferences, and decisions stay in the graph. The wiki is for long-form structured knowledge that would be awkward as graph nodes — entity pages, concept docs, decision records, project syntheses.
- **Not the file system.** Agents never write wiki content by editing files directly. All mutations go through wiki tools. The content lives in the database.
- **Not the legacy bulletin fallback.** Knowledge synthesis and the legacy `bulletin_*` compatibility surfaces are ephemeral, per-agent, and auto-generated. Wiki pages are persistent, instance-wide, and intentionally authored.

---

## Page Types

| Type | What it covers | Examples |
|------|---------------|---------|
| **Entity** | A person, project, codebase, tool, or organisation | "Jamie Pine", "Spacebot", "Anthropic API" |
| **Concept** | A pattern, approach, or idea that recurs | "component conventions", "auth migration pattern" |
| **Decision** | The record of a significant decision | "why SQLite over Postgres", "model routing choices" |
| **Project** | A running synthesis of an ongoing workstream | "v2 launch", "dashboard redesign" |
| **Reference** | Stable factual content used repeatedly | "deployment checklist", "API rate limits" |

---

## Storage

The wiki lives in the **global instance database** — not in any agent's workspace, not on the filesystem.

```sql
CREATE TABLE wiki_pages (
    id           TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    slug         TEXT NOT NULL UNIQUE,   -- url-safe identifier, derived from title
    title        TEXT NOT NULL,
    page_type    TEXT NOT NULL,          -- entity | concept | decision | project | reference
    content      TEXT NOT NULL,          -- current markdown content (no frontmatter)
    related      TEXT DEFAULT '[]',      -- JSON array of related page slugs
    created_by   TEXT NOT NULL,          -- agent_id or user_id
    updated_by   TEXT NOT NULL,
    version      INTEGER NOT NULL DEFAULT 1,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE wiki_page_versions (
    id           TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    page_id      TEXT NOT NULL REFERENCES wiki_pages(id),
    version      INTEGER NOT NULL,
    content      TEXT NOT NULL,          -- full content snapshot at this version
    edit_summary TEXT,                   -- optional one-line description of what changed
    author_type  TEXT NOT NULL,          -- 'agent' | 'user'
    author_id    TEXT NOT NULL,          -- agent_id or user_id
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(page_id, version)
);

CREATE INDEX wiki_pages_type    ON wiki_pages(page_type);
CREATE INDEX wiki_pages_updated ON wiki_pages(updated_at DESC);
CREATE INDEX wiki_versions_page ON wiki_page_versions(page_id, version DESC);
```

`wiki_pages` always reflects the current state. `wiki_page_versions` is the immutable history — every edit appends a new row, nothing is ever deleted. Version numbers increment monotonically per page.

---

## The Wiki Edit Tool

Wiki editing uses the same `old_string` / `new_string` pattern as `file_edit`, but with tolerant matching. Since wiki content is LLM-authored and formatting can drift slightly across edits, strict exact-match fails too often. The tool uses a multi-pass fallback strategy:

1. **Exact match** — direct string replacement
2. **Line-trimmed match** — ignores leading/trailing whitespace per line
3. **Whitespace-normalised match** — collapses all whitespace to single spaces
4. **Indentation-flexible match** — strips indentation and matches
5. **Block anchor match** — for multi-line blocks, anchors on first and last line with Levenshtein similarity scoring in between

If none pass, return a clear error with a suggestion to provide more surrounding context. The LLM should always read the page before editing it.

This is the same approach OpenCode uses for file editing. It works well for LLM-authored content because the tolerance strategies handle the subtle variations that cause strict matching to fail.

### Tool Definitions

**`wiki_create`** — create a new page (first version)

```rust
struct WikiCreateInput {
    title: String,
    page_type: WikiPageType,
    content: String,          // full markdown body
    related: Vec<String>,     // slugs of related pages
    edit_summary: Option<String>,
}
```

**`wiki_edit`** — partial edit on an existing page (creates new version)

```rust
struct WikiEditInput {
    slug: String,
    old_string: String,       // text to find (tolerant matching)
    new_string: String,       // replacement text
    replace_all: bool,        // default false — requires unique match
    edit_summary: Option<String>,
}
```

**`wiki_read`** — read current version of a page

```rust
struct WikiReadInput {
    slug: String,
    version: Option<u32>,     // if omitted, returns current version
}
```

**`wiki_search`** — FTS search across all pages

```rust
struct WikiSearchInput {
    query: String,
    page_type: Option<WikiPageType>,
}
```

**`wiki_list`** — list all pages, optionally filtered by type

```rust
struct WikiListInput {
    page_type: Option<WikiPageType>,
}
```

**`wiki_history`** — list version history for a page

```rust
struct WikiHistoryInput {
    slug: String,
    limit: Option<u32>,   // default 20
}
```

**Channels** do not have wiki tools by default — they delegate to branches. When delegation mode is disabled, the channel has direct access to all tools including wiki.

**Branches** have `file_read` (not write/edit), `wiki_read`, and `wiki_write`, in addition to their existing memory and task tools. This makes branches the natural unit for wiki authoring: a branch can read files for context, reason about what to write, and call `wiki_create` / `wiki_edit` directly — without a worker relay. For memory-derived wiki pages (the autonomy loop's escalation flow), the branch reads the memory graph and synthesises the page; for file-derived content, it reads the source files directly.

**Workers** can be granted wiki write access via a `wiki_write: bool` flag on `WorkerContextMode` (default: `false`). Enable it when the worker is explicitly tasked with wiki creation so it can read and write in one pass. Workers doing background task enrichment for the autonomy loop leave this off and surface findings via task comments instead.

**The autonomy channel** has full wiki access -- memory escalation, ingest-triggered synthesis, and wiki maintenance (stale pages, broken links, contradictions). It has the context to do all of this meaningfully: knowledge synthesis, working memory events, and run history.

---

## Versioning

Every `wiki_create` writes version 1. Every `wiki_edit` increments the version and appends a row to `wiki_page_versions` with a full content snapshot. Pages are never deleted — only a user (via the UI or API) can archive a page, which sets a soft `archived` flag but preserves all history.

This means:
- An agent can never destroy wiki content by misusing a tool
- Every change is traceable: who edited, when, what the content was before
- Any version can be restored from `wiki_page_versions`
- The complete evolution of the wiki is always available

---

## When the Wiki Gets Updated

Four triggers:

**1. Mid-conversation, branch-initiated.** The channel identifies something worth a wiki page — a decision made, a pattern established, an entity worth documenting. It delegates to a branch which calls `wiki_create` or reads + `wiki_edit`.

**2. Post-conversation reflection.** The reflection branch (see `skill-authoring.md`) is extended to include wiki updates:

```
**Wiki**: Did this conversation establish or substantially update knowledge about
a person, project, concept, or decision that would benefit from a wiki page?
If so, create or update the relevant page. Only write pages for knowledge that
is structured, reusable, and likely to be referenced again.
```

**3. Ingest.** When files are dropped into `ingest/`, they are chunked and fed into the memory graph as today. When an ingest batch completes, it emits a working memory event. On its next wake, the autonomy channel checks what was recently ingested and considers whether any of it warrants a wiki page — a reference document, a spec, a decision record, a README. If so, it reads the relevant memories extracted from the ingest and synthesises a wiki page from them.

Structured documents are the best candidates: markdown files with clear headings, PDFs that read like reference material, anything that looks like it was written to be read as a whole rather than searched by fragment. The autonomy channel uses judgement — not every ingested file needs a wiki page.

**4. Memory escalation in the autonomy loop.** Memories accumulate fast — a conversation can produce a dozen facts about a topic. A wiki page needs time and attention to get right. The autonomy loop bridges these two speeds.

On each wake, the autonomy channel looks for memory clusters: groups of related memories around the same entity, concept, or decision that have no corresponding wiki page. When it finds one, it synthesises the cluster into a wiki page — reading all the relevant memories, reasoning about what the stable long-form version should say, and calling `wiki_create`. When a page already exists, it checks whether accumulated memories since the last edit are substantial enough to warrant an update.

This is "promotion" — memories are the raw signal, wiki pages are the settled knowledge. The autonomy loop decides when enough has accumulated to be worth writing up.

Instruction in the autonomy channel system prompt:
```
Periodically review recent memories for clusters around a shared topic.
If you find multiple memories about the same entity, concept, or decision
with no corresponding wiki page, synthesise them into one. If a page exists
but feels stale relative to what's accumulated since, update it.
Wiki pages should be earned — only promote when there's enough to say something
true and useful that will hold up over time.
```

**4. Autonomy loop maintenance.** On each wake, the autonomy channel also scans the wiki for maintenance needs — it has the context to do this meaningfully:
- Pages not updated in > `wiki_stale_days` with substantial new memories accumulated — update them
- Broken `wiki:` links and `related` references — fix in place (it can reason about what the correct target should be)
- Contradictions between wiki content and memory graph — surface as a working memory observation or resolve directly

The actual cortex maintenance pass (`memory/maintenance.rs`) is a pure algorithmic background process: importance decay, low-importance pruning, and near-duplicate memory merging by embedding similarity. It has no wiki awareness and no LLM reasoning — it operates entirely on the memory graph.

---

## Links and the Page Graph

### Link Syntax

Standard markdown links with a `wiki:` URI scheme:

```markdown
[display text](wiki:slug)
```

Examples in prose:
```markdown
The decision to use [SQLite over Postgres](wiki:sqlite-choice) came down to
the single-binary constraint. See also the [auth migration pattern](wiki:auth-pattern)
for how this affects our approach.
```

This is standard markdown — every model writes it naturally. The `wiki:` scheme is the only signal needed to distinguish wiki links from external URLs. No double-bracket conventions, no special syntax to forget.

### In the UI

The wiki renderer detects `[text](wiki:slug)` links and renders them as clickable internal navigation to `/wiki/[slug]`. Resolved links look like standard inline links. Broken links (slug not found) render in a distinct style — muted colour or strikethrough — so authors notice them immediately.

Clicking a link navigates to that wiki page within the same Wiki view. Back navigation works as expected.

### In the Agent

The agent does not click links, but `wiki_read` resolves them. The response includes a `links` field — all slugs referenced inline, resolved to their current titles — without fetching full page content. The agent sees what's connected and decides what to pull.

```json
{
  "slug": "auth-rewrite",
  "title": "Auth Rewrite",
  "content": "...",
  "related": ["v2-launch", "sqlite-choice"],
  "links": [
    { "slug": "jamie-pine", "title": "Jamie Pine" },
    { "slug": "clerk", "title": "Clerk" },
    { "slug": "auth-pattern", "title": "Auth Migration Pattern" }
  ],
  "version": 4
}
```

**Worker briefing** follows this graph one level deep — reads the page, fetches any linked pages automatically. One hop, not recursive.

### Two Edge Types

- **`related`** (frontmatter) — declared relationships. Coarse-grained, set at create/edit time.
- **`[text](wiki:slug)` inline links** — semantic connections within prose. Fine-grained, emerge naturally from writing.

Both are traversable. The cortex lint pass validates all `wiki:` links on each maintenance pass and fixes broken ones when a page is renamed or archived — updating all pages that linked to it.

---

## Writing Guide

Wiki pages are written to be useful, not impressive. The agent's system prompt includes the following guidance whenever wiki tools are in scope:

```
## Wiki Writing Guide

A wiki page should be true, stable, and useful to someone — human or agent —
who knows nothing about the current conversation.

**Structure**
- Lead with what the page is about in one sentence
- Use ## headings to organise sections. Keep them short and factual.
- Use bullet points for lists of facts. Use prose for reasoning and context.
- Link related pages inline using [display text](wiki:slug).
  Don't over-link — only where the connection is genuinely useful to follow.
  First mention of a notable entity on a page should be linked. Subsequent
  mentions don't need to be.

**Tone**
- Factual and direct. No hedging, no filler.
- Write in the present tense where possible ("The API uses OAuth 2.0",
  not "The API was updated to use OAuth 2.0 in January").
- No speculation. If something is uncertain, say so explicitly.

**When to create vs update**
- Create a new page when a topic is substantial enough to stand alone.
- Update an existing page rather than creating a near-duplicate.
- Read the existing page before editing it. Preserve what's still true.
- Write an edit_summary describing what changed and why.

**What not to write**
- Don't transcribe conversation. Synthesise.
- Don't duplicate what's in the memory graph. Wiki pages are for
  structured knowledge that benefits from prose and context.
- Don't write pages for things that are likely to change tomorrow.
  Wiki pages should earn their place.
```

---

## Agent Access

Agents do not get all wiki pages injected into every conversation. The agent knows what exists; it fetches what it needs.

**Index in knowledge context.** Knowledge synthesis can include a compact wiki index: page titles and types only, no content. The agent sees what pages exist and can fetch any of them with `wiki_read`.

```
## Wiki (14 pages)
Projects: v2-launch, auth-rewrite, dashboard-redesign
Entities: jamie-pine, spacebot, spacedrive, anthropic-api
Concepts: component-conventions, auth-pattern, deploy-process
Decisions: sqlite-choice, model-routing, tailwind-migration
Reference: deployment-checklist, api-rate-limits
```

**Worker briefing.** `WorkerContextMode::Briefed` pulls relevant wiki pages via FTS against the task description. A worker briefed on an auth task gets the auth-rewrite project page and the auth-pattern concept page injected alongside memory recall.

---

## User Access

Wiki is a top-level item in the sidebar alongside Dashboard, Tasks, and Workbench — not nested under any agent. It belongs at the top level because it's instance-wide. Users can:
- Browse by page type
- Read any page with full rendered markdown
- Edit any page directly (creates a version with `author_type: user`)
- Restore any prior version
- See full version history per page with diffs

User edits are respected — the cortex lint pass does not overwrite user-authored content without explicit re-synthesis instruction.

---

## Relationship to Memory Graph

| | Memory Graph | Wiki |
|--|------------|------|
| **Scope** | Per-agent | Instance-wide |
| **Unit** | Atomic typed fact | Full page |
| **Retrieval** | Vector + FTS + RRF | Title lookup + FTS |
| **Volatility** | Frequently updated | Relatively stable |
| **Versioning** | None (replace) | Full history |
| **Primary reader** | Agent (fast retrieval) | Agent + all humans |

The two complement each other. A preference in the memory graph and a section on a wiki entity page may express the same knowledge at different granularities. In conflicts, the more recently updated record is the signal — the agent can reconcile via a lint pass or a branch.

---

## Configuration

```toml
[wiki]
enabled = true
wiki_stale_days = 30          # Flag pages not updated in this many days
max_pages = 1000              # Soft cap — lint warns above this
```

---

## Implementation

**Phase 1 — Core**
- `wiki_pages` + `wiki_page_versions` migrations (global database)
- `wiki_create`, `wiki_edit`, `wiki_read`, `wiki_search`, `wiki_list`, `wiki_history` tools
- Tolerant matching in `wiki_edit` (multi-pass fallback)
- `wiki:` link parsing in `wiki_read` response
- Branch tool registration: add `file_read`, `wiki_read`, `wiki_write` to branch toolset
- Cortex + autonomy channel tool registration
- `wiki_write: bool` field on `WorkerContextMode` (default: `false`) — enables wiki write tools on a per-spawn basis
- Wiki index block in knowledge-synthesis template
- Writing guide in branch system prompt when wiki tools are in scope
- API endpoints: CRUD + version history + restore

**Phase 2 — Integration**
- Post-conversation reflection extended to cover wiki updates
- Autonomy loop: memory cluster detection → wiki promotion
- Ingest: working memory event on batch complete; autonomy channel checks for structured docs worth synthesising into wiki pages
- Worker briefing (`WorkerHistoryMode::Briefed`) pulls relevant wiki pages via FTS + one-hop link traversal
- Autonomy loop wiki maintenance: stale page updates, broken `wiki:` link repair, contradiction surfacing

**Phase 3 — UI**
- Wiki tab: browse by type, read with rendered links, edit
- Version history viewer with diffs
- Version restore

---

## Non-Goals

- **No cross-instance wiki federation.** One wiki per instance.
- **No real-time collaborative editing.** Last write wins. Version history covers conflicts.
- **No structured schema enforcement** on page content. Format guidance lives in the agent's system prompt.
- **No batch wiki extraction from ingest.** Ingest feeds the memory graph. The autonomy loop decides whether ingested material warrants a wiki page — it is not automatic or per-file.
- **Workers do not write wiki pages.** Workers surface findings via task comments. Branches and the autonomy channel decide what's worth persisting to the wiki.
