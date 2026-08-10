# Cortex History

An action-oriented log of everything the cortex does. The cortex currently has no persistence — the signal buffer, bulletin output, and maintenance results are all in-memory and lost on restart. This adds a durable audit trail surfaced on the agent-level Cortex tab.

## What Gets Logged

Only things the cortex **did**, not things it passively observed. Every action falls into one of these categories:

| Event Type | When | Data |
|------------|------|------|
| `bulletin_generated` | Bulletin successfully synthesized | word count, section counts, duration_ms, model used |
| `bulletin_failed` | Bulletin generation failed | error message, retry count |
| `maintenance_run` | Memory maintenance completed | decayed count, pruned count, merged count, duration_ms |
| `memory_merged` | Two memories consolidated | survivor_id, merged_id, reason |
| `memory_decayed` | Batch of memories decayed | count, threshold used |
| `memory_pruned` | Low-importance orphans removed | count, importance threshold |
| `association_created` | Cross-channel association made | source_id, target_id, relation_type, reason |
| `contradiction_flagged` | Contradicting memories found | memory_a_id, memory_b_id, description |
| `worker_killed` | Cortex killed a hanging worker | worker_id, channel_id (optional), timeout_secs, reason |
| `branch_killed` | Cortex killed a stale branch | branch_id, channel_id, timeout_secs, reason |
| `circuit_breaker_tripped` | Component hit failure threshold | key, failure_count, threshold, action_taken |
| `observation_created` | Cortex created an observation memory | memory_id, content_preview |
| `health_check` | Periodic health summary | kill_skipped_due_to_lag, kill_budget, kill_attempts, kill_actions, worker_timeout_secs, branch_timeout_secs, pruned_dead_channels |

These map to the phases in `cortex-implementation.md`: bulletin (exists today), maintenance (Phase 3), health supervision (Phase 2, implemented), consolidation (Phase 4). Health supervision currently emits `worker_killed`, `branch_killed`, `circuit_breaker_tripped`, and `health_check`; maintenance and consolidation emit their own events as those phases land.

## Data Model

Single table per agent:

```sql
CREATE TABLE cortex_events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    summary TEXT NOT NULL,
    details TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_cortex_events_type ON cortex_events(event_type, created_at);
CREATE INDEX idx_cortex_events_created ON cortex_events(created_at);
```

`summary` is a human-readable one-liner (e.g. "Bulletin generated: 487 words, 8 sections, 2.3s"). `details` is a JSON blob with event-specific structured data for filtering and display.

Intentionally flat — no foreign keys to memories or channels. Memory IDs and channel IDs are stored as data in the JSON details, not as FK constraints, since referenced entities may be decayed/pruned/deleted over time.

## Rust Implementation

### `CortexLogger`

New struct in `src/agent/cortex.rs`. Follows the same fire-and-forget pattern as `ProcessRunLogger` and `ConversationLogger`:

```rust
pub struct CortexLogger {
    pool: SqlitePool,
}

impl CortexLogger {
    pub fn log(&self, event_type: &str, summary: &str, details: Option<serde_json::Value>) { ... }
}
```

One method. The caller constructs the summary and details.

### Wiring into existing cortex code

**Bulletin generation** (`spawn_bulletin_loop`): Log `bulletin_generated` on success with word count, section counts, duration. Log `bulletin_failed` on error with the error message.

**Maintenance** (Phase 3, future): The `MaintenanceReport` from `run_maintenance()` already contains decayed/pruned/merged counts — log a single `maintenance_run` event.

**Health supervision** (Phase 2, implemented): Log `worker_killed`, `branch_killed`, `circuit_breaker_tripped`, and `health_check` from the supervision tick.

Phase 2 breaker state is intentionally volatile (in-memory only). After restart, counters/tripped flags reset and begin accumulating again from live events.

**Consolidation** (Phase 4, future): Log `memory_merged`, `association_created`, `contradiction_flagged`, `observation_created` as the consolidation agent acts.

## API Endpoint

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/cortex/events?agent_id=...&limit=50&offset=0&event_type=...` | Paginated cortex event history |

Response:

```json
{
  "events": [
    {
      "id": "...",
      "event_type": "bulletin_generated",
      "summary": "Bulletin generated: 487 words, 8 sections, 2.3s",
      "details": { "word_count": 487, "sections": 8, "duration_ms": 2300 },
      "created_at": "2026-02-13T10:30:00Z"
    }
  ],
  "total": 142
}
```

Optional `event_type` query param filters to a specific category.

## Frontend — Cortex Tab

Replaces the cortex tab placeholder (`/agents/$agentId/cortex`). Two sections:

### Event Timeline (primary content)

Chronological list of cortex events, newest first. Each event row:
- Timestamp
- Event type badge (color-coded: blue for bulletin, green for maintenance, amber for health, violet for consolidation)
- Summary text
- Expandable details (click to show formatted JSON)

Filter bar at top: pill buttons for event type categories (same pattern as memory type filters). Pagination at bottom.

### Cortex Chat Panel (right side, optional)

The cortex chat panel from `cortex-chat.md` is accessible here via a toggle button. When opened from the cortex tab, `channelId` is null — no channel context injected. Gives admins a way to talk to the cortex from its dedicated page.

## File Changes

**New files:**
- `interface/src/routes/AgentCortex.tsx` — cortex tab page (event timeline + optional chat panel)
- Migration file for `cortex_events` table

**Modified files:**
- `src/agent/cortex.rs` — add `CortexLogger`, wire into `spawn_bulletin_loop`
- `src/api/server.rs` — add `/api/cortex/events` endpoint
- `src/db/migrations.rs` — register new migration
- `interface/src/api/client.ts` — new types + API method
- `interface/src/router.tsx` — replace cortex placeholder with `AgentCortex` component

## Relationship to Cortex Chat

The cortex tab becomes the cortex's home page — history (what it did) plus chat (talk to it). The cortex chat panel is the same component used on channel pages, just without channel context. The event timeline is unique to this tab.

These two features are independent on the backend but share UI space. Cortex history is simpler (no streaming, no new process type) and delivers value immediately because cortex synthesis and maintenance loops already run.
