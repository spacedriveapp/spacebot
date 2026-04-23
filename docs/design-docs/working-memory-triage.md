# Working Memory Triage — PR #454

Findings from CodeRabbit review + bug reports. Tracking resolution before merge.

## Review Findings

### Critical

- [x] **R1 — `matches!` moves non-Copy `ProcessEvent`** (`src/agent/cortex.rs:903`)
  `matches!(event, ...)` consumes `event`, preventing `signal_from_event(...)` on the next line. **Fixed:** `matches!(&event, ...)`.

### Major

- [x] **R2 — Bulletin fallback gate too aggressive** (`prompts/en/channel.md.j2:172`)
  Condition `not working_memory and not knowledge_synthesis` hides bulletin when working memory exists but knowledge synthesis hasn't run yet. **Fixed in PR #570:** fallback now depends on missing `knowledge_synthesis`, and prompt data preserves that original absence.

- [x] **R3 — Don't exclude participant-role facts yet** (`prompts/en/cortex_knowledge_synthesis.md.j2:21`)
  Exclusion of "The user is the CEO" drops participant context with nowhere else to live until Phase 6 ships. **Fixed in this slice:** knowledge synthesis now preserves concise participant/user role facts when they affect future routing, authority, relationships, or interpretation.

- [x] **R4 — Raw worker task in working memory** (`src/agent/channel_dispatch.rs:596`)
  `task` from user input persisted verbatim; could capture secrets/PII. **Fixed in this slice:** worker-spawn task text is now redacted and bounded via shared working-memory scrub helpers.

- [ ] **R5 — Dirty flag only bumps on merges** (`src/agent/cortex.rs:1958`)
  Prunes and decays also change the memory set but don't trigger knowledge synthesis re-gen. Add `report.pruned > 0 || report.decayed > 0`. **Partial in PR #570:** prunes and merges now dirty synthesis; decay remains intentionally importance-only and needs a follow-up decision.

- [x] **R6 — Dirty-flag synthesis not mutex-guarded** (`src/agent/cortex.rs:2106`)
  Can race with warmup synthesis path. **Fixed in this slice:** dirty-triggered synthesis now acquires the warmup/synthesis mutex and re-checks the dirty version after the lock is held.

- [x] **R7 — Intraday/daily synthesis blocks main cortex loop** (`src/agent/cortex.rs:2166`)
  LLM calls awaited inline inside `tokio::select!`; events stop draining during synthesis. **Fixed in PR #570:** intraday and daily synthesis now run as background tasks with single-flight scheduling and failure backoff.

- [x] **R8 — Empty sections treated as successful no-op** (`src/agent/cortex.rs:2558`)
  Returns before tasks can contribute to synthesis; dirty flag never clears, causing infinite rescheduling. **Fixed in PR #570:** true empty input clears the target version, while gather failures fail the synthesis path and keep it retryable.

- [ ] **R9 — Missing `default_max_turns(1)` + inline preambles** (`src/agent/cortex.rs:2579`)
  Three cortex agent builders lack explicit max_turns; two have inline preamble strings instead of prompt files. **Stacked in PR #571:** one-shot synthesis prompt hardening is kept out of PR #570 to keep the reliability diff focused.

- [x] **R10 — Version snapshot after async work** (`src/agent/cortex.rs:2614`)
  `knowledge_synthesis_last_version` read after LLM call; concurrent writes can advance the version past what was actually synthesized. **Fixed in PR #570:** synthesis snapshots the target version before async work and only marks that version complete.

- [x] **R11 — Unsynthesized yesterday events dropped** (`src/agent/cortex.rs:2916`)
  Raw events that didn't hit count/time trigger before midnight are lost from daily summary. Roll them into the summary. **Fixed:** daily summary now fetches all raw events, filters to the unsynthesized tail after the last intra-day synthesis, and includes them in the LLM input.

- [x] **R12 — Silent error swallowing in inspect_prompt** (`src/api/channels.rs:649`)
  `unwrap_or_default()` / `.ok()` hides DB/template errors. **Fixed in this slice:** inspect prompt now logs and returns internal errors when DB/template rendering fails.

- [x] **R13 — Raw error strings in working memory** (`src/cron/scheduler.rs:386`)
  Full error text persisted; could contain sensitive internals. **Fixed in this slice:** cron error events now persist scrubbed and bounded summaries (including encoded leak fail-closed redaction).

- [ ] **R14 — Timezone fallback drops valid `cron_timezone`** (`src/main.rs:2559`)
  If `user_timezone` is present but unparseable, `cron_timezone` is never tried. Parse each independently.

- [x] **R15 — UTF-8 panic on topic truncation** (`src/memory/working.rs:739`)
  Byte-index slice at 80 can split multibyte chars. **Fixed:** `floor_char_boundary(80)`.

- [x] **R16 — Task update event always says "status change"** (`src/tools/task_update.rs:246`)
  Every update emits `"updated to <status>"` even for title/description edits. **Fixed in this slice:** working-memory task updates now describe actual field deltas and preserve status-only wording when only status changed.

## Live Observations (from prompt inspect, March 19)

- [x] **O1 — March 18 daily summary missing** (confirmed R11)
  Yesterday had a full day of heavy working memory implementation work. None of it appears in "Earlier This Week" — only `2026-03-17: No activity`. This is real content loss. **Fixed with R11.**

## Bug Reports

<!-- Add bug reports below -->
