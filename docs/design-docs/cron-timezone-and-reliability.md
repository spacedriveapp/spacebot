# Cron Timezone and Reliability Plan

Fix cron behavior so users can trust schedule execution, deletion, and status reporting.

## Problem Statement

Users are reporting that cron behavior feels inconsistent:

- Deleting a cron sometimes still results in future executions.
- Daily jobs expected in the morning do not fire.
- The assistant can claim a deleted cron still exists.
- Timezone semantics are unclear and not user-visible.

Current behavior has multiple state surfaces that can drift from each other (in-memory scheduler, DB rows, memory bulletin), and active-hour evaluation is tied to server local time without explicit configuration.

## Goals

1. Make cron timezone explicit and configurable.
2. Support global env override for cron timezone.
3. Support per-agent timezone overrides.
4. Keep default behavior safe: server timezone when no explicit setting exists.
5. Remove ghost executions after deletion.
6. Make timezone semantics visible in tool/API responses and docs.

## Non-Goals

1. Full cron-expression scheduling in this change.
2. Auto-detecting user timezone from messaging adapters.
3. Converting interval scheduling to wall-clock calendar scheduling.
4. Retrofitting memory reconciliation for every historical cron operation.

## Existing Issues in Code

1. Tool deletion does not stop timers:
   - `src/tools/cron.rs` delete path removes DB row only.
   - API delete path does call `scheduler.unregister` in `src/api/cron.rs`.
2. Active hours use server local timezone directly:
   - `src/cron/scheduler.rs` uses `chrono::Local` for hour checks.
3. Schedule expectations mismatch:
   - Jobs are interval-based; active hours are a gating window on ticks.
4. Source-of-truth ambiguity in conversational responses:
   - Memory context can mention stale cron state unless the model calls live list tools.

## Configuration Design

### New Config Fields

- `defaults.cron_timezone` (optional string)
- `agents[].cron_timezone` (optional string override)

### New Environment Variable

- `SPACEBOT_CRON_TIMEZONE` (optional string)

### Accepted Values

- IANA timezone names (examples: `UTC`, `America/Los_Angeles`, `Europe/Berlin`).

### Resolution Order

Per agent:

1. `agents[].cron_timezone`
2. `defaults.cron_timezone`
3. `SPACEBOT_CRON_TIMEZONE`
4. server local timezone

If the configured timezone is invalid:

- log a warning with agent id and invalid value
- fall back to server local timezone

## Runtime Design

### Resolved Config

Add a resolved `cron_timezone` field to `ResolvedAgentConfig` and carry it into `RuntimeConfig` so scheduler logic can read the live value.

### Scheduler Behavior

Replace active-hour evaluation from hardcoded local time to resolved cron timezone. Keep current interval cadence and active-hour gating semantics unchanged.

### Deletion Semantics

Unify delete behavior across entry points:

- Tool delete should call `scheduler.unregister(id)` before/alongside DB deletion.
- API delete remains the same.

Outcome: no timer should continue firing after successful delete regardless of whether delete originated via tool call or API endpoint.

## API and Tool UX Changes

### Tool (`cron`) responses

- Include timezone context in create/list messages.
- Example: "Active hours evaluated in America/Los_Angeles."

### API responses

- Add resolved timezone to cron list payload.
- Optionally include timezone source (`agent`, `defaults`, `env`, `system`) for debugging.

## Documentation Changes

Update existing docs in the same change:

1. `docs/content/docs/(configuration)/config.mdx`
   - Add `cron_timezone` in defaults and per-agent sections.
   - Document `SPACEBOT_CRON_TIMEZONE`.
   - Document precedence and fallback behavior.
2. `docs/content/docs/(features)/cron.mdx`
   - Clarify active-hour timezone resolution.
   - Clarify interval-based semantics vs wall-clock expectations.
3. `README.md`
   - Add concise timezone example for cron config.

## Testing Plan

Add focused tests for cron reliability and timezone behavior:

1. Resolution precedence: agent > defaults > env > system.
2. Invalid timezone fallback to system local.
3. Active-hour pass/fail in named timezones.
4. Midnight-wrap windows in named timezones.
5. Tool delete unregister behavior (no post-delete execution).

## Implementation Phases

### Phase 1: Config and resolution plumbing

1. Extend TOML structs for defaults and agent timezone fields.
2. Extend runtime/resolved config structs.
3. Implement precedence and validation logic.
4. Wire into hot reload so runtime timezone updates are applied.

### Phase 2: Scheduler timezone integration

1. Replace `chrono::Local` active-hour checks with resolved timezone.
2. Keep current interval/tick behavior unchanged.
3. Add logs that include timezone context for cron evaluation decisions.

### Phase 3: Delete path reliability

1. Update tool delete path to unregister scheduler timer.
2. Keep API delete path aligned.
3. Ensure idempotent behavior when timer handle does not exist.

### Phase 4: API/tool transparency

1. Include timezone in list/create responses.
2. Return timezone in API cron list payloads.

### Phase 5: Docs and tests

1. Update config and cron feature docs.
2. Add regression tests for timezone and deletion behavior.
3. Validate expected behavior manually with one daily cron and one active-window cron.

## Acceptance Criteria

1. Deleting a cron via tool or API prevents future firings.
2. Active hours are evaluated in resolved agent timezone.
3. Env and config precedence is deterministic and documented.
4. Cron list/create outputs clearly state timezone used.
5. Docs reflect actual runtime behavior.

## Follow-Up Work

1. Add wall-clock scheduling primitives (`daily_at`, cron expressions).
2. Force live cron-list tool calls when users ask "what crons are active".
3. Add optional memory sync actions for cron create/delete/toggle lifecycle.
