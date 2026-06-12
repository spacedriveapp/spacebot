---
name: wiki-writing
description: Use when creating or substantially editing wiki pages. Covers language, tone, structure, page type selection, linking discipline, and what separates a useful wiki article from filler.
---

# Wiki Writing

Wiki pages are the persistent knowledge layer of the instance. They outlive conversations, outlive individual agents, and serve anyone who reads them months or years from now. A wiki page that reads like a hastily assembled summary is worse than no page at all — it teaches the reader not to trust the wiki.

The standard is encyclopedic: neutral, precise, well-structured, and written with the same care you'd bring to a Wikipedia article. Not a style exercise. A useful one. The goal is that someone encountering a page cold — no prior context, no conversation history — understands the subject fully on first read.

## Before Writing

Read before creating. Run `wiki_list` and `wiki_search` to check whether the topic already has coverage. Expanding an existing page is almost always better than creating a new one. Duplicate pages fragment knowledge and create maintenance burden.

If a page exists but covers the topic poorly, edit it. If the topic genuinely has no coverage and there is enough stable, verified information to sustain a standalone article, create one.

Ask: will this page still be accurate and useful in six months? If the answer depends on a conversation that happened today, the information belongs in memory, not the wiki.

## Choosing a Page Type

Each type carries expectations about what the reader will find.

**entity** — A person, team, tool, service, or organization. The reader expects to learn what it is, what it does, and how it relates to other entities. Keep it factual and current.

**concept** — A pattern, approach, methodology, or idea. The reader expects to understand what it means, when it applies, and what distinguishes it from similar concepts. Provide enough context for someone unfamiliar with the term.

**decision** — Why a significant choice was made. The reader expects the context that led to the decision, the alternatives considered, and the reasoning. Decision pages age well when they capture the "why" rather than just the "what."

**project** — A running synthesis of a workstream. The reader expects to understand current state, goals, and key milestones. Project pages require periodic updates — create one only if there is commitment to maintain it.

**reference** — Stable factual content used repeatedly. Configuration formats, API conventions, environment setup, naming schemes. The reader expects accuracy and completeness. Reference pages should be authoritative enough that people link to them rather than restating the information.

If the topic doesn't fit any type cleanly, it probably isn't ready for a standalone page. Fold it into an existing page or reconsider.

## Language

### Tone

Write in the third person. Address the subject, not the reader.

The tone is neutral, professional, and confident. State facts directly. Let the information carry its own weight — no hype, no hedging, no editorializing. The prose should feel like it was written by someone who understands the subject well enough to explain it plainly.

Good: "The routing layer selects a model based on the task's token budget and the configured priority tiers."
Bad: "The amazing routing layer intelligently selects the best model for your needs."
Bad: "It should be noted that the routing layer attempts to select a model."

### Clarity

Use plain, precise language. Prefer concrete terms over abstract ones. If a sentence can be shorter without losing meaning, make it shorter.

Define internal jargon, acronyms, and project-specific terms on first use. Assume the reader is intelligent but unfamiliar with the specific context.

Avoid:
- Filler phrases: "It is worth noting that," "Basically," "In order to," "It is important to understand that"
- Vague qualifiers: "very," "really," "quite," "somewhat," "fairly"
- Puffery: "robust," "seamless," "comprehensive," "cutting-edge," "powerful"
- Setup language: "In this section, we will cover..." — just cover it.
- Editorializing: "Unfortunately," "Fortunately," "Obviously," "Interestingly"

### Voice and Tense

Active voice is the default. Passive voice is acceptable when the actor is unknown, irrelevant, or when it genuinely improves flow — not as a habit.

Good (active): "The cortex regenerates the knowledge synthesis when memories change."
Acceptable (passive): "The migration was applied during the 0.4.0 release."
Bad (unnecessary passive): "The configuration file is read by the agent at startup." → "The agent reads the configuration file at startup."

Present tense for anything currently true. Past tense for events and discontinued practices. No future tense for aspirational features unless clearly labeled as planned.

### Sentence and Paragraph Discipline

