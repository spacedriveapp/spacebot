# Workers Tab

A two-column worker run viewer in the agent interface. Left column lists all worker runs for the agent (across all channels), right column shows the selected worker's full transcript. Replaces the current "coming soon" placeholder at `/agents/$agentId/workers`.

## Concept

Workers currently persist minimal data: a summary row in `worker_runs` (task, result, status, timestamps) and optional filesystem log files. The summary row is enough for the channel timeline but not for a dedicated workers page. The log files are debug artifacts — unstructured, not queryable, and may contain sensitive tool output.

The workers tab needs the full conversation transcript for each worker. The challenge is persisting this without bloating the database — a single worker can generate 30-50 messages with multi-KB tool outputs.

**Solution:** store the full transcript as a single gzipped JSON blob on the `worker_runs` row at completion. No per-message rows, no separate event tables. A typical 30-message transcript compresses from ~15-50KB raw to ~3-8KB gzipped. The blob is only loaded when someone clicks into a specific worker — list queries never touch it.

While a worker is running, the UI reads live state from the in-memory `StatusBlock` (via API snapshot on page load) and SSE events (for real-time updates after load). Once the worker completes, the transcript blob becomes available for full inspection.

Key properties:
- **Agent-scoped** — shows workers across all channels, not per-channel
- **Two-column layout** — list on left, detail on right, URL-driven selection
- **Compressed transcript** — full `Vec<Message>` serialized as gzipped JSON blob on `worker_runs`, written once at completion
- **Live view from memory** — running workers tracked via `StatusBlock` snapshot + SSE, no database writes during execution
- **Lazy loading** — transcript blob only fetched on detail view, never on list queries

## Data Model

### Existing: `worker_runs`

Already exists in `migrations/20260213000003_process_runs.sql`.

```sql
CREATE TABLE IF NOT EXISTS worker_runs (
    id TEXT PRIMARY KEY,
    channel_id TEXT,
    task TEXT NOT NULL,
    result TEXT,
    status TEXT NOT NULL DEFAULT 'running',
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP,
    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
);
```

### New columns on `worker_runs`

```sql
ALTER TABLE worker_runs ADD COLUMN worker_type TEXT NOT NULL DEFAULT 'builtin';
ALTER TABLE worker_runs ADD COLUMN agent_id TEXT;
ALTER TABLE worker_runs ADD COLUMN transcript BLOB;
```

`transcript` stores the gzipped JSON of the worker's Rig message history. Written once on completion (success or failure). NULL while the worker is running.

Workers don't have user messages. A worker is spawned by a channel with a task string (passed as the initial prompt) and a system prompt. The Rig history that accumulates is strictly assistant actions and tool results — the "User" messages in Rig's `Vec<Message>` are just tool result containers, not actual user input.

The JSON format reflects this — a flat array of transcript steps:

```json
[
  {
    "type": "action",
    "content": [
      {"type": "text", "text": "I'll run the test suite."},
      {"type": "tool_call", "id": "shell:0", "name": "shell", "args": "{\"command\":\"cd ~/app && pytest\"}"}
    ]
  },
  {
    "type": "tool_result",
    "call_id": "shell:0",
    "name": "shell",
    "text": "12 passed, 0 failed"
  },
  {
    "type": "action",
    "content": [
      {"type": "text", "text": "All 12 tests passed."}
    ]
  }
]
```

`action` entries come from Rig's `Message::Assistant` — they contain the model's reasoning text and/or tool calls. `tool_result` entries come from Rig's `Message::User` tool result wrappers. This flattened format strips the misleading role labels and presents the transcript as what it actually is: a sequence of agent actions and tool outputs.

Tool result text is truncated to 50KB per result (same cap as `SpacebotHook` already applies to broadcast events) before serialization. Tool call args are truncated to 2KB.

## Transcript Lifecycle

