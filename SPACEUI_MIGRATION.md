# SpaceUI Migration

**345 files changed | +39,873 / -17,225**

Migrates the entire frontend to [SpaceUI](https://github.com/spacedriveapp/spaceui), Spacedrive's component library. The local UI primitives (~25 components, ~3000 lines) are gone — replaced by `@spacedrive/primitives`, `@spacedrive/ai`, `@spacedrive/forms`, and `@spacedrive/explorer`. Tailwind v3 is out, Tailwind v4 via `@tailwindcss/vite` is in, with SpaceUI's design token system and theme imports.

But this isn't just a component swap. The interface has been restructured from the ground up.

## Layout

The old flat nav is replaced with a persistent sidebar (220px, Spacedrive-style) with labeled sections, accordion agent sub-nav, and a global workers popover in the footer. The org chart has its own dedicated tab instead of being crammed into the main view.

A new **Dashboard** serves as the landing page with real data wired to action items (notifications), token usage, and recent activity cards.

## UI rewrites

Every major view was decomposed from monolithic files into modular components:

- **Settings** — 2900-line monolith → 12 section components
- **AgentConfig** — 1452-line monolith → ConfigSidebar, section editors, shared types
- **TopologyGraph** — 2074-line monolith → OrgGraph, ProfileNode, GroupNode, edge/config panels, graph builder
- **Workbench** — rewritten with modular WorkbenchSidebar, WorkerColumn, and OpenCode theme inheriting CSS custom properties

**Tasks** got the biggest conceptual change — the kanban board is gone, replaced with a Linear-style task list with detail views, GitHub metadata badges, and SSE-driven updates.

## New systems

**Wiki** — full implementation from scratch. SQLite-backed (`wiki_pages` + `wiki_page_versions`), tolerant multi-pass edit matching, 6 tools (create/edit/read/list/search/history), REST API, frontend route with page browser and wiki-link navigation.

**Notifications** — SQLite store, API endpoints, SSE real-time broadcasting, `useNotifications` hook with optimistic dismiss, wired into the dashboard's action items card. Task approval and worker failure notifications emitted automatically.

**Portal** — webchat renamed to portal throughout. Modular PortalPanel/Timeline/Composer/Header replacing the monolithic WebChatPanel. File attachments with multipart upload, drag-and-drop, and timeline rendering. Conversation persistence with full CRUD.

**Streaming** — `prompt_once_streaming` on SpacebotHook with token-by-token `WorkerText` deltas. OpenAI Responses API SSE streaming (~636 lines) supporting function call deltas, text deltas, reasoning summaries.

**Built-in skills** — compiled into the binary via `include_str!`, starting with a wiki-writing skill.

## Backend

**Projects** elevated from per-agent to instance level — migration with full dedup logic, dropped `agent_id` from the projects table, updated all store/API/tool paths. Auto logo detection scanning `.github/`, `public/`, `src-tauri/`.

**Conversation settings** — `ConversationSettings` struct with memory mode, delegation, worker context, model selection. Per-channel persistence via `ChannelSettingsStore` with resolution chain (per-channel DB > binding defaults > agent defaults). `ModelOverrides` threaded through Channel, Branch, Worker, Compactor. Settings hot-reload via `ProcessEvent::SettingsUpdated`.

**Channel settings unification** — `ResponseMode` enum (Active/Observe/MentionOnly) replacing the old `listen_only_mode` system. Per-channel persistence, `[bindings.settings]` TOML support, slash commands that persist to DB. MentionOnly fixed to retain context and capture memories even when not responding.

**Direct mode** — channels can now get full worker-level tools (shell, file, browser, wiki, web search, memory) with tool calls rendered in the portal timeline.

**Tool-use enforcement** — configurable per-model prompt injection ("you MUST use tools") across all process types.

**Token usage tracking** — `token_usage` table with `UsageAccumulator` flushed per-process, wired to the dashboard chart.

## Design docs

Added: conversation-settings, wiki, token-usage-tracking, cron-outcome-delivery, skill-authoring, worker-briefing, attachment-portal-and-defaults, slash-commands, autonomy, goals. Some implemented, others queued.

---

## Commit Log (newest first)

> 14 merge commits omitted — no code changes.

### 1. `248d740` — "tweaks" `+153 / -92`
Reformatted Sidebar imports to multi-line, renamed task approval notifications from "Approval"/"Approve" to "Review" with Clock icon, unified cron job button variants to "gray", fixed workbench EmptyState centering.

### 2. `de9e680` — "tweaks" `+6 / -480`
Deleted the 474-line `autonomy-loop.md` design doc. Minor Tauri-to-desktop platform abstraction renames (`IS_TAURI` → `IS_DESKTOP`).

### 3. `945ea8e` — "probably bad, likely revert" `+62 / -16`
Rewrote `apply_history_after_turn` to extract `chat_history` from `PromptCancelled` and `MaxTurnsError` variants instead of the external history param (which Rig doesn't update on error). Flushes accumulated tool calls into `chat_history` before returning `PromptCancelled` in SpacebotHook.

### 4. `28e7951` — "ui" `+164 / -9`
New `ApprovalModal` component — full dialog for task approval notifications with task detail view, approve/dismiss actions, and query invalidation. Replaced the inline action button in ActionItemsCard.

### 5. `7bb1874` — "Add CONTRIBUTING.md, SpaceUI link workflow, and npm version refs" `+219 / -5`
Added CONTRIBUTING.md, switched `@spacedrive/*` deps from `link:` to `^0.2.0` (npm published with local bun link override), added `just spaceui-link` / `just spaceui-unlink` commands.

### 6. `b841d1c` — "progress" `+458 / -406`
Abstracted Tauri into a generic desktop host layer (`IS_DESKTOP`, `hasBundledServer`, `spawnBundledProcess`). Added active-project highlighting in sidebar, pinned "main" agent first, added macOS traffic-light padding, added stubs for cortex_chat and new tool registration.

### 7. `1da3fbf` — "a lot of stuff" `+1,343 / -500`
Built-in skills system (compiled via `include_str!`, wiki-writing skill). Added `company_name` to global settings with InstanceSection page. Implemented "direct mode" for channels giving them full worker-level tools (shell, file, browser, wiki, web search, memory). Added tool_call_run timeline items to portal view. Company name switcher popover in sidebar. Ingest section in agent config. Browser tool structs made `pub(crate)`.

### 8. `731efa2` — "workers" `+43 / -47`
Cosmetic Rust formatting only — line-break adjustments in usage.rs, wiki.rs, model.rs, pricing.rs, set_outcome.rs. No functional changes.

### 9. `2790fac` — "workers" `+1,033 / -800`
Workers panel refactor: added cancel button with `channel_id` tracking, extracted `channel_dispatch.rs` from `channel.rs`, added retrigger prompt fragment, improved wiki store error handling, switched hardcoded hex colors to semantic tokens (`text-status-success`, etc.).

### 10. `bc097ea` — "wiki pt.2" `+91 / -2`
Wired wiki tools into branches, workers, and channel dispatch. Added `wiki_enabled` flag to branch/worker prompt templates, `wiki_write: bool` to `WorkerContextMode`, propagated wiki store through all process spawn paths.

### 11. `855db49` — "wiki" `+2,590 / -13`
Full wiki system: SQLite migration (`wiki_pages`, `wiki_page_versions`), `WikiStore` with tolerant multi-pass text matching for edits, 6 wiki tools (create/edit/read/list/search/history), REST API endpoints, frontend Wiki route with page browser/viewer/create form and wiki-link navigation, TypeScript types.

### 12. `7b704b4` — "wiki" `+419 / -11`
Wiki design document (398 lines — page types, storage schema, tolerant edit matching, tool definitions, versioning, link syntax, agent access patterns, implementation phases). Added sidebar nav for Wiki, wired RecentActivityCard to real task data, added `.db*` to gitignore.

### 13. `5cd0398` — "cron outcome delivery rewrite, token usage tracking, dashboard UI, task notifications" `+1,968 / -309`
Replaced CronReplyBuffer with `set_outcome()` tool for clean cron delivery. Token usage tracking with new migration/table and `UsageAccumulator`. Connected TokenUsageCard to real API data. Rewired dashboard cards to SpaceUI primitives. WorkersPanel modal → inline slide-over. Wired `ApiState` through `AgentDeps` for task notification emission. Added token-usage-tracking and slash-commands design docs.

### 14. `055bdba` — "cron, openai streaming improvements, ui" `+918 / -417`
Wall-clock cron expression support alongside legacy interval mode. OpenAI Responses API SSE streaming in the LLM layer. Created `set_outcome` tool and prompt. Token-usage-tracking design doc. README rewrite. Dashboard cards switched to dark variant.

### 15. `24b7abd` — "ui" `+146 / -226`
Redesigned ChannelCard from list-style to square aspect-ratio with fade-masked message stream. Replaced inline WorkerBadge/BranchBadge with compact pills. Added PlatformIcon, CircleButton, typing indicator, auto-scroll on send to PortalTimeline.

### 16. `6118909` — "design docs, new readme and attachments" `+2,401 / -443`
Complete README rewrite. 6 design docs (autonomy, goals, cron-outcome-delivery, skill-authoring, worker-briefing, attachment-portal-and-defaults). File attachment support for portal chat (multipart upload API, drag-and-drop composer, attachment rendering in timeline, metadata in conversation history).

### 17. `5059a55` — "ui" `+451 / -19`
Global WorkersPanel as a sidebar footer popover with search, history/interactive tabs, per-agent worker queries, live SSE integration, modal detail view. Added PortalHeader component.

### 18. `9ce5d23` — "notifications" `+1,167 / -293`
Full notification system: SQLite migration, `NotificationStore` with CRUD/filtering, API endpoints, SSE real-time broadcasting, `useNotifications` hook with optimistic dismiss, wired ActionItemsCard to real data, cortex observation notifications on worker failure, task approval notification emission.

### 19. `aa0f389` — "ui" `+1,692 / -895`
Dashboard route with four cards (ActionItemsCard, TokenUsageCard, RecentActivityCard, GetSpacedriveCard). Extracted AgentSkills into modular components (SkillsSidebar, SkillsDirectory, SkillInspector, BundledSkills, InstalledSkillRow, RegistrySkillRow).

### 20. `4331139` — "ui" `+1,497 / -1,457`
Decomposed monolithic 1452-line AgentConfig.tsx into modular components: ConfigSidebar, ConfigSectionEditor (routing, tuning, compaction, cortex, coalesce, memory, browser, sandbox, projects), GeneralEditor, IdentityEditor, SaveBar, shared types/constants/utils. Added Config sub-item to sidebar agent nav.

### 21. `7157fe3` — "settings" `+3,055 / -2,915`
Decomposed monolithic 2900-line Settings page into 12 section components (ApiKeysSection, AppearanceSection, ChangelogSection, ChannelsSection, ChatGptOAuthDialog, ConfigFileSection, OpenCodeSection, ProviderCard, SecretsSection, ServerSection, UpdatesSection, WorkerLogsSection). Replaced ReactFlow Controls in OrgGraph with custom zoom buttons.

### 22. `5b66aea` — "better workbench" `+715 / -407`
Rewrote Workbench into modular components (WorkbenchSidebar, WorkerColumn, EmptyState, utils). Replaced hardcoded hex colors in OpenCode embed theme with CSS custom property references (`var(--color-*)`) for theme inheritance.

### 23. `e92674b` — "fixes" `+60 / -17`
Renamed Orchestrate → Workbench (route + sidebar). Fixed OpenCode proxy path prefix stripping. Fixed worker list to resolve project names from global ProjectStore. Added `@layer opencode-portals` CSS to prevent style conflicts.

### 24. `c6b3489` — "ui" `+2,108 / -2,080`
Decomposed 2074-line TopologyGraph.tsx into modular Org Chart components: OrgGraph, OrgGraphInner, ProfileNode, GroupNode, LinkEdge, EdgeConfigPanel, GroupConfigPanel, AgentEditDialog, HumanEditDialog, buildGraph, constants, handles, storage utils.

### 25. `b1a6be5` — "color fix" `+5,080 / -2,298`
Foundational SpaceUI component integration across 32 files: converted all UI from inline Tailwind/raw HTML to `@spacedrive/primitives` (Button, Input, Dialog, Badge, Switch, Select, etc.). Replaced hardcoded color classes with semantic design tokens. Added new TopBar component.

### 26. `8a12e3d` — "progress" `+2,094 / -1,051`
Replaced monolithic WebChatPanel and TopBar with modular Portal system (PortalPanel, PortalTimeline, PortalComposer, PortalHeader, PortalHistoryPopover, PortalActiveWorkers). Added AgentProjects route. Migrated projects from per-agent SQLite DBs to shared instance DB with full dedup migration logic.

### 27. `2432984` — "ui" `+1,201 / -1,730`
Rewrote task management: deleted 868-line TaskBoard (kanban) and replaced with Linear-style TaskList/TaskDetail/TaskCreateForm. Added GitHub metadata badges (issues/PRs) via TaskUtils.tsx. Rebuilt GlobalTasks with full task CRUD and SSE-driven updates.

### 28. `b8ceff1` — "Rename @spaceui/* packages to @spacedrive/* (npm org)" `+56 / -56`
Pure package rename across all imports and config: `@spaceui/*` → `@spacedrive/*` in package.json, vite.config.ts, styles.css, bun.lock.

### 29. `c53a1e3` — "SpaceUI migration: sidebar redesign, global projects, logo detection" `+983 / -641`
Three changes: (1) Sidebar redesign to Spacedrive style — 220px, labeled nav items, section headers, accordion agent sub-nav. (2) Projects made global by dropping `agent_id` from projects table + all Rust store/API/tool updates. (3) Auto project logo detection scanning `.github/`, `public/`, `src-tauri/` with new `GET /projects/{id}/logo` endpoint.

### 30. `fb5c0f3` — "remove Channel Behaviour config tab" `+3 / -18`
Removed the Channel Behaviour tab from agent config — per-channel response modes in channel settings replaced it.

### 31. `7e223f4` — "rename Quiet to Observe, remove Listen Only toggle" `+89 / -88`
Renamed `Quiet` response mode to `Observe` across full stack (Rust enums with serde alias, TS types, UI labels, docs). Observe = agent learns from context and captures memories but never responds.

### 32. `ce2e9d7` — "docs: add Configuring Channels guide" `+141 / -1`
New getting-started doc covering per-channel settings: response modes, model overrides, memory modes, delegation, worker context, slash commands. Added Channel Settings section to core channels doc.

### 33. `f07cecc` — "fix helper text to include commands as a trigger" `+2 / -2`
Updated two helper text strings to mention commands (not just mentions/replies) trigger responses in MentionOnly mode.

### 34. `d421b03` — "address review: compaction guard, doc fixes, checkbox accessibility" `+31 / -17`
Compaction check after injecting suppressed MentionOnly messages. Drops history write lock before async calls. Wraps checkbox inputs in `<label>` for accessibility.

### 35. `99467d2` — "fix: MentionOnly mode now retains channel context and memory capture" `+64 / -29`
Fixed MentionOnly to inject suppressed messages into in-memory LLM context and run passive memory capture, so the agent stays aware even when not responding. Added UI descriptions for the behavioral difference between binding-level and channel-level mention filtering.

### 36. `955efd3` — "release: v0.4.1" `+21 / -2`
Version bump 0.4.0 → 0.4.1 with CHANGELOG entry.

### 37. `a1677ba` — "Remove audit-routes.sh script" `+0 / -78`
Deleted the one-off route audit script added in `eba3c4a`.

### 38. `10d683d` — "Make prompt inspector modal 80% viewport width" `+1 / -1`
Single CSS change: PromptInspectModal width → `80vw`.

### 39. `eba3c4a` — "Fix remaining API route mismatches from OpenAPI migration" `+94 / -16`
Fixed 9 frontend API route paths broken during OpenAPI migration. Added audit-routes.sh script used to discover them.

### 40. `1b1e14f` — "Fix API route mismatches broken during OpenAPI migration" `+4 / -4`
Fixed ChatGPT OAuth routes and raw config editor route paths.

### 41. `0d89b3b` — "spaceui" `+722 / -3,593`
The foundational SpaceUI commit. Deleted the entire local UI component library (~25 files, ~3000 lines) and replaced all imports with SpaceUI packages. Added Vite aliases for local monorepo HMR. Replaced PostCSS/Tailwind v3 with Tailwind v4 via `@tailwindcss/vite`. Created new styles.css with SpaceUI design tokens. Extracted API client into `packages/api-client`.

### 42. `5399f85` — "fix: correct ingest file upload endpoint URL" `+1 / -1`
One-line API client fix for the ingest upload path.

### 43. `5c85557` — "fix: create stub for openapi-spec binary in Docker dependency cache step" `+2 / -1`
Dockerfile fix: touch stub for `openapi-spec` binary during dependency cache layer.

### 44. `6d95ecd` — "release: v0.4.0" `+68 / -2`
Version bump to 0.4.0 with CHANGELOG entry.

### 45. `b914b3a` — "fix: address review feedback on deferred injection queue" `+16 / -7`
Per-channel cap of 64 messages on deferred injection queue, avoids unnecessary cloning on send errors, re-queues inbound message on delivery failure.

### 46. `bf99bdf` — "fix: scope active channels by (agent_id, conversation_id)" `+130 / -15`
Fixed cron output leaking across agents sharing a conversation. Re-keyed `active_channels` HashMap with compound `ActiveChannelKey(agent_id, conversation_id)`. Added deferred injection queue for cross-agent message buffering.

### 47. `389d9fb` — "fix: resolve clippy warnings" `+14 / -27`
Three clippy fixes in Azure provider code: `ok_or` → `ok_or_else`, collapsed nested ifs, removed unnecessary `format!`.

### 48. `e8bded2` — "style: cargo fmt" `+107 / -83`
Pure formatting on Azure provider code.

### 49. `c0f80cc` — "fix: address review findings on Azure provider PR" `+80 / -43`
HTTPS validation for Azure endpoints, fixed Azure form validation in Settings UI, normalized trailing slashes, JSON errors instead of raw status codes.

### 50. `9d582fe` — "style: fix cargo fmt" `+6 / -6`
Formatting fix on channel_dispatch.rs and config/types.rs.

### 51. `73f1f6e` — "fix(ui): allow save with empty API key for Azure" `+1 / -5`
Removed frontend validation blocking Azure save with empty API key.

### 52. `3a89130` — "fix(ui): allow test model with empty API key for Azure" `+0 / -4`
Removed frontend validation blocking Azure test with empty key (backend uses stored key).

### 53. `47d9cb2` — "fix(azure): use stored API key for model testing when none provided" `+43 / -11`
Backend reads stored Azure API key from config.toml when request has empty key, instead of rejecting.

### 54. `9f2dadb` — "feat(ui): enable test/save buttons even when API key is blank" `+20 / -12`
Removed `!keyInput.trim()` from disabled conditions on Test/Save buttons, added Azure-specific validation.

### 55. `583ea3b` — "fix(azure): correct deployment validation logic" `+2 / -1`
Fixed `update_azure_provider` to construct full `azure/{deployment}` string before validation.

### 56. `4b5d012` — "fix(azure): prevent API key exposure and correct model routing" `+57 / -40`
Removed `api_key` from `ProviderConfigResponse`, preserves stored key on empty update, `azure/{deployment}` routing format, `AbortController` for stale config fetches, trailing slash strip.

### 57. `43eb063` — "feat(azure): add complete Azure OpenAI provider support" `+1,098 / -77`
Initial Azure OpenAI implementation: backend handler with validation (endpoint, API version, deployment regex), frontend config UI (base URL, API version, deployment), model routing in LLM layer, docs.

### 58. `d158491` — "fix(cron): pass gate follow-up cleanup" `+44 / -22`
Added `failure_class` parameter to `emit_cron_error()` distinguishing execution vs delivery failures. Formatting fixes.

### 59. `2c3e953` — "fix(cron): restore breaker and re-enable semantics" `+100 / -36`
`CronRunError` enum with Execution/Delivery variants. `set_job_enabled_state()` helper. Delivery failures now properly feed the circuit breaker. Unit tests.

### 60. `bae1465` — "style: cargo fmt fixes for cron integration tests" `+15 / -7`
Pure formatting on cron integration tests.

### 61. `538f161` — "fix(cron): delivery failures should not count as execution failures" `+23 / -12`
Returns `Ok(())` after delivery failure so `consecutive_failures` isn't incremented when execution succeeded. Reordered enable/disable lock logic.

### 62. `68f2923` — "fix(cron): address P2 findings and add integration tests" `+276 / -1`
4 integration tests for `claim_and_advance` atomicity: concurrent claim races, duplicate rejection, run-once behavior, stale cursor detection.

### 63. `261afc6` — "fix(cron): address review feedback — race conditions and validation" `+39 / -9`
5-second backoff after claim error, restores previous job on cursor failure, rejects `interval_secs == 0` without `cron_expr`, fixed cold re-enable sequence.

### 64. `f880be1` — "fix(cron): address code review findings" `+23 / -15`
SAFETY doc comment on `ExecutionGuard::drop` atomic ordering. Centralized `emit_cron_error()` helper replacing inline error emission.

### 65. `1782e70` — "fix(cron): close remaining base review gaps" `+63 / -8`
Added `cron_id` to `CronExecutionEntry`, separated execution-only metrics from delivery, structured error fields with 5s backoff on stale cursor, explicit match arms replacing silent `let _` on channel errors.

### 66. `48b4783` — "fix(cron): close review gaps in runtime branch" `+384 / -140`
Docs clarifying at-most-once run-once semantics. `claim_and_advance`/`advance_cursor` on CronStore. `broadcast_proactive` on MessagingManager with bounded retry/backoff. Refactored sandbox detection exports. Cron delivery metrics.

### 67. `1a8d08f` — "fix(cron): persist scheduler state and delivery outcomes" `+2,026 / -344`
Persistent `next_run_at` cursor with migration. Claim-before-run scheduling preventing double-firing across restarts. Separate execution/delivery outcome tracking with new migration columns. `broadcast_proactive` with retry/backoff. `src/messaging/traits.rs` with delivery target parsing. Cron UI showing split execution/delivery status. Run-once with at-most-once claiming.

### 68. `2f990bd` — "feat(sandbox): add initialization test for sandbox startup" `+12 / -0`
Compile-time test verifying `Sandbox`, `SandboxConfig`, and `detect_backend` public API exports.

### 69. `02a003c` — "feat(sandbox): add workspace accessor and integration test for ShellTool" `+34 / -0`
Added `pub fn workspace(&self) -> &Path` to Sandbox. Compile-time tests for ShellTool/Sandbox public exports.

### 70. `85783e0` — "feat(sandbox): implement backend detection for bubblewrap and sandbox-exec" `+140 / -27`
`SandboxBackend` enum (Bubblewrap/SandboxExec/None) and `detect_backend()` probing `bwrap` on Linux and `sandbox-exec` on macOS. Renamed internal enum to `InternalBackend`. Integration tests.

### 71. `24c8583` — "feat(sandbox): add SandboxConfig unit tests" `+23 / -0`
Unit tests for `SandboxConfig::default()` and `SandboxMode` variants.

### 72. `3fb01be` — "fix: address PR review feedback for tool-use enforcement" `+41 / -32`
Skips enforcement for compactor (no tools). Moves enforcement after skills rendering. Adds enforcement to resumed idle workers and coalesced-turn prompt. Cortex clones base prompt for fallback. Ingestion returns error on missing `memory_persistence_complete`. Bare TOML boolean support in deserialization.

### 73. `a73bde2` — "fix: use subtler hover/active colors in conversation sidebar" `+271 / -225`
Tab-based indentation rewrite of ConversationsSidebar.tsx. Active highlight → `bg-app-hover text-ink`, hover → `bg-app-hover/50`. Inline button → `<Button variant="outline">`. User message bubble → `bg-app-hover/30`.

### 74. `584e55e` — "fix: use accent color for user message bubbles in portal chat" `+2 / -2`
User message bubble → `bg-accent/15`. Empty-state text shows `agentDisplayName` instead of raw ID.

### 75. `3e910e4` — "fix: show agent display name instead of ID in portal chat header" `+8 / -1`
Queries agents list to look up display_name, renders it in portal chat header.

### 76. `7cb1839` — "fix: replace unnecessary to_string() with as_ref() in main.rs" `+2 / -2`
Avoids unnecessary string allocation in portal conversation and channel settings store lookups.

### 77. `75d0ca4` — "fix: resolve clippy errors" `+53 / -62`
Five clippy fixes: `too_many_arguments` allow on Branch::new, collapsed identical if/else, let-chains for nested if-let, flattened nested match, `or_default()`.

### 78. `f9263a0` — "fix: remove stale settings field from platform config structs" `+8 / -11`
Removed leftover `settings: None` from test initializers, added missing fields to context_dump tests.

### 79. `f36b527` — "fix(slack): upgrade slack-morphism from 2.17 to 2.19" `+52 / -15`
Dependency bump pulling in updated signal-hook, rand, chacha20. Motivated by intermittent Slack HTTP client failures.

### 80. `0f1406b` — "fix: add missing settings field to all Binding initializers in tests" `+32 / -0`
Added `settings: None` to every `Binding` struct in test cases after the field was added.

### 81. `f165c25` — "fix: address remaining PR review feedback" `+153 / -76`
ChannelCard resets settings from DB on reopen, invalidates channels query on save. WebChatPanel hydrates from cached data on switch. `reload_settings` preserves existing on DB errors. `set_response_mode` uses `tokio::spawn` for non-blocking writes. Config loader warns on unknown enum values instead of silent default.

### 82. `d70163c` — "fix: address PR review feedback" `+146 / -74`
`/status` shows resolved model overrides. ChannelCard waits for settings data before rendering. `parse_response_mode` maps `listen_only_mode=false` → Active. Binding settings only override when explicitly set in TOML. Idle worker resume loads from ChannelSettingsStore. Batched turn skips memory when MemoryMode is Off. Channel settings API validates channel existence.

### 83. `76ba6b3` — "fix: settings popover overflow when advanced section is open" `+2 / -2`
Added `max-h-[80vh]`, `overflow-y-auto`, `collisionPadding={16}` to settings popover.

### 84. `c6eb977` — "fix: channel settings popover not opening on click" `+1 / -0`
Added manual `setShowSettings` toggle because `stopPropagation` was blocking Radix's PopoverTrigger click.

### 85. `c3e7db5` — "feat: settings hot-reload, response mode badges, portal header info" `+151 / -11`
`ProcessEvent::SettingsUpdated` variant with `reload_settings()` method. Channels API includes `response_mode` and `model` from live state. Frontend shows response mode badges on ChannelCard and portal header with current model name.

### 86. `15c9fe9` — "refactor: remove legacy listen_only_mode infrastructure" `+20 / -216`
Deleted entire `listen_only_mode`/`listen_only_session_override` system, `sync_listen_only_mode_from_runtime()`, `set_listen_only_mode()`, RuntimeConfig fields, SettingsStore methods. Replaced with `is_suppressed()` checking `resolved_settings.response_mode`.

### 87. `9e5b802` — "feat: channel settings unification — ResponseMode, per-channel persistence" `+903 / -72`
`ResponseMode` enum (Active/Quiet/MentionOnly), `save_attachments` on ConversationSettings. `channel_settings` SQLite table + migration + `ChannelSettingsStore`. GET/PUT `/channels/{channel_id}/settings` API. Settings resolution: per-channel DB > binding defaults > agent defaults. `[bindings.settings]` TOML support. `/quiet`, `/active`, `/mention-only` slash commands persist to DB. ChannelCard gear icon + settings popover.

### 88. `e804610` — "ui tweaks" `+284 / -89`
Visual overhaul of ConversationSettingsPanel: readable multi-line preset definitions, contextual description text per setting, new `SettingField` component, widened popover to `w-96`, reformatted advanced section.

### 89. `5a77c00` — "feat: finish conversation settings — load from DB, model overrides, worker memory tools" `+561 / -242`
Loads ConversationSettings from PortalConversationStore at channel creation. `ModelOverrides` struct threaded through Channel, Branch, Worker, Compactor. Worker memory tools wired to `WorkerMemoryMode`. Memory persistence guard for Ambient/Off. Dynamic available models from models.dev catalog. Frontend Apply Settings button + per-process model override selects.

### 90. `7957a4b` — "feat: add configurable tool-use enforcement" `+332 / -8`
`tool_use_enforcement` config section with per-model pattern matching. Appends "you MUST use tools" prompt fragment to system prompts across all process types. Jinja2 template + `maybe_append_tool_use_enforcement` on prompt engine.

### 91. `d454798` — "conversation settings impl" `+1,634 / -318`
Foundational conversation settings: renamed webchat → portal throughout (API routes, stores, hooks). `ConversationSettings` struct (memory mode, delegation, worker context, model selection). `ResolvedConversationSettings` with resolution logic. ConversationSettingsPanel + ConversationsSidebar with conversation CRUD. Portal conversation API endpoints with settings persistence. DB migration. `WorkerContextSettings` threaded through dispatch/spawning. `/portal/defaults` endpoint.

### 92. `b9dd1df` — "design doc" `+1,276 / -1`
Conversation settings design document (1056 lines). Also includes desktop Tauri lockfile/schema changes (global-hotkey, gethostname crates).

### 93. `9f9e7e0` — "streaming support" `+1,087 / -4`
`prompt_once_streaming` on SpacebotHook using `StreamingCompletion` for token-by-token responses with `ProcessEvent::WorkerText` deltas. ~636 lines of OpenAI Responses API streaming logic in model.rs (function call deltas, text deltas, reasoning summary deltas, response completion). Channel now calls `prompt_once_streaming`.

### 94. `05bbc07` — "feat: persist webchat conversations and add API client package" `+1,503 / -163`
SQLite-backed webchat conversation persistence (`webchat_conversations` table + migration + `WebChatConversationStore` with full CRUD). New `@spacebot/api-client` npm package with typed client, SSE helpers, and re-exported schema types. Expanded OpenAPI schema types.
