# Agentic Backend Readiness

Spacebot's architecture — per-agent durable storage, multi-agent communication graph, the `pending_approval` task pattern, sandbox containment, skill injection — already supports a deployment shape that the dev-coding harness lane can't: **embedding Spacebot as a backend dependency of a parent application that runs many agents on behalf of its own tenants**. One agent per customer, per entity, per listing — wired into the parent app via webhooks and an HTTP control surface.

This doc is the cluster of changes that close the gap between "Spacebot can do this in principle" and "Spacebot is genuinely ready to be the agentic backend of a production multi-tenant application." Five changes, all surgical, none architectural rewrites.

## The use case

The parent application has many entities (customers, accounts, listings, projects — domain depends). Each entity benefits from a dedicated, persistent agent that:

- Owns its entity's history forever (not polluted by other entities' context)
- Holds isolated credentials (vendor keys, OAuth tokens) that other entities' agents must not see
- Wakes only when there's something to do — entities are mostly idle most of the time
- Reports outcomes back to the parent app via the existing HTTP / webhook surface
- Fails gracefully when blocked (captcha, rate limit, vendor outage) without retry-looping
- Can be bounded with predictable wall-clock timeouts so the parent app can plan

Spacebot supports the *shape* of this today (multi-agent first-class, per-agent SQLite + LanceDB + identity files, `send_agent_message` for cross-agent dispatch, `pending_approval` for human-in-the-loop). The friction is in the operational sharp edges that matter when you go from "a handful of agents on one developer's machine" to "thousands of dormant agents on a shared instance, each holding its tenant's credentials, expected to run reliably without supervision."

## What needs to change

