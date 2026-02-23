# Milestone 3: Intelligence (Layers 3 & 4 + Cross-Cutting)

## Phase 0: Foundation
- [x] Fix pre-existing clippy error in engine.rs
- [x] Add SCHEMA_V3 to learning store
- [x] Add shared types (Phase, AuthorityLevel, WatcherSeverity, etc.)
- [x] Update learning.rs module declarations

## Phase 5: Cross-Cutting Systems (build first — dependencies for 3 & 4)
- [x] 5G: Runtime tuneables (tuneables.rs)
- [x] 5D: Phase state machine (phase.rs)
- [x] 5F: Memory gate / importance scoring (importance.rs)
- [x] 5B: Evidence store (evidence.rs)
- [x] 5C: Truth ledger (truth.rs)
- [x] 5A: Control Plane watchers (control.rs)
- [x] 5E: Escape protocol (escape.rs)

## Phase 3: Advisory Gating
- [x] 3A+3B+3C: Advisory scoring engine + authority levels + phase boost (gate.rs)
- [x] 3D: Advisory packet store (packets.rs)
- [x] 3E: Cooldowns & suppression (cooldowns.rs)
- [x] 3F: Quarantine audit trail (quarantine.rs)
- [x] 3G+3H: Tiered synthesis + emission budget (synthesis.rs)
- [x] 3I: Prefetch system (prefetch.rs)
- [x] 3J: Auto-tuner (tuner.rs)
- [x] 3K: Insight demotion (in gate.rs)
- [x] 3L: Bulletin integration (cortex.rs changes)

## Phase 4: Domain Chips
- [x] 4A: Chip runtime (chips/runtime.rs)
- [x] 4B: 6-dimensional insight scoring (chips/scoring.rs)
- [x] 4C: Chip-to-cognitive merger pipeline (chips/merger.rs)
- [x] 4D: Built-in domain chip YAML definitions (chips/*.yaml)
- [x] 4E: Chip schema validation (chips/schema.rs)
- [x] 4F: Safety policy (chips/policy.rs)
- [x] 4G: Chip evolution (chips/evolution.rs)

## Integration & Wiring
- [x] Wire all new modules into learning engine event loop
- [x] Bulletin integration for learning insights
- [x] SpacebotHook integration for Control Plane enforcement
- [x] Engine startup: load tuneables, chips, init all subsystems

## Quality
- [x] cargo clippy passes (no new errors)
- [x] cargo test passes (430 learning tests, 561 total)
- [x] Incremental commits throughout
