/**
 * Vite config for building the OpenCode embed bundle.
 *
 * Builds as a normal SPA (not library mode) so we get Vite's full
 * automatic code splitting for shiki grammars, lazy routes, etc.
 * The entry script attaches `mountOpenCode` to `window.__opencode_embed__`
 * instead of auto-rendering.
 *
 * Output:
 *   - dist-embed/index.html           (minimal HTML, loads entry JS)
 *   - dist-embed/assets/index-*.js    (main entry chunk, ~2-3MB)
 *   - dist-embed/assets/index-*.css   (all CSS)
 *   - dist-embed/assets/*.js          (lazy chunks: shiki grammars, etc.)
 *
 * Build with:
 *   ./node_modules/.bin/vite build --config vite.config.embed.ts
 *
 * Usage in host app:
 *   1. Load the entry JS as a <script type="module">
 *   2. Call window.__opencode_embed__.mountOpenCode(element, config)
 */

import { defineConfig } from "vite"
import solidPlugin from "vite-plugin-solid"
import tailwindcss from "@tailwindcss/vite"
import { fileURLToPath } from "url"

export default defineConfig({
  plugins: [
    {
      name: "opencode-embed:config",
      config() {
        return {
          resolve: {
            alias: {
              "@": fileURLToPath(new URL("./src", import.meta.url)),
            },
          },
          worker: {
            format: "es" as const,
          },
        }
      },
    },
    tailwindcss(),
    solidPlugin(),
  ],
  build: {
    target: "esnext",
    outDir: "dist-embed",
    emptyOutDir: true,
    // Use the embed HTML entry — attaches mountOpenCode to window
    // instead of auto-rendering.
    rollupOptions: {
      input: fileURLToPath(new URL("./index-embed.html", import.meta.url)),
    },
    chunkSizeWarningLimit: 3000,
  },
})