| Change | Why | Section |
|---|---|---|
| 1. **Per-agent secret isolation** | Today all agents on one instance share `secrets.redb` — agent A can read agent B's tool secrets. Multi-tenant deployments cannot ship without scoped credentials. | [§1](#1-per-agent-secret-isolation) |
| 2. **Dormant cortex mode** | Today every agent's cortex ticks on `tick_interval_secs`. With thousands of mostly-idle agents this is wasted bulletin generation that's also stale by the time it's used. | [§2](#2-dormant-cortex-mode) |
| 3. **Wall-clock worker timeout** | Today workers have segment cap (15 × 10 = 150 turns) but no wall-clock bound. A stuck browser session can drift indefinitely. | [§3](#3-wall-clock-worker-timeout) |
| 4. **Browser captcha / blocked detection** | Today `tools/browser.rs` has SSRF protection only. Captcha and fraud-detect manifest as opaque retry-loops or generic errors instead of structured "blocked: needs human" outcomes. | [§4](#4-browser-captcha--blocked-detection) |
| 5. **Per-agent cron defaults** | Today cron timeout is 1500s with optional per-job override. Agent-class defaults (e.g. research-heavy agents need 30min default) need to be agent-level, not per-job. | [§5](#5-per-agent-cron-defaults) |

These are independent changes that can ship in any order. Together they form a deployment-readiness milestone for the agentic-backend use case.

---

## 1. Per-agent secret isolation

### Today

`main.rs:1480-1543` consolidated what were once per-agent secret stores into a single instance-level `secrets.redb`. The migration moved data from `<instance>/agents/<agent_id>/data/secrets.redb` into `<instance>/data/secrets.redb`. This was the right call for a single-user instance with a handful of agents — one master key, simpler backup, easier credential reuse across agents.

For agentic-backend deployments it's the wrong shape. Every agent calling `tool_secret_names()` (`store.rs:532`, no agent argument today) sees the entire instance set. `Sandbox` already holds a `secrets_store: ArcSwap<Option<Arc<SecretsStore>>>` (`sandbox.rs:170`) and `wrap()` injects via `tool_secrets()` (`sandbox.rs:256-262`), but the read is unscoped — same store regardless of which agent is spawning the worker. There is no scoping primitive.

In a multi-tenant deployment this means a worker spawned by agent-A reads its tenant's vendor API key out of the env, but the same worker, if it deviates from its prompt, can call `printenv` and see the API keys of every other tenant on the instance. That's a confidentiality breach by construction, not by bug.

### Design

Keep the master key instance-level (one OS-credential-store entry, one Argon2id-derived KEK). Scope the **records** by agent.

**Composes with existing `SecretCategory`.** `SecretCategory { System, Tool }` already exists (`secrets/store.rs:78`) and auto-categorizes LLM/messaging keys as `System` (excluded from subprocess env via `auto_categorize` at `:1138-1164`). The new `SecretScope` is **orthogonal** to category — keying becomes effectively `(scope, name)` with category preserved as a row attribute. `System` secrets stay `InstanceShared` always (singleton `LlmManager` / `MessagingManager` consume them). `Tool` secrets default to `Agent(...)` for agentic-backend deployments; admin can promote any `Tool` secret to `InstanceShared` when all agents legitimately share it (e.g. a single-tenant `GH_TOKEN`).

**Schema:** `secrets.redb` rows keyed by `(scope, name)` instead of `name`. Migration is a one-shot in-place key rewrite — every legacy unprefixed key becomes `InstanceShared:<name>` (preserves current behavior; nothing breaks). Admin reclassifies `Tool` secrets to `Agent(...)` post-migration as needed.

**`tool_secret_names()` and friends take `agent_id`:** returns the calling agent's tool secrets — i.e. `InstanceShared(Tool) ∪ Agent(this_agent, Tool)`. New signatures: `fn tool_secret_names(&self, agent_id: &AgentId) -> Vec<String>`, same for `tool_env_vars` (`store.rs:499`) and `tool_secret_pairs`.

**`wrap()` reads scoped secrets:** `Sandbox` is already constructed per-agent (one per `AgentDeps`, see `lib.rs:450`). Pass `agent_id` into `Sandbox::new` once; `Sandbox::tool_secrets()` then calls `store.tool_env_vars(&self.agent_id)`. `wrap()` callers don't change. Injection still flows via `--setenv` (bubblewrap) or `Command::env()` (passthrough / sandbox-exec).

This composes with the env-sanitization work in `sandbox-hardening.md` — `--clearenv` strips inherited vars, then per-agent tool secrets are re-injected. System secrets (LLM keys, messaging tokens) stay out of every worker subprocess regardless.

**Instance-shared escape hatch:** some secrets legitimately are instance-level (the LLM key). A typed enum `SecretScope { InstanceShared, Agent(AgentId) }` lets callers be explicit. Default for tool secrets created via the dashboard / API is `Agent(...)`. `InstanceShared` is admin-only and rare.

### Schema migration

```rust
// Old key
struct SecretKey {
    name: String,
}

// New key
enum SecretScope {
    InstanceShared,
    Agent(AgentId),
}

struct SecretKey {
    scope: SecretScope,
    name: String,
}
```

Migration: existing records become `InstanceShared` rows. The dashboard surfaces both scopes; a one-time admin task may reclassify select tool secrets to per-agent. The migration is non-destructive — `InstanceShared` always remains a valid scope.

### Files Changed

| File | Change |
|------|--------|
| `src/secrets/store.rs` | Add `SecretScope` enum; rekey rows by `(scope, name)`; `tool_secret_names(agent_id)`, `tool_env_vars(agent_id)`, `tool_secret_pairs(agent_id)` filter by scope |
| `src/secrets/migration.rs` (new) | One-shot in-place key rewrite: legacy unprefixed → `InstanceShared:<name>`. Idempotent. |
| `src/sandbox.rs` | `Sandbox::new` accepts owning `agent_id`; `tool_secrets()` calls `store.tool_env_vars(&agent_id)`. Sandbox is already per-agent, so `wrap()` callers don't change. |
| `src/agent/cortex.rs:3740`, `src/agent/channel_dispatch.rs:600,1279`, `src/tools/spawn_worker.rs:446` | Pass `&deps.agent_id` to `tool_secret_names()` (4 production callsites) |
| `src/tools.rs:971`, `src/secrets/scrub.rs:229` | Scrubber pair source becomes scoped — only the calling agent's tool secrets are redaction candidates |
| `src/api/secrets.rs` | API surface accepts `scope` on create; lists by scope; new `GET /api/agents/:id/secrets` for per-agent listing |
| `interface/...` | Dashboard scope picker on secret create; per-agent secret listing |

---

## 2. Dormant cortex mode

### Today

Every agent spawns four loops on startup (`spawn_warmup_loop`, `spawn_cortex_loop`, `spawn_association_loop`, `spawn_ready_task_loop` — defined at `agent/cortex.rs:1754`, `:1805`, `:3682`, `:3694`, called from `main.rs:3740-3754` and `api/agents.rs:1024-1028`). The cortex loop ticks on `tick_interval_secs`, regenerating the memory bulletin via `memory_recall` and observing system signals; memory consolidation/decay runs inside it. `spawn_ready_task_loop` independently polls `task_store.claim_next_ready(agent_id)` for `ready` tasks.

This is correct for an actively-used agent — the bulletin needs to be fresh when a user message arrives, ready tasks need to be picked up between user interactions. It's wasteful for an agent that's mostly idle. At thousands of agents on a shared instance, regenerating bulletins every 60 minutes for agents that haven't been triggered in days is meaningful LLM cost and produces bulletins that are stale by the time they're consulted (the agent wakes hours or days later when an external trigger arrives, with a bulletin from the wrong moment).

### Design

A new per-agent setting `cortex_mode: active | dormant` (default `active` for backwards compat).

When `dormant`:

- None of the four cortex loops are spawned. No periodic bulletin generation, no ready-task polling.
- Tasks get picked up via wake-on-trigger paths instead.
- Memory consolidation / decay (currently passive cortex work) moves to a separate instance-wide janitor cron that walks all agents on a slow schedule. See "Memory maintenance" below.

Wake triggers (each sends into `wake_tx: mpsc::Sender<(AgentId, WakeTrigger)>`, consumed by a central `WakeManager` that calls `cortex::wake(agent_id, trigger)`):

1. **`send_agent_message` post-insert hook** — when another agent dispatches a task to a dormant agent, the wake fires synchronously after `task_store.create()` returns (`tools/send_agent_message.rs:226`).
2. **Messaging / webhook routing dispatch** — `MessagingManager` and `webhook.rs` are agent-blind; binding-to-agent resolution happens in the `main.rs:2192` event loop ("Main event loop: route inbound messages to agent channels"). Wake fires from that loop after the binding resolves an `agent_id`, before spawning the channel or dispatching the payload. Covers Discord / Slack / Telegram / webchat / webhook uniformly.
3. **Cron timer fire** — cron jobs still work in dormant mode; they're just one more wake trigger. Each cron fire wakes the agent, runs the cron channel, then re-sleeps after grace.
4. **Internal admin wake API** — `POST /api/agents/:id/wake` for debugging / testing.

Wake `try_send` is non-blocking; on full channel, log and drop (the trigger work proceeds regardless — wake is best-effort bulletin-warming).

Wake sequence:

```text
trigger fires
  → cortex::wake(agent_id, trigger)
      → check cached bulletin: if within grace period, reuse
      → else: synthesize bulletin from memory store (no LLM tool calls — pure memory query + RRF + single LLM synthesis call)
      → cache bulletin with TTL = wake_grace_period_secs
      → hand off to trigger-appropriate process:
          ─ task triggers → spawn worker via existing ready-task path
          ─ messaging triggers → spawn channel
          ─ webhook triggers → dispatch into the existing handler
          ─ cron triggers → spawn ephemeral channel (existing cron pattern)
      → grace timer starts; on expiry without new triggers, return to fully dormant
```

The grace period matters because triggers cluster — three tasks dispatched to the same agent in 30 seconds shouldn't synthesize the bulletin three times. A short cache (default 5min) amortizes across bursts.

### Memory maintenance for dormant agents

Cortex's passive work (memory consolidation, decay, importance recalibration) currently runs as part of the tick loop. In dormant mode it can't, but it shouldn't run on every wake either (latency on the trigger path matters).

Move it to an **instance-wide memory janitor**. A separate cron job that walks all agents on a slow schedule (default daily at off-peak), running consolidation/decay against each agent's memory store. The janitor doesn't fire wake — it operates directly against the memory store as a privileged background process.

`active`-mode agents continue running their own cortex passes (the janitor is additive; consolidation is idempotent). `dormant` agents get their maintenance exclusively from the janitor.

### Configuration

```toml
[cortex]
mode = "active"                  # or "dormant"
tick_interval_secs = 3600        # only consulted when mode = "active"
wake_grace_period_secs = 300     # only consulted when mode = "dormant"

[memory_janitor]
enabled = true
schedule = "0 4 * * *"           # daily at 04:00 instance-local
```

Per-agent `cortex.mode` override in agent config. `tick_interval_secs` ignored for dormant agents. `wake_grace_period_secs` ignored for active agents.

### Files Changed

| File | Change |
|------|--------|
| `src/agent/cortex.rs` | Branch on `cortex_mode`: skip warmup/cortex/association/ready_task loops when dormant; new `wake(agent_id, trigger)` entry point; per-agent bulletin cache with TTL |
| `src/config/types.rs` | `CortexConfig.mode: CortexMode { Active, Dormant }`; `wake_grace_period_secs`; `MemoryJanitorConfig { enabled, schedule }` on top-level `Config` |
| `src/agent/wake.rs` (new) | `WakeManager` registry + `wake_tx` mpsc dispatcher; per-agent `Mutex<Option<CachedBulletin>>` with TTL |
| `src/tools/send_agent_message.rs` | Post-insert hook (after `task_store.create()`) fires `wake_tx.try_send(...)` for the receiving agent |
| `src/cron/scheduler.rs` | Cron fire path fires `wake_tx.try_send(...)` for the owning agent (no-op branch if active) |
| `src/api/agents.rs` | `POST /api/agents/:id/wake` admin endpoint; gate the four cortex loops on `cortex_mode` at agent-create path (~line 1024) |
| `src/main.rs` | Event loop (line 2192) fires `wake_tx.try_send(...)` for messaging + webhook inbound after binding resolves `agent_id`; gate the four cortex loops at agent-startup path (~line 3740); spawn `WakeManager` and memory janitor at startup |
| `src/agent/maintenance.rs` (new) | Instance-wide memory janitor — walks agents on cron schedule; uses existing `memory::maintenance` machinery |

### Cost framing

For an instance running 2,000 mostly-idle agents on a 60-minute tick: ~48,000 LLM-backed bulletin generations per day. With dormant mode and an average of ~5 wakes per agent per day, that drops to ~10,000 bulletin generations — a 79% reduction without losing any quality (wake-time bulletins are *fresher* than tick-generated ones, since they're synthesized at the moment of work). For deployments billing through a fixed LLM provider plan, this is the difference between "fits within quota" and "needs metered billing."

---

## 3. Wall-clock worker timeout

### Today

`agent/worker.rs:26,48` defines `TURNS_PER_SEGMENT = 15` and `MAX_SEGMENTS = 10`, giving a 150-turn ceiling. There is no wall-clock bound. A worker stuck on slow browser navigation, retried network calls, or a tool that yields slowly can run for hours before the segment ceiling kicks in. For agentic-backend deployments where the parent app expects to plan around predictable per-task latency, this is a problem.

There's a transient-error retry budget (`MAX_TRANSIENT_RETRIES = 5`, `worker.rs:40`) and an overflow-retry budget (`MAX_OVERFLOW_RETRIES = 2`, `worker.rs:34`), but neither is wall-clock-shaped.

### Design

**Naming.** `CortexConfig.worker_timeout_secs` (`config/types.rs:1043`, default 600s) already exists as the supervisor's idle-kill bound, measured from `last_activity_at`. The new wall-clock bound is a different mechanism; pick a non-colliding name. Use `worker_wall_clock_timeout_secs` (or rename the supervisor field to `worker_idle_timeout_secs`).

**Structured outcome surface.** Today `Worker::run` returns `Result<String>` (`worker.rs:325`). There is no `WorkerOutcome` enum; the LLM signals completion via `set_status(kind: Outcome)` and the cron-only `SetOutcomeTool` writes `CronOutcome { content: String }` for scheduler delivery. This phase invents the structured surface from scratch — it's the shared substrate phase 4 also needs (see [§4](#4-browser-captcha--blocked-detection)).

```rust
pub enum WorkerOutcome {
    Success { result: String },
    Failed  { reason: String },
    Partial { result: String, segments_run: usize },        // existing max-segment exit
    Timeout { elapsed_secs: u64, segments_run: usize },     // this phase
    Blocked { reason: BlockReason, url: Option<String>, evidence: BlockEvidence }, // phase 4
}
```

`Worker::run` becomes `-> Result<WorkerOutcome>`. Refactor the segment loop into `run_inner`; wrap the call in `tokio::time::timeout`:

```rust
pub async fn run(mut self) -> Result<WorkerOutcome> {
    let timeout = self.resolve_wall_clock_timeout(); // conversation > agent > 1800s default
    match tokio::time::timeout(Duration::from_secs(timeout), self.run_inner()).await {
        Ok(outcome) => outcome,
        Err(_) => {
            self.persist_transcript().await?;
            self.write_failure_log("wall_clock_timeout").await?;
            Ok(WorkerOutcome::Timeout { elapsed_secs: timeout, segments_run: self.segments_run })
        }
    }
}
```

Wall-clock encompasses the entire worker lifetime, not per-segment. Per-segment bounds come from `MAX_TRANSIENT_RETRIES` and the segment cap; the wall-clock bound is a separate ceiling that catches the slow-drift case.

**Resolution chain.** `ConversationSettings.worker_wall_clock_timeout_secs` → `ResolvedAgentConfig.worker_wall_clock_timeout_secs` → 1800s system default. Mirrors the existing `ResolvedConversationSettings::resolve` chain.

```toml
[conversation_settings]
worker_wall_clock_timeout_secs = 1800       # 30 min — generous default for research-heavy work
```

Per-task override flows through the task description (already plumbed via `tool_budget` in the task-creation API).

**Resumed workers.** Each `Worker::run` invocation gets a fresh wall-clock budget — the timer starts at resumption, not at original spawn. Interactive resumption (`worker.rs:387-395`) is treated as a new run for budgeting purposes.

### Interaction with `set_outcome`

`SetOutcomeTool` (`tools/set_outcome.rs`) is unrelated — it's the cron-job content-delivery tool that writes a `CronOutcome { content: String }` for the scheduler. It stays as-is; this phase doesn't extend it. The new `WorkerOutcome` is a Rust-side return value, not an LLM-callable tool. The LLM continues to signal completion intent via `set_status(kind: Outcome)`.

Caller updates: `agent/channel_dispatch.rs`'s `WorkerCompletionError` (line 44) and `map_worker_completion_result` (line 94) are the existing convergence point; extend them to handle the new variants. `cortex.rs:3694+` (`spawn_ready_task_loop` → `pickup_one_ready_task` at `:3718`) and `tools/spawn_worker.rs` also pattern-match on the new enum.

### Files Changed

| File | Change |
|------|--------|
| `src/conversation/settings.rs` | Add `worker_wall_clock_timeout_secs: Option<u64>` |
| `src/config/types.rs` | Add `worker_wall_clock_timeout_secs: Option<u64>` to `ResolvedAgentConfig` |
| `src/agent/worker.rs` | New `WorkerOutcome` enum (shared with phase 4); `Worker::run` returns `Result<WorkerOutcome>`; segment loop extracted to `run_inner`; wrapped in `tokio::time::timeout` |
| `src/agent/channel_dispatch.rs`, `src/agent/cortex.rs`, `src/tools/spawn_worker.rs` | Update worker call sites for new return type; pattern-match `WorkerOutcome` |
| `src/api/tasks.rs` | Surface `Timeout` state in task detail responses |

---

## 4. Browser captcha / blocked detection

### Today

`tools/browser.rs` lines 72-131 ship SSRF protection — blocks requests to cloud metadata endpoints, private IPs, loopback, and link-local addresses. Solid for what it does. Nothing for captcha, fraud-detect, login walls, or rate-limit ceilings. Browser sessions that hit these manifest as opaque DOM states or generic errors; the LLM sees confusing output and either retry-loops or invents a workaround that doesn't apply.

For agentic-backend deployments where browser tooling runs against many vendors with varying defenses, this is a daily failure mode. Without a structured "blocked" signal, the agent can't capture the failed path in memory and avoid it next time.

### Design

Detection heuristics layered into `tools/browser.rs` after existing SSRF checks:

**Captcha detection.** On page load and after navigation, scan the DOM and active iframes for known captcha frame patterns:

- recaptcha: iframe `src` containing `google.com/recaptcha/`, `gstatic.com/recaptcha/`
- hcaptcha: iframe `src` containing `hcaptcha.com/captcha/`
- Cloudflare Turnstile: iframe `src` containing `challenges.cloudflare.com/turnstile/`
- Cloudflare challenge page: `cf-chl-bypass`, `cf-mitigated` headers; `cf_challenge_response` cookie name
- Generic challenge heuristics: presence of `<meta name="captcha">`, `<input name="g-recaptcha-response">`, etc.

**Login wall heuristics.** Detect when the navigation target redirected to a login URL:

- Final URL host matches the navigated host but path is in a small set of login signals (`/login`, `/signin`, `/auth/`, `/account/login`)
- Status 401 / 403 with auth-prompting `WWW-Authenticate` headers

**Rate-limit / fraud-detect.** Status-code-based:

- `429 Too Many Requests` — block, capture `Retry-After` if present
- `403 Forbidden` with body matching common WAF banners (Akamai, Cloudflare, AWS WAF, Imperva)
- Empty 200 responses on pages where navigation was expected to land somewhere meaningful (heuristic, opt-in per-tool)

**On detection.** The browser tool returns `BrowserToolError::Blocked(BlockReason)` with captured evidence. The worker's tool-error handler converts to `WorkerOutcome::Blocked` (the structured surface from [§3](#3-wall-clock-worker-timeout)), terminating the segment loop:

```rust
WorkerOutcome::Blocked {
    reason: BlockReason::Captcha { provider: "cloudflare-turnstile" },
    url: Some("https://example.com/signup".into()),
    evidence: BlockEvidence {
        screenshot: Option<Vec<u8>>,
        html_snippet: Option<String>,
        request_headers: HashMap<String, String>,
    },
}
```

The structured outcome is the worker's output. The calling agent's memory captures the blocked path so future attempts at the same URL/path don't retry the same dead end.

### What this is not

This is **detection**, not **bypass**. We don't try to solve captchas, rotate user-agents to evade fraud-detect, or use residential proxies. The signal exists so the agent can fail cleanly and surface to a human; the parent app's escalation path takes over from there.

### Files Changed

| File | Change |
|------|--------|
| `src/tools/browser.rs` | Add detection hooks at navigation/click/snapshot entry points; on positive detection return `BrowserToolError::Blocked(...)` with captured evidence |
| `src/tools/browser_detection.rs` (new) | Pure detection logic — testable in isolation. Flat-file alongside `browser.rs` (currently a 2729-line monolith). Module-restructure to `tools/browser/{mod,detection}.rs` is optional and out of scope for this phase. |
| `src/agent/worker.rs` | `BlockReason` and `BlockEvidence` types live next to `WorkerOutcome` from §3; `BrowserToolError::Blocked` → `WorkerOutcome::Blocked` conversion in the worker's tool-error path |

---

## 5. Per-agent cron defaults

### Today

`cron/scheduler.rs:1387` defines a 1500s (25min) default timeout with optional per-job override (`CronJob.timeout_secs: Option<u64>`, defined at `scheduler.rs:39, 107`). Useful for the typical case but doesn't compose well with agent classes. Research-heavy agents legitimately need longer for a single cron firing (browser navigation + LLM reasoning + tool calls); short-task agents need bounded timeouts to fail fast.

Setting per-job timeouts on every cron job for an agent class is repetitive and scales poorly. Per-agent default + per-job override is the right shape.

### Design

Add `cron_default_timeout_secs` to `ResolvedAgentConfig`. Cron resolution chain becomes: `job.timeout_secs` (per-job override) → `agent.cron_default_timeout_secs` (per-agent default) → `1500` (existing system default, preserved).

```rust
fn resolve_cron_timeout(job: &CronJob, agent: &ResolvedAgentConfig) -> u64 {
    job.timeout_secs
        .or(agent.cron_default_timeout_secs)
        .unwrap_or(DEFAULT_CRON_TIMEOUT_SECS)
}
```

Small change, surfaces in agent config TOML, no migration needed.

### Files Changed

| File | Change |
|------|--------|
| `src/config.rs` | `ResolvedAgentConfig.cron_default_timeout_secs: Option<u64>` |
| `src/cron/scheduler.rs` | `resolve_cron_timeout` chain |

---

## Phase plan

Four changes are truly independent. **Phase 4 has a hard dependency on Phase 3** because both surface results through the new `WorkerOutcome` enum that doesn't exist today — Phase 3 builds the enum, Phase 4 just adds the `Blocked` variant + tool-error conversion.

Suggested sequence based on impact for the agentic-backend deployment shape:

### Phase 1 — Per-agent secret isolation

The hardest gating problem. No multi-tenant deployment ships without it. Touches schema, sandbox env injection, dashboard. Ship first because it blocks all other deployment work.

### Phase 2 — Dormant cortex mode

The cost-of-scale problem. Required before any deployment exceeding ~100 agents on shared infrastructure. Includes the memory janitor as the maintenance counterpart.

### Phase 3 — Wall-clock worker timeout

Predictable per-task latency for the parent app's planning. Builds the `WorkerOutcome` structured-outcome enum that Phase 4 consumes — sequence Phase 3 before Phase 4.

### Phase 4 — Browser captcha / blocked detection

Quality-of-failure improvement. Lets agents fail cleanly and surface to humans rather than retry-loop opaquely. Depends on the `WorkerOutcome` enum from Phase 3.

### Phase 5 — Per-agent cron defaults

Smallest of the five. Quality-of-life for agent-class config. Ship anytime.

---

## Non-goals

- **No new agent topology primitives.** The communication graph (`multi-agent-communication-graph.md`) and link channels are unchanged. Agentic-backend deployments use the same `send_agent_message` / `LinkKind` / `LinkDirection` primitives as a single-user instance.
- **No new task primitives.** The `pending_approval` pattern, `assigned_agent_id` ownership, ready-task pickup, and task comments (`autonomy.md`) all stay as-is. Dormant cortex changes *when* tasks get picked up (on wake instead of on tick), not the task model itself.
- **No new sandbox backend.** bubblewrap (Linux) and sandbox-exec (macOS) remain the runtime sandboxes. Per-agent secret isolation composes with the env-sanitization work in `sandbox-hardening.md`. VM-level isolation (`stereos-integration.md`) is a separate, much later, opt-in deployment path.
- **No bypass capabilities.** Captcha / blocked detection is for failing cleanly, not for bypassing protections. The structured outcome is the surface; what the parent app does with it is its decision.
- **No identity-file changes.** `SOUL.md` / `IDENTITY.md` / `USER.md` / `ROLE.md` (per `multi-agent-communication-graph.md`) all stay as-is. Per-agent identity files are already first-class.

## Cross-references

- `secret-store.md` — the master-key model and tool/system-secret distinction this work scopes per agent.
- `sandbox-hardening.md` — env sanitization (`--clearenv`) composes with per-agent secret injection in `wrap()`.
- `autonomy.md` — task pickup model. Dormant cortex changes the *trigger* for ready-task pickup (wake vs tick) without changing the task model.
- `multi-agent-communication-graph.md` — the link / topology system. `send_agent_message` post-insert hook is the integration point for dormant wake.
- `cron-outcome-delivery.md` — cron outcome surfacing. `set_outcome` timeout / blocked variants surface here.
- `production-worker-failures.md` — adjacent operational concerns for worker reliability.
