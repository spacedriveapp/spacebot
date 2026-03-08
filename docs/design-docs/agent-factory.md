# Agent Factory

A natural-language-driven system for creating, configuring, and refining agents. The main agent (or cortex chat) guides the user through agent creation using conversational questions, preset soul archetypes, and the main agent's existing memories — producing fully configured agents with rich identity files, appropriate model routing, org links, and platform bindings.

## Problem

Creating a new agent today requires:
1. Give it a name and ID (the only thing the UI captures)
2. Manually write SOUL.md, IDENTITY.md, USER.md, ROLE.md from scratch
3. Edit config.toml to set routing, tuning, and platform bindings
4. Create links to other agents in the topology
5. Understand the architecture well enough to make good choices

There are no presets, no guidance on writing a soul, no way to leverage existing context. Most users give up after step 1 and end up with a blank agent that doesn't know who it is.

## Solution

The factory is a guided creation flow where the user describes what they want in natural language. The system synthesizes a fully configured agent from three sources:

1. **User intent** — conversational answers about what the agent should do, how it should communicate, what platforms it should be on
2. **Preset archetypes** — 8 curated soul/identity/role templates that serve as starting material for synthesis (never used directly)
3. **Main agent's memories** — searched at creation time for company context, domain knowledge, preferences, organizational structure

The factory is not a form. It's a conversation with the agent that happens to produce another agent.

---

## Phase 0: Deprecate USER.md — Humans Own Their Context

### Motivation

USER.md is a per-agent file that describes "the human this agent interacts with." This made sense for single-agent setups, but breaks down in multi-agent orgs:

- Every agent needs its own copy of USER.md, manually maintained
- There's no way to have multiple humans interact with multiple agents without duplicating context everywhere
- The org graph already has `[[humans]]` nodes with `display_name`, `role`, and `bio`, but this data is completely disconnected from USER.md
- `build_org_context()` doesn't even use the human's `display_name` — it falls back to the raw id (e.g., "admin")

The fix: human context belongs to the human node in the org graph. When a human is linked to an agent, the agent inherits that human's description in its system prompt. USER.md becomes unnecessary.

### Changes

#### 1. Add `description` field to `HumanDef`

The existing `bio` field is a short 2-3 sentence blurb for the topology UI. The new `description` field is the rich, long-form equivalent of what USER.md was — paragraphs of context about the person (preferences, background, communication style, timezone, etc.).

```rust
// config.rs
pub struct HumanDef {
    pub id: String,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub bio: Option<String>,
    pub description: Option<String>,  // NEW — replaces USER.md
}
```

TOML representation:

```toml
[[humans]]
id = "jamie"
display_name = "Jamie Pine"
role = "Founder"
bio = "Creator of Spacebot and Spacedrive."
description = """
Jamie Pine (@jamiepine). Timezone: America/Vancouver.

Full-stack developer, open source maintainer. Created Spacedrive (cross-platform 
file manager) and Spacebot (agentic AI system). Works in Rust and TypeScript.

Streams development on Twitch. Prefers direct, concise communication. Comfortable 
with technical depth. Uses AI tools extensively in his workflow.
"""
```

#### 2. Propagate human descriptions into org_context

Currently `build_org_context()` creates `LinkedAgent { name, id, is_human }` structs and the template only shows the name. This needs to:

a. Look up the `HumanDef` for linked humans (not just negative-lookup against `agent_names`)
b. Use `display_name` for the human's name (falling back to id)
c. Pass `description` (and `role`, `bio`) through to the template

New template data:

```rust
// engine.rs
pub struct LinkedEntity {
    pub name: String,        // display_name or id
    pub id: String,
    pub is_human: bool,
    pub role: Option<String>,
    pub description: Option<String>,  // human description, injected into prompt
}
```

Updated org_context.md.j2 fragment:

```jinja2
{% if entry.is_human -%}
- **{{ entry.name }}** (human){% if entry.role %} — {{ entry.role }}{% endif %}
{% if entry.description %}
{{ entry.description }}
{% endif %}
{% else -%}
- **{{ entry.name }}** — ...
{% endif -%}
```

This means: when a human is linked to an agent, the agent's system prompt includes that human's full description in the Organization section. Multiple humans linked to the same agent all appear. The agent knows who it's working with based on the org graph, not a static file.

#### 3. Deprecate USER.md loading