One idea per sentence. One theme per paragraph. Paragraphs stay short — three to six sentences in most cases. Longer paragraphs signal that the content should be broken up or restructured with headings.

Vary sentence length for rhythm, but keep the average readable (15–25 words). A paragraph of exclusively short sentences reads as choppy. A paragraph of exclusively long sentences reads as dense. Mix them.

No contractions in article body prose. Use "does not" instead of "doesn't."

### Words to Watch

These words and phrases almost always signal that the sentence should be rewritten:

- "leverage," "utilize" → use
- "facilitate" → help, enable, or just describe what happens
- "synergy," "paradigm," "ecosystem" → say what you actually mean
- "best practice" → name the practice
- "going forward" → cut it
- "in terms of" → restructure the sentence
- "at the end of the day" → cut it
- "a number of" → some, several, or the actual number

## Structure

### Lead Sentence

Every page opens with a single sentence that defines the subject. The lead sentence tells the reader what the thing is in the plainest terms possible. It should make sense to someone who has never encountered the topic.

Good: "The cortex is a background process that maintains an agent's long-term memory and synthesizes knowledge across conversations."
Good: "Branch workers are short-lived processes spawned by a channel to handle tasks that require tool use or extended reasoning."
Bad: "This page documents the cortex." (Says nothing about the subject.)
Bad: "The cortex is a really important part of the system that does a lot of things." (Says nothing specific.)

### Body

Use `##` headings to organize the body into logical sections. Choose section titles that are short noun phrases — not questions, not gerunds, not full sentences.

Good: `## Architecture`, `## Configuration`, `## Versioning`
Bad: `## How Does the Architecture Work?`, `## Understanding the Configuration`

Organize by the structure that best serves the topic:
- Chronological for decisions and project histories
- Thematic for concepts and entities
- Procedural for reference pages that describe workflows

### Lists vs. Prose

Use bulleted lists for discrete items: feature sets, configuration options, enumerated choices. Use prose for explanation, context, and narrative. If a bullet point needs more than two sentences of explanation, it belongs in a paragraph instead.

### Wiki Links

Link to other wiki pages on first mention of a notable entity using `[display text](wiki:slug)`. After the first link, use the plain term without relinking.

Only link when the target page adds genuine context. Overlinking clutters the prose and trains readers to ignore links. Do not link common terms that do not have meaningful wiki pages.

Bad: "The [agent](wiki:agent) uses [memory](wiki:memory) to store [information](wiki:information)."
Good: "The agent uses the [memory graph](wiki:memory-graph) to persist knowledge across sessions."

### Related Pages

The `related` field captures coarse relationships — pages that share significant context with this one. Use it for pages that a reader of this article would plausibly want to read next. Three to five related slugs is typical. Do not list every page that mentions the same topic.

### Edit Summaries

Every edit gets a summary. The summary explains what changed and why in one sentence. Future readers and agents use these to understand the page's evolution without diffing every version.

Good: "Added configuration section covering environment variables and defaults."
Good: "Corrected the API endpoint path from /v1/chat to /v2/chat."
Bad: "Updated." (Useless.)
Bad: "Fixed some stuff." (Useless.)

## When Not to Create a Page

Not everything belongs in the wiki. Skip it when:

- The information is transient or conversation-specific → use memory instead
- There is not enough verified content for a substantive article → wait until there is
- The topic is already covered adequately on an existing page → add a section there
- The page would consist of a single paragraph with no room to grow → fold it into a related page

A wiki with fifty well-written pages is more valuable than one with two hundred stubs.

## Editing Existing Pages

Read the full page with `wiki_read` before making any edit. Understand the existing structure, tone, and coverage before changing anything.

When adding content, match the existing style. If the page uses third-person encyclopedic tone, do not introduce second-person instructions. If headings use noun phrases, do not add a heading in question form.

When correcting factual content, verify the correction against authoritative sources before applying it. Include the basis for the correction in the edit summary.

When restructuring, preserve all existing information unless it is demonstrably wrong or duplicated. Reorganization is not a license to delete content.