1. Worker starts — `worker_runs` row created with `transcript = NULL`
2. Worker runs — live tracking via in-memory `StatusBlock` + SSE events
3. Worker completes (success or failure) — serialize `Vec<Message>` history:
   a. Convert Rig messages to the `TranscriptStep` format
   b. Truncate tool result text (50KB cap per result)
   c. Serialize to JSON
   d. Gzip compress
   e. UPDATE `worker_runs SET transcript = ?` (fire-and-forget)
4. UI loads detail — API decompresses blob, returns structured JSON

For OpenCode workers, there's no Rig `Vec<Message>`. The transcript column stays NULL for these — their detail view shows the `result` text when completed.

## Live Worker State

Running workers are tracked in two places, serving different needs:

### Snapshot on page load: `StatusBlock` via API

The `StatusBlock` (`src/agent/status.rs`) lives in memory per channel and tracks active workers with: id, task, status text, started_at, tool_calls count. It's registered with `ApiState.channel_status_blocks` and already exposed via `GET /api/channels/status` (`src/api/channels.rs:132`).

The existing frontend calls `syncStatusSnapshot()` on mount and on SSE reconnect, which fetches this endpoint and hydrates `ChannelLiveState.workers`. This is how running workers survive a page refresh today — they're pulled from memory, not from SSE history.

For the workers tab, the list endpoint merges two sources:
1. **DB query** — `SELECT ... FROM worker_runs WHERE agent_id = ?` for the base rows (id, task, channel_id, worker_type, started_at, completed_at, status)
2. **StatusBlock snapshot** — iterate `channel_status_blocks`, extract `active_workers`, merge current `status` text and `tool_calls` count onto running DB rows

This gives the list endpoint both the persistent metadata (worker_type, channel_id) and the live ephemeral state (current status text, tool call count) without any new persistence.

### Live updates after load: SSE events

Once the page is loaded, SSE events (`worker_started`, `worker_status`, `worker_completed`, `tool_started`, `tool_completed`) keep the UI current. These already flow through `ApiState.event_tx` to the SSE endpoint at `GET /api/events`.

The workers tab subscribes to these same SSE events to update running worker cards in real-time. On `worker_completed`, the UI refetches the detail endpoint to load the now-available transcript blob.

### What doesn't exist (and doesn't need to)

`ChannelState.active_workers: Arc<RwLock<HashMap<WorkerId, Worker>>>` is declared but never populated — workers are consumed by `Worker::run()` which takes ownership of `self`. The map is dead code. Worker cancellation goes through `worker_handles` (JoinHandles), not through this map.

## Prerequisite Fixes

Issues discovered during audit that should be addressed as part of this work.

### Fix: `worker_runs.status` never set to `'failed'`

Currently, `ProcessRunLogger::log_worker_completed` (`src/conversation/history.rs`) always writes `status = 'done'`, even when the worker errored. The `spawn_worker_task` wrapper (`src/agent/channel.rs:1671`) sends `WorkerComplete` for both success and failure — failures get `result = "Worker failed: {error}"` but the same `'done'` status.

For the workers tab to show distinct failed/done badges, the completion path needs to differentiate. Options:
- Thread a `success: bool` through `ProcessEvent::WorkerComplete` and use it in `log_worker_completed` to write `'done'` vs `'failed'`
- Alternatively, infer from the result text prefix `"Worker failed:"` — but that's fragile

The cleaner approach: add a `success` field to `ProcessEvent::WorkerComplete`, and update `log_worker_completed` to write `status = 'failed'` when `success = false`.

### Fix: `worker_type` not on `ProcessEvent::WorkerStarted`

The doc plans to persist `worker_type` on the `worker_runs` row, but the current `ProcessEvent::WorkerStarted` variant doesn't carry this field. The `agent_id` is already on the event (and currently discarded by the `..` match pattern in `handle_event`).