- `Identity::load()` stops loading USER.md (set `user: None` unconditionally)
- `Identity::render()` stops rendering the `## User` section
- `scaffold_identity_files()` stops creating USER.md for new agents
- The file tool's `PROTECTED_FILES` list removes USER.md
- If a USER.md file exists on disk, it is ignored (not deleted — just not loaded)

#### 4. Startup migration for existing users

On startup, after config is loaded but before agents are initialized:

1. **For each agent with a non-empty USER.md in its workspace:**
   - Read the content of `workspace/USER.md`
   - If there is exactly one human in the config (the default "admin"), set its `description` to the USER.md content
   - If there are multiple humans, log a warning: "USER.md found for agent '{id}' but multiple humans exist. Please manually assign the content to the appropriate human's description."
   - Rename the file to `USER.md.deprecated` so it's preserved but no longer loaded

2. **Auto-link the default human to the main agent:**
   - If there is a human with no links to any agent, create a hierarchical link from that human to the default agent (the human is the superior — they are the owner)
   - This only runs on first migration — once links exist, it doesn't touch them

3. **Persist changes:**
   - Write the updated `[[humans]]` with `description` to config.toml
   - Write the new `[[links]]` entry to config.toml

#### 5. Default human creation with auto-link

Currently, if no `[[humans]]` entries exist, a bare "admin" human is created at config parse time. Extend this:

- Still create the default "admin" human
- Also create a default link: `from = "admin"`, `to = "{default_agent_id}"`, `direction = "two_way"`, `kind = "hierarchical"`
- This ensures new installations start with the human connected to their main agent

#### 6. API updates

- `CreateHumanRequest` and `UpdateHumanRequest` gain a `description: Option<String>` field
- `TopologyHuman` response includes `description`
- `GET /api/agents/identity` and `PUT /api/agents/identity` drop the `user` field (breaking change, but the field becomes meaningless)
- Add a deprecation note to the API: `user` field in identity endpoints is ignored

#### 7. Dashboard UI updates

- `HumanEditDialog` in TopologyGraph.tsx gets a new `description` textarea (larger than `bio`, with a label like "Full context — background, preferences, communication style")
- The agent config identity tab (`AgentConfig.tsx`) removes the USER.md tab
- The agent detail overview (`AgentDetail.tsx`) removes the USER.md preview tab

#### 8. Humans lookup in build_org_context

`AgentDeps` or the channel needs access to the humans list to look up descriptions. Options:

a. Add `humans: Arc<ArcSwap<Vec<HumanDef>>>` to `AgentDeps` (already exists as `state.agent_humans` in the API layer — pass it through)
b. Build a `HashMap<String, HumanDef>` at startup for O(1) lookup

The channel's `build_org_context()` then:
- For each linked node, checks `agent_names` (is it an agent?) then `humans_map` (is it a human?)
- For humans: uses `display_name.unwrap_or(id)` for the name, passes `role` and `description` through

---

## Phase 1: Preset System

### Preset Structure

Eight preset archetypes ship embedded in the binary. Each is a directory of markdown files:

```
presets/
  community-manager/
    SOUL.md
    IDENTITY.md
    ROLE.md
    meta.toml
  research-analyst/
  customer-support/
  engineering-assistant/
  content-writer/
  sales-bdr/
  executive-assistant/
  project-manager/
```

#### meta.toml

```toml
id = "community-manager"
name = "Community Manager"
description = "Engages with community members, moderates discussions, answers FAQs, and keeps the vibe positive."
icon = "users"
tags = ["discord", "slack", "community", "moderation", "support"]

[defaults]
max_concurrent_workers = 3
max_turns = 5
```

Note: model routing is intentionally excluded from presets. Presets are provider-agnostic — model selection happens during the factory conversation when the user's available providers are known.

#### Presets

| Preset | Description |
|--------|-------------|
| Community Manager | Discord/Slack engagement, moderation, FAQ handling |
| Research Analyst | Deep research, document synthesis, report generation |
| Customer Support | Ticket triage, escalation, empathetic communication |
| Engineering Assistant | Code review, architecture, PR management, technical docs |
| Content Writer | Blog posts, docs, social media, consistent voice |
| Sales / BDR | Lead qualification, outreach drafting, follow-ups |
| Executive Assistant | Email drafting, meeting prep, information synthesis |
| Project Manager | Task tracking, status updates, cross-team coordination |

