# interface/

Web UI for Spacebot. Single-page app embedded into the Rust binary at build time.

## Stack

- React 19, Vite 6
- TanStack Router v1 (file-based routing), TanStack Query v5 (server state)
- Tailwind CSS 3.4, Radix UI primitives
- CVA (class-variance-authority) for component variants

## Package Manager

**bun only.** Never npm, pnpm, or yarn.

```
bun install
bun run dev
bun run build
bunx <tool>
```

## Routes (15+)

```
/
/settings
/agents/$agentId
/agents/$agentId/chat
/agents/$agentId/channels
/agents/$agentId/channels/$channelId
/agents/$agentId/memories
/agents/$agentId/workers
/agents/$agentId/cortex
/agents/$agentId/cron
/agents/$agentId/config
/agents/$agentId/ingest
/agents/$agentId/skills
/agents/$agentId/projects
/agents/$agentId/tasks
```

## State Management

- **TanStack Query** — all server/API state, caching, refetching
- **LiveContext** — React Context for real-time SSE events
- `useEventSource` hook — connects to `/api/events`
- `useChannelLiveState` — accumulates per-channel live state from SSE stream

## Design System (`src/ui/`)

25+ components built on Radix UI primitives + CVA + Tailwind.

Theming: HSL-based CSS variables in `src/ui/style/colors.scss`. Supports dark/light modes.

## API Client

`src/api/client.ts` — 2500+ lines of typed interfaces generated from OpenAPI spec. All API calls go through this file.

## Key Dependencies

| Package | Purpose |
|---|---|
| `@dnd-kit` | Drag-and-drop |
| `sigma` + `graphology` | Memory graph visualization |
| `recharts` | Charts/metrics |
| `framer-motion` | Animations |
| `codemirror` | Code editing |
| `react-markdown` | Markdown rendering |
| `zod` + `react-hook-form` | Form validation |

## Build Pipeline

```
bun run build → Vite → dist/ → rust_embed → single Rust binary
```

The Rust binary serves both the API and the frontend from a single process. No separate web server.

## Known Issues

DRY violations tracked in `DRY_VIOLATIONS.md`. Check before adding new components.