Add `worker_type: String` to `ProcessEvent::WorkerStarted`. Set it to `"builtin"` in `spawn_worker_from_state` and `"opencode"` in `spawn_opencode_worker_from_state`. The event handler then passes it through to the logger.

### Note: `flate2` dependency needed

Gzip compression requires the `flate2` crate, which is not currently in `Cargo.toml`. Add `flate2 = "1"` to dependencies. It's a well-maintained, widely-used crate with no transitive bloat.

## Phase 1: Persistence Layer

### 1a. Migration

New file `migrations/YYYYMMDD000001_worker_transcript.sql`:
- `ALTER TABLE worker_runs ADD COLUMN worker_type TEXT NOT NULL DEFAULT 'builtin'`
- `ALTER TABLE worker_runs ADD COLUMN agent_id TEXT`
- `ALTER TABLE worker_runs ADD COLUMN transcript BLOB`

### 1b. Transcript Serialization

New module `src/conversation/worker_transcript.rs`:

```rust
/// A single step in a worker transcript.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptStep {
    /// Agent reasoning and/or tool calls (from Rig's Message::Assistant).
    Action {
        content: Vec<ActionContent>,
    },
    /// Tool execution result (from Rig's Message::User tool result wrapper).
    ToolResult {
        call_id: String,
        name: String,
        text: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionContent {
    Text { text: String },
    ToolCall { id: String, name: String, args: String },
}
```

Functions:
- `serialize_transcript(history: &[rig::message::Message]) -> Vec<u8>` — convert Rig messages to `Vec<TranscriptStep>`, truncate tool output, JSON serialize, gzip
- `deserialize_transcript(blob: &[u8]) -> Result<Vec<TranscriptStep>>` — gunzip, JSON parse

The conversion walks Rig's `Vec<Message>` and maps `Message::Assistant` to `TranscriptStep::Action` and `Message::User` (which only contains `ToolResult` items in worker history) to `TranscriptStep::ToolResult`. Any `Message::User` text content (e.g. follow-up prompts for interactive workers, or the "Continue where you left off" segment continuation prompt) becomes a `TranscriptStep::Action` with a text note.

Tool result truncation reuses `crate::tools::truncate_output` with the existing `MAX_TOOL_OUTPUT_BYTES` (50KB) constant. Tool call args are truncated to 2KB.

Add `flate2 = "1"` to `Cargo.toml` dependencies.

### 1c. Prerequisite: Fix `ProcessEvent::WorkerStarted` + completion status

Before wiring persistence:

1. Add `worker_type: String` to `ProcessEvent::WorkerStarted` in `src/lib.rs`
2. Set `worker_type: "builtin".into()` in `spawn_worker_from_state`, `"opencode".into()` in `spawn_opencode_worker_from_state`
3. Add `success: bool` to `ProcessEvent::WorkerComplete` in `src/lib.rs`
4. Set `success: true` on `Ok(_)`, `success: false` on `Err(_)` in `spawn_worker_task`
5. Update `handle_event()` match arms for the new fields
6. Update `ApiEvent` variants and the event forwarder in `ApiState::register_agent_events`
7. Update `StatusBlock::update()` to handle the new fields

### 1d. Transcript Persistence

In `Worker::run()` (`src/agent/worker.rs`), after the run loop completes (before returning the result):

```rust
let transcript_blob = worker_transcript::serialize_transcript(&history);
let pool = self.deps.sqlite_pool.clone();
let worker_id = self.id;
tokio::spawn(async move {
    sqlx::query("UPDATE worker_runs SET transcript = ? WHERE id = ?")
        .bind(&transcript_blob)
        .bind(worker_id.to_string())
        .execute(&pool)
        .await
        .ok();
});
```

This happens on both success and failure paths — failed workers have transcripts too (often more useful for debugging).

### 1e. Update `ProcessRunLogger`