#### Embedding

Presets are compiled into the binary via `include_dir!`:

```rust
impl PresetRegistry {
    fn list() -> Vec<PresetMeta>;
    fn load(id: &str) -> Option<Preset>;
}

struct Preset {
    meta: PresetMeta,
    soul: String,
    identity: String,
    role: String,
}
```

#### API

```
GET /api/factory/presets           → Vec<PresetMeta>
GET /api/factory/presets/:id       → Preset (full content)
```

### Files

- `presets/` directory with 8 archetype subdirectories
- `src/factory/mod.rs` — module root
- `src/factory/presets.rs` — `PresetRegistry`, `Preset`, `PresetMeta`, `include_dir!` loading
- `src/api/factory.rs` — preset listing/loading endpoints

---

## Phase 2: Factory Tools

Six new LLM-callable tools:

| Tool | Purpose |
|------|---------|
| `factory_list_presets` | Lists available archetypes with metadata |
| `factory_load_preset` | Loads a preset's full content (soul, identity, role, defaults) |
| `factory_search_context` | Searches the main agent's memories for relevant context |
| `factory_create_agent` | Creates a fully configured agent — calls create API, writes identity files, creates links |
| `factory_update_identity` | Updates identity files for any agent (refinement) |
| `factory_update_config` | Updates routing/tuning config for any agent |

`factory_create_agent` takes a complete specification:

```rust
struct FactoryCreateAgentArgs {
    agent_id: String,
    display_name: String,
    role: String,
    soul_content: String,
    identity_content: String,
    role_content: String,
    routing: Option<RoutingOverrides>,
    links: Vec<LinkSpec>,
}
```

Note: no `user_content` — human context now flows through org links, not identity files.

### Files

- `src/tools/factory_list_presets.rs`
- `src/tools/factory_load_preset.rs`
- `src/tools/factory_search_context.rs`
- `src/tools/factory_create_agent.rs`
- `src/tools/factory_update_identity.rs`
- `src/tools/factory_update_config.rs`
- `src/factory/synthesis.rs` — shared logic for identity file generation, preset merging
- `tools.rs` — `factory_tool_server()` function

---

## Phase 3: Factory Prompt

A dedicated template (`prompts/en/factory.md.j2`) that instructs the LLM on how to be an agent factory:

1. **What you're doing** — Creating a new agent in a multi-agent system. You have tools to load presets, search memories, and create agents.
2. **How agents work** — Soul/identity/role system, what each file controls, how agents link, routing/tuning options.
3. **The creation flow** — Step-by-step: understand intent → search memories → present presets → ask about personality → ask about platforms → ask about org position → synthesize → summarize → create → offer refinement.
4. **Soul writing guide** — How to write effective souls. Voice, boundaries, judgment. Preset examples as reference.
5. **Synthesis rules** — Presets are starting material, not templates. Always customize. Inject organizational context from memory search. Match existing org communication style.

### Files

- `prompts/en/factory.md.j2`
- `src/prompts/engine.rs` — `render_factory_prompt()`
- `src/prompts/text.rs` — register template

---

## Phase 4: Structured Message Types

New generic message primitives for interactive chat components, reusable by any feature.

### Schema

Messages gain an optional `structured` field:

```rust
struct StructuredContent {
    kind: StructuredKind,
}

enum StructuredKind {
    /// Clickable options. User picks one (or types custom).
    Buttons {
        prompt: String,
        buttons: Vec<ButtonOption>,
    },
    /// Card grid for preset selection. Visual, with descriptions.
    SelectCards {
        prompt: String,
        cards: Vec<SelectCard>,
        allow_multiple: bool,
    },
    /// Step progress indicator.
    Progress {
        steps: Vec<ProgressStep>,
        current: usize,
    },
    /// Summary card with key-value pairs and action buttons.
    Summary {
        title: String,
        fields: Vec<(String, String)>,
        actions: Vec<ButtonOption>,
    },
}

struct ButtonOption {
    label: String,
    value: String,           // injected as user message when clicked
    description: Option<String>,
    icon: Option<String>,
}

struct SelectCard {
    id: String,
    title: String,
    description: String,
    icon: Option<String>,
    tags: Vec<String>,
}

struct ProgressStep {
    label: String,
    status: StepStatus,      // Pending | Active | Done | Error
}
```

