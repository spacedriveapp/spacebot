---
title: "Evolving Intelligence System — Milestone 3: Intelligence (Layers 3 & 4 + Cross-Cutting)"
type: feat
date: 2026-02-22
status: completed
milestone: 3 of 4
depends_on: milestone-2-core-learning
brainstorm: docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md
research: docs/research/spark-parity.md
---

# Evolving Intelligence — Milestone 3: Intelligence (Layers 3 & 4 + Cross-Cutting)

Implement the advisory gating system, domain chips, and all cross-cutting systems. After this milestone, Spacebot actively injects learned knowledge into conversations, surfaces risk flags, runs domain-specialized learning modules, and enforces safety guardrails.

## Overview

Layer 3 (Advisory Gating) decides WHAT to surface and WHEN — it transforms raw learnings into actionable advice with 5 authority levels, agreement gating, cooldowns, and emission budgets. Layer 4 (Domain Chips) adds specialized learning per domain. Cross-cutting systems (Control Plane, Evidence Store, Truth Ledger, Phase State Machine, Escape Protocol, Memory Gate) provide the infrastructure that all layers share.

**Deliverable:** Spacebot actively advises the user based on learned patterns, runs domain-specialized chips that produce quality-gated insights, enforces safety guardrails via the Control Plane, and tracks evidence/truth/phase state across the entire learning pipeline.

## Proposed Solution

### Phase 3: Advisory Gating (Layer 3)

#### 3A: Advisory Scoring Engine

Compute advisory scores for all candidate insights/distillations.

**Base score formula:**
```
score = 0.45 * context_match + 0.25 * confidence + 0.15
```

**Context match** is computed from:
- Tool name overlap with insight triggers
- Domain overlap with current task context
- Keyword overlap with current intent
- Returns 0.0-1.0

**Score modifiers:**
- Phase-based boost: multiply by phase-category matrix value
- Negative advisory boost: × 1.3 for "don't do X" advice
- Failure-context boost: × 1.5 during debugging (consecutive failures >= 1)
- Outcome risk: boost cautionary advice when outcome predictor predicts likely failure

#### 3B: 5-Level Advisory Authority

| Level | Threshold | Behavior | Gate |
|-------|-----------|----------|------|
| BLOCK | >= 0.95 | Hard stop. Handled by Control Plane. | None — always fires |
| WARNING | >= 0.80 | Bulletin + recall + risk flags | Agreement gating required |
| NOTE | >= 0.48 | Bulletin if space, recall available | None |
| WHISPER | >= 0.30 | Recall only, very brief | None |
| SILENT | < 0.30 | Log only. Quarantine audit trail. | None |

**Agreement gating for WARNING:**
- Group candidates by normalized text signature
- Count distinct source IDs (distillation, insight, chip)
- WARNING requires 2+ independent sources
- Exception: Policy-type distillations bypass agreement gating (SpecFlow Gap Q6)

#### 3C: Phase-Based Boost Matrix

Full phase detection + boost system.

**Phase detection** (from current activity signals):

```rust
#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Explore,
    Plan,
    Execute,
    Validate,
    Consolidate,
    Diagnose,
    Simplify,
    Escalate,
    Halt,
}
```

Detection heuristics (Spacebot tool names):
- `Explore`: `file(read)` / `web_search` / `browser` / `shell` (read-only commands like `rg`, `ls`) dominant
- `Plan`: No tools, long messages, "plan"/"approach" keywords
- `Execute`: `file(write)` / `shell` / `exec` dominant
- `Validate`: test commands observed via `shell`/`exec` (e.g. `cargo test`, `pytest`) + "test"/"verify" keywords
- `Consolidate`: Compaction active, summary generation
- `Diagnose`: Consecutive failures >= 2
- `Simplify`: Refactor keywords after failures
- `Escalate`: Escape protocol triggered
- `Halt`: No activity for extended period

**Boost matrix** (reconciled phase names per SpecFlow Gap Q9):

