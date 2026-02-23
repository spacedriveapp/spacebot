---
title: "Evolving Intelligence System — Milestone 2: Core Learning (Layers 1 & 2)"
type: feat
date: 2026-02-22
milestone: 2 of 4
depends_on: milestone-1-foundation
brainstorm: docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md
research: docs/research/spark-parity.md
---

# Evolving Intelligence — Milestone 2: Core Learning (Layers 1 & 2)

Implement the outcome tracking and meta-learning layers — the system starts actually learning from interactions. After this milestone, Spacebot records episodes, tracks predictions vs outcomes, extracts distillations, detects cognitive signals, and gates quality through Meta-Ralph.

## Overview

Layer 1 (Outcome Tracking) is the foundation — it records what happened and extracts lessons. Layer 2 (Meta-Learning) builds on Layer 1 — it categorizes insights, checks quality, detects contradictions, and promotes durable learnings to the memory graph. Together they form the "learn" portion of the core loop: `Predict → Act → Evaluate → Learn`.

**Deliverable:** The system records every worker/branch interaction as an episode, generates step envelopes for tool calls, produces distillations (5 types), extracts cognitive signals from user messages, quality-gates everything through Meta-Ralph, detects contradictions, and promotes validated insights to the memory graph.

## Proposed Solution

### Phase 1: Outcome Tracking (Layer 1)

#### 1A: Episode Lifecycle

Wire the learning engine's event handler to create and complete episodes.

**On `WorkerStarted`:**
1. Create episode row in `learning.db` (recommend: `episode_id = format!("worker:{worker_id}")`) and persist:
   - `process_id`, `process_type`
   - `channel_id` (nullable)
   - `trace_id` (if present)
2. Initialize `predicted_outcome: "unknown"` and `predicted_confidence: 0.0`
3. Query Outcome Predictor for a prediction (async, non-blocking)
4. Update episode with prediction when available

**On `WorkerComplete`:**
1. Load episode by `episode_id` / `process_id` (fail-open: create a placeholder if missing)
2. Record `actual_outcome` from `success` + persist `duration_secs`
3. Compute `surprise_level = |predicted_confidence - actual_confidence|`
4. Mark episode `completed_at`
5. Queue for distillation extraction (batch timer)

**On `BranchStarted` / `BranchResult`:**
Same pattern as Worker (`episode_id = format!("branch:{branch_id}")`), but shorter-lived and with conclusion adoption tracking.

**Stale episode cleanup:**
Timer task (configurable, default 30 min) closes episodes that never received a completion event. Marks as `actual_outcome: "abandoned"`.

#### 1B: Step Envelopes

The learning engine creates step envelopes from `ToolStarted` / `ToolCompleted` event pairs.

> Spark parity: step pairing must use a deterministic tool call correlation ID (`call_id`), **never** `(episode_id, tool_name)`. Tool names repeat frequently within a single episode.

**Note on OpenCode workers:** OpenCode executes tools out-of-process. In Milestone 2, step envelopes are guaranteed only for Rig tool calls observed by `SpacebotHook`. For OpenCode workers, record coarse steps (or only episode-level summaries) until we add OpenCode SSE → `ToolStarted`/`ToolCompleted` translation.

**On `ToolStarted`:**
1. Create step record linked to the active episode, keyed by `(episode_id, call_id)`
2. Persist `tool_name`, `call_id`, `trace_id`, and a **sanitized args summary** (never raw file contents)
3. For high-stakes calls (deploy/delete/file writes, etc.): queue richer “before-action” fields for LLM enrichment on the batch timer
4. For routine calls: store a lightweight envelope only (tool_name + timestamp)

**On `ToolCompleted`:**
1. Find matching step by `call_id` (fail-open: create a placeholder step if ToolStarted was missed due to lag)
2. Record capped `result`, `evaluation`, timestamps
3. Compute `surprise_level` based on expected vs actual
4. Set `evidence_gathered` and `progress_made` flags

**Args summarization (required):**
- `file`: store `operation` + `path` only; drop `content`
- `shell`/`exec`: store command “shape” (first token + key flags), redact secrets, cap length

**High-stakes tool detection:**
Extend `LearningConfig` with a tool+operation aware configuration:
```rust
pub high_stakes_tools: Vec<String>,          // default: ["shell", "exec"]
pub high_stakes_file_operations: Vec<String> // default: ["write"]
```

#### 1C: Outcome Predictor

Lightweight counter table with Beta prior smoothing.

**Schema:**