- Extend `log_worker_started` signature to accept and persist `worker_type` and `agent_id`
- Update `log_worker_completed` to accept `success: bool` and write `status = 'done'` or `status = 'failed'` accordingly
- Add query methods:
  - `list_worker_runs(agent_id, limit, offset, status_filter) -> Vec<WorkerRunRow>` — SELECT without transcript column, LEFT JOIN channels for display name
  - `get_worker_detail(worker_id) -> Option<WorkerDetailRow>` — SELECT with transcript blob

## Phase 2: API

### 2a. New Module `src/api/workers.rs`

Handlers for the workers endpoints. Follows the same pattern as `src/api/channels.rs`.

### 2b. Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/agents/workers?agent_id=...&limit=50&offset=0&status=...` | List worker runs (no transcript blob) |
| `GET` | `/api/agents/workers/detail?agent_id=...&worker_id=...` | Run metadata + decompressed transcript |

### 2c. Response Types

**Worker list item** (no transcript, fast query):
```json
{
  "id": "uuid",
  "task": "run the test suite",
  "status": "done",
  "worker_type": "builtin",
  "channel_id": "discord:123:456",
  "channel_name": "#general",
  "started_at": "2026-02-23T10:30:00Z",
  "completed_at": "2026-02-23T10:31:45Z",
  "has_transcript": true,
  "live_status": null,
  "tool_calls": 0
}
```

`channel_name` resolved via LEFT JOIN to `channels` table. `has_transcript` is `transcript IS NOT NULL`. `live_status` and `tool_calls` are populated from the in-memory `StatusBlock` for running workers (null/0 for completed ones).

**Worker detail** (includes transcript):
```json
{
  "id": "uuid",
  "task": "run the test suite",
  "result": "All 12 tests passed.",
  "status": "done",
  "worker_type": "builtin",
  "channel_id": "discord:123:456",
  "channel_name": "#general",
  "started_at": "2026-02-23T10:30:00Z",
  "completed_at": "2026-02-23T10:31:45Z",
  "transcript": [
    {
      "type": "action",
      "content": [
        {"type": "text", "text": "I'll run the test suite."},
        {"type": "tool_call", "id": "shell:0", "name": "shell", "args": "{\"command\":\"cd ~/app && pytest\"}"}
      ]
    },
    {
      "type": "tool_result",
      "call_id": "shell:0",
      "name": "shell",
      "text": "12 passed, 0 failed"
    },
    {
      "type": "action",
      "content": [
        {"type": "text", "text": "All 12 tests passed."}
      ]
    }
  ]
}
```

`transcript` is null while the worker is running or for OpenCode workers.

### 2d. Live Status Merge (list endpoint)

The list handler merges DB results with in-memory `StatusBlock` data for running workers:

1. Query `worker_runs` from DB (returns all runs including `status = 'running'`)
2. Read `channel_status_blocks` from `ApiState` — flatten all `active_workers` across channels into a `HashMap<WorkerId, WorkerStatus>`
3. For each DB row with `status = 'running'`, overlay the `StatusBlock` snapshot: current `status` text and `tool_calls` count

This merge happens in the API handler, not in the DB layer. The handler already has access to `ApiState` which holds both the SQLite pool and the status blocks.

### 2e. Route Registration

Add to `src/api/server.rs`:
```rust
.route("/agents/workers", get(workers::list_workers))
.route("/agents/workers/detail", get(workers::worker_detail))
```

Add `mod workers;` to `src/api.rs`.

## Phase 3: Frontend

### 3a. API Client Types

New types in `interface/src/api/client.ts`:

```typescript
type ActionContent =
    | { type: "text"; text: string }
    | { type: "tool_call"; id: string; name: string; args: string };

type TranscriptStep =
    | { type: "action"; content: ActionContent[] }
    | { type: "tool_result"; call_id: string; name: string; text: string };

interface WorkerRunInfo {
    id: string;
    task: string;
    status: string;
    worker_type: string;
    channel_id: string | null;
    channel_name: string | null;
    started_at: string;
    completed_at: string | null;
    has_transcript: boolean;
    live_status: string | null;
    tool_calls: number;
}

interface WorkerDetailResponse {
    id: string;
    task: string;
    result: string | null;
    status: string;
    worker_type: string;
    channel_id: string | null;
    channel_name: string | null;
    started_at: string;
    completed_at: string | null;
    transcript: TranscriptStep[] | null;
}

interface WorkerListResponse {
    workers: WorkerRunInfo[];
    total: number;
}
```

### 3b. `AgentWorkers` Page Component

New file `interface/src/routes/AgentWorkers.tsx`.

**Layout:**
```
┌──────────────────────────────────────────────────────────┐
│ [Search...] [Status: All ▾]              42 workers      │
├─────────────────────┬────────────────────────────────────┤
│ Worker List         │ Worker Detail                      │
│                     │                                    │
│ ▸ run test suite    │ Task: run the test suite           │
│   done · #general   │ Channel: #general · builtin        │
│   2m ago            │ Duration: 1m 45s · done            │
│                     │                                    │
│ ▸ check deployment  │ ┌─ Transcript ─────────────────┐  │
│   running · #ops    │ │                               │  │
│   just now          │ │ I'll run the test suite.      │  │
│                     │ │                               │  │
│ ▸ update docs       │ │ ▶ shell                       │  │
│   failed · #dev     │ │   cd ~/app && pytest          │  │
│   5m ago            │ │ ✓ 12 passed, 0 failed         │  │
│                     │ │                               │  │
│                     │ │ All 12 tests passed.          │  │
│                     │ └───────────────────────────────┘  │
└─────────────────────┴────────────────────────────────────┘
```

**Left column (worker list):**
- Search input (filters task text)
- Status filter pills: All / Running / Done / Failed
- Scrollable list of worker run cards
- Each card: task (truncated), status badge (running=amber, done=green, failed=red), channel name, relative time
- Running workers show `live_status` text from the StatusBlock merge
- Selected worker highlighted with `bg-app-selected`
- Polling: `refetchInterval: 5_000`

**Right column (worker detail):**
- Empty state when no worker selected: "Select a worker to view details"
- Header: task, channel, duration, status badge, worker type badge

- **Running worker:** on initial load, status/tool_calls come from the StatusBlock snapshot merged into the list response. After load, SSE events update status text, active tool name, and tool call count in real-time. Transcript section shows "Transcript available when worker completes." On `worker_completed` SSE event, the UI refetches the detail endpoint to load the transcript blob.

- **Completed worker with transcript:**
  - Renders as a linear sequence of agent actions and tool results — no user/assistant role labels since workers are system-spawned
  - `action` steps: reasoning text (markdown rendered) and tool call blocks
  - Tool calls shown as collapsible blocks: tool name + truncated args header, expand for full args
  - `tool_result` steps: tool name label + result text (monospace, collapsible for long output)
  - Scrollable, takes full available height

- **Completed worker without transcript** (OpenCode, or pre-migration runs):
  - Show result text (markdown rendered)
  - Note: "Full transcript not available for this worker"

- **Result section** (always shown if completed): markdown-rendered `result` text at the top

**URL state:**
- Selected worker ID in search params: `/agents/$agentId/workers?worker=<uuid>`
- Deep links and browser back/forward

### 3c. Router Update

Replace the placeholder in `router.tsx`:

```tsx
import { AgentWorkers } from "@/routes/AgentWorkers";

const agentWorkersRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/agents/$agentId/workers",
    validateSearch: (search: Record<string, unknown>): { worker?: string } => ({
        worker: typeof search.worker === "string" ? search.worker : undefined,
    }),
    component: function AgentWorkersPage() {
        const { agentId } = agentWorkersRoute.useParams();
        return (
            <div className="flex h-full flex-col">
                <AgentHeader agentId={agentId} />
                <div className="flex-1 overflow-hidden">
                    <AgentWorkers agentId={agentId} />
                </div>
            </div>
        );
    },
});
```

