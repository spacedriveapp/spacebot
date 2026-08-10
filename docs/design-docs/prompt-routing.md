# Prompt-Level Routing

Design for adding prompt complexity analysis to Spacebot's existing model routing system.

## Context

Spacebot already routes by process type and task type (see [routing.md](../routing.md)). A channel gets sonnet, a compaction worker gets haiku, a coding worker gets sonnet. This covers the structural axis — we know what kind of process is running and pick a model tier accordingly.

What we don't do is route by prompt complexity *within* a process type. A channel handling "what's 2+2" and "design a distributed consensus algorithm" both get sonnet. The simple query wastes money. The complex query is fine.

ClawRouter (TypeScript, OpenClaw ecosystem) solves this with a 15-dimension weighted keyword scorer that classifies prompts into four tiers and picks the cheapest model per tier. The scoring runs in <1ms with no external calls. It's crude (keyword matching, not semantic understanding) but effective for the common cases and the cost savings are real.

This doc proposes integrating prompt-level complexity scoring natively into Spacebot's existing routing system.

## What ClawRouter Does

Source: [github.com/BlockRunAI/ClawRouter](https://github.com/BlockRunAI/ClawRouter)

### Core Loop

```
prompt → 15 weighted dimensions scored → weighted sum → tier boundary mapping → model selection
```

### Four Tiers

| Tier | Purpose | Typical Models |
|------|---------|----------------|
| SIMPLE | Factual Q&A, definitions, translations | Cheapest available |
| MEDIUM | Summaries, explanations, moderate code | Mid-tier |
| COMPLEX | Multi-step code, system design, analysis | Strong general |
| REASONING | Proofs, formal logic, step-by-step | Reasoning-optimized |

### Scoring Dimensions (15)

Each dimension scores [-1, 1] via keyword/pattern matching:

1. **Token count** — short prompts score low, long score high
2. **Code presence** — function, class, import, async, etc.
3. **Reasoning markers** — prove, theorem, step by step, chain of thought
4. **Technical terms** — algorithm, distributed, kubernetes, architecture
5. **Creative markers** — story, poem, brainstorm, imagine
6. **Simple indicators** — what is, define, translate, hello (negative score)
7. **Multi-step patterns** — "first...then", "step 1", numbered lists
8. **Question complexity** — count of question marks
9. **Imperative verbs** — build, create, implement, deploy
10. **Constraint indicators** — at most, within, maximum, O(n)
11. **Output format** — json, yaml, table, schema
12. **Reference complexity** — "the code above", "the api", "attached"
13. **Negation complexity** — don't, avoid, never, except
14. **Domain specificity** — quantum, fpga, genomics, zero-knowledge
15. **Agentic task** — read file, execute, deploy, debug, iterate

Weights are configurable. Default weights sum to ~1.0. Score is mapped to tiers via configurable boundaries with sigmoid confidence calibration.

### What's Good

- **Fast** — keyword matching in <1ms, no external calls for 70-80% of requests
- **Configurable** — weights, boundaries, keyword lists, tier-to-model mappings all configurable
- **Routing profiles** — eco/auto/premium presets swap entire tier configs
- **Context-aware** — filters out models that can't handle the prompt's context window

### What's Not Good

- **Keyword-only** — no semantic understanding. "Write a poem about algorithms" scores both creative and technical, producing a muddled tier.
- **Inconsistent prompt scoping** — reasoning markers are scored against user prompt only, but all other dimensions score against system + user prompt concatenated. System prompts are keyword-rich by nature, which biases non-reasoning dimensions.
- **No conversation awareness** — scores each prompt in isolation. A follow-up "yes, do that" scores as SIMPLE even if the conversation is about distributed systems.
- **Multilingual bloat** — keyword lists in 5 languages. Useful for a general-purpose router, overhead for Spacebot's use case.
- **LLM fallback classifier** — for ambiguous cases, sends a classification request to a cheap model. Adds 200-400ms latency for marginal accuracy improvement.

## Design for Spacebot

### Where It Fits

Spacebot's routing has three levels: process-type defaults, task-type overrides, and fallback chains. Prompt-level routing adds a fourth level between process-type defaults and task-type overrides:

```
1. Task-type override (explicit, highest priority)
2. Prompt complexity tier (inferred from prompt content)    ← NEW
3. Process-type default (structural)
4. Fallback chain (on provider failure)
```

Task-type overrides still win — if a branch spawns a worker with `task_type: "coding"`, that's explicit and should be respected regardless of what the prompt says.

Prompt-level routing applies when there's no explicit task type. This primarily affects **channels** and **branches** — the processes where prompt content varies most.

Workers already get a focused task and a task type. Compactors and cortex are fixed-purpose. Prompt routing would add overhead with no benefit for those process types.

### Scoring: Simplified

ClawRouter's 15 dimensions are overkill for Spacebot. We know more about the context than a generic router does:

- We know the process type (channel vs branch vs worker)
- We have conversation history (not just the current prompt)
- We have knowledge synthesis and working-memory layers (what the agent knows and what recently happened)
- System prompts are excluded from scoring (they're ours, not the user's)

A simplified scorer for Spacebot:

**Score only the user message.** System prompts, memory context layers, and compaction summaries are excluded. ClawRouter partially does this (reasoning markers check user prompt only) but inconsistently applies it -- most dimensions still score against the concatenated system + user text. We score user message only across all dimensions.

**6-8 dimensions instead of 15:**

| Dimension | Signal | Weight |
|-----------|--------|--------|
| Token count | Short vs long | 0.10 |
| Code presence | Code keywords, backticks | 0.20 |
| Reasoning markers | Prove, theorem, step by step | 0.20 |
| Simple indicators | What is, define, hello (negative) | 0.15 |
| Technical depth | Algorithm, architecture, distributed | 0.15 |
| Multi-step | First...then, step N, numbered lists | 0.10 |
| Constraint complexity | At most, O(n), maximum | 0.10 |

Drop creative markers (Spacebot is task-oriented), domain specificity (too niche), imperative verbs (too noisy in an agent context where everything is imperative), reference complexity (the agent always has references), negation (too common in instructions), and agentic task detection (redundant — Spacebot already knows when it's in agentic mode because it spawns workers explicitly).

**Three tiers, not four:**

| Tier | Spacebot Mapping |
|------|-----------------|
| LIGHT | Cheapest capable model (haiku-class) |
| STANDARD | Default process-type model (sonnet-class) |
| HEAVY | Strongest available model (opus-class) |

ClawRouter separates COMPLEX and REASONING because different models handle them differently. That's valid for a generic router, but Spacebot's task-type system already handles this — a `deep_reasoning` task type routes to an appropriate model. Three tiers keeps prompt routing focused on cost optimization.

### Configuration

```toml
[defaults.routing]
channel = "anthropic/claude-sonnet-4"
worker = "anthropic/claude-haiku-4.5"

# Prompt-level routing (optional, disabled by default)
[defaults.routing.prompt_routing]
enabled = false
process_types = ["channel", "branch"]  # only apply to these

[defaults.routing.prompt_routing.tiers]
light = "anthropic/claude-haiku-4.5"
standard = "anthropic/claude-sonnet-4"   # same as process default
heavy = "anthropic/claude-opus-4"

[defaults.routing.prompt_routing.boundaries]
light_standard = 0.0     # below this → LIGHT
standard_heavy = 0.4     # above this → HEAVY
```

When `enabled = false` (default), routing works exactly as it does today. Process-type defaults, task-type overrides, fallback chains. No change.

When enabled, the scorer runs on the user message before model resolution. If the score maps to a different tier than the process default, the model is swapped. Task-type overrides still take priority.

### Implementation

Prompt routing lives in the existing `routing.rs` module. No new module needed.

```rust
pub struct PromptRouter {
    config: PromptRoutingConfig,
}

pub struct PromptRoutingConfig {
    pub enabled: bool,
    pub process_types: Vec<ProcessType>,
    pub tiers: PromptTiers,
    pub boundaries: TierBoundaries,
    pub weights: DimensionWeights,
}

pub struct PromptTiers {
    pub light: String,
    pub standard: String,
    pub heavy: String,
}

#[derive(Clone, Copy)]
pub enum PromptTier {
    Light,
    Standard,
    Heavy,
}

impl PromptRouter {
    /// Score a user message and return a tier.
    /// Returns None if prompt routing is disabled or doesn't apply to this process type.
    pub fn classify(&self, user_message: &str, process_type: ProcessType) -> Option<PromptTier> {
        if !self.config.enabled {
            return None;
        }
        if !self.config.process_types.contains(&process_type) {
            return None;
        }

        let score = self.score_dimensions(user_message);
        Some(self.map_to_tier(score))
    }

    fn score_dimensions(&self, text: &str) -> f64 {
        let lower = text.to_lowercase();
        let weights = &self.config.weights;

        let mut score = 0.0;
        score += self.score_token_count(text) * weights.token_count;
        score += self.score_keywords(&lower, &CODE_KEYWORDS) * weights.code_presence;
        score += self.score_keywords(&lower, &REASONING_KEYWORDS) * weights.reasoning;
        score += self.score_keywords(&lower, &SIMPLE_KEYWORDS) * weights.simple; // negative
        score += self.score_keywords(&lower, &TECHNICAL_KEYWORDS) * weights.technical;
        score += self.score_multi_step(&lower) * weights.multi_step;
        score += self.score_keywords(&lower, &CONSTRAINT_KEYWORDS) * weights.constraints;
        score
    }
}
```

Integration into `RoutingConfig::resolve()`:

```rust
impl RoutingConfig {
    pub fn resolve(
        &self,
        process_type: ProcessType,
        task_type: Option<&str>,
        user_message: Option<&str>,
    ) -> &str {
        // 1. Task-type override (explicit, highest priority)
        if let Some(task) = task_type {
            if matches!(process_type, ProcessType::Worker | ProcessType::Branch) {
                if let Some(model) = self.task_overrides.get(task) {
                    return model;
                }
            }
        }

        // 2. Prompt complexity tier (inferred)
        if let Some(message) = user_message {
            if let Some(tier) = self.prompt_router.classify(message, process_type) {
                return self.prompt_routing.tiers.model_for(tier);
            }
        }

        // 3. Process-type default
        match process_type {
            ProcessType::Channel => &self.channel,
            ProcessType::Branch => &self.branch,
            ProcessType::Worker => &self.worker,
            ProcessType::Compactor => &self.compactor,
            ProcessType::Cortex => &self.cortex,
        }
    }
}
```

The change to `resolve()` is additive. An `Option<&str>` parameter for the user message, defaulting to `None`. All existing call sites pass `None` and get exactly the same behavior. Only channel and branch loops pass the actual message.

### Routing Profiles

ClawRouter's routing profiles (eco/auto/premium) are a good idea. Each profile swaps the entire tier-to-model mapping. In Spacebot, profiles could be per-agent:

```toml
[agents.budget-bot]
routing_profile = "eco"

[agents.premium-bot]
routing_profile = "premium"
```

| Profile | LIGHT | STANDARD | HEAVY |
|---------|-------|----------|-------|
| eco | cheapest free model | haiku-class | sonnet-class |
| balanced | haiku-class | sonnet-class | opus-class |
| premium | sonnet-class | opus-class | opus-class |

This is orthogonal to prompt routing — profiles change what models the tiers map to, prompt routing changes which tier a message gets. They compose.

### Cost Tracking

ClawRouter tracks cost savings per request and exposes a `/stats` command. Spacebot should track this too:

- Per-request: model used, tier selected, estimated cost, baseline cost (what the process default would have cost)
- Per-agent: aggregate savings over time
- Exposed via the status block or a stats command

This is a reporting concern — `SpacebotHook` already tracks model usage per request. Adding cost estimates from the model's pricing metadata is straightforward.

### What We Skip

- **LLM classifier fallback** — ambiguous prompts get the process-type default (STANDARD tier). No secondary LLM call. The cost of a wrong classification (sending a medium prompt to sonnet instead of haiku) is much lower than the latency of a classifier call.
- **Multilingual keyword lists** — English only for now. Configurable keyword lists mean operators can add their own.
- **Agentic detection** — Spacebot knows when it's agentic because workers are explicitly spawned. No need to infer this from keywords.
- **Session pinning** — processes already have a fixed model for their lifetime. No need for session-level model persistence.
- **x402 payment integration** — Spacebot uses standard API keys.

## Phasing

### Phase 1: Keyword Scorer

Implement `PromptRouter` in `routing.rs`. Keyword lists, dimension weights, tier boundaries. All configurable. Disabled by default. No changes to existing routing behavior unless explicitly opted in.

### Phase 2: Channel Integration

Wire prompt routing into the channel loop. Pass the user message to `resolve()`. Log tier decisions for analysis. Track cost estimates.

### Phase 3: Routing Profiles

Implement eco/balanced/premium profile presets. Per-agent profile selection.

### Phase 4: Cost Dashboard

Aggregate cost tracking and savings reporting. Expose via status block, CLI stats command, or cron-triggered reports.

## Open Questions

- **Should branches inherit the channel's tier?** A branch forks from the channel's context. If the channel downgraded to haiku for a simple message, should the branch also use haiku? Probably not — branches do memory recall and curation, which benefits from a stronger model regardless of the triggering message's complexity.
- **Compaction workers** — these are already on the cheapest tier. Prompt routing wouldn't help. But if we ever run compaction on a stronger model by default, prompt routing could downgrade simple compaction tasks.
- **Model quality feedback** — ClawRouter has no feedback loop. If haiku gives a bad answer, it doesn't learn. Should Spacebot track response quality (user reactions, follow-up corrections) and adjust tier boundaries? This is a much harder problem and probably not worth solving early.