| Phase | Self-Awareness | Wisdom | Reasoning | User Model | Domain Expertise | Context |
|-------|---------------|--------|-----------|------------|-----------------|---------|
| Explore | 1.0 | 1.0 | 1.2 | 1.0 | 1.5 | 1.2 |
| Plan | 1.0 | 1.5 | 1.5 | 1.0 | 1.2 | 1.0 |
| Execute | 1.2 | 1.0 | 1.0 | 1.0 | 1.2 | 1.0 |
| Validate | 1.0 | 1.0 | 1.5 | 1.0 | 1.0 | 1.0 |
| Consolidate | 1.0 | 1.2 | 1.0 | 1.0 | 1.0 | 1.0 |
| Diagnose | 1.5 | 1.2 | 1.5 | 1.0 | 1.2 | 1.0 |
| Simplify | 1.2 | 1.0 | 1.2 | 1.0 | 1.0 | 1.0 |
| Escalate | 1.0 | 1.5 | 1.0 | 1.0 | 1.0 | 1.2 |
| Halt | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 |

#### 3D: Advisory Packet Store

Full lifecycle with effectiveness tracking.

**Schema:**

```sql
CREATE TABLE advisory_packets (
    id TEXT PRIMARY KEY,
    intent_family TEXT,
    tool_name TEXT,
    phase TEXT,
    context_key TEXT,
    advice_text TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_type TEXT NOT NULL,         -- 'distillation'/'insight'/'chip'
    authority_level TEXT NOT NULL,     -- BLOCK/WARNING/NOTE/WHISPER/SILENT
    score REAL NOT NULL,
    usage_count INTEGER DEFAULT 0,
    emit_count INTEGER DEFAULT 0,
    deliver_count INTEGER DEFAULT 0,
    helpful_count INTEGER DEFAULT 0,
    unhelpful_count INTEGER DEFAULT 0,
    noisy_count INTEGER DEFAULT 0,
    effectiveness_score REAL DEFAULT 0.5,
    last_surfaced_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_packets_lookup ON advisory_packets(intent_family, tool_name, phase);
CREATE INDEX idx_packets_effectiveness ON advisory_packets(effectiveness_score);
```

**Packet retrieval** (relaxed multi-dimensional scoring):
- Tool weight: 4.0
- Intent weight: 3.0
- Phase weight: 2.0
- Effectiveness weight: 2.0
- Low-effectiveness packets (< 0.3) get penalized

**Effectiveness score update:**
```
effectiveness = (helpful_count + 1) / (helpful_count + unhelpful_count + noisy_count + 2)
```
(Bayesian smoothing with prior of 0.5)

#### 3E: Cooldowns & Suppression

**Cooldown mechanisms:**
- Per-tool cooldown: suppress same tool advice for N seconds (default 10s)
- Per-advice TTL: suppress same advice for N seconds (default 600s)
- Category-aware scaling: different categories get different cooldown multipliers
- Dedup within bulletin cycle: `HashSet<InsightId>`
- Cross-session dedup: by `advice_id` AND text signature hash

**Obvious-from-context suppression (Spacebot tool names):**
- "Read before Write" suppressed if the same file was `file(read)` within 120s
- Deployment warnings suppressed during Explore phase
- Meta-constraints suppressed on non-planning tools
- Tool-specific warnings suppressed on non-matching tools

Source of truth for recent actions: learning engine's step envelope buffer (resolves SpecFlow Gap 5.4).

#### 3F: Quarantine Audit Trail

Every dropped/suppressed advisory persisted.

**Schema:**

```sql
CREATE TABLE advisory_quarantine (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    source_type TEXT NOT NULL,
    stage TEXT NOT NULL,               -- 'primitive'/'cooldown'/'suppression'/'budget'/'agreement'
    reason TEXT NOT NULL,
    quality_score REAL,
    readiness_score REAL,
    metadata TEXT,                     -- JSON
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_quarantine_stage ON advisory_quarantine(stage, created_at);
```

Fire-and-forget writes (never raises exceptions).

#### 3G: Tiered Synthesis

Compose advice into coherent guidance.

**Tier 1 (programmatic, always available):**
- Template-based composition: `"{action} — {evidence}"`
- Action-first formatting: lead with the command, then the explanation
- Example: "Run `cargo test` first — last 3 deploys without tests failed"

**Tier 2 (AI-enhanced, budget-permitting):**
- LLM synthesis for WARNING+ when advisory budget allows
- Input: multiple advice items + context → coherent paragraph
- Timeout: configurable, part of `advisory_budget_ms`

**Policy:** Force Tier 1 on hot path. Allow Tier 2 for WARNING+ when time budget permits.

#### 3H: Emission Budget

