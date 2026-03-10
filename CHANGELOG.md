# Changelog

Seeded from GitHub releases; maintained by the release bump workflow.

## v0.3.2

### Release Story

v0.3.2 tightens channel reliability and release ergonomics without changing the core architecture. Hosted deployments can now manage the SSH daemon through a dedicated API, and operators get stronger visibility through prompt inspection snapshots and richer live status context.

This patch also closes several turn-handling edge cases that could cause surprising behavior in busy channels. `require_mention` now falls through to the default agent instead of dropping messages, `skip` terminates the channel turn immediately (including error paths), and history backfill is treated as read-only archival context in the system prompt so older transcript text is not actioned as fresh instructions.

Highlights:
- Hosted SSH daemon management API for safer remote operations
- Prompt inspect snapshots plus expanded system/status introspection
- Safer channel turn flow and backfill handling across resumed sessions

## What's Changed
* Enable metrics in Docker build, add comprehensive Prometheus instrumentation by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/274
* Add SSH daemon management API by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/369
* fix: terminate channel turn immediately after skip tool call by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/373
* feat: system config self-awareness, prompt inspect API, release version check by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/374
* fix: require_mention falls through to default agent instead of dropping message by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/376
* fix: move backfill history from chat history to system prompt by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/377


**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.3.0...v0.3.2
## v0.3.0

### Release Story

Agents now understand the codebases they work in. Projects bring repo discovery, worktree tracking, disk usage reporting, and automatic sandbox allowlist injection — so agents have spatial awareness of what they're operating on.

Browser tools were rewritten from scratch: 12 focused tools built on CDP's Accessibility API replacing DOM extraction, with persistent sessions that survive worker restarts. OpenCode is embedded directly in the worker detail view via Shadow DOM, with idle worker persistence and automatic reconnection on restart.

The cortex is now a real supervisor. It monitors worker health, detects stalls, manages detached worker lifecycles, and gained self-awareness through embedded docs and live config diagnostics. Workers got an outcome gate that prevents text-only exits without signaling completion, context injection for receiving relevant conversation history, and duplicate spawn prevention.

The Agent Factory adds a preset system and cortex-driven creation flow for spinning up new agents with dedicated identity files (SOUL.md, IDENTITY.md, ROLE.md) and scoped tool access. Native OpenAI SSE streaming and token-by-token webchat delivery landed alongside a full config.rs refactor into focused submodules with proper hot-reload.

Other additions: listen-only mode with cross-platform invoke handling, channel attachment persistence with recall, temporal filtering for channel_recall, wall-clock cron support, theme switcher, shell/exec tool consolidation, and MCP auth header fixes with rmcp 1.1.