## Build Order

```
Phase 1a     Migration                 standalone
Phase 1b     Transcript serialization  standalone, parallel with 1a (add flate2 dep)
Phase 1c     ProcessEvent fixes        standalone, parallel with 1a-1b
Phase 1d     Transcript persistence    depends on 1a, 1b
Phase 1e     ProcessRunLogger updates  depends on 1a, 1c
Phase 2a-2e  API endpoints + merge     depends on 1b, 1e
Phase 3a     Client types              depends on 2
Phase 3b-3c  UI components             depends on 3a
```

Phases 1a, 1b, and 1c are all independent and can run in parallel. Phase 3 is entirely frontend.

## File Changes

**New files:**
- `migrations/YYYYMMDD000001_worker_transcript.sql` — alter existing table (3 new columns)
- `src/conversation/worker_transcript.rs` — transcript serialization/deserialization
- `src/api/workers.rs` — list and detail handlers
- `interface/src/routes/AgentWorkers.tsx` — two-column workers page

**Modified files:**
- `Cargo.toml` — add `flate2 = "1"` dependency
- `src/lib.rs` — add `worker_type` to `ProcessEvent::WorkerStarted`, add `success` to `ProcessEvent::WorkerComplete`
- `src/api.rs` — add `mod workers`
- `src/api/state.rs` — update `ApiEvent` variants and event forwarder for new fields
- `src/api/server.rs` — register two new routes
- `src/agent/channel.rs` — update `spawn_worker_from_state` and `spawn_opencode_worker_from_state` to set `worker_type`, update `handle_event()` match arms
- `src/agent/worker.rs` — persist transcript blob on completion
- `src/agent/status.rs` — update `StatusBlock::update()` for new event fields
- `src/conversation.rs` — add `pub mod worker_transcript`
- `src/conversation/history.rs` — extend `ProcessRunLogger` (log_worker_started, log_worker_completed signatures, add query methods)
- `interface/src/api/client.ts` — new types + api methods
- `interface/src/router.tsx` — replace placeholder, add search param validation

## Notes

- Transcript blob is written once on completion — no incremental updates. While a worker is running, the UI uses the existing in-memory SSE pipeline for live tracking and the StatusBlock snapshot for page load recovery. Once done, the full transcript becomes available.
- No separate events table. Live state comes from memory (`StatusBlock`), historical state comes from the transcript blob. One source of truth for each mode, no duplicate data.
- Compression ratio is favorable: JSON with repetitive key names and ASCII tool output compresses well with gzip. Expect ~5x compression on typical worker transcripts.
- The transcript serialization truncates tool results at 50KB (reusing the existing `MAX_TOOL_OUTPUT_BYTES` constant from `src/tools.rs`) and tool args at 2KB. This caps worst-case uncompressed size at ~1.5MB for a 30-tool-call worker, which compresses to ~200KB. Typical workers are much smaller.
- Old transcripts can be pruned by a future maintenance task (e.g. "nullify transcript blobs older than 30 days"). The `worker_runs` summary row survives — only the blob is cleared.
- OpenCode workers don't have a Rig `Vec<Message>` — their transcript column stays NULL. The detail view shows the `result` text only.
- Channel name resolution uses a LEFT JOIN to the `channels` table. Standalone workers (channel_id IS NULL) show no channel label.
- Pre-migration worker runs will have `transcript = NULL`, `worker_type = 'builtin'` (default), and `agent_id = NULL`. The UI handles all three gracefully.
- `ChannelState.active_workers` is dead code (never populated) and can be cleaned up separately. The `check_worker_limit` function reads from it, meaning the max concurrent worker limit is currently unenforced. This is a pre-existing issue, not introduced by this work, but worth fixing alongside it by switching the limit check to read from `worker_handles.len()` instead.
