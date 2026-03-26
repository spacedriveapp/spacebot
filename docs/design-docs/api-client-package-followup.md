# API Client Package Follow-up

The first extraction of `@spacebot/api-client` is intentionally light.

It creates a reusable package boundary for Spacedrive and other consumers, but it still re-exports generated types from `interface/src/api/`.

## Follow-up Work

### 1. Make `spacebot/interface` consume `@spacebot/api-client`

The Spacebot interface should stop importing from its local `src/api/*` modules directly and instead consume the shared package.

That will make the package the single source of truth for:

- OpenAPI client setup
- generated schema types
- friendly exported types
- manual SSE event types

### 2. Move OpenAPI generation output into the package

The long-term target is for `schema.d.ts` to be generated directly into `packages/api-client/` instead of `interface/src/api/`.

That will let the shared package fully own the generated contract and avoid the current re-export bridge.

## Why Deferred

These steps are worth doing, but they are not required for the first Spacedrive integration slice.

For now, the package boundary exists, and Spacedrive can start consuming it immediately.
