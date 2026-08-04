# Deep Review: `feat/task-dag-foundations`

**Target:** branch `feat/task-dag-foundations` vs `main` (merge-base `ac52277`) — 47 commits, 151 files, +39,610/−10,246.
**Method:** 10 read-only reviewer lanes (security, logic ×3 [tasks / workflows / llm-config], conformity, quality, tests, contracts, docs, UI) over a shared change map, synthesized and adjudicated by the orchestrator. Every P1 was spot-checked against the code first-hand.

## Verdict

**Do not merge** — six P1s: a fan-out race that double-runs branch sets, template gates that deadlock or settle on stale data, gate/loop-hold invariants bypassed outside the sweep, LiteLLM-only boots broken by Anthropic-pointed routing, worker timeouts settling tasks as `done` (releasing the graph on incomplete work), and LiteLLM usage double-counted.

---

## P1 — must fix before merge

### P1-1. Fan-out expansion has no double-emission guard; sweep and completion path can emit duplicate branch sets
- **Area:** logic/tasks · **Confidence:** high · **Orchestrator spot-check:** confirmed
- **Location:** `src/tasks/store.rs:2057-2148` (`expand_placeholder`), `:2173-2348` (`emit_fan_out_branches`), `:1765` (sweep callsite), `src/agent/cortex.rs:4101` (completion callsite)
- **Evidence:** `expand_placeholder` reads the placeholder and emits branches without ever conditionally claiming it. `emit_fan_out_branches` reads parents/children/bindings BEFORE `BEGIN IMMEDIATE`, inserts branches with freshly allocated numbers, and finishes with `DELETE FROM tasks WHERE task_number = ?` whose `rows_affected` is never checked. Two callers genuinely race: `handle_detached_completion` calls `expand_fan_outs_for` while the per-agent tick calls `recompute_ready → expand_fan_outs`; both select the same backlog placeholder with `block_kind IS NULL`. The loser's transaction blocks on the write lock, then commits a full second set of branches (new task numbers, so `INSERT OR IGNORE` on edges does not dedupe), duplicate edges to every downstream child, and its placeholder DELETE silently affects 0 rows. Contrast with the loop path, which has both the conditional `SET loop_resolution … WHERE loop_resolution IS NULL` claim inside the tx and the unique index `idx_tasks_loop_iteration` — fan-out has neither. Downstream impact: both duplicate branches run (double model spend), and `resolve_fan_in` collapses same-branch_key duplicates, so the board shows and pays for two branches while fan-in reports one.
- **Recommendation:** Move the parent/child/binding reads inside the transaction; delete the placeholder first inside the tx and roll back + return `None` when `rows_affected() == 0`.
- **Verification:** Tokio test — create source (done, 3-item outputs) + placeholder, `tokio::join!(store.expand_fan_outs_for(src), store.expand_fan_outs(agent))`, assert 3 branches not 6. `cargo test --lib tasks::store`.

### P1-2. Template gates compile to ephemeral task numbers: fan-out source deadlocks its step; loop-body source decides the branch on pass 1 forever
- **Area:** logic/workflows · **Confidence:** high · **Orchestrator spot-check:** confirmed
- **Location:** `src/workflows/store.rs:1051-1092` (launch validation) and `:1531-1595` (translation)
- **Evidence:** Launch validation for gates checks only that the source step exists, is not the gated step itself, and that the config validates. The translation freezes `config.task_number` to the task the source step compiled into, and nothing ever rewrites it. (a) Fan-out source: the compiled task is the placeholder, which never runs and is DELETEd on expansion; `evaluate_task_output(config, None)` returns Pending forever and the derived disposition is Wait — the gated step is held permanently. The binding layer refuses exactly this shape (`StepBindingOnFanOut`); the gate layer has no equivalent. (b) Loop-body source: the gate reads iteration 1's task for the loop's whole life; `emit_loop_iteration` repoints `task_input_bindings` at the newest pass but contains no `task_gates` handling (confirmed: `task_gates` appears nowhere in the loop/fan-out emission code). Pass 1's terminal ends `done`, so the derived disposition is Route; a false predicate yields Pending, and `should_route(Route, Pending)` is true — the branch settles SKIPPED from pass-1 data even though pass 2 would satisfy it; a satisfied predicate releases the branch one pass early.
- **Recommendation:** Refuse both source shapes at launch (new `LaunchError` variants mirroring `StepBindingOnFanOut`), or rewire `task_gates.config.task_number` during loop iteration/fan-out emission. Refusal is the decided fix.
- **Verification:** (A) fan-out template + gate on the fan-out step → launch refuses with `GateOnFanOut`; (B) loop template + gate on a body step → refuses with `GateOnLoopBody`. `cargo test --lib workflows::store::tests`.

