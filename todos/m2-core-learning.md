# Milestone 2: Core Learning (Layers 1 & 2)

## Phase 0: Schema & Config Extensions
- [x] Add SCHEMA_V2 constant with all new tables
- [x] Run V2 migration in LearningStore::run_migrations
- [x] Add high_stakes_tools and high_stakes_file_operations to LearningConfig
- [x] Add TOML parsing for new config fields

## Phase 1A: Episode Lifecycle (outcome.rs)
- [x] Create src/learning/outcome.rs with ActiveEpisode + EpisodeTracker
- [x] Create episode on WorkerStarted/BranchStarted
- [x] Complete episode on WorkerComplete/BranchResult
- [x] Stale episode cleanup timer
- [x] Wire into engine.rs handle_event

## Phase 1B: Step Envelopes (outcome.rs)
- [x] Create step on ToolStarted (keyed by call_id)
- [x] Complete step on ToolCompleted (match by call_id)
- [x] Process ID to episode mapping

## Phase 1C: Outcome Predictor (outcome.rs)
- [x] outcome_predictions table CRUD
- [x] Prediction logic with Beta prior
- [x] Fallback chain (exact → coarser → prior 0.75)
- [x] Update on episode completion
- [x] LRU eviction at 2000 keys

## Phase 1D: Distillation Engine (distillation.rs)
- [x] Create src/learning/distillation.rs with Distillation type
- [x] 5 distillation types with priority + confidence caps
- [x] Batch extraction from completed episodes
- [x] Quality gate (reject short/tautology/generic)
- [x] Duplicate check (word overlap > 0.5)
- [x] Type classification heuristic
- [x] Confidence evolution (validation/contradiction)
- [x] Pruning of low performers

## Phase 1E: Structural Retrieval (retriever.rs)
- [x] Create src/learning/retriever.rs
- [x] retrieve_relevant() by type priority
- [x] Trigger matching, domain matching, keyword overlap
- [x] Anti-trigger check
- [x] Cache with timer-based refresh

## Phase 1F: Implicit Outcome Tracking (feedback.rs)
- [x] Create src/learning/feedback.rs
- [x] Record advice before tool call
- [x] Match outcome after tool call (5-min TTL)
- [x] Purge expired entries

## Phase 1G: Policy Patches (patches.rs)
- [x] Create src/learning/patches.rs
- [x] PolicyPatch + TriggerType + ActionType enums
- [x] Default patches on first run (Two Failures, File Thrash)
- [x] Evaluate patches against events in engine
- [x] Error counter for ERROR_COUNT triggers

## Phase 2A: Cognitive Signal Extraction (signals.rs)
- [x] Create src/learning/signals.rs
- [x] 10 domain detection categories
- [x] 5 cognitive pattern regex detectors (LazyLock)
- [x] Extract learning-worthy sentence
- [x] Create insight candidate → send to Meta-Ralph

## Phase 2B: Meta-Learning Insight Store (meta.rs)
- [x] Create src/learning/meta.rs with Insight type
- [x] 8 categories with decay half-lives
- [x] Reliability scoring (Bayesian prior)
- [x] Confidence boost per validation
- [x] Advisory readiness formula
- [x] Cold start factor interpolation

## Phase 2C: Meta-Ralph Quality Gate (ralph.rs)
- [x] Create src/learning/ralph.rs
- [x] Primitive filter (no LLM)
- [x] Duplicate detection via semantic hashing (md5)
- [x] 6-dimension scoring (programmatic)
- [x] Verdict assignment (QUALITY/NEEDS_WORK/PRIMITIVE/DUPLICATE)
- [x] Auto-refinement for NEEDS_WORK (one attempt)

## Phase 2D: Contradiction Detection (contradiction.rs)
- [x] Create src/learning/contradiction.rs
- [x] 12 opposition pairs
- [x] Negation asymmetry detection
- [x] 4 contradiction types (TEMPORAL/CONTEXTUAL/DIRECT/UNCERTAIN)
- [x] Resolution logic

## Phase 2E: Auto-Promotion (meta.rs + engine.rs)
- [x] Promotion criteria check with cold-start interpolation
- [x] Type mapping (insight → MemoryType)
- [x] Promotion flow (create Memory, save, embed)
- [x] Demotion flow (reliability < 0.5 → soft-delete)
- [x] Rate-limited check on promotion timer

## Phase 3: Engine Integration
- [x] Wire all modules into engine.rs event handler
- [x] Batch timer for distillation extraction
- [x] Stale episode cleanup timer
- [x] Promotion check timer
- [x] Layer ordering (L1 before L2)
- [x] Policy patch evaluation on worker events

## Phase 4: Quality
- [x] cargo check passes
- [x] cargo clippy no new warnings
- [x] cargo test passes (131 pass, 1 pre-existing failure)
- [x] Module registrations in learning.rs