### Transport

Outbound SSE events include an optional `structured` JSON field. The frontend renders the appropriate component. When the user clicks a button or card, the frontend injects the `value` as a regular user message. No bidirectional widget protocol — the agent controls the flow through tool calls.

### Frontend Components

New components in `interface/src/components/structured/`:

- `ChatButtons.tsx` — button group
- `CardSelect.tsx` — selectable card grid with icon, title, description, tags
- `ProgressSteps.tsx` — horizontal step indicator
- `SummaryCard.tsx` — key-value summary with action buttons

Integrated into both `WebChatPanel` and `CortexChatPanel`.

### Files (Rust)

- `src/api/types.rs` or inline — `StructuredContent`, `StructuredKind`, serde
- Extend `ApiEvent::OutboundMessage` with optional `structured` field
- Extend `webchat.rs` and `cortex.rs` SSE payloads

### Files (TypeScript)

- `interface/src/api/client.ts` — extend types
- `interface/src/components/structured/ChatButtons.tsx`
- `interface/src/components/structured/CardSelect.tsx`
- `interface/src/components/structured/ProgressSteps.tsx`
- `interface/src/components/structured/SummaryCard.tsx`
- `WebChatPanel.tsx` and `CortexChatPanel.tsx` — render structured content

---

## Phase 5: Factory Integration

### Entry Points

Both paths converge on the same backend:

**Portal webchat (main agent):** User says "create me a support agent" in chat. The channel detects factory intent and delegates to a factory branch with factory tools + memory tools + the factory system prompt.

**Cortex chat:** Factory tools are added to the cortex chat tool server. Same conversational flow, same backend.

### Creation Flow

```
User: "I need an agent that handles customer support tickets from email"

Agent: [branch → factory_search_context("company info", "support process")]
       [branch → factory_list_presets()]

Agent: "Found context about your company. Here are starting points:"
       [structured: SelectCards — Customer Support, Community Manager,
        Executive Assistant, with relevance tags]

User: [clicks Customer Support]

Agent: [branch → factory_load_preset("customer-support")]
       "How should this agent communicate?"
       [structured: Buttons — "Professional & formal", "Friendly & warm",
        "Match the customer's tone", "Same as {main_agent}"]

User: "Friendly but professional. We're a dev tools company."

Agent: "Should it handle everything or escalate some things?"
       [structured: Buttons — "Handle everything", "Escalate billing/refunds",
        "Escalate complex issues", "Custom rules"]

User: "Escalate billing, refunds, and anything legal."

Agent: "Where does it fit in the org?"
       [structured: Buttons — "Reports to main", "Peer of main",
        "Independent"]

User: [clicks "Reports to main"]

Agent: [factory_create_agent with synthesized identity files]
       [structured: Summary — ID, role, personality, escalation rules,
        model, org position, with "Refine" and "Looks good" buttons]

User: "Looks good!"

Agent: "support is live. Route email to it from Settings > Bindings."
```

### Refinement

After creation, or for existing agents:

```
User: "make the support agent more casual"
Agent: [reads current SOUL.md → rewrites with casual voice → factory_update_identity]
       "Updated. More relaxed tone, still precise on technical stuff."
```

### Files

- `src/agent/channel.rs` — factory intent detection, factory branch spawning
- `src/agent/cortex_chat.rs` — factory mode for cortex chat
- `src/factory/flow.rs` — shared orchestration logic (if needed)

---

## Phase 6: Polish & Testing

- Integration tests for the full factory flow
- Error handling / rollback if creation fails mid-flow
- Edge cases: duplicate IDs, hosted agent limits, permissions
- UI polish: animations, loading states, error display
- Documentation updates

---

## Open Questions

1. **Factory as a skill?** Could the factory prompt and tools be packaged as a built-in skill? Keeps it modular and consistent with the skills system.

2. **Binding creation:** Should the factory also handle setting up platform bindings (e.g., "route emails from support@company.com to this agent")?

3. **Agent cloning:** Should there be a "clone this agent" shortcut?

4. **Preset contributions:** V2 could allow users to export agent configs as presets and share via registry (like skills.sh).

5. **Memory seeding:** Should the factory optionally copy a subset of the main agent's memories to the new agent?

6. **Structured content in other adapters:** Discord/Slack both support rich embeds and buttons. Map structured types to platform-native components in V2?