```sql
CREATE TABLE outcome_predictions (
    key TEXT PRIMARY KEY,           -- "phase:intent_family:tool"
    success_count REAL NOT NULL DEFAULT 3.0,  -- Beta prior
    failure_count REAL NOT NULL DEFAULT 1.0,
    last_updated TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Prediction logic:**
```
predicted_success_rate = success_count / (success_count + failure_count)
```

**Fallback chain:** exact key → `phase:*:tool` → `*:intent_family:tool` → `*:*:tool` → prior (0.75).

**Update on episode completion:**
- Success: `success_count += 1.0`
- Failure: `failure_count += 1.0`
- Max 2000 keys (LRU eviction on `last_updated`)
- Confidence saturates at ~20 samples

#### 1D: Distillation Engine

Extract lessons from completed episodes. Runs on batch timer (`batch_interval_secs`, default 60s).

**5 distillation types:**

| Type | Priority | Initial confidence cap |
|------|----------|----------------------|
| Policy | 1 | 0.40 |
| Playbook | 2 | 0.30 |
| Sharp Edge | 3 | 0.35 |
| Heuristic | 4 | 0.40 |
| Anti-Pattern | 5 | 0.35 |

**Schema:**

```sql
CREATE TABLE distillations (
    id TEXT PRIMARY KEY,
    distillation_type TEXT NOT NULL,     -- policy/playbook/sharp_edge/heuristic/anti_pattern
    statement TEXT NOT NULL,
    confidence REAL NOT NULL,
    triggers TEXT NOT NULL DEFAULT '[]',  -- JSON array
    anti_triggers TEXT NOT NULL DEFAULT '[]',
    domains TEXT NOT NULL DEFAULT '[]',
    times_retrieved INTEGER DEFAULT 0,
    times_used INTEGER DEFAULT 0,
    times_helped INTEGER DEFAULT 0,
    validation_count INTEGER DEFAULT 0,
    contradiction_count INTEGER DEFAULT 0,
    source_episode_id TEXT,
    revalidate_by TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_distillations_type ON distillations(distillation_type);
CREATE INDEX idx_distillations_confidence ON distillations(confidence);
```

**Extraction pipeline (per completed episode):**
1. Gather episode + steps + outcome data
2. LLM-assisted extraction: "Given this episode, extract any lessons as distillations"
3. For each candidate distillation:
   a. **Quality gate**: Reject < 20 chars, reject tautologies, reject generic advice
   b. **Duplicate check**: Word overlap > 0.5 against existing distillations → merge
   c. **Playbook validation**: Requires 2+ unique step types
   d. **Type classification**: LLM classifies into 5 types
   e. **Trigger extraction**: LLM extracts trigger patterns from episode context
4. Store with initial confidence caps

**Confidence evolution (on subsequent episodes):**
- Success validates: `confidence + (1 - confidence) * 0.1`
- Failure contradicts: `confidence * 0.85`

**Pruning (on maintenance timer):**
- Remove distillations with `success_ratio < 0.15` after 10+ uses
- Collapse duplicates by normalized text (strip punctuation, lowercase, replace numbers)

#### 1E: Structural Retrieval

Retrieve distillations by type priority, not embedding similarity.

```rust
pub async fn retrieve_relevant(
    store: &LearningStore,
    intent: &str,
    tool: Option<&str>,
    domain: Option<&str>,
    max_results: usize,
) -> Vec<Distillation> {
    // Query in priority order: Policy → Playbook → Sharp Edge → Heuristic → Anti-Pattern
    // Within each type: trigger match → domain match → keyword overlap
    // Stop when max_results reached
}
```

**Matching logic:**
1. Trigger matching: any trigger substring found in intent
2. Domain matching: any domain matches current context
3. Keyword overlap: tokenize both, compute Jaccard similarity (with stop-word filtering)
4. Anti-trigger check: skip if any anti-trigger matches

**Cache:** `DashMap<String, Vec<Distillation>>` keyed by `distillation_type`, refreshed every `tick_interval_secs`.

#### 1F: Implicit Outcome Tracking

Link pre-action advisory advice to post-action outcomes.

**Schema:**

```sql
CREATE TABLE implicit_feedback (
    id TEXT PRIMARY KEY,
    advice_text TEXT NOT NULL,
    source_id TEXT,                -- distillation or insight ID
    tool_name TEXT,
    episode_id TEXT,
    outcome TEXT,                  -- 'followed'/'unhelpful'/'ignored'
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX idx_implicit_feedback_source ON implicit_feedback(source_id);
```

**Flow:**
1. Before tool call: record any advice texts and source IDs
2. After tool call completes (within 5-min TTL): match outcome
3. Record "followed" (advice + success) or "unhelpful" (advice + failure)

#### 1G: Policy Patches

Convert high-confidence distillations into enforced behavior.

**Schema:**

```sql
CREATE TABLE policy_patches (
    id TEXT PRIMARY KEY,
    source_distillation_id TEXT NOT NULL,
    trigger_type TEXT NOT NULL,       -- ERROR_COUNT/PHASE_ENTRY/PATTERN_MATCH/CONFIDENCE_DROP
    trigger_config TEXT NOT NULL,     -- JSON: threshold, pattern, etc.
    action_type TEXT NOT NULL,        -- EMIT_WARNING/REQUIRE_STEP/ADD_CONSTRAINT/FORCE_VALIDATION
    action_config TEXT NOT NULL,      -- JSON: message, step details, etc.
    enabled INTEGER DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Default patches (created on first run):**
- "Two Failures Rule": trigger=ERROR_COUNT(2), action=REQUIRE_STEP("Run diagnostic before retrying")
- "File Thrash Prevention": trigger=PATTERN_MATCH(same file edited 3x), action=EMIT_WARNING("Consider a different approach")

**Enforcement point:** In the learning engine's event handler, check policy patches against each event. If a patch fires, inject the action into the worker context via the existing `CortexHook` or by writing to a shared advisory buffer.

### Phase 2: Meta-Learning (Layer 2)

#### 2A: Cognitive Signal Extraction

Parse user messages for learning signals (runs on `UserMessage` events).

**10 domain detection categories** with keyword lists:
```rust
const DOMAINS: &[(&str, &[&str])] = &[
    ("coding", &["code", "function", "bug", "compile", "deploy", "git", "rust", "python"]),
    ("research", &["search", "find", "look up", "source", "reference", "paper"]),
    ("productivity", &["task", "deadline", "schedule", "priority", "organize"]),
    ("communication", &["email", "message", "reply", "draft", "tone"]),
    ("health", &["exercise", "sleep", "diet", "wellness", "mental"]),
    ("finance", &["budget", "expense", "invest", "save", "cost"]),
    ("learning", &["study", "course", "practice", "understand", "concept"]),
    ("creative", &["design", "write", "create", "idea", "brainstorm"]),
    ("social", &["relationship", "friend", "family", "network", "community"]),
    ("maintenance", &["fix", "repair", "clean", "organize", "maintain"]),
];
```

**5 cognitive pattern types** detected via regex:
- `remember`: "remember that...", "keep in mind...", "don't forget..."
- `preference`: "I prefer...", "I like...", "I don't like...", "always use..."
- `decision`: "let's go with...", "I decided...", "we'll use..."
- `correction`: "that's wrong", "actually...", "no, I meant...", "fix that"
- `reasoning`: "because...", "the reason is...", "this works because..."

**Pipeline:**
1. Detect domains from keywords
2. Detect cognitive patterns from regex
3. Extract most learning-worthy sentence
4. Create insight candidate → send to Meta-Ralph quality gate

**Schema:**

```sql
CREATE TABLE cognitive_signals (
    id TEXT PRIMARY KEY,
    message_content TEXT NOT NULL,
    detected_domains TEXT NOT NULL,     -- JSON array
    detected_patterns TEXT NOT NULL,    -- JSON array
    extracted_candidate TEXT,
    ralph_verdict TEXT,                 -- QUALITY/NEEDS_WORK/PRIMITIVE/DUPLICATE
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

#### 2B: Meta-Learning Insight Store

**8 categories** with category-specific behavior:

| Category | Decay half-life | Example |
|----------|----------------|---------|
| Self-Awareness | 90 days | "I tend to over-explain" |
| User Model | Never | "User prefers bullet points" |
| Reasoning | 60 days | "File search before grep reduces false starts" |
| Context | 45 days | "User is terse in mornings" |
| Wisdom | Never | "Ask before acting on irreversible operations" |
| Communication | 60 days | "On Discord, keep under 300 words" |
| Domain Expertise | 90 days | "Strong at Rust, weaker at CSS" |
| Relationship | Never | "User jokes about deadlines but takes them seriously" |

**Schema:**

```sql
CREATE TABLE insights (
    id TEXT PRIMARY KEY,
    category TEXT NOT NULL,
    content TEXT NOT NULL,
    reliability REAL NOT NULL DEFAULT 0.5,
    confidence REAL NOT NULL DEFAULT 0.3,
    validation_count INTEGER DEFAULT 0,
    contradiction_count INTEGER DEFAULT 0,
    quality_score REAL,                    -- Meta-Ralph score (0-12)
    advisory_readiness REAL DEFAULT 0.0,
    source_type TEXT,                      -- 'outcome'/'cognitive_signal'/'chip'/'compaction'
    source_id TEXT,                        -- episode_id / signal_id / chip_id
    promoted INTEGER DEFAULT 0,            -- 1 if promoted to memory graph
    promoted_memory_id TEXT,               -- memory graph ID after promotion
    last_validated_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_insights_category ON insights(category);
CREATE INDEX idx_insights_reliability ON insights(reliability);
CREATE INDEX idx_insights_promoted ON insights(promoted);
```

**Scoring model:**
```
reliability = weighted_validated / (weighted_validated + contradicted)
```
With Bayesian prior (starts at 0.5).

**Confidence boost per validation:** `confidence + (1 - confidence) * 0.25`, capped at 0.99.

**Validation quality weights:**
- Direct user confirmation: 1.0x
- Outcome-linked evidence: 0.5x
- Auto-evidence (from matching events): 0.25x
- Low-signal tasks: 0.15x

**Advisory readiness formula:**
```
readiness = 0.10 + 0.45 * quality + 0.20 * confidence + validation_bonus - contradiction_penalty
```

#### 2C: Meta-Ralph Quality Gate

Every proposed learning is challenged before storage.

**Pipeline:**

1. **Primitive filter** (fast, no LLM):
   - Reject if tool name appears in text body
   - Reject operational keyword combos ("executed", "returned", "output")
   - Reject arrow patterns ("→", "->")
   - Reject statements < 20 chars
   - Reject tautologies matching known patterns ("always check", "be careful", "make sure")
   - Reject generic advice ("generally", "usually", "often")

2. **Duplicate detection**:
   - Compute semantic hash: `md5(normalize(text))` where normalize = lowercase, strip punctuation, replace numbers with `N`
   - Skip if hash exists in `ralph_verdicts` table

3. **6-dimension quality scoring** (programmatic heuristic, not LLM):

   | Dimension | Score 0 | Score 1 | Score 2 |
   |-----------|---------|---------|---------|
   | actionability | No verb/action | Vague action | Clear, specific action |
   | novelty | Exact duplicate | Similar to existing | Genuinely new |
   | reasoning | No reasoning | Some logic | Clear cause-effect |
   | specificity | Very generic | Somewhat specific | Concrete, measurable |
   | outcome_linked | No outcome | Weak link | Direct outcome evidence |
   | ethics | Harmful/risky | Neutral | Clearly safe |

   Scoring via keyword/pattern heuristics per dimension. Total out of 12.

4. **Verdict assignment:**
   - `quality_score >= 4` → QUALITY (store)
   - `quality_score >= 2` → NEEDS_WORK (attempt auto-refinement, re-score once)
   - `quality_score < 2` → PRIMITIVE (reject)
   - Hash collision → DUPLICATE (skip)

5. **Outcome tracking:**
   When a stored learning is later retrieved and used, create an outcome record. On task completion within attribution window (1200s), record good/bad/neutral.

**Schema:**

```sql
CREATE TABLE ralph_verdicts (
    id TEXT PRIMARY KEY,
    input_text TEXT NOT NULL,
    input_hash TEXT NOT NULL,          -- semantic hash for dedup
    verdict TEXT NOT NULL,             -- QUALITY/NEEDS_WORK/PRIMITIVE/DUPLICATE
    scores TEXT NOT NULL,              -- JSON: {actionability, novelty, reasoning, specificity, outcome_linked, ethics}
    total_score REAL NOT NULL,
    refinement_attempted INTEGER DEFAULT 0,
    source_type TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_ralph_hash ON ralph_verdicts(input_hash);
CREATE INDEX idx_ralph_verdict ON ralph_verdicts(verdict);

CREATE TABLE outcome_records (
    id TEXT PRIMARY KEY,
    insight_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    outcome TEXT,                      -- 'good'/'bad'/'neutral'
    episode_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX idx_outcome_trace ON outcome_records(trace_id);
```

#### 2D: Contradiction Detection

Detect when new insights contradict existing ones.

**Algorithm:**
1. For each existing insight in same category: extract topics (strip common prefixes)
2. Compute similarity: embedding cosine + word overlap
3. If similarity >= 0.6, check for opposition:
   - 12 opposition pairs: prefer/avoid, like/hate, always/never, should/shouldn't, good/bad, increase/decrease, enable/disable, include/exclude, before/after, more/less, fast/slow, simple/complex
   - Negation asymmetry: one text contains negation ("not", "don't", "never"), other doesn't
4. Classify contradiction type:
   - TEMPORAL: contains "now", "currently", "recently", "changed" → new supersedes old
   - CONTEXTUAL: contains "when", "if", "during", "sometimes" → both valid in different contexts
   - DIRECT: mutually exclusive → resolve
   - UNCERTAIN: unclear → flag for review

**Resolution:**
- TEMPORAL: Update old insight's `contradiction_count`, mark as superseded
- CONTEXTUAL: Keep both, tag with context
- DIRECT: Increment `contradiction_count` on lower-reliability insight
- UNCERTAIN: Keep both, log for potential human review

**Schema:**

```sql
CREATE TABLE contradictions (
    id TEXT PRIMARY KEY,
    insight_a_id TEXT NOT NULL,
    insight_b_id TEXT NOT NULL,
    contradiction_type TEXT NOT NULL,   -- TEMPORAL/CONTEXTUAL/DIRECT/UNCERTAIN
    resolution TEXT,                    -- 'update'/'context'/'keep_both'/'discard_new'
    similarity_score REAL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

#### 2E: Auto-Promotion

Promote validated insights to the memory graph as existing MemoryTypes.

**Trigger:** Rate-limited check (default hourly) + session end.

**Promotion criteria:**
- `reliability >= 0.7`
- `validation_count >= 3`
- `quality_score >= 4` (passed Meta-Ralph)
- Not already promoted

**Type mapping:**
| Insight source | Memory type | Importance |
|---------------|-------------|------------|
| Heuristic distillation | Observation | 0.6 |
| Sharp Edge distillation | Observation | 0.7 |
| Anti-Pattern distillation | Observation | 0.7 |
| Playbook distillation | Observation | 0.8 |
| Policy distillation | Decision | 0.9 |
| User preference signal | Preference | 0.7 |
| Self-awareness insight | Observation | 0.5 |
| Reasoning insight | Observation | 0.6 |

**Promotion flow:**
1. Query insights meeting threshold
2. For each: create `Memory` with mapped type and importance
3. Save to main memory graph via `deps.memory_search.store().save()`
4. Embed via `deps.memory_search.embedding_table().store()`
5. Update insight: `promoted = 1`, `promoted_memory_id = memory.id`
6. Log promotion event

**Demotion flow (periodic check):**
- Query promoted insights where `reliability` has dropped below 0.5
- Soft-delete the promoted memory: `deps.memory_search.store().forget(memory_id)`
- Update insight: `promoted = 0`, clear `promoted_memory_id`
- Log demotion event

## Acceptance Criteria

### Functional Requirements

- [ ] Episodes created on WorkerStarted, completed on WorkerComplete
- [ ] Step envelopes created from ToolStarted/ToolCompleted using `call_id` (and `args_summary` when available)
- [ ] High-stakes tools get richer step data (configurable tool list)
- [ ] Outcome Predictor tracks success/failure rates with Beta prior
- [ ] Prediction fallback chain works (exact → coarser → prior)
- [ ] Distillation extraction runs on batch timer (not blocking hot path)
- [ ] 5 distillation types classified and stored
- [ ] Distillation quality gate rejects tautologies, short statements, generic advice
- [ ] Duplicate distillations merged by word overlap > 0.5
- [ ] Structural retrieval returns distillations in type priority order
- [ ] Trigger matching, domain matching, keyword overlap all work
- [ ] Distillation cache refreshes on timer
- [ ] Implicit outcome tracking links advice to outcomes within 5-min TTL
- [ ] Policy patches stored and checked against events
- [ ] Default policy patches created on first run
- [ ] Cognitive signals extracted from user messages (10 domains, 5 patterns)
- [ ] 8 insight categories with category-specific decay
- [ ] Meta-Ralph primitive filter rejects operational noise
- [ ] Meta-Ralph duplicate detection via semantic hashing
- [ ] Meta-Ralph 6-dimension scoring produces verdicts
- [ ] NEEDS_WORK insights get one auto-refinement attempt
- [ ] Contradiction detection finds opposing insights (12 opposition pairs)
- [ ] 4 contradiction types classified with appropriate resolution
- [ ] Auto-promotion runs hourly and at session end
- [ ] Promoted insights appear in memory graph as correct MemoryTypes
- [ ] Demotion removes memories for insights that fell below threshold
- [ ] Stale episodes cleaned up after timeout
- [ ] All learning processing is async and fail-open
- [ ] `cargo clippy` and `cargo test` pass

### Quality Gates

- [ ] Outcome Predictor accuracy improves over 20+ episodes (can verify with test data)
- [ ] Meta-Ralph pass rate is between 20-60% (not too permissive, not too strict)
- [ ] Contradiction detection has zero false positives on test data (conservative matching)

## Technical Considerations

### LLM batch timer design

Distillation extraction is the first LLM-dependent operation. Design the batch system:
1. Queue completed episodes that need extraction
2. On batch timer tick: dequeue up to N episodes (start with 3)
3. Call LLM with episode summary + steps
4. Parse structured output (distillation candidates)
5. On LLM failure: re-queue episode, max 3 retries, then close without extraction

### Meta-Ralph: programmatic, not LLM

Per SpecFlow Gap Q5, Meta-Ralph scoring should be programmatic (keyword/regex heuristics) for zero latency. The 6 dimensions are scored via pattern matching:
- **actionability**: presence of verbs, action words
- **novelty**: word overlap with existing insights (low overlap = novel)
- **reasoning**: causal connectors ("because", "therefore", "since")
- **specificity**: named entities, numbers, specific tools/files
- **outcome_linked**: references to success/failure, outcome terms
- **ethics**: absence of harmful patterns

### Cold start thresholds (SpecFlow Gap Q4)

During first 50 episodes, apply relaxed thresholds:

| Threshold | Normal | Cold start (episodes 0-25) | Transition (25-50) |
|-----------|--------|--------------------------|-------------------|
| Meta-Ralph quality floor | 4/12 | 2/12 | Linear interpolation |
| Promotion reliability | 0.7 | 0.4 | Linear interpolation |
| Promotion validation count | 3 | 1 | Linear interpolation |
| Advisory readiness | formula | formula * 0.5 | Linear interpolation |

### Step envelope creation (SpecFlow Gap Q3)

The learning engine synthesizes lightweight step envelopes from ToolStarted/ToolCompleted event pairs. Full envelopes (with hypothesis, alternatives, assumptions) are only generated for high-stakes tools via LLM on the batch timer. This avoids blocking the hot path while still capturing rich data for important actions.

### Layer ordering (SpecFlow Gap Q8)

For `WorkerCompleted` events:
1. Layer 1 outcome evaluation runs first (lightweight, synchronous within the async handler)
2. Layer 2 meta-learning runs after Layer 1 completes (awaits Layer 1)
3. LLM batch extraction (Layer 1 distillation) runs independently on its timer

This ensures Layer 2 always has Layer 1's outcome data, without waiting for expensive LLM operations.

## Dependencies & Risks

- **Depends on Milestone 1** completing successfully (events, database, config, module structure)
- **LLM costs:** Distillation extraction calls the LLM per completed episode. At high activity, this could be expensive. Batch timer + max queue depth mitigate this.
- **Embedding model availability:** Contradiction detection uses cosine similarity. Requires `EmbeddingModel` from `deps`. Should already be available via `AgentDeps`.
- **Memory graph writes during promotion:** Must handle the case where the memory store is temporarily unavailable or under heavy load. Retry with backoff.

## References

### Internal References
- Brainstorm Layer 1: `docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md:133-276`
- Brainstorm Layer 2: `docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md:279-376`
- Memory types: `src/memory/types.rs:94` (MemoryType enum)
- Memory store save: `src/memory/store.rs` (MemoryStore::save)
- Embedding model: `src/memory/embedding.rs`
- LLM integration pattern: `src/agent/worker.rs` (how workers call LLMs)

### New Files (this milestone)
```
src/learning/
  outcome.rs          — Episode lifecycle, step envelopes, outcome predictor
  distillation.rs     — Distillation engine, extraction, quality gate, pruning
  retriever.rs        — Structural retrieval by type priority
  meta.rs             — Meta-learning insights, scoring model, auto-promotion
  ralph.rs            — Meta-Ralph quality gate (primitive filter, 6-dim scoring)
  contradiction.rs    — Contradiction detection (similarity + opposition pairs)
  signals.rs          — Cognitive signal extraction from user messages
  feedback.rs         — Implicit outcome tracking (advice → outcome linkage)
  patches.rs          — Policy patches (distillation → enforcement)
```