- Max 2 items per event
- Target 5-15% emission rate
- Fallback rate guard: only triggers when Tier 2 was attempted but fell back to Tier 1 (SpecFlow Gap Q7). If such fallbacks exceed 55% of last 80 deliveries, stop Tier 2 attempts temporarily.
- Track emission rate as a rolling window metric

#### 3I: Prefetch System

Pre-generate advisory packets during idle time.

**Intent-to-tool probability mapping** (learned from episode data):

Spacebot tool names differ from Spark/Claude Code. Use Spacebot worker tools:
- `shell` (terminal)
- `file` (read/write/list via operation)
- `exec` (subprocess)

Example mapping:
```
"deploy" → shell: 0.85, file(read): 0.70
"search" → shell(rg): 0.90, file(read): 0.80
"edit"   → file(write): 0.95, file(read): 0.85
```

**Schema:**

```sql
CREATE TABLE prefetch_queue (
    id TEXT PRIMARY KEY,
    intent_family TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    tool_operation TEXT,              -- optional: e.g. "read"|"write" for the `file` tool
    phase TEXT,
    priority REAL NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',  -- pending/processing/done
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Background processing:** Between active events, process prefetch queue items. Generate advisory packets and store in `advisory_packets` table. Advice ready at action time.

#### 3J: Auto-Tuner

Closed-loop self-optimization on configurable interval (default 24h).

**Algorithm:**
1. Read per-source effectiveness data
2. Compute ideal boost: `source_effectiveness / global_average`
3. Bounded adjustment: max 15% change per run, min/max guards
4. Snapshot current tuneables before changes (for rollback)
5. Write adjusted tuneables

**Schema:**

```sql
CREATE TABLE tuneables_snapshots (
    id TEXT PRIMARY KEY,
    snapshot TEXT NOT NULL,            -- JSON of all tuneable values
    reason TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Modes:** suggest (log only) / conservative (max 5%) / moderate (max 15%) / aggressive (max 25%). Default: conservative.

#### 3K: Insight Demotion

Periodic re-evaluation of promoted memories.

- Query promoted insights where reliability < 0.5
- Soft-delete from memory graph
- Learning store retains raw data
- Log demotion to quarantine

#### 3L: Bulletin Integration

Add a new `BulletinSection` for learning insights in `gather_bulletin_sections()`.

**Integration point:** `src/agent/cortex.rs`, in the bulletin generation code. Add a section that:
1. Queries advisory gate for NOTE+ items relevant to current context
2. Formats using action-first templates
3. Respects emission budget (max 2 items in bulletin)
4. Includes distillation risk flags for active workers

### Phase 4: Domain Chips (Layer 4)

#### 4A: Chip Runtime

**Chip definition structure** (extends SKILL.md):

```rust
pub struct ChipDefinition {
    pub id: String,
    pub name: String,
    pub triggers: Vec<ChipTrigger>,
    pub observations: Vec<ObservationSpec>,
    pub success_criteria: Vec<String>,
    pub human_benefit: String,
    pub harm_avoidance: Vec<String>,
    pub risk_level: RiskLevel,        // Low, Medium, High
    pub evolution: EvolutionConfig,
}

pub struct ChipTrigger {
    pub event: Option<String>,        // ProcessEvent variant name
    pub domain: Option<String>,
    pub pattern: Option<String>,      // regex
}
```

**Event routing:** On each ProcessEvent, iterate loaded chips and check trigger matching. For each match, invoke chip's observation handler.

**Per-chip namespaced storage:**

```sql
CREATE TABLE chip_observations (
    id TEXT PRIMARY KEY,
    chip_id TEXT NOT NULL,
    field_name TEXT NOT NULL,
    field_type TEXT NOT NULL,
    value TEXT NOT NULL,
    episode_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_chip_obs ON chip_observations(chip_id, created_at);

CREATE TABLE chip_insights (
    id TEXT PRIMARY KEY,
    chip_id TEXT NOT NULL,
    content TEXT NOT NULL,
    scores TEXT NOT NULL,              -- JSON: 6-dimensional scores
    total_score REAL NOT NULL,
    promotion_tier TEXT,               -- long_term/working/session/discard
    merged INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_chip_insights ON chip_insights(chip_id, total_score);

CREATE TABLE chip_predictions (
    id TEXT PRIMARY KEY,
    chip_id TEXT NOT NULL,
    prediction TEXT NOT NULL,
    outcome TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);

CREATE TABLE chip_state (
    chip_id TEXT PRIMARY KEY,
    observation_count INTEGER DEFAULT 0,
    success_rate REAL DEFAULT 0.5,
    status TEXT NOT NULL DEFAULT 'active',  -- active/provisional/deprecated
    confidence REAL DEFAULT 0.5,
    last_triggered_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

#### 4B: 6-Dimensional Insight Scoring

```rust
pub struct InsightScores {
    pub cognitive_value: f64,      // weight 0.30
    pub outcome_linkage: f64,      // weight 0.20
    pub uniqueness: f64,           // weight 0.15
    pub actionability: f64,        // weight 0.15
    pub transferability: f64,      // weight 0.10
    pub domain_relevance: f64,     // weight 0.10
}
```

**Scoring enhancements:**
- PRIMITIVE_PATTERNS: 8+ regex for auto-low-scoring operational insights
- VALUABLE_PATTERNS: 10+ regex for auto-boosting cognitive insights
- VALUE_BOOST_KEYWORDS: "decision" +0.2, "rationale" +0.2, "lesson" +0.2
- VALUE_REDUCE_KEYWORDS: "timeout" -0.1, "tool used" -0.15
- Uniqueness: word-overlap similarity (threshold 0.8) against last 5000 seen insights

**Promotion tiers:**
- `long_term >= 0.75`: promote through merger pipeline
- `working >= 0.50`: keep in chip state, consider for promotion
- `session >= 0.30`: session-only, discard at session end
- `discard < 0.30`: drop immediately

#### 4C: Chip-to-Cognitive Merger Pipeline

Transform raw chip insights into clean learnings for Layer 2.

**Steps:**
1. Strip chip prefixes (e.g., `[Coding Intelligence]`)
2. Remove telemetry fields (`tool_name:`, `event_type:`, `cwd:`)
3. Extract structured learning payload (decision + rationale + evidence + expected_outcome)
4. Enforce quality floors: cognitive_value >= 0.35, actionability >= 0.25, transferability >= 0.2, statement length >= 28 chars
5. Duplicate churn protection: track duplicate ratio per merge cycle. If > 80% duplicates for 10+ insights, impose 30-minute cooldown
6. Category inference: map chip domains to cognitive categories
7. Output goes to Meta-Ralph quality gate (Layer 2)

#### 4D: Built-in Domain Chips

Create 5 built-in chip definitions:

| Chip | Triggers | Key observations |
|------|----------|-----------------|
| `coding` | WorkerComplete + coding tools | Tool effectiveness, error patterns, language insights |
| `research` | `web_search` / `browser` / `file(read)` / `shell` (search commands like `rg`) | Source quality, search strategy effectiveness |
| `communication` | Platform message events | Per-platform style effectiveness, response length |
| `productivity` | All worker completions | Task completion patterns, time-of-day, duration trends |
| `personal` | UserMessage + preference signals | User preference learning, correction patterns |

#### 4E: Chip Schema Validation

Required fields: `id`, `name`, `triggers`, `observations`, `human_benefit`, `harm_avoidance`, `risk_level`.

Validation strictness (configurable): warn (default) / block / strict / error.

#### 4F: Safety Policy

```rust
const BLOCKED_PATTERNS: &[&str] = &[
    "deceptive", "manipulate", "coerce", "exploit",
    "harass", "weaponize", "mislead",
];
```

- Block chip insights matching BLOCKED_PATTERNS
- Risk levels: Low (auto-evolve), Medium (log evolution), High (require human approval)
- Ethics dimension in Meta-Ralph scoring

#### 4G: Chip Evolution

- **Effectiveness tracking:** Per-chip success rates and observation counts in `chip_state`
- **Auto-deprecation:** Chips below threshold for N consecutive checks flagged and disabled
- **Trigger refinement:** Narrow triggers when chip fires without producing insights; widen when similar events produce insights elsewhere
- **Provisional chip genesis:** When 5+ valuable unmatched insights accumulate, auto-suggest new chip from keyword clustering. Provisional chips start at confidence 0.3. Validation: `insight_count >= 10 AND confidence >= 0.7` → promote to full chip.
- **Deprecated chip cleanup (SpecFlow Gap Q10):** Chip deprecation triggers re-evaluation of all insights with that chip's origin. Below-threshold insights demoted.

### Phase 5: Cross-Cutting Systems

#### 5A: Control Plane

Deterministic enforcement layer with watchers.

```rust
pub struct Watcher {
    pub name: &'static str,
    pub check: fn(&LearningState) -> Option<WatcherAction>,
    pub severity: WatcherSeverity,
}

pub enum WatcherSeverity {
    Warning,   // Advisory only
    Block,     // Prevents action
    Force,     // Forces a specific action
}
```

**Built-in watchers:**

| Watcher | Detection | Severity |
|---------|-----------|----------|
| REPEAT_ERROR | Same error 2+ times without new approach | Block |
| NO_NEW_INFO | N steps without new evidence | Warning |
| DIFF_THRASH | Same file edited 3+ times | Block |
| CONFIDENCE_STAGNATION | Confidence not improving over N steps | Warning |
| SCOPE_CREEP | Task expanding beyond original intent | Warning |
| VALIDATION_GAP | Work completed without validation | Force |

**Integration:** Watchers run after Layer 1 processes each event. Watcher results feed Layer 3 advisory gating.

**Enforcement point (not Claude Code–specific):** Spacebot is not always running inside Claude Code hooks. For Rig-based channels/branches/workers, enforce BLOCK/FORCE by intercepting tool calls in `SpacebotHook::on_tool_call()` and returning `ToolCallHookAction::Skip { reason }` when the Control Plane decides an action is not allowed.

Discord-first UX:
- When a tool call is blocked, emit a structured event (or worker status) so the channel can surface a short, user-friendly explanation.
- Optional future: provide a Discord button-based “continue anyway” override for high-stakes actions (do **not** block on user input by default).

#### 5B: Evidence Store

```sql
CREATE TABLE evidence (
    id TEXT PRIMARY KEY,
    evidence_type TEXT NOT NULL,       -- tool_output/diff/test_result/error_trace/deploy/user_flagged
    content TEXT NOT NULL,
    episode_id TEXT,
    tool_name TEXT,
    retention_hours INTEGER NOT NULL,  -- type-based: 72/168/168/168/720/0(permanent)
    compressed INTEGER DEFAULT 0,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_evidence_episode ON evidence(episode_id);
CREATE INDEX idx_evidence_expires ON evidence(expires_at);
```

**Auto-detection:** Map **Spacebot** tool calls to evidence types (use `tool_name` + sanitized args summary):
- `shell` / `exec` → `tool_output` (72h)
  - If command looks like a test runner (`cargo test`, `pytest`, etc.) → `test_result` (7d)
  - If command looks like a deploy/release (`deploy`, `release`, etc.) → `deploy` (30d)
- `file` with `operation=write` → `diff` (7d)
- Any failed tool result or worker failure summary → `error_trace` (7d)

**Cleanup (SpecFlow Gap 8.2):** Timer-based sweep (hourly), skipping evidence referenced by active (uncompleted) episodes. Compress content > 10KB before storage.

#### 5C: Truth Ledger

```sql
CREATE TABLE truth_ledger (
    id TEXT PRIMARY KEY,
    claim TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'claim',  -- claim/fact/rule/stale/contradicted
    evidence_level TEXT NOT NULL DEFAULT 'none',  -- none/weak/strong
    reference_count INTEGER DEFAULT 0,
    source_insight_id TEXT,
    last_validated_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_truth_status ON truth_ledger(status, evidence_level);
```

**Transition rules (SpecFlow Gap 8.1):**
- CLAIM → FACT: `evidence_level` reaches STRONG (3+ references)
- FACT → RULE: Validated across 3+ distinct episodes AND `reference_count >= 5`
- FACT/RULE → STALE: No validation in 60 days
- Any → CONTRADICTED: Contradiction detection flags direct conflict
- STALE → FACT: Re-validated with new evidence

**Guard:** Only FACTS and RULES with STRONG evidence can feed Policy Patches or BLOCK-level advisories.

#### 5D: Phase State Machine

Maintain current phase and phase history.

```rust
pub struct PhaseState {
    pub current: Phase,
    pub previous: Phase,
    pub entered_at: Instant,
    pub history: VecDeque<(Phase, Instant)>,  // last 20 transitions
    pub consecutive_failures: u32,
}
```

**Phase detection rules** (checked on each event):
1. Consecutive failures >= 2 → DIAGNOSE
2. Escape protocol triggered → ESCALATE
3. No activity for 5 min → HALT
4. Tool usage pattern dominant (> 60% of last 5 tools)
5. Keyword detection in task descriptions

**Consumers:** Advisory boost matrix, Control Plane watcher activation, distillation context tagging, Escape Protocol triggering.

#### 5E: Escape Protocol

Triggered when stuck: 3+ watcher firings, low confidence (< 0.3) + low evidence, or time budget > 80%.

**Steps:**
1. FREEZE: Set phase to ESCALATE
2. SUMMARIZE: Compile what has been tried and failed (from episode steps)
3. ISOLATE: Narrow problem scope to last failing step
4. FLIP: Generate alternative framing
5. HYPOTHESIZE: Generate 3 alternative hypotheses (LLM-assisted if budget allows, template if not)
6. DISCRIMINATE: Rank by testability
7. TEST: Execute minimal test of top hypothesis
8. ESCALATE: If still stuck, build structured escalation request

**Structured escalation output:**
```rust
pub struct EscalationRequest {
    pub escalation_type: EscalationType,  // Budget/Loop/Confidence/Blocked/Unknown
    pub request_type: RequestType,        // Info/Decision/Help/Review
    pub attempts: Vec<AttemptSummary>,
    pub evidence_gathered: Vec<String>,
    pub current_hypothesis: Option<String>,
    pub suggested_options: Vec<String>,
}
```

**Always produces a learning artifact** (sharp edge, anti-pattern, or heuristic) even when stuck.

#### 5F: Memory Gate

5-signal importance scoring before persistence.

```rust
pub fn score_importance(signals: &ImportanceSignals) -> f64 {
    let raw = signals.impact * 0.3
        + signals.novelty * 0.2
        + signals.surprise * 0.3
        + signals.recurrence * 0.2
        + signals.irreversibility * 0.4;
    raw.min(1.0)
}
```

**Threshold:** >= 0.5 → persist to learning.db. Below → session-only, discarded at session end.

**Signal computation:**
- Impact: Did this unblock progress? (from step envelope `progress_made`)
- Novelty: Word overlap with existing insights < 0.5
- Surprise: From episode/step `surprise_level`
- Recurrence: Seen 3+ times in recent episodes
- Irreversibility: Tool is in high-stakes list OR phase is Execute/Validate

#### 5G: Runtime Tuneables

All thresholds in a dedicated table, hot-reloadable.

```sql
CREATE TABLE tuneables (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    description TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**Populated with defaults on first run.** Read into a `HashMap<String, String>` cached in memory, refreshed on timer. Includes all scoring weights, authority thresholds, cooldown intervals, decay rates, promotion thresholds, emission budgets, quality floors.

**Overrides:** Config TOML values override tuneable defaults. Auto-tuner writes to this table.

## Acceptance Criteria

### Functional Requirements

- [x] Advisory scoring engine produces correct scores with base + modifiers
- [x] 5 advisory levels correctly assigned based on score thresholds
- [x] Agreement gating blocks WARNING without 2+ sources (except Policy distillations)
- [x] Phase detection identifies current phase from tool/task signals
- [x] Phase boost matrix applied correctly to advisory scores
- [x] Advisory packets stored with full lifecycle tracking
- [x] Packet effectiveness scores update from feedback
- [x] Cooldowns suppress repeat advice (per-tool, per-advice, per-category)
- [x] Obvious-from-context suppression works (`file(read)`→`file(write)`, phase-aware)
- [x] Quarantine audit trail logs all dropped advisories
- [x] Tier 1 programmatic synthesis produces action-first formatted advice
- [x] Tier 2 AI synthesis runs within advisory budget when available
- [x] Emission budget enforced (max 2 per event, 5-15% target rate)
- [x] Prefetch system generates packets during idle time
- [x] Auto-tuner runs on schedule, makes bounded adjustments, snapshots before changes
- [x] Bulletin section includes learning insights (advisory-gated)
- [x] 5 built-in domain chips loaded and triggered correctly
- [x] Chip 6-dimensional insight scoring produces correct scores and promotion tiers
- [x] Merger pipeline transforms chip insights for cognitive layer
- [x] Quality floors enforced in merger pipeline
- [x] Duplicate churn protection prevents excessive chip output
- [x] Chip schema validation rejects invalid definitions
- [x] Safety policy blocks harmful content patterns
- [x] Chip evolution tracks effectiveness and can deprecate underperformers
- [x] Provisional chip genesis suggests new chips from unmatched insights
- [x] Control Plane watchers fire correctly on detected conditions
- [x] Evidence Store captures and retains evidence by type
- [x] Evidence cleanup respects active episode references
- [x] Truth Ledger tracks claim lifecycle with correct transitions
- [x] Phase State Machine tracks transitions and history
- [x] Escape Protocol triggers on stuck detection and produces artifacts
- [x] Memory Gate scores importance and gates persistence
- [x] Runtime tuneables loaded, cached, and hot-reloadable
- [x] All systems fail-open with logging
- [x] `cargo clippy` and `cargo test` pass

## Technical Considerations

### Advisory gate hot path performance

The advisory gate is the closest to the hot path — it generates risk flags for pre-dispatch injection. Critical performance targets:
- Cached distillation lookup: < 1ms (in-memory DashMap)
- Packet scoring: < 5ms (pure computation)
- Tier 1 synthesis: < 1ms (template formatting)
- Total advisory budget: configurable, default 4000ms (for Tier 2 LLM calls)

### Chip loading and hot-reload

Chips are YAML definitions in skill directories. Follow the existing `SkillSet` hot-reload pattern:
- Watch skill directories for changes
- Reload chip definitions on change
- Running chip state is preserved across reloads (only definition changes)

### Phase name reconciliation (SpecFlow Gap Q9)

Use the Phase State Machine names (`Explore`, `Plan`, `Execute`, `Validate`, etc.) everywhere. The boost matrix is extended with rows for all 9 phases. `Testing` → `Validate`, `Debugging` → `Diagnose`.

### Cold start advisory behavior (SpecFlow Gap 5.3)

During cold start (< 50 episodes), the advisory scoring floor of 0.15 means nothing emits without context match. Mitigation: during cold start, boost all scores by +0.20 to ensure some advice surfaces. Linearly reduce boost from episode 25 to 50.

## Dependencies & Risks

- **Depends on Milestone 2** (Layer 1 outcome data feeds Layer 3 scoring; Layer 2 Meta-Ralph gates chip insights)
- **Bulletin integration** touches existing Cortex code (`gather_bulletin_sections`). Must not regress existing bulletin behavior.
- **SpacebotHook upgrade** for Control Plane enforcement (`on_tool_call` can Skip). CortexHook can remain observation-only; enforcement happens where tools are invoked.
- **LLM budget competition:** Tier 2 synthesis, escape protocol hypotheses, and distillation extraction all compete for LLM budget. Need clear priority order: distillation > escape > synthesis.

## References

### Internal References
- Brainstorm Layer 3: `docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md:379-499`
- Brainstorm Layer 4: `docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md:501-612`
- Brainstorm Cross-Cutting: `docs/brainstorms/2026-02-22-evolving-intelligence-system-brainstorm.md:615-730`
- Cortex bulletin generation: `src/agent/cortex.rs:249`
- CortexHook: `src/hooks/cortex.rs`
- Skills system: `src/skills.rs`

### New Files (this milestone)
```
src/learning/
  gate.rs             — Advisory scoring, 5 levels, agreement gating
  packets.rs          — Advisory packet store, effectiveness tracking
  cooldowns.rs        — Cooldown mechanisms, suppression rules
  quarantine.rs       — Quarantine audit trail
  synthesis.rs        — Tiered synthesis (programmatic + AI)
  prefetch.rs         — Advisory prefetch system
  tuner.rs            — Auto-tuner (closed-loop optimization)
  chips/
    runtime.rs        — Chip event routing, observation handling
    scoring.rs        — 6-dimensional insight scoring
    merger.rs         — Chip-to-cognitive merger pipeline
    policy.rs         — Safety policy, blocked patterns
    schema.rs         — Chip definition validation
    evolution.rs      — Effectiveness tracking, deprecation, genesis
  control.rs          — Control Plane watchers
  evidence.rs         — Evidence store with retention
  truth.rs            — Truth ledger (claim lifecycle)
  phase.rs            — Phase state machine
  escape.rs           — Escape protocol
  importance.rs       — Memory gate (5-signal scoring)
  tuneables.rs        — Runtime tuneable storage
```
