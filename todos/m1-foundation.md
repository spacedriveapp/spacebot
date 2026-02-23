# Milestone 1: Foundation & Infrastructure

## Phase 0A: ProcessEvent Enrichment
- [x] Add trace_id, call_id, args_summary, success, duration_secs to ProcessEvent variants
- [x] Add UserMessage and UserReaction variants
- [x] Update all match sites (channel.rs, status.rs, cortex.rs, state.rs, opencode/worker.rs, branch.rs, set_status.rs, hooks/spacebot.rs)
- [x] Emit MemorySaved from memory_save tool
- [x] Emit CompactionTriggered from compactor
- [x] Emit UserMessage from channel.handle_message
- [x] Add trace_id propagation into SpacebotHook and spawn_worker_task

## Phase 0B: Module Scaffolding
- [x] Create src/learning.rs + src/learning/{engine,store,types,config}.rs
- [x] Register pub mod learning in lib.rs
- [x] Add LearningError to error.rs

## Phase 0C: Learning Database
- [x] Create learning.db migration SQL (embedded)
- [x] Implement LearningStore with WAL mode + embedded migrations

## Phase 0D: LearningConfig + RuntimeConfig
- [x] Create LearningConfig struct with Default
- [x] Add learning field to RuntimeConfig + hot-reload
- [x] Add TOML parsing for [defaults.learning]
- [x] Wire into ResolvedAgentConfig

## Phase 0E: LearningEngine Async Loop
- [x] Implement spawn_learning_loop with event_rx + heartbeat
- [x] Implement handle_event that logs to learning_events
- [x] Wire into main.rs startup

## Phase 0F: Owner-Only Filtering
- [x] Implement should_process_event with trace-based filtering
- [x] Integrate into learning engine event loop

## Quality
- [x] cargo clippy passes (no new warnings)
- [x] cargo test passes (131 pass, 1 pre-existing failure)
- [x] cargo check passes
