/**
 * `bun test` preload (see `bunfig.toml`).
 *
 * `@/api/client` reads `window.__SPACEBOT_BASE_PATH` at module scope — fine in
 * the browser and under Vite, a ReferenceError in the bun test runner, where
 * no window exists. The logic modules under test import the client for its
 * types and endpoint table, so the window has to be there before any test
 * file's import graph evaluates.
 */

// The DOM lib types insist `window` always exists; under `bun test` it does
// not, so the check has to go through a shape that admits its absence.
const globalScope: {window?: unknown} = globalThis;
if (typeof globalScope.window === "undefined") {
	globalScope.window = globalThis;
}
