# Token Usage Tracking

Track token consumption and estimated cost per API call, accumulated across conversations, agents, and the full instance. The dashboard TokenUsageCard currently shows dummy data. This makes it real.

---

## What Gets Tracked

Seven fields per API call:

| Field | Description |
|-------|-------------|
| `input_tokens` | Non-cached prompt tokens |
| `output_tokens` | Completion/generation tokens |
| `cache_read_tokens` | Tokens served from prompt cache |
| `cache_write_tokens` | Tokens written to prompt cache |
| `reasoning_tokens` | Thinking tokens (extended thinking, o3-family) |
| `request_count` | Number of API calls (always 1 per call, summed over time) |
| `estimated_cost_usd` | Cost in USD, null if pricing is unknown |

Derived values (computed on read, not stored):
- `prompt_tokens` = `input_tokens + cache_read_tokens + cache_write_tokens`
- `total_tokens` = `prompt_tokens + output_tokens`

---

## Schema

### New table: `token_usage`

One row per agent process invocation (channel turn, branch, worker, cortex run, cron job).

```sql
CREATE TABLE token_usage (
    id                  TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    agent_id            TEXT NOT NULL,
    process_type        TEXT NOT NULL,     -- 'channel' | 'branch' | 'worker' | 'cortex' | 'cron'
    conversation_id     TEXT,              -- null for workers/branches not tied to a conversation
    model               TEXT NOT NULL,
    provider            TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens    INTEGER NOT NULL DEFAULT 0,
    request_count       INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd  REAL,              -- null if provider pricing is unknown
    cost_status         TEXT NOT NULL DEFAULT 'unknown',  -- see CostStatus below
    recorded_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX token_usage_agent    ON token_usage(agent_id, recorded_at DESC);
CREATE INDEX token_usage_conv     ON token_usage(conversation_id) WHERE conversation_id IS NOT NULL;
CREATE INDEX token_usage_recorded ON token_usage(recorded_at DESC);
```

One row is written at the end of each process invocation (not per API call). The process accumulates in memory and flushes to the DB once on completion. Workers that crash without completing do not write a row — acceptable loss.

### `cost_status` values

```rust
pub enum CostStatus {
    Estimated,   // computed from our pricing table, may be slightly off
    Included,    // subscription-included route (GitHub Copilot, etc.) — $0
    Unknown,     // provider or model not in pricing table
}
```

---

## Pricing

### Data structure

```rust
pub struct PricingEntry {
    pub input_per_million: Option<Decimal>,
    pub output_per_million: Option<Decimal>,
    pub cache_read_per_million: Option<Decimal>,
    pub cache_write_per_million: Option<Decimal>,
}
```

### Lookup order

1. **Hardcoded table** — covers all providers we ship with. Snapshotted from official pricing pages, versioned with a date string (e.g. `"anthropic-2026-04-07"`). Updated manually when pricing changes.
2. **OpenRouter `/models` API** — fetched on startup, cached for 1 hour. Covers any model routed through OpenRouter.
3. **Custom endpoint `/models`** — fetched on startup for configured custom endpoints. Cached per base URL.
4. **Subscription-included** — certain routes (e.g. GitHub Copilot) are always $0, status `Included`.
5. **Unknown** — cost is `null`, status `Unknown`.

### Cost formula

```
cost = (input_tokens × input_per_million / 1_000_000)
     + (output_tokens × output_per_million / 1_000_000)
     + (cache_read_tokens × cache_read_per_million / 1_000_000)
     + (cache_write_tokens × cache_write_per_million / 1_000_000)
```

Arithmetic uses `Decimal` throughout to avoid floating-point drift. Stored as `REAL` in SQLite (precision is fine at these magnitudes).

### Normalization

API providers report cache tokens differently. Normalize before computing:

**Anthropic** — straightforward:
```
input_tokens       = usage.input_tokens
cache_read_tokens  = usage.cache_read_input_tokens
cache_write_tokens = usage.cache_creation_input_tokens
```

**OpenAI-compatible** — cached tokens are bundled inside `prompt_tokens`, must subtract:
```
cache_read_tokens  = usage.prompt_tokens_details.cached_tokens
cache_write_tokens = usage.prompt_tokens_details.cache_write_tokens
input_tokens       = usage.prompt_tokens - cache_read_tokens - cache_write_tokens
```

**Reasoning tokens** (extended thinking, o3-family):
```
reasoning_tokens = usage.output_tokens_details.reasoning_tokens
```

All fields default to 0 if absent. `max(0, ...)` guards against negative values from provider bugs.

---

## Accumulation

Each process type accumulates token counts in memory and writes once on completion.

```rust
pub struct UsageAccumulator {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub request_count: u32,
    pub estimated_cost_usd: Option<Decimal>,
    pub cost_status: CostStatus,
}
```

After each streaming response completes, call `accumulator.add(canonical_usage)`. On process exit (normal or error), flush to `token_usage`.

Cost status is the most conservative status seen across all calls in the accumulation window: `Unknown > Estimated > Included`.

---

## API

### `GET /api/usage`

Returns aggregated token usage for the instance.

Query params:
- `agent_id` — filter to one agent (optional)
- `since` — ISO 8601 datetime lower bound (optional, default: 30 days ago)
- `until` — ISO 8601 datetime upper bound (optional)
- `group_by` — `day | agent | model` (optional, default: no grouping)

Response:
```json
{
  "total": {
    "input_tokens": 1240000,
    "output_tokens": 380000,
    "cache_read_tokens": 890000,
    "cache_write_tokens": 120000,
    "reasoning_tokens": 45000,
    "request_count": 1840,
    "estimated_cost_usd": 4.82,
    "cost_status": "estimated"
  },
  "by_agent": [...],
  "by_model": [...],
  "by_day": [...]
}
```

`estimated_cost_usd` is `null` in the response if `cost_status` is `unknown`.

### `GET /api/usage/conversation/:id`

Aggregated usage for a single conversation (all channel turns, branches, and workers spawned from it).

---

## Dashboard

The `TokenUsageCard` currently shows dummy data. Replace with real data from `GET /api/usage`.

Display:
- **Total cost** (period) — prominent, with "estimated" label and tooltip explaining it
- **Token breakdown** — input / output / cache read / cache write / reasoning
- **Period selector** — 7d / 30d / 90d
- **By model breakdown** — collapsed by default, expandable

If `cost_status` is `unknown` for any portion of the period, show a note: "Some usage has unknown pricing and is excluded from the cost total."

Cache tokens are worth surfacing explicitly — cache read tokens cost ~10× less than input tokens on Anthropic. Users with heavy caching should see the savings.

---

## What's Not In Scope

- **Budget alerts / spend limits** — future work
- **Per-user attribution** in multi-user channels — too noisy, agent-level is enough
- **Actual billing reconciliation** — we show estimates only, not actuals from provider invoices
- **Retroactive pricing** — usage rows store the cost at time of recording; we don't recalculate if pricing changes