## What's Changed
* fix: send BranchResult on failure to prevent channel orphaning by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/279
* fix: surface upstream provider details in proxy API errors by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/278
* fix: resolve test isolation failures in config tests by @vsumner in https://github.com/spacedriveapp/spacebot/pull/293
* fix: skip workspace path enforcement on file/send_file when sandbox is disabled by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/295
* fix: stop worker secret scans from killing internal work by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/297
* Fix ProcessType routing for Cortex and Compactor by @artlu99 in https://github.com/spacedriveapp/spacebot/pull/290
* api: support channel archiving and channel list filters by @txchen in https://github.com/spacedriveapp/spacebot/pull/291
* fix: block bracketed fallback and bare skip output by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/303
* fix: hot-reload opencode settings in active agents by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/302
* fix: warn on unrecognised top-level config keys (closes #221) by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/277
* Fix OpenRouter app attribution: send X-Title header by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/275
* Fix UTF-8 boundary panic in cortex_chat and discord reply truncation by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/276
* refactor: extract config.rs into focused submodules by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/306
* fix: harden OpenAI-compatible parsing for OpenRouter responses by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/304
* feat: rig 0.31 upgrade, tool nudging, and invariant harness by @vsumner in https://github.com/spacedriveapp/spacebot/pull/292
* feat(cortex): add self-awareness docs and config diagnostics by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/311
* fix(llm): implement native OpenAI-compatible SSE streaming by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/313
* fix(prompts): send generated files as attachments by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/321
* fix(interface): replace crypto.randomUUID with secure-context-safe IDs by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/322
* fix(docker): add missing files for self_awareness.rs embeds by @toanalien in https://github.com/spacedriveapp/spacebot/pull/316
* feat: temporal filtering for channel_recall + self-channel recall by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/284
* feat: persistent browser sessions with configurable close policy by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/309
* fix: let workers/branches continue when secrets detected in tool output by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/323
* fix: bump rmcp to 0.17 to fix 401 on MCP SSE streams by @islerfab in https://github.com/spacedriveapp/spacebot/pull/300
* feat(interface): add theme switcher to settings by @toanalien in https://github.com/spacedriveapp/spacebot/pull/317
* fix: support plaintext IMAP/SMTP for local mail bridges by @spacebot-fcc in https://github.com/spacedriveapp/spacebot/pull/299
* feat: listen-only mode toggle + cross-platform invoke handling by @deanfluencebot in https://github.com/spacedriveapp/spacebot/pull/288
* feat(cortex): Implement Phase 1 ("The Tick Loop") by @vsumner in https://github.com/spacedriveapp/spacebot/pull/301
* cortex: implement phase 2 health supervision by @vsumner in https://github.com/spacedriveapp/spacebot/pull/305
* fix(worker): gate text-only exits on explicit outcome signal by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/327
* fix(channel): use worker_handles as guard for WorkerComplete events by @ciaranashton in https://github.com/spacedriveapp/spacebot/pull/331
* fix(cortex): use idle time instead of lifetime for worker timeout by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/332
* docs: sync rig usage/design docs with implementation by @vsumner in https://github.com/spacedriveapp/spacebot/pull/329
* fix(channel): stop re-summarising worker results that were already relayed by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/333
* feat: embed OpenCode web UI in worker detail view by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/314
* fix: persist transcripts on idle + reconnect idle workers on restart by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/334
* fix: prevent long text overflow in agent chat message bubbles by @toanalien in https://github.com/spacedriveapp/spacebot/pull/336
* Embed OpenCode UI via Shadow DOM and improve worker interactivity by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/343
* misc: UI improvements and docs updates by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/345
* test: harden tool_nudge assertions after UBS scan by @vsumner in https://github.com/spacedriveapp/spacebot/pull/328
* fix: browser session persistence across workers and restarts by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/348
* feat: persist channel attachments to disk with recall tool by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/350
* feat: Projects — workspace layout awareness for agents by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/346
* refactor: rewrite browser tools — 12 flat tools, CDP accessibility API, secure credential entry by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/352
* fix: browser context management — dedup snapshots, consecutive loop guard, overflow resilience by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/359
* refactor: merge exec tool into shell, add per-command env vars by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/360
* fix: fork before keychain access to prevent SIGBUS on macOS daemon start by @chanyeinthaw in https://github.com/spacedriveapp/spacebot/pull/353
* feat: wall-clock cron support in UI, anchor interval crons to last execution on restart by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/361
* feat: support single-repo projects and enhance skills UI by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/362
* feat: worker context injection and duplicate worker spawn prevention by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/358
* fix(mcp): fix auth header handling and bump rmcp to 1.1 for stateless HTTP support by @ciaranashton in https://github.com/spacedriveapp/spacebot/pull/330
* feat: Agent Factory — preset system, factory tools, and cortex integration by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/315

## New Contributors
* @artlu99 made their first contribution in https://github.com/spacedriveapp/spacebot/pull/290
* @txchen made their first contribution in https://github.com/spacedriveapp/spacebot/pull/291
* @toanalien made their first contribution in https://github.com/spacedriveapp/spacebot/pull/316
* @islerfab made their first contribution in https://github.com/spacedriveapp/spacebot/pull/300
* @spacebot-fcc made their first contribution in https://github.com/spacedriveapp/spacebot/pull/299
* @deanfluencebot made their first contribution in https://github.com/spacedriveapp/spacebot/pull/288
* @ciaranashton made their first contribution in https://github.com/spacedriveapp/spacebot/pull/331
* @chanyeinthaw made their first contribution in https://github.com/spacedriveapp/spacebot/pull/353

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.2.2...v0.3.0
## v0.2.2

- Tag: `v0.2.2`
- Published: 2026-03-01T13:00:02Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.2.2

## What's Changed
* fix: infer default routing from configured provider by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/266
* Sandbox hardening: dynamic mode, env sanitization, leak detection by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/259
* Secret store: credential isolation, encryption at rest, output scrubbing by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/260
* feat: auto-download Chrome via fetcher, unify Docker image, fix singleton lock by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/268
* fix: preserve conversation history and improve worker retrigger reliability by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/270
* Add interface CI workflow by @marijnvdwerf in https://github.com/spacedriveapp/spacebot/pull/267
* Split channel.rs and standardize adapter metadata keys by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/271
* fix: allow trustd mach service in macOS sandbox for TLS cert verification by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/272
* feat: add OpenRouter app attribution headers by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/264
* feat: implement link channels as task delegation (v3) by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/255


**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.2.1...v0.2.2

## v0.2.1

- Tag: `v0.2.1`
- Published: 2026-02-27T11:54:42Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.2.1

## What's Changed
* Improve task UI overflow handling and docker update rollback by @fyzz-dev in https://github.com/spacedriveapp/spacebot/pull/237
* fix Anthropic empty text blocks in retrigger flow by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/243
* Fix: Cleanup twitch_token.json when disconnecting Twitch platform by @Nebhay in https://github.com/spacedriveapp/spacebot/pull/212
* feat: add email messaging adapter and setup docs by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/244
* fix: match installed skills by source repo, not just name by @mwmdev in https://github.com/spacedriveapp/spacebot/pull/205
* chore: add delivery gates and repo-local pr-gates skill by @vsumner in https://github.com/spacedriveapp/spacebot/pull/238
* feat(channel): add deterministic temporal context by @vsumner in https://github.com/spacedriveapp/spacebot/pull/239
* feat: add IMAP email_search tool for branch read-back by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/246
* feat(cron): add strict wall-clock schedule support by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/247
* feat: named messaging adapter instances by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/249
* fix: log cross-channel messages to destination channel history by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/252
* add DeepWiki badge to README by @devabdultech in https://github.com/spacedriveapp/spacebot/pull/251
* fix(cortex): harden startup warmup and bulletin coordination by @vsumner in https://github.com/spacedriveapp/spacebot/pull/248
* feat: Download images as bytes for interpretation in Slack/Discord etc and fix Slack file ingestion by @egenvall in https://github.com/spacedriveapp/spacebot/pull/159
* fix: make Ollama provider testable from settings UI by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/253

## New Contributors
* @fyzz-dev made their first contribution in https://github.com/spacedriveapp/spacebot/pull/237
* @devabdultech made their first contribution in https://github.com/spacedriveapp/spacebot/pull/251

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.2.0...v0.2.1

## v0.2.0

- Tag: `v0.2.0`
- Published: 2026-02-26T08:16:48Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.2.0

## v0.2.0 is the _biggest_ Spacebot release yet. 

The agent is no longer a single-channel chatbot, it's a multi-agent system with real orchestration primitives.

<img width="1074" height="655" alt="Screenshot_2026-02-23_at_12 48 29_PM copy" src="https://github.com/user-attachments/assets/04b4e72d-c646-4900-8096-eec7e5222ed6" />

Agents coordinate through a spec-driven task system with a full kanban board in the UI. Tasks are structured markdown documents with requirements, constraints, and acceptance criteria. The cortex background loop picks up ready tasks, spawns workers, and handles completion or re-queuing on failure. Agents see the shared task board through the bulletin system, so delegation happens through specs, not conversation.

Workers got a complete visibility overhaul. Full transcript persistence with gzip compression, live SSE streaming of tool calls as they happen, and a new worker_inspect tool so branches can verify what a worker actually did instead of trusting a one-line summary.

On the security front, the old string-based command filtering (215+ lines of whack-a-mole regex) has been replaced with kernel-enforced filesystem sandboxing via bubblewrap on Linux and sandbox-exec on macOS. The LLM can't write outside the workspace because the OS won't let it.

This release also brings OpenAI and Anthropic subscription auth support, better channel history preservation with deterministic retrigger handling, structured text payload blocking to keep raw JSON/XML out of user-facing messages, self-hosted update controls in the settings UI, new provider support (Kilo Gateway, OpenCode Go), prebuilt Linux binaries for amd64/arm64, a Nix flake, and a pile of fixes across cron scheduling, OAuth, model routing, and more.

## What's Changed
* feat(nix): add Nix flake for building and deploying Spacebot by @skulldogged in https://github.com/spacedriveapp/spacebot/pull/47
* Fix chatgpt oauth by @marijnvdwerf in https://github.com/spacedriveapp/spacebot/pull/187
* Multi-agent communication graph by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/150
* Process sandbox: kernel-enforced filesystem containment for shell/exec by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/188
* Workers tab: full transcript viewer, live SSE streaming, introspection tool by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/192
* add settings update controls and harden self-hosted update flow by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/207
* feat(web): ui/ux cleanup by @skulldogged in https://github.com/spacedriveapp/spacebot/pull/143
* feat(ci): publish binaries for linux/amd64 and linux/arm64 on release by @morgaesis in https://github.com/spacedriveapp/spacebot/pull/94
* block structured text payloads from user replies by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/209
* Fix Z.AI Coding Plan model routing by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/210
* fix: use Bearer auth when key comes from ANTHROPIC_AUTH_TOKEN by @worldofgeese in https://github.com/spacedriveapp/spacebot/pull/196
* fix: use Bearer auth for ANTHROPIC_AUTH_TOKEN and add ANTHROPIC_MODEL by @worldofgeese in https://github.com/spacedriveapp/spacebot/pull/197
* Task tracking system with kanban UI and spec-driven delegation by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/227
* fix: make background result retriggers deterministic by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/231
* fix: guide users to enable device code login for ChatGPT OAuth by @mwmdev in https://github.com/spacedriveapp/spacebot/pull/214
* fix(cron): make cron scheduler reliable under load and in containers by @mmmeff in https://github.com/spacedriveapp/spacebot/pull/186
* Fix Z.AI coding-plan model remap for GLM-5 by @vsumner in https://github.com/spacedriveapp/spacebot/pull/223
* fix: Default cron delivery target to current conversation by @jaaneh in https://github.com/spacedriveapp/spacebot/pull/213
* feat(llm): add Kilo Gateway and OpenCode Go provider support by @skulldogged in https://github.com/spacedriveapp/spacebot/pull/225

## New Contributors
* @morgaesis made their first contribution in https://github.com/spacedriveapp/spacebot/pull/94
* @worldofgeese made their first contribution in https://github.com/spacedriveapp/spacebot/pull/196
* @mwmdev made their first contribution in https://github.com/spacedriveapp/spacebot/pull/214
* @mmmeff made their first contribution in https://github.com/spacedriveapp/spacebot/pull/186
* @jaaneh made their first contribution in https://github.com/spacedriveapp/spacebot/pull/213

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.15...v0.2.0

## v0.1.15

- Tag: `v0.1.15`
- Published: 2026-02-24T01:37:52Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.15

## What's Changed
* fix: resolve pre-existing CI failures (clippy, fmt, test) by @Marenz in https://github.com/spacedriveapp/spacebot/pull/174
* fix: wire up ollama_base_url shorthand in config by @Marenz in https://github.com/spacedriveapp/spacebot/pull/175
* fix: return synthetic empty text on Anthropic empty content response by @Marenz in https://github.com/spacedriveapp/spacebot/pull/171
* fix: accept string values for timeout_seconds from LLMs by @Marenz in https://github.com/spacedriveapp/spacebot/pull/169
* feat(telegram): use send_audio for audio MIME types by @Marenz in https://github.com/spacedriveapp/spacebot/pull/170
* feat(skills): workers discover skills on demand via read_skill tool by @Marenz in https://github.com/spacedriveapp/spacebot/pull/172
* fix: avoid panic on multibyte char boundary in log message truncation by @Marenz in https://github.com/spacedriveapp/spacebot/pull/176
* ChatGPT OAuth browser flow + provider split by @marijnvdwerf in https://github.com/spacedriveapp/spacebot/pull/157
* Fix worker completion results not reaching users by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/182
* Default MiniMax to M2.5 and enable reasoning by @hotzen in https://github.com/spacedriveapp/spacebot/pull/180
* fix: register groq/together/xai/mistral/deepseek providers from shorthand config keys by @Marenz in https://github.com/spacedriveapp/spacebot/pull/179
* Bugfix: Update dependencies for Slack TLS by @egenvall in https://github.com/spacedriveapp/spacebot/pull/165
* Add warmup readiness contract and dispatch safeguards by @vsumner in https://github.com/spacedriveapp/spacebot/pull/181
* fix(slack): Slack channel fixes, DM filtering, emoji sanitization, and restore TLS on websocket by @sra in https://github.com/spacedriveapp/spacebot/pull/148

## New Contributors
* @vsumner made their first contribution in https://github.com/spacedriveapp/spacebot/pull/181
* @sra made their first contribution in https://github.com/spacedriveapp/spacebot/pull/148

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.14...v0.1.15

## v0.1.14

- Tag: `v0.1.14`
- Published: 2026-02-23T00:19:45Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.14

## What's Changed
* feat(mcp): add retry/backoff and CRUD API by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/109
* feat(ux): add drag-and-drop sorting for agents in sidebar by @MakerDZ in https://github.com/spacedriveapp/spacebot/pull/113
* fix(channel): roll back history on PromptCancelled to prevent poisoned turns by @Marenz in https://github.com/spacedriveapp/spacebot/pull/114
* Fix CI failures: rustfmt, clippy, and flaky test by @Marenz in https://github.com/spacedriveapp/spacebot/pull/116
* fix(channel): prevent bot spamming from retrigger cascades by @PyRo1121 in https://github.com/spacedriveapp/spacebot/pull/115
* feat(security): add auth middleware, SSRF protection, shell hardening, and encrypted secrets by @PyRo1121 in https://github.com/spacedriveapp/spacebot/pull/117
* remove obsolete plan document from #58 by @hotzen in https://github.com/spacedriveapp/spacebot/pull/142
* fix(build): restore compile after security middleware + URL validation changes by @bilawalriaz in https://github.com/spacedriveapp/spacebot/pull/125
* fix(telegram): render markdown as Telegram HTML with safe, telegram-only fallbacks by @bilawalriaz in https://github.com/spacedriveapp/spacebot/pull/126
* Fix Fireworks by @Nebhay in https://github.com/spacedriveapp/spacebot/pull/91
* fix: harden 13 security vulnerabilities (phase 2) by @PyRo1121 in https://github.com/spacedriveapp/spacebot/pull/119
* fix: replace .expect()/.unwrap() with proper error propagation in production code by @PyRo1121 in https://github.com/spacedriveapp/spacebot/pull/122
* feat(twitch): Add Twitch token refresh by @Nebhay in https://github.com/spacedriveapp/spacebot/pull/144
* feat: add minimax-cn provider for CN users by @shuuul in https://github.com/spacedriveapp/spacebot/pull/140
* feat(telemetry): complete metrics instrumentation with cost tracking and per-agent context by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/102
* feat: support ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN and SPACEBOT_MODEL env vars by @adryserage in https://github.com/spacedriveapp/spacebot/pull/135
* Fix cron timezone resolution and delete drift by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/149
* feat: update Gemini model support with latest Google models by @adryserage in https://github.com/spacedriveapp/spacebot/pull/134
* feat(web): add favicon files and update HTML to include them by @the-snesler in https://github.com/spacedriveapp/spacebot/pull/154
* fix: add API body size limits and memory content validation by @PyRo1121 in https://github.com/spacedriveapp/spacebot/pull/123

## New Contributors
* @PyRo1121 made their first contribution in https://github.com/spacedriveapp/spacebot/pull/115
* @hotzen made their first contribution in https://github.com/spacedriveapp/spacebot/pull/142
* @bilawalriaz made their first contribution in https://github.com/spacedriveapp/spacebot/pull/125
* @shuuul made their first contribution in https://github.com/spacedriveapp/spacebot/pull/140
* @adryserage made their first contribution in https://github.com/spacedriveapp/spacebot/pull/135
* @the-snesler made their first contribution in https://github.com/spacedriveapp/spacebot/pull/154

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.13...v0.1.14

## v0.1.13

- Tag: `v0.1.13`
- Published: 2026-02-21T23:00:59Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.13

## What's Changed
* Improve channel reply flow and Discord binding behavior by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/95
* feat(messaging): unify cross-channel delivery target resolution by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/97
* feat: add dedicated voice model routing and attachment transcription by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/98
* Fix all warnings and clippy lints by @Marenz in https://github.com/spacedriveapp/spacebot/pull/87
* Add CI workflow (check, clippy, fmt, test) by @Marenz in https://github.com/spacedriveapp/spacebot/pull/101
* Add native poll support to telegram adapter by @Marenz in https://github.com/spacedriveapp/spacebot/pull/93
* docs(agents): update existing documentation when adding features by @Marenz in https://github.com/spacedriveapp/spacebot/pull/106
* feat(llm): add Google Gemini API provider support by @MakerDZ in https://github.com/spacedriveapp/spacebot/pull/111
* prompts: add missing memory-type guidance in memory flows by @marijnvdwerf in https://github.com/spacedriveapp/spacebot/pull/112
* add mcp client support for workers by @nexxeln in https://github.com/spacedriveapp/spacebot/pull/103
* Avoid requiring static API key for OAuth Login by @egenvall in https://github.com/spacedriveapp/spacebot/pull/100

## New Contributors
* @MakerDZ made their first contribution in https://github.com/spacedriveapp/spacebot/pull/111
* @marijnvdwerf made their first contribution in https://github.com/spacedriveapp/spacebot/pull/112
* @nexxeln made their first contribution in https://github.com/spacedriveapp/spacebot/pull/103

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.12...v0.1.13

## v0.1.12

- Tag: `v0.1.12`
- Published: 2026-02-21T02:15:01Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.12

## Note about v0.1.11

v0.1.11 was removed due to a bad migration, and its tag/release were deleted.
v0.1.12 includes those intended changes plus additional fixes.

## Highlights included from the missing v0.1.11 window

- Added hosted agent limit functionality
- Added backup export and restore endpoints
- Added storage status endpoint and filesystem usage reporting
- Improved release tagging/version bump workflow (including Cargo.lock handling)

## What's Changed
* fix: register NVIDIA provider and base URL by @Nebhay in https://github.com/spacedriveapp/spacebot/pull/82
* fix: Portal Chat isolation by @jnyecode in https://github.com/spacedriveapp/spacebot/pull/80
* Nudge previously rejected DM users when added to allow list by @Marenz in https://github.com/spacedriveapp/spacebot/pull/78
* Telegram adapter fixes: attachments, reply-to, and retry on startup by @Marenz in https://github.com/spacedriveapp/spacebot/pull/77
* Anthropic OAuth authentication with PKCE and auto-refresh by @Marenz in https://github.com/spacedriveapp/spacebot/pull/76
* fix: Prevent duplicate message replies by differentiating skip and replied flags by @thesammykins in https://github.com/spacedriveapp/spacebot/pull/69
* fix(cron): prevent timer leak and improve scheduler reliability by @michaelbship in https://github.com/spacedriveapp/spacebot/pull/81
* feat(cron): add configurable timeout_secs per cron job by @michaelbship in https://github.com/spacedriveapp/spacebot/pull/83
* feat: add mention-gated Discord bindings and one-time cron jobs by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/88
* docs: update README for new features since last update by @Marenz in https://github.com/spacedriveapp/spacebot/pull/92

## New Contributors
* @Nebhay made their first contribution in https://github.com/spacedriveapp/spacebot/pull/82
* @michaelbship made their first contribution in https://github.com/spacedriveapp/spacebot/pull/81

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.10...v0.1.12

## v0.1.10

- Tag: `v0.1.10`
- Published: 2026-02-20T08:45:11Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.10

## What's Changed
* feat: Add Z.AI Coding Plan provider by @thesammykins in https://github.com/spacedriveapp/spacebot/pull/67
* chore: optimize release profile to reduce binary size by @thesammykins in https://github.com/spacedriveapp/spacebot/pull/70
* feat(slack): cache user identities and resolve channel names by @jamiepine in https://github.com/spacedriveapp/spacebot/pull/71

## New Contributors
* @jamiepine made their first contribution in https://github.com/spacedriveapp/spacebot/pull/71

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.9...v0.1.10

## v0.1.9

- Tag: `v0.1.9`
- Published: 2026-02-20T04:25:44Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.9

## What's Changed
* add local ollama provider by @mmattbtw in https://github.com/spacedriveapp/spacebot/pull/18
* fix(docs): add favicon, fix theme toggle, and resolve og:image localhost issue by @andrasbacsai in https://github.com/spacedriveapp/spacebot/pull/29
* feat(llm): add NVIDIA NIM provider support by @skulldogged in https://github.com/spacedriveapp/spacebot/pull/46
* Update slack connector to include additional sender metadata by @ACPixel in https://github.com/spacedriveapp/spacebot/pull/43
* feat(telemetry): add Prometheus metrics with feature-gated instrumentation by @l33t0 in https://github.com/spacedriveapp/spacebot/pull/35
* fix(ingestion): do not delete ingest files when chunk processing fails by @sookochoff in https://github.com/spacedriveapp/spacebot/pull/57
* feat: Improve Slack Markdown by @egenvall in https://github.com/spacedriveapp/spacebot/pull/52
* fix: key Discord typing indicator by channel ID to prevent stuck indicator by @tomasmach in https://github.com/spacedriveapp/spacebot/pull/53
* fix: Telegram adapter improvements by @Marenz in https://github.com/spacedriveapp/spacebot/pull/50
* Adds pdf ingestion by @ACPixel in https://github.com/spacedriveapp/spacebot/pull/63
* feat: add markdown preview toggle to identity editors by @tomasmach in https://github.com/spacedriveapp/spacebot/pull/59
* Add GitHub CLI to default docker image by @ACPixel in https://github.com/spacedriveapp/spacebot/pull/61
* feat(llm): add custom providers and dynamic API routing by @sbtobb in https://github.com/spacedriveapp/spacebot/pull/36
* fix: prevent panic in split_message on multibyte UTF-8 char boundaries by @tomasmach in https://github.com/spacedriveapp/spacebot/pull/49
* feat(slack): app_mention, ephemeral messages, Block Kit, scheduled messages, typing indicator by @sookochoff in https://github.com/spacedriveapp/spacebot/pull/58
* feat(slack): slash commands (Phase 3) + Block Kit interactions (Phase 2b) by @sookochoff in https://github.com/spacedriveapp/spacebot/pull/60
* Add Portal Chat for direct web-based agent interaction by @jnyecode in https://github.com/spacedriveapp/spacebot/pull/64
* feat: add MiniMax as native provider by @ricorna in https://github.com/spacedriveapp/spacebot/pull/26
* feat: add Moonshot AI (Kimi) as native provider by @ricorna in https://github.com/spacedriveapp/spacebot/pull/25
* feat: Discord rich messages (Embeds, Buttons, Polls) by @thesammykins in https://github.com/spacedriveapp/spacebot/pull/66

## New Contributors
* @mmattbtw made their first contribution in https://github.com/spacedriveapp/spacebot/pull/18
* @skulldogged made their first contribution in https://github.com/spacedriveapp/spacebot/pull/46
* @ACPixel made their first contribution in https://github.com/spacedriveapp/spacebot/pull/43
* @l33t0 made their first contribution in https://github.com/spacedriveapp/spacebot/pull/35
* @sookochoff made their first contribution in https://github.com/spacedriveapp/spacebot/pull/57
* @egenvall made their first contribution in https://github.com/spacedriveapp/spacebot/pull/52
* @Marenz made their first contribution in https://github.com/spacedriveapp/spacebot/pull/50
* @sbtobb made their first contribution in https://github.com/spacedriveapp/spacebot/pull/36
* @jnyecode made their first contribution in https://github.com/spacedriveapp/spacebot/pull/64
* @ricorna made their first contribution in https://github.com/spacedriveapp/spacebot/pull/26

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.8...v0.1.9

## v0.1.8

- Tag: `v0.1.8`
- Published: 2026-02-19T05:26:17Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.8

## What's Changed
* fix: set skip_flag in ReplyTool to prevent double reply by @tomasmach in https://github.com/spacedriveapp/spacebot/pull/39
* fix(daemon): create instance directory before binding IPC socket by @BruceMacD in https://github.com/spacedriveapp/spacebot/pull/37
* otel by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/30
* fix otel by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/41
* make otel actually work by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/42

## New Contributors
* @tomasmach made their first contribution in https://github.com/spacedriveapp/spacebot/pull/39
* @BruceMacD made their first contribution in https://github.com/spacedriveapp/spacebot/pull/37

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.7...v0.1.8

## v0.1.7

- Tag: `v0.1.7`
- Published: 2026-02-18T18:09:56Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.7

## What's Changed
* fix(config): support numeric telegram chat_id binding match by @cyllas in https://github.com/spacedriveapp/spacebot/pull/34
* Add ARM64 multi-platform Docker images by @andrasbacsai in https://github.com/spacedriveapp/spacebot/pull/27

## New Contributors
* @cyllas made their first contribution in https://github.com/spacedriveapp/spacebot/pull/34
* @andrasbacsai made their first contribution in https://github.com/spacedriveapp/spacebot/pull/27

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.6...v0.1.7

## v0.1.6

- Tag: `v0.1.6`
- Published: 2026-02-18T10:14:21Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.6

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.5...v0.1.6

## v0.1.5

- Tag: `v0.1.5`
- Published: 2026-02-18T07:50:05Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.5

## What's Changed
* Fix broken documentation links in README by @joseph-lozano in https://github.com/spacedriveapp/spacebot/pull/11
* fix: IPv6 socket address parsing for Docker deployments by @pablopunk in https://github.com/spacedriveapp/spacebot/pull/14

## New Contributors
* @joseph-lozano made their first contribution in https://github.com/spacedriveapp/spacebot/pull/11
* @pablopunk made their first contribution in https://github.com/spacedriveapp/spacebot/pull/14

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.4...v0.1.5

## v0.1.4

- Tag: `v0.1.4`
- Published: 2026-02-17T23:23:34Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.4

## What's Changed
* Run release workflow on x86 and ARM runners by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/3
* Fix OpenCode Zen provider icon by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/5
* better provider list by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/6
* Fix Z.ai provider icon by @jiunshinn in https://github.com/spacedriveapp/spacebot/pull/7
* improve docker build by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/4
* Fix quick start by @doanbactam in https://github.com/spacedriveapp/spacebot/pull/8

## New Contributors
* @doanbactam made their first contribution in https://github.com/spacedriveapp/spacebot/pull/8

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.3...v0.1.4

## v0.1.3

- Tag: `v0.1.3`
- Published: 2026-02-17T03:59:34Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.3

## What's Changed
* Add OpenCode Zen provider support by @Brendonovich in https://github.com/spacedriveapp/spacebot/pull/2


**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.2...v0.1.3

## v0.1.2

- Tag: `v0.1.2`
- Published: 2026-02-17T01:14:40Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.2

**Full Changelog**: https://github.com/spacedriveapp/spacebot/compare/v0.1.1...v0.1.2

## v0.1.1

- Tag: `v0.1.1`
- Published: 2026-02-17T00:04:10Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.1

## What's Changed
* Add native Z.ai (GLM) provider by @jiunshinn in https://github.com/spacedriveapp/spacebot/pull/1

## New Contributors
* @jiunshinn made their first contribution in https://github.com/spacedriveapp/spacebot/pull/1

**Full Changelog**: https://github.com/spacedriveapp/spacebot/commits/v0.1.1

## v0.1.0

- Tag: `v0.1.0`
- Published: 2026-02-15T22:31:48Z
- URL: https://github.com/spacedriveapp/spacebot/releases/tag/v0.1.0

**Full Changelog**: https://github.com/spacedriveapp/spacebot/commits/v0.1.0