### P1-3. Claim path enforces neither awaiting-loop holds nor gates; unblock/execute/retry/task_update bypass both
- **Area:** logic/tasks + security (merged from LogicTasks T2 and SecurityReview SEC-4; escalated P2→P1 by the orchestrator because ordinary, non-adversarial API operations defeat the feature's core guarantee) · **Confidence:** high · **Orchestrator spot-check:** claim path confirmed first-hand
- **Location:** `src/tasks/store.rs:1173-1223` (`claim_next_ready`), `:1626-1652` (`unblock_task`), `src/api/tasks.rs:748-792` (execute), `:700-725` (retry), `:1362-1396` (create gate — no status check), `src/tools/task_update.rs:117-123`
- **Evidence:** `claim_next_ready` re-checks only `fan_out_placeholder` and unsettled parents — its own doc comment establishes "the sweep is not the only way into `ready`" — but there is no `awaiting_loop_group IS NULL` clause and no gate predicate. Gates are consulted only at promote time. But `ready` is reachable without the sweep: `unblock_task`, `execute_task` (forces Ready for backlog/blocked/done), `retry_task`, any PATCH using legal transitions, and the branch-scope `task_update` tool available to channel/cortex LLMs. Two concrete breaks: (a) after a loop boundary settles, `unblock_task` on an untaken-arm task lands it in ready and the next pickup claims it — the branch the loop ruled out runs; (b) a gate added to an already-ready task is never consulted before the task runs, and a later route-disposition poll can `skip_task` an in_progress task, orphaning its worker (completion then fails the `skipped→done` transition and the work is discarded).
- **Recommendation:** Add both predicates to claim's SELECT and conditional UPDATE — the single chokepoint every path funnels through.
- **Verification:** Tests: pending-gate task forced ready → claim returns None; satisfied gate → claimed; awaiting-loop task forced ready → None. `cargo test --lib tasks::store`.

### P1-4. Routing inference returns Anthropic defaults for non-Anthropic providers — LiteLLM-only boots are broken
- **Area:** logic/llm-config · **Confidence:** high · **Orchestrator spot-check:** confirmed end to end
- **Location:** `src/llm/routing.rs:37-41` (`Default` = `for_model("anthropic/claude-sonnet-4")`), `:168-174` (`defaults_for_provider` `_` arm), `src/config/providers.rs:24-46` (`infer_routing_from_providers`), `src/config/load.rs:671`, `src/config/onboarding.rs:257-269`
- **Evidence:** `defaults_for_provider` special-cases only `"anthropic"`; every other provider falls through to `RoutingConfig::default()` = `anthropic/claude-sonnet-4` for all five roles. So (a) env-only LiteLLM boots (`LITELLM_API_KEY` only — exactly the quickstart `litellm.mdx:99-102` documents) get routing pointed at a provider that does not exist; (b) any single custom provider hits the same arm; (c) the onboarding wizard's OpenAI-compatible path writes `[defaults.routing] channel = "anthropic/claude-sonnet-4"` into config.toml for a `litellm` provider — the `if routing.channel.is_empty()` placeholder guard is dead code because the returned routing is never empty. Since `[defaults.routing]` is then present, load skips inference and every LLM call fails `UnknownProvider("anthropic")` on a daemon that considers itself fully configured.
- **Recommendation:** `RoutingConfig::empty()` for non-Anthropic providers; empty routing → setup mode / explicit "no model configured" error, never a silent anthropic default. (Decided against guessing a LiteLLM model name — `defaults_for_provider`'s own doc says a guessed string "looks right and 404s on first use".)
- **Verification:** `SPACEBOT_DIR=$(mktemp -d) LITELLM_API_KEY=sk-test cargo run` → setup mode, not UnknownProvider; unit: `infer_routing_from_providers` with only `litellm` returns empty channel.

### P1-5. Worker wall-clock Timeout/Cancelled settles the task as Done, releasing the graph on incomplete work
- **Area:** logic/llm-config (cortex) · **Confidence:** high · **Orchestrator spot-check:** confirmed
- **Location:** `src/agent/cortex.rs:3726-3729` (`route_detached_outcome`), `:3964-3971` (Terminal→Done), `:4006+` (Done triggers `expand_fan_outs_for`/`advance_loops_for`)
- **Evidence:** `route_detached_outcome` maps `WorkerOutcome::Timeout` and `WorkerOutcome::Cancelled` → `DetachedRouting::Terminal`, and `handle_detached_completion` maps Terminal → `TaskStatus::Done`. Done then triggers fan-out expansion and loop advancement and releases downstream dependencies in the next sweep — a task whose worker exceeded the wall-clock default (or was cancelled) is treated as successfully completed: dependents promoted, loops turn over, fan-outs expand, no failure budget spent. The same enum's `Blocked` arm documents "Done would claim work that never happened" — the Terminal arm does exactly that. Contradicts the sibling supervisor idle-timeout path (requeue with budget, park after limit) and channel-owned workers (`map_worker_completion` classifies Timeout as unsuccessful).
- **Recommendation:** Timeout → `Requeue` (budget-spending, parks after limit); Cancelled → sticky `Blocked { reason: "cancelled…" }` (needs a person). Remove the `Terminal` variant.
- **Verification:** Unit: `route_detached_outcome(Timeout) == Requeue`, `Cancelled == Blocked`; repro with 5s wall-clock and a dependent task — dependent must not promote. `cargo test --lib agent::cortex`.

### P1-6. OpenAI-compatible (LiteLLM) usage is recorded twice per completion()
- **Area:** logic/llm-config · **Confidence:** high · **Orchestrator spot-check:** confirmed
- **Location:** `src/llm/model.rs:135-137` (`attempt_completion` OpenAiCompatible arm), `:671` (accumulator clone), `:786`/`:820` (`record_streaming_usage` in the generator), `:449-462` (`completion()` epilogue), `:1217-1238` (`collect_streaming_completion_response`)
- **Evidence:** For `ApiType::OpenAiCompatible`, `attempt_completion` builds the SSE stream and fully consumes it via `collect_streaming_completion_response`. The stream generator calls `record_streaming_usage(&stream_accumulator, …)` — a clone of the same shared `usage_accumulator` — immediately before yielding FinalResponse; consuming the stream adds usage once. `completion()`'s epilogue then unconditionally adds the same usage a second time from `response.raw_response.body`. `UsageAccumulator::add` has no dedup, so every worker/branch/cortex completion through LiteLLM records 2× input/output/cache tokens, 2× cost, and request_count += 2 in `token_usage`.
- **Recommendation:** Mark usage as already recorded on the collected response (`RawResponse.usage_recorded`); the epilogue skips when set.
- **Verification:** Mock OpenAI SSE server emitting a final usage chunk; one `completion()` → `request_count == 1` (currently 2).

---

## P2 — should fix

### Security

**P2-7. HTTP gates have no SSRF guardrail** — `src/tasks/gates.rs:243, 719-790`. `validate_config` checks only the `http(s)://` prefix; nothing blocks loopback, RFC1918, or link-local (e.g. `http://169.254.169.254/latest/meta-data/`). The instance polls the URL every 15s+ with config-chosen headers, follows 3 redirects, and persists the pointed-at value into `last_detail`, served by `GET /tasks/{n}/gates` and logged to cortex_events — a built-in exfil channel. Bounded by API-token-only creation (no worker/LLM tool can create gates), hence P2. Any future allowlist must be enforced per redirect hop. *Verify:* POST a gate at a metadata URL → accepted; observe recurring requests at a local listener.

**P2-8. `evaluate_http` buffers the entire gzip-decompressed body with no size cap** — `src/tasks/gates.rs:778`; reqwest `gzip` feature enabled. `response.json()` reads the full body; the 10s timeout bounds duration, not bytes; the poll repeats on its interval — stored, repeating memory-exhaustion DoS against the whole instance. Cap bytes (1–4 MiB accumulator or Content-Length pre-check) and treat overflow as Erroring. *Verify:* 10 MB gzip bomb endpoint; watch process RSS across two polls.

**P2-9. Gate configs carry plaintext credentials, stored and echoed verbatim** — `src/tasks/gates.rs:734-742, 191-210`; `src/api/tasks.rs:1331-1343`; workflow detail endpoint. Authorization/Private-Token header values sit in `task_gates.config` as plaintext TEXT and are returned in full by the API; `TaskGate` derives Serialize with no redaction. The codebase's own `secret:` indirection is unavailable to gates. Resolve `secret:` references at evaluation time and/or redact header values on serialization. *Verify:* create a gate with a token header; GET gates → token in plaintext.

**P2-10. `POST /providers/test-model` sends the stored API key to a caller-supplied base_url** — `src/api/providers.rs:396-490`. When `api_key` is empty the handler loads the stored provider key but honors the request's `base_url` — an API-token holder exfiltrates the real key to their own server, defeating the "never includes the API key" boundary (ProviderEntry doc, line 18) without filesystem access. When the key is stored-mode, ignore the request base_url. *Verify:* point test-model at a listener with empty api_key; observe the incoming Authorization header.

**P2-11. Worker-filed input bindings accept any task number — cross-agent output reads** — `src/tools/task_create.rs:336-355`; `src/agent/cortex.rs:5027-5031`. `input_bindings.source_task_number` has no ownership/run/tenant check; resolved outputs are embedded into the claiming worker's prompt. A prompt-injected worker on agent A files a card bound to agent B's task outputs and B's data lands in A's context — crossing the boundary `SecretScope::Agent` explicitly models. Restrict worker-filed binding sources to the same filing chain or workflow run. Confidence: medium. *Verify:* file a card bound to another agent's completed task; inspect the child worker's first message.

### Logic — tasks

**P2-12. Gate polling duplicated per agent; racing record_evaluation inflates consecutive_errors** — `src/agent/cortex.rs:4341-4342, 4627-4635`; `src/tasks/gates.rs:390-441`; `src/main.rs:3805`. `run_ready_task_loop` is spawned per agent and each loop polls, but `due_for_poll` is global with no claim/lease: N agents evaluate one due gate N times per tick, and each racer increments `consecutive_errors` — geometric backoff and `GATE_ERROR_LIMIT=5` trip ~N× early (a gate can stop after ~2 real failures in a 3-agent instance). *Verify:* two concurrent `poll_gates_once` over one erroring gate → `consecutive_errors == 2` after one round.

**P2-13. Fan-out width is unbounded** — `src/tasks/store.rs:4260-4333, 2205-2290`. One task per array element in a single transaction, no ceiling. Loops got `MAX_LOOP_ITERATIONS=25` with the explicit rationale that this is "the only path in the system that creates tasks because other tasks finished" — fan-out has the same property and no bound; a hallucinated 50k-element array materializes 50k tasks (each later a live model call) in one tick. *Verify:* expand a 10k-item source; count created tasks.

**P2-14. `block_task` unconditionally unsettles terminal tasks** — `src/tasks/store.rs:1592-1611`; `src/api/tasks.rs:986-1008` (no status check); `:2655-2667`. `POST /tasks/{n}/block` flips `done → backlog/blocked`, silently demoting its children on the next sweep while stale outputs remain readable by downstream bindings. In-tree, `exhaust_loop`'s comment claims the terminal "leaves done, which is the honest state" while `block_task` does the opposite — comment or code is wrong; children wired to that terminal wait forever. *Verify:* block a done task; observe status change and child demotion.

### Logic — workflows

**P2-15. Launch emission is non-atomic; a concurrent sweep can promote the whole pipeline before its edges exist** — `src/workflows/store.rs:1299-1370, 1443-1455`; promote query `src/tasks/store.rs:1826-1852`. `emit_graph` creates each task and stamps `workflow_run_id` in separate autocommitted UPDATEs, linking edges only afterwards; the promote query treats `workflow_run_id IS NOT NULL` with no unsettled parents as eligible (orchestrator-verified). A cortex tick in the window promotes every step at once; a binding-less mid-pipeline step runs out of order. `rollback_run` can also delete a task a racing sweep already claimed. Both stores share one pool — the `BEGIN IMMEDIATE` pattern used by loop/fan-out emission is available and not taken. Confidence: medium (timing-dependent). *Verify:* interleave-by-hand test — emit + stamp, then `recompute_ready`, assert non-entry steps promoted; then wrap emission in one transaction and assert the opposite.

**P2-16. Deleting a workflow cascade-deletes its run history** — `migrations/global/20260803000002_workflows.sql:101` (`workflow_runs.workflow_id … ON DELETE CASCADE`; FK enforcement pinned by `src/db.rs:134-146`). `WorkflowStore::delete_workflow` silently deletes every `workflow_runs` row, contradicting the delete handler's doc ("deleting the recipe never deletes the history of work that was done", `src/api/workflows.rs:436-441`) and making `GET /workflow-runs/{id}` 404 for every run of a deleted template. The hand-written test fixture declares no FKs, so no test can observe it (shared root cause with P2-29). *Verify:* against a migrated DB: delete a workflow, count its runs → 0.

**P2-17. Fan-out over a loop-body step is not refused; placeholder expands from pass 1 and is deleted** — `src/workflows/store.rs:1783-1790` (only `LoopStepIsAlsoFanOut` refused); `src/agent/cortex.rs:4101` then `:4118` (completion path expands fan-outs BEFORE advancing loops). The placeholder expands from pass-1 outputs immediately, is deleted, and the branches' `item` inputs forever contain pass-1 data; the fan-in collector aggregates results computed from a superseded pass. Refuse `for_each_step_key` naming a loop-body member at launch. Confidence: medium. *Verify:* loop template + for_each over the loop step; run pass 1; assert premature expansion.

### Logic — LLM/config/cortex

**P2-18. Usage accounting incomplete: no `include_usage` request; Anthropic streaming records nothing** — `src/llm/model.rs:1128-1131, 1162-1213`. (a) `with_streaming_enabled` sets only `stream: true`, never `stream_options: {include_usage: true}`, so OpenAI/LiteLLM streaming usually synthesizes zero-usage bodies; (b) the Anthropic `stream()` branch wraps `attempt_completion` in `stream_from_completion_response`, which has no `record_streaming_usage` and no epilogue — channels record no accumulator usage for Anthropic at all. Confidence: medium. *Verify:* one channel message; inspect `token_usage` rows → 0/absent.

**P2-19. Per-process thinking effort is dead config** — `src/llm/model.rs:497-499`; `src/llm/routing.rs:93-109`. `thinking_effort_for_model` compares full routing strings (`anthropic/claude-sonnet-4`) against the provider-stripped model name (`claude-sonnet-4`) — never matches; every request uses `"auto"` regardless of `channel_thinking_effort` etc. Orchestrator-verified both sides of the comparison. *Verify:* unit — set `channel_thinking_effort="low"`, query with the stripped name → still "auto".

**P2-20. `update_provider`/`delete_provider` skip `config_write_mutex`** — `src/api/providers.rs:296-380, 497-585`; contrast `src/api/agents.rs:657`, `src/api/config.rs:457`, `src/api/state.rs:253`. Read-modify-write race on config.toml loses whole writes (a provider add racing an agent-config update). *Verify:* concurrent POST /providers + POST /config in a loop; diff config.toml for lost keys.

**P2-21. `needs_onboarding` claims `PROVIDER_*` env vars configure a provider; nothing parses them** — `src/config/load.rs:571-582` vs `:633-665`. A deployment setting `PROVIDER_OPENAI_API_KEY` boots with zero providers, no onboarding, every LLM call failing — the exact silent-no-provider boot the retired-key handling was built to prevent. *Verify:* `PROVIDER_FOO_API_KEY=x cargo run` → no onboarding, no providers.

**P2-22. Supervisor-timeout path never closes the task_runs attempt row** — `src/agent/cortex.rs:5340-5480` vs `:4506-4523` (reaper) and `:3815-3827` (completion). The KILLING→timeout settlement updates task status without `finish_run`; every supervisor-timed-out attempt leaves a permanently 'running' row — phantom in-flight attempts in run history. *Verify:* trigger a supervisor timeout; `SELECT * FROM task_runs WHERE finished_at IS NULL`.

### Conformity

**P2-23. Silently discarded DB error in compensating delete** — `src/tasks/store.rs:729`. `let _ = self.delete(task_number).await;` in the depends_on rollback: if the delete fails, an orphaned committed task row exists with zero log trace. Explicit violation of AGENTS.md and RUST_STYLE_GUIDE ("No `let _ =` on Results… the only exception is `.ok()` on channel sends"). Downgraded from the lane's P1: convention violation with teeth, not a live correctness bug. Orchestrator verbatim-verified. *Fix:* log with `tracing::warn!`. *Verify:* `grep -n "let _ = " src/tasks/store.rs`.

**P2-24. `task_complete` tool description embedded as a string constant** — `src/tools/task_complete.rs:69`. All three sibling task tools load descriptions from `prompts/en/tools/*_description.md.j2` via `crate::prompts::text::get`; no `task_complete_description.md.j2` exists. Second convention beside an established one. *Verify:* add the prompt file; `grep prompts::text::get src/tools/task_complete.rs`.

### Docs

**P2-25. The task-DAG/workflow feature shipped with no user-facing doc update; tasks.mdx wholesale falsified** — `docs/content/docs/(features)/tasks.mdx` (per-agent stores, five columns, three task tools, no dependencies/gates/contracts/workflows — all falsified) and `README.md:113` ("A task moves through five states"; seven exist). The AGENTS.md docs rule was applied to the provider collapse but not to this feature. Merged CONF-3 + DOCS-7 + DOCS-8. *Verify:* `grep -cni "skipped\|depends_on\|task_complete\|max_retries" tasks.mdx` → 0.

**P2-26. Docker docs prescribe retired provider env vars** — `docs/content/docs/(getting-started)/docker.mdx:73-75`, `docs/docker.md:68-70` list `OPENAI_API_KEY`/`OPENROUTER_API_KEY`; `src/config/load.rs:215-237` retires them to warn-only. Following the docs boots a container with zero providers. Merged DOCS-1 + COMPAT-1. *Verify:* boot with only `OPENAI_API_KEY` → retired-var warning, setup mode.

**P2-27. Config/routing/roadmap doc falsifications (cluster)** —
- DOCS-2: documented sonnet/haiku per-process routing defaults don't exist — `RoutingConfig::default()` is sonnet for all five roles (`src/llm/routing.rs:37-66`) vs `config.mdx:29-30, 476-480`, `routing.mdx:24-28`.
- DOCS-3: `branch_timeout_secs` documented 60, code default 600 (`src/config/types.rs:1047` vs `config.mdx:100, 538`).
- DOCS-4: `config.mdx:202` claims a missing `secret:` ref falls back to env; provider api_key actually hard-fails config load (`src/config/load.rs:30-44, 837-840`). secrets.mdx contradicts config.mdx.
- DOCS-5: secrets migration no longer covers `[llm.provider.*].api_key` (`LlmConfig::secret_fields()` returns `&[]`); docs say it migrates "All [llm] provider keys".
- DOCS-6: secrets.mdx "No agent scoping" falsified by the new `SecretScope::{InstanceShared, Agent}` (`src/secrets/store.rs:53-58`; worker-created secrets are agent-scoped and hidden from the shared list endpoint).
- DOCS-9: roadmap.mdx claims "fallback chains across 14 providers (including Azure OpenAI)" — the built-in list is deleted; `azure` hard-errors.
- DOCS-10: routing.mdx instructs `azure/<deployment>` and built-in `openai/` configs that cannot resolve.

### Contracts

**P2-28. Deleted OAuth flows leave orphaned credentials and no runtime migration notice** — deleted `src/openai_auth.rs` / `src/github_copilot_auth.rs`. Orchestrator-confirmed via `git show main:` — ChatGPT credentials lived at `<instance_dir>/openai_chatgpt_oauth.json` (0600); nothing cleans it up, and a deployment whose routing references the deleted providers boots cleanly then fails every LLM call with `UnknownProvider` and no pointer to the removal (the retired-key hard-fail only catches config keys OAuth users never had). *Verify:* boot with routing referencing the old provider id; observe error quality.

### Tests

**P2-7 (tests lane T1 — top of P2). `evaluate_http` has zero coverage** — `src/tasks/gates.rs:719-789`. The one gate path touching the network, guardian of the erroring-never-routes invariant the code calls "the most important line in the feature". A `Failed`-instead-of-`Erroring` regression silently settles (skips) a branch on a DNS hiccup; none of the module's 20 tests would fail. Kept at P2 (a coverage gap is not itself a bug) but tops the list given what it guards. *Verify:* TcpListener-based tests — 503→Erroring, 404+expect 200→Pending, closed port→Erroring, 200+expect 200→Satisfied. `cargo test --lib tasks::gates`.

**P2-29. Migration backfills never run against pre-existing data; store tests use hand-written schemas** — `src/tasks/store.rs:4903`, `src/workflows/store.rs:3917` build schemas by hand instead of running `sqlx::migrate!("./migrations/global")`; the one real-migrator test (`src/db.rs:135`) runs on an empty DB where both backfills are no-ops. A wrong backfill (e.g. resurrecting human-parked cards) ships silently, and the fixtures can drift from the 11-migration reality. Shared root cause with P2-16. *Verify:* populate a pre-20260802-shape DB, run the migrator, assert backfill results.

**P2-30. Bound-directory task pickup wiring and its refusal path untested** — `src/agent/cortex.rs:5053-5145`; `src/tools/spawn_worker.rs:607` (no test module). A regression that falls back to the workspace on `with_working_dir` rejection, or resolves the wrong repo, runs worker shell/file tools in the wrong tree. *Verify:* unit-test `resolve_directory_from_project` + the pickup rejection path.

**P2-31. All new interface logic modules have zero coverage; the package has no test runner** — `taskTransitions.ts`, `dependencyGate.ts`, `taskGraphLayout.ts`, `loops.ts` (which duplicates `MAX_LOOP_ITERATIONS=25` from Rust — drift makes the canvas offer exactly the give-up edge the server refuses), `layout.ts`, `graph.ts`, `conditions.ts`, `schemaForm.ts`. `interface/package.json` has no test script; no `*.test.*` exists. These mirror backend invariants the Rust side tests heavily. *Verify:* add vitest; test `planStatusChange`, `dependencyRefusal`, layout cycle handling, loop-constant parity.

**P2-32. Worker-scope privileged-field refusal in task_update untested** — `src/tools/task_update.rs:160-171`. The changeset tests sibling refusal paths (`task_create.rs:649`, `task_complete.rs:300`) but not this one; deleting the guard lets a worker retitle/reprioritize/transition any task it can name. *Verify:* Worker-scope call with `status`/`title` set → assert error.

### Quality

**P2-33. Dead module `interface/src/api/client-typed.ts`** — 26-line openapi-fetch wrapper never wired in (the branch rewrote `client.ts` to consume generated types directly); exports a second global `setServerUrl` and the `paths` type a new feature would reach for first. *Verify:* `grep -rn client-typed interface/src` → only the file itself; delete and `bunx tsc --noEmit`.

**P2-34. `isRecord` triplicated** — `interface/src/components/workflows/schemaForm.ts:28-30` and `interface/src/components/tasks/TaskUtils.tsx:16-18` are byte-identical to the export this same branch added in `interface/src/lib/json.ts:10-12` precisely to be the shared narrowing helper. *Verify:* `grep -rn "function isRecord" interface/src` → 1 hit after fix.

**P2-35. `src/tasks/store.rs` is 8,216 lines** — ten subsystems under section banners; fan-out/loops/contracts are sibling-file-sized (`gates.rs` at 1,386 is the precedent). Medium confidence — judgment call, see Open Questions. *Verify:* `wc -l src/tasks/store.rs`; if split, `cargo test tasks::` stays green.

### UI

**P2-36. Worker-id button is a dead control in both real usages** — `interface/src/components/tasks/TaskRunHistory.tsx:135-141`; both call sites (`GlobalTasks.tsx:702-704`, `AgentTasks.tsx:419-421`) omit `onWorkerClick`. A link-styled button that does nothing. *Verify:* click it in the /tasks drawer — nothing happens.

**P2-37. Board card cannot be activated from the keyboard** — `interface/src/components/tasks/TaskBoard.tsx:399-412, 145-146`. `role="button"` div with no key handler, and dnd-kit's KeyboardSensor captures Enter/Space for drag — a keyboard-only user can drag a card but never open its drawer (the card's primary action) in board view. *Verify:* Tab to a card, press Enter → lifts into drag instead.

**P2-38. Focus outline removed on all React Flow nodes** — `interface/src/styles.css:246-249` vs `nodesFocusable` in both canvases (`WorkflowCanvas.tsx` ~633, `TaskGraphCanvas.tsx` ~184). Selection ≠ focus: an unselected focused node shows no indicator on three routes — WCAG 2.4.7 failure. *Verify:* Tab through /workflows/<id> canvas — no focus indicator.

---

## P3 — optional

### Security
- **SEC-7** `src/tasks/gates.rs:728,734-742` — Custom credential headers (GitLab `PRIVATE-TOKEN`, Jenkins-style `X-Api-Key`) survive cross-host redirects; reqwest strips only authorization/cookie families. Disable redirects or re-attach headers only on same-origin hops. (Behavior verified in reqwest source.)
- **SEC-8** `src/tasks/gates.rs:751,781-784` — Gate error details embed the full URL; `https://user:password@host/…` writes the password into `last_detail`, the board, and cortex_events. Strip userinfo before formatting.
- **SEC-9** `src/tasks/gates.rs:250-253,757-763` — `expect_status` is presence-checked but not type-checked; a string value is silently ignored at evaluation, and with no pointer the gate is then satisfied by any status. Validate integer 100..=599 at creation.
- **SEC-10** `src/api/providers.rs:355-357` (also onboarding.rs:249) — POST /providers writes api_key plaintext into config.toml instead of the secret store, bypassing at-rest encryption and the scrub registry; the same diff hardens exactly this elsewhere (`LEGACY_LLM_SECRET_NAMES`).
- **SEC-11** `src/config/load.rs:117-200`; `src/api/workflows.rs` `LaunchRequest.launched_by` — legacy token files left on disk with no cleanup path; `launched_by` is caller-asserted with no agent-registry check (admin-only surface).

### Logic — tasks
- **T6** `src/agent/cortex.rs:3983-3995` — Done tasks keep `worker_id` bound; a late duplicate output submission from the same worker id passes the ownership check and can overwrite outputs after downstream tasks consumed them.
- **T7** `src/tasks/store.rs:3446-3456` — `SourceHasNoOutputs` conflates "upstream still running" with "finished without outputs"; a healthy run gets false `stalled` logs every tick.
- **T8** `src/agent/cortex.rs:4505-4531` — Reaper can count a spurious failure against an in-flight completion (narrow window between `finish_run` and the status update). Confidence: medium.
- **T9** `src/tasks/store.rs:2567-2640, 1828-1849` — Untaken loop-arm tasks are held forever; their children can neither run nor skip-propagate — a pipeline joining below both arms deadlocks silently. Confidence: low (see Open Questions).
- **T10** `src/tasks/store.rs:668-778` — `create()` uses a deferred transaction (SQLITE_BUSY possible under concurrency) while its UNIQUE-retry branch (code 2067) is unreachable given in-transaction number allocation.
- **T11** `src/tasks/store.rs:3329-3338` — `resolve_fan_in` mixes all loop iterations of a step key (no `loop_iteration` filter); stale branch outputs contribute. Confidence: low.

### Logic — LLM/config
- **LLM-9** `src/config/onboarding.rs:262-302` — TOML written by unescaped string interpolation; a credential containing `"` or `\` produces a config that fails the next boot's parse.
- **LLM-10** `src/llm/usage.rs:131-141` — Cost-status conditional is dead (`if cost > 0.0 { Estimated } else { Estimated }`); `Unknown`/`Included` unreachable; every row labeled 'estimated' even when pricing is unknown.
- **LLM-11** `src/llm/model.rs:343-462,150-156` — Fallback successes are metered and priced under the primary model's name (fallback models built via bare `make` inherit neither accumulator nor context).
- **LLM-12** `src/llm/manager.rs:52-60` — `set_instance_dir` never stores the dir (dead, misleading; no callers). Delete or fix.
- **LLM-13** `src/llm/routing.rs:67-84` — `task_overrides` is dead config: every `resolve()` call passes `task_type: None`. Wire it through or drop the surface.
- **LLM-14** `src/api/providers.rs:497-585` — `delete_provider` leaves dangling routing entries (subsequent calls fail UnknownProvider), skips `refresh_defaults_config`, and env-bootstrapped providers silently reappear on next load while the delete reports success.
- **LLM-15** `src/api/providers.rs:230-280`; `src/llm/manager.rs:124-156` — Env-only boot: settings UI shows an empty provider list while env providers work; expired-token-on-refresh-failure yields a generic 401 instead of "re-login required". Confidence: medium.
- **LLM-16** `src/llm/manager.rs:158-177`; `src/llm/anthropic/auth.rs:97-101` — `use_bearer_auth=true` on a static anthropic provider defeats the OAuth override path (Bearer without the headers OAuth tokens require). Confidence: medium.

### Logic — workflows
- **W-P3-1** `src/workflows/store.rs:1752-1805` — `loop_until`/`loop_max_iterations` without `loop_group` (and `for_each_pointer`/`for_each_key` without `for_each_step_key`) are silently ignored — "a setting nothing reads is worse than a missing one", the failure the neighboring refusal exists to prevent.
- **W-P3-2** `src/workflows/store.rs:1503-1514` — A fan_in binding's `source_pointer` is accepted at save and silently dropped at launch; the collected map contains whole outputs, not the projection.
- **W-P3-3** `src/workflows/store.rs:1110-1145` — Plain `step` bindings get no "must wait" check (unlike fan_in/for_each); a healthy run is logged `stalled` every tick until the source lands.
- **W-P3-4** `src/api/workflows.rs:346-360, 421-432` — Workflow name uniqueness: create's 409 pre-check is racy (TOCTOU); update has no check and maps a duplicate to a 500.
- **W-P3-5** `src/api/workflows.rs:686-692, 1099-1115`; `src/workflows/store.rs:1967-1978, 1255-1266` — Stale error message (omits `previous_iteration`); cycle error lists downstream steps as if in the ring; empty `launched_by` accepted (run stalls silently); post-emit `get_run` failure leaves a live run (retry double-launches); `rollback_run` orphans dependency/binding rows.

### Quality
- **Q4** `src/api/workflows.rs:234-240` — `get_task_store` duplicated verbatim from `src/api/tasks.rs:263-269`; first duplicate of the same store's 503-semantics helper.
- **Q5** `src/tasks/gates.rs:879-884` — `gate_from_row_public` one-line alias; make `gate_from_row` itself `pub(crate)` instead.
- **Q6** `src/workflows/store.rs:2032-2038` vs `src/tasks/store.rs` — two `read_json`/`read_optional_json` helpers with divergent failure behavior (one warns on malformed JSON, one silently swallows).
- **Q7** `interface/src/components/ModelSelect.tsx:111-130` — `providerOrder` lists 19 pre-collapse vendor ids, including `openai-chatgpt`/`github-copilot` the backend can never produce again. (Deduped with UI-9's second half.)
- **Q8** `interface/src/api/server.rs` — 0-byte Rust file inside the TS tree; orchestrator confirmed via git that this branch added it. Delete.
- **Q9** `src/tasks/store.rs:1856-1861` — O(promoted × gated) linear scan plus per-hold clone in the per-tick sweep; build a HashMap once. Bounded in practice.
- **Q10** `interface/src/components/workflows/StepDetail.tsx` ~1800 — `body` names both the loop body prop and the binding-save request payload in one component.
- **Q11** `interface/src/components/tasks/TaskViewToggle.tsx` vs `workflows/ViewToggle.tsx` — near-identical persisted-choice toggles written in the same branch.

### UI
- **UI-4** `StepDetail.tsx:685-700` (also LaunchPanel.tsx:184-204, Workflows.tsx:271-274) — Form labels not programmatically associated with controls (no htmlFor/id) across the whole step editor; highest-multiplicity a11y gap in the diff.
- **UI-5** `GlobalTasks.tsx:526-536` + eight more error surfaces — Refusal/error messages never announced to screen readers (no role=alert/aria-live anywhere in the new UI).
- **UI-6** `TaskBoard.tsx:330,119-124` — Priority signaled by color only; empty columns render the dead-end copy "Nothing here."
- **UI-7** `GlobalTasks.tsx:547`, `AgentTasks.tsx:302`, `Settings.tsx:413-414,610-611,626-627`, `workflows/ViewToggle.tsx:71` — Hardcoded palette colors (`text-red-400`, `green-500`…) amid otherwise strict token discipline; clash under non-default themes.
- **UI-8** `designSystemTask.ts:47-50` — Unknown future statuses silently relabeled `pending_approval` in the drawer, contradicting the board's "Unrecognised status" honesty policy.
- **UI-9** `ModelSelect.tsx:130-196` — Model picker lacks combobox semantics and arrow-key navigation; no "no matches" empty state.
- **UI-10** `WorkflowCanvas.tsx:735-741` — Server edge-refusal bar is undismissable and can outlive its meaning (cleared only by edge mutations, not step edits).
- **UI-11** `WorkersPanel.tsx:431-434` (also PortalTimeline.tsx:248-250, PortalWorkerCard.tsx:41-43) — `copyText`'s failure boolean ignored; "Copied" shown even when the copy failed — reintroduces the exact lie the lib was written to remove.
- **UI-12** `GlobalTasks.tsx:185-196,544-560` — Active repo filter with zero matches renders an empty board with no explanation; toolbar count reports the unfiltered total.

### Tests
- **T6 (tests lane)** `src/api/tasks.rs`, `src/api/workflows.rs` — New HTTP layer has no tests; LaunchError/StoreError→status mapping unpinned (sibling API modules do have test modules). Low blast radius — the store refuses bad state regardless.
- **T7 (tests lane)** `src/tasks/gates.rs:196`; `src/tools/task_complete.rs:163-167` — Gate `MissingField` variants and the `NotAssigned` arm of task_complete untested; cosmetic-to-minor.

### Docs
- **DOCS-11** `config.mdx` On-Disk Layout — stale per-agent filenames after the instance-level move (`agents/main/data/spacebot.db` shown; actual per-agent `agent.db`, instance-level `data/spacebot.db`). Confidence: medium on attribution.
- **DOCS-12** `quickstart.mdx` CLI flags reference omits the auth, secrets, and skill subcommands. Confidence: low on attribution.
- **DOCS-13** `config.mdx:36,458` — "At least one provider is required" contradicts setup-mode boot (`load.rs:663-664`).
- **DOCS-14** `roadmap.mdx:8` — "Six messaging platforms" undercounts (Signal and Mattermost exist). Confidence: low on attribution.

### Conformity
- **CONF-4** `AGENTS.md` — Module map not extended for `workflows`, `tasks/gates`, `api/`, `task_complete` (the map was already stale; drift-continuation).

---

## Adjudications and dropped findings

- **COMPAT-3 dropped (false positive):** claimed `/api/status`'s `sandbox` field changed shape. Orchestrator check `git show main:src/api/system.rs` — no `sandbox` field existed; it is new, not reshaped. No break.
- **CONF-5 dropped:** `docs/pnpm-lock.yaml` predates the branch (last touched f4169c9, outside the range; absent from the diff). Pre-existing repo issue, not this branch's.
- **CONF-1 downgraded P1→P2** (see P2-23): convention violation with teeth, not a live correctness bug.
- **LogicTasks T2 + SecurityReview SEC-4 merged and escalated P2→P1** (see P1-3): both lanes described the same claim-path bypass; ordinary API operations defeat the gate guarantee.
- **Tests lane T1 kept at P2** (see P2-7) rather than the lane's P1: a coverage gap is not itself a bug, but it tops P2 because it guards a P1-class invariant.
- **Q7 / UI-9 second half deduped** (stale providerOrder list).
- **DOCS-1 ≡ COMPAT-1** and **CONF-3 ≡ DOCS-7 ≡ DOCS-8** merged (P2-26, P2-25).
- **Reviewer method note:** the conformity and contracts lanes had no git binary; their git-dependent claims were re-verified by the orchestrator directly: migrations are 11 additions and 0 modifications (`git diff --name-status -- migrations/` shows only `A`); deleted HTTP endpoints are `/providers/openai/browser-oauth/start`, `/providers/openai/browser-oauth/status`, `/providers/{provider}/config`; the deleted ChatGPT credential artifact is `<instance_dir>/openai_chatgpt_oauth.json`.

## Coverage

All 10 roster areas ran: security, logic (split into tasks / workflows / llm-config — justified by the 40k-line surface), conformity, quality, tests, contracts, docs, UI. None skipped. No unresolved contradictions beyond the adjudications above.

## Residual risk

Static review cannot see: actual race frequency of the fan-out/launch/gate-polling windows under real tick timing; runtime behavior of the provider collapse against a live LiteLLM proxy (streaming usage chunks, fallback metering); migration backfills against real production data; UI behavior under real browsers (all UI findings are code-read; dnd-kit/React Flow default behaviors are library defaults observed in the registered props). The untested `evaluate_http` (P2-7) is the largest blind spot relative to its blast radius.

## Open questions

1. **P2-35 (store.rs split):** 8,216 lines is a merge-conflict magnet, but the repo's "no many small files" rule is a real counterpressure. Owner's call.
2. **P1-4 fix shape:** empty-routing→setup-mode was chosen over guessing a LiteLLM model name (the code's own comments argue against guessing). If the product would rather ship a conventional default alias, that is a product decision.
3. **T9/T11 (low-confidence loop/fan-in corners):** should pipelines that join below both loop arms deadlock silently or settle the unneeded half?

---

## Appendix A — verified clean (no findings)

- **Security lane:** all SQL parameterized (the one dynamic `IN ({...})` is built from i64s); JSON pointers via serde_json RFC-6901; all new routes under the existing Bearer middleware; worker task tools enforce scope server-side (`task_complete` verifies worker↔task assignment; `task_update` worker scope rejects privileged fields; `task_create` enforces 10-card/3-hop filing limits and exposes no gate/system_prompt args); Anthropic OAuth flow intact (0600 perms, redacted status); retired config keys fail loudly with migration guidance; legacy secret names stay System-classified; sandbox honesty fix well-designed (three-state containment, `require_containment` opt-in, shell tool blocks LD_PRELOAD/BASH_ENV/NODE_OPTIONS at both layers, working_dir validated against the allowlist); no XSS sinks in the new UI (no dangerouslySetInnerHTML; UiLab correctly dev-gated with the import inside the branch; external links `rel=noopener noreferrer`); migrations additive with no sensitive defaults; jsonschema used via `validator_for` only (no remote $ref fetches).
- **Logic/tasks lane:** loop double-emission guard genuinely race-safe (conditional claim + unique index); `skip_task` is a conditional update excluding done/skipped; `record_failure` transactional with correct budget binding and rate-limit exclusion; `start_run` attempt allocation race-free, first attempt 1; `link_tasks` cycle detection inside BEGIN IMMEDIATE; gate truth table sound (erroring never routes; satisfied latches; backoff math correct; both timestamp shapes parsed); failure-budget reset semantics correct; recurrence limiter survives promotion; SETTLED_STATUSES centralized and used consistently; emission paths single-transaction.
- **Logic/workflows lane:** loop_wiring refusal set correct for covered classes; Kahn cycle detection; reachability check; normal/on_exhausted arm wiring (both arms held at launch, released at the boundary in one transaction); iteration emission (claim-before-allocate, unique index second guard, edges moved + bindings repointed atomically, downstream reads newest pass); `loop_until` evaluated by the same evaluator as gates; run-input freezing; jsonschema crate validation; exhaustive LaunchError→status mapping; duplicate branch keys refused; fan-in Pending-vs-Problem distinction; delete_step cascade both directions; save-time cycle/reference checks in the API.
- **Logic/llm lane:** retired-key hard-fail with guidance; legacy api_type migration incl. /v1 fixup; provider-id normalization consistent; task metadata deep-merge safe; reaper (300s grace, registry-liveness, FailureDisposition guards, closes abandoned runs, budget escrow) with tests; detached completion-vs-kill race is single-winner via lifecycle CAS; `with_working_dir` canonicalizes and enforces the sandbox allowlist with direct tests; gate-poll ordering (reap → gates → sweep → claim) coherent; secrets store set before Config::load so `secret:` refs resolve on first load.
- **Conformity lane:** migrations additions-only (the redundant index was fixed via a NEW migration, textbook immutability); no mod.rs; no new #[async_trait]; no embedded prompt strings in new Rust (except P2-24); the only `let _ =` violation is P2-23; event-send discards match the repo's documented idiom; no new abbreviations; bun-only JS tooling; check-api-types.sh correctly wired into gate-pr.sh before fmt/check; AGENTS.md process anti-patterns respected (gate polling in cortex not channel; workers get fresh prompts; compactor untouched/programmatic).
- **Quality lane:** `validate_step_gate` genuinely shares the task-level validator; row mapping centralized; provider collapse left no dead Rust (ApiType exactly 2 variants; zero references to deleted auth modules); all suspicious interface exports have real consumers; gate polling is sequential with no per-tick JSON re-parse smell.
- **Tests lane:** ~120 inline tests defend the gate truth table (all 8 disposition×result combos), dependency block/unblock + recurrence escalation, failure-budget semantics, contract enforcement at sweep/completion, fan-out (duplicate keys, partial-expansion rollback, empty collections), loop wiring (every refusal + double-emission guard), topological cycles, launch refusals, the cortex reaper, and the provider/config collapse. No isolation hazards; every store test builds its own in-memory pool.
- **Contracts lane:** migrations additive-only, every new NOT NULL column has a safe default; DROP INDEX is IF EXISTS and correctly a new migration; backfills idempotent and cannot resurrect sticky blocks; task_number keying consistent across new tables; new enums parse only from brand-new tables; retired-key/api_type/env-var compat deliberate and complete; no in-repo consumer breaks (CLI only calls unchanged endpoints; packages/api-client re-exports the generated schema); SSE ApiEvent additive-only.
- **Docs lane:** litellm.mdx fully accurate; config.mdx provider-collapse specifics match the code row-for-row; secrets.mdx auto-categorization lists match; README Task Board mechanics verified; workflow-branching.md matches the implementation precisely; the five future-work design docs explicitly frame their subjects as missing; worker.md.j2 names only real tools with real semantics.
- **UI lane (positives):** UiLab dev-gating correct and complete; illegal status moves well-guarded (dimmed columns, named-parent refusals, unblock-vs-requeue handled); blocked/skipped communication exemplary (banners name cause and way out); edge kinds distinguished by color+dash+caption (not color-only); loop bodies drawn as labeled regions, never as branches; every new route has loading/error/empty states; token discipline strict in new components.
