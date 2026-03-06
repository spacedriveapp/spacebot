/**
 * SPA entry point for the embeddable OpenCode build.
 *
 * Unlike entry.tsx, this does NOT auto-render. Instead it attaches
 * `mountOpenCode` to the window object so the host app can call it
 * after loading this script.
 *
 * This file is used as the entry in index-embed.html for a normal
 * Vite SPA build (not library mode), so we get full code splitting.
 */

import { mountOpenCode } from "./embed"
export type { MountOpenCodeConfig, MountOpenCodeHandle } from "./embed"

// Attach to window so the host app can call it after script load.
;(window as any).__opencode_embed__ = { mountOpenCode }
