# Embedded Spacedrive Explorer

Spacebot gains a Files surface that *is* the Spacedrive Explorer. When the integration flag is on, the sidebar shows a folder button next to the workers panel; clicking it opens a route that loads the live Spacedrive web interface in an iframe. From a Docker container running both binaries, an operator can explore the filesystem their Spacebot is sitting on top of from a web browser — using the actual Spacedrive UI, with no parallel file manager built into Spacebot.

This is a UI and deployment story. The protocol boundary stays exactly where the spacebot–spacedrive integration contract puts it: HTTP between processes, Spacedrive owning device identity and file topology, Spacebot owning agents. The iframe is just a way to *show* a UI that already exists, on top of an HTTP boundary that already exists.

---

## Why now

Spacedrive's Explorer is built and working — file kinds, locations, devices, volumes, tags, the nine view modes, QuickPreview, all of it. Spacebot has agents that need a file surface but no file manager of its own, and shouldn't build one. The integration contract between the two products is already written down. The bones of the Files-and-Spaces UI have been alive in the Spacedrive desktop app for months.

What's missing is a clean way for a user to get the Spacedrive UI in front of them when Spacebot is the thing they're looking at — particularly in the headless server case, where there's no native Spacedrive app to switch to. Embedding gives Spacebot a Files surface without inventing one, and gives Spacedrive's server a real reason to ship a web UI.

---

## The approach: iframe

Not a shared component package, not module federation, not a re-bundled React app. The Spacebot interface adds one route, that route renders one `<iframe>`, and the iframe loads the entire Spacedrive web interface.

This works because:

- **The Explorer is deeply coupled to Spacedrive's runtime.** React contexts, query client, RPC client, hooks, the lot. Extracting it as a shared component means rewriting the foundation across a codebase that's actively churning on the SpaceUI migration.
- **Both products already run as separate processes that talk over HTTP.** The iframe boundary is the UI shape of the same process boundary the integration contract already establishes. Nothing gets coupled at the source level that wasn't coupled at the protocol level.
- **No iframe blockers.** Spacedrive's web UI sets no `X-Frame-Options`, no `frame-ancestors` CSP, and has no frame-busting JavaScript. It loads cleanly from any origin.
- **Updates flow automatically.** A change in Spacedrive's Files view ships when Spacedrive ships, not when Spacebot does. No version coordination, no shared release pipeline.

### What we accept

- Two React apps loaded in one tab. The Spacedrive frontend is small enough that this isn't a real cost in practice.
- Cross-origin for v1 — cookies and storage don't share, auth happens twice if both sides have auth. The proxy story below removes this once it's needed.
- The iframe surface is opaque to Spacebot. The agent UI can't deep-link into a Spacedrive path without postMessage choreography, which we don't build for v1. Spacebot navigates *to* the Files surface, and from there the user is in Spacedrive's hands.

---

## What's missing first: Spacedrive Server has no UI

Today the Spacedrive server is RPC-only. The repo has `apps/web` (the React build) and `apps/server` (the Axum daemon), and they're not joined together — the server's `/` route literally returns the string "Spacedrive Server - RPC only (no web UI)". The desktop app glues `apps/web` to the embedded core via Tauri. The dev workflow uses Vite to proxy `/rpc` calls to a running daemon. In production, headless, there's nothing to iframe.

So the first thing this design ships is the missing piece on the Spacedrive side: the server embeds `apps/web` at compile time and serves it with an SPA fallback at `/`. The existing `/rpc` and `/health` routes stay where they are. Cold start is hands-off because the core already auto-creates a default library on first run.

This is independently valuable. It turns Spacedrive Server from "a daemon you can hit with RPC calls" into "a self-contained file manager you can drop into any container runtime and open in a browser." The Spacebot embed is the first consumer of that capability, but it won't be the last.

---

## The Spacebot side

### The config section

The integration contract already specifies a `[spacedrive]` section in Spacebot's `config.toml`. This design adopts that struct and adds one field: `web_url`, the URL the iframe should load.

```toml
[spacedrive]
enabled = false   # default — standalone Spacebot, no awareness

# Co-located deployment:
[spacedrive]
enabled = true
api_url = "http://127.0.0.1:8080"   # internal RPC endpoint
web_url = "http://localhost:8080"   # what the browser iframe loads
```

`api_url` is for any future Spacebot → Spacedrive RPC client described in the integration contract — device graph, remote execution, file system intelligence. `web_url` is purely for the iframe.

These two URLs are allowed to differ because the iframe runs in the **user's browser**, not inside the Spacebot process. In a Docker deployment behind a reverse proxy, `api_url` can be `http://127.0.0.1:8080` (container-internal) while `web_url` is `https://files.example.com` (publicly reachable). When `web_url` is unset it falls back to `api_url`.

`enabled = false` is the default. Spacebot operates exactly as it does today: no flag, no button, no awareness. The "both products work alone" principle from the contract holds.

### How the flag reaches the interface

Spacebot's existing global settings response gains a small block:

```json
{
  "company_name": "...",
  "spacedrive": { "enabled": true, "web_url": "http://localhost:8080" }
}
```

That's it. No new endpoint, no new context provider, no new state machine — just two fields on a response the interface already queries on every page load. The sidebar reads them, decides whether to render the button, and the route reads them to know what to iframe.

### When Spacedrive is unreachable

If `enabled = true` but Spacedrive doesn't answer at startup, Spacebot logs a warning and keeps running. The iframe surfaces its own connection error when the user opens the route. We don't proactively probe, retry, or block — the contract is clear that Spacebot must function standalone, and the iframe is a self-contained surface that knows how to fail visibly.

---

## The UI surface

One button, one route, one iframe.

### The button

Lives in the **sidebar footer**, between the workers panel button and the settings gear. The footer today holds two controls; this design adds a third. The button only renders when the Spacedrive flag is on and a URL resolves.

The icon is the Spacedrive **brand folder PNG** from `@spacedrive/icons` — the same 3D-rendered folder that appears throughout the Spacedrive Explorer itself. Not a Phosphor outline icon, even though the rest of the Spacebot sidebar uses Phosphor. This is an intentional break, and the only place it happens. The button is announcing "you're about to enter Spacedrive," and the visual commits to that.

### The route

`/spacedrive` renders the iframe filling the available space. It uses the "bare" layout pattern that the workbench and dashboard already use, so the iframe is not wrapped in Spacebot's rounded chrome — the Spacedrive UI brings its own chrome and we don't want a rounded box inside a rounded box.

States:
- **Configured and reachable** — iframe loads the Spacedrive Explorer.
- **Configured but no URL resolves** — empty state with a one-line "Spacedrive is not configured" hint and a link to settings.
- **Iframe fails to load** — Spacedrive's own error surface shows through. Spacebot doesn't try to interpret it.

### What the user sees

The footer button reads as "Files." Clicking it doesn't switch apps, doesn't open a new window, doesn't pop a modal — it navigates within Spacebot's interface, and the contents of the main pane become the Spacedrive Explorer. The Spacebot sidebar stays where it is, the active route highlights the Files button, and the user can navigate back to chat, tasks, dashboard, or any other surface without losing their place in the file browser.

That's the entire interaction model. The first time the user clicks Files, Spacebot becomes a two-surface app: agents on one side, files on the other, in the same window. (Side note: the screenshot you mocked this up with — the Spacedrive Mac app "positiioned" over the Spacebot webui — already shows exactly this layout. Nice double-i, by the way.)

---

## Local development

The same architecture works for day-to-day dev — Docker isn't required for this feature, only for the hosted deployment. Spacebot and Spacedrive run as independent processes the way they already do, point one at the other in `config.toml`, and the iframe works.

The catch: the **Tauri desktop app can't be the iframe target.** The desktop app embeds the core directly and serves its UI through Tauri's webview, not over HTTP — there's no port to hit. Local dev for this feature uses `sd-server` (the headless Axum daemon, gaining its embedded web UI in Phase 1) in place of the Tauri app. That's fine: `sd-server` runs in a terminal, takes `--data-dir`, and serves the API and the UI on a single port. The Tauri app can stay closed while developing this, or run against a separate data directory if you want both alive at once.

Two dev shapes worth knowing:

- **Built mode** — `sd-server` serves the embedded `apps/web` build at `http://localhost:8080`. Spacebot's `web_url` points at `http://localhost:8080`. The iframe loads the same bundle that ships in Docker. No HMR for Spacedrive UI work — you rebuild between changes — but the code path is identical to production.
- **Vite dev mode** — `sd-server` runs on `:8080` for the API. `bun run dev` in `apps/web/` runs the Vite dev server on its own port (e.g. `:5173`), and Vite proxies `/rpc` to `sd-server`. Spacebot's `web_url` points at the Vite dev URL so the iframe loads the dev server with HMR. Useful when iterating on the Spacedrive UI itself.

A note on data directories: `sd-server` and the Tauri app keep separate libraries by default (different platform paths). You can point them at the same directory with `--data-dir`, but only one process can hold the SQLite locks at a time, so they can't both be running against the same library. For developing this feature it doesn't matter — `sd-server` having its own scratch library is enough to exercise the embed end-to-end. The hosted users running the unified container hit the same `sd-server` code path, so anything you verify locally in built mode is what they'll get.

---

## The unified container

The endgame is a single Docker image that ships both binaries pre-paired. An operator runs `docker run`, opens the Spacebot port in their browser, and the Files button works out of the box.

Build flow:

1. Build the Spacedrive web bundle.
2. Build `sd-server` with the bundle embedded.
3. Build `spacebot` with its own embedded interface.
4. Assemble both binaries on a slim runtime base with a process supervisor.

The container ships a default `config.toml` with `[spacedrive] enabled = true`, `api_url = http://127.0.0.1:8080`, and a `web_url` that defaults to `http://localhost:8080` but can be overridden by env var so server deployments behind a public proxy can point at their hostname.

Two ports are exposed in v1: one for Spacebot, one for Spacedrive. The iframe runs in the user's browser and needs to reach Spacedrive directly, so both must be addressable. This is the rough edge of the cross-origin model — if you only expose Spacebot's port through your reverse proxy, the iframe gets a connection error. The proxy story below addresses this.

Process supervision uses `tini` or `s6-overlay`: start `sd-server` in the background, run `spacebot` in the foreground, propagate signals so `docker stop` shuts both cleanly.

---

## The proxy (next step, not v1)

Cross-origin works for v1 but it has edges: two ports in firewalls, two URLs to configure, no shared auth, the public-URL footgun in Docker.

The clean version is a same-origin reverse proxy. Spacebot adds a wildcard route at `/spacedrive/{*path}` that streams requests through to the configured upstream — method, headers, body, status, response stream all pass through, SSE upgrades stay open and chunks forward unbuffered. With this in place, the iframe targets a relative `/spacedrive/` URL, the whole thing lives at one origin, the Docker image collapses to a single exposed port, and `sd-server` only listens on `127.0.0.1:8080` instead of being publicly reachable.

This is real Rust to write — streaming HTTP proxies with SSE pass-through aren't trivial — and it isn't on the v1 critical path. Ship cross-origin first, get it in someone's hands, add the proxy when the rough edges become the bottleneck.

---

## Phases

1. **Spacedrive server serves its own web UI.** Embed the web bundle, SPA fallback, updated Dockerfile. Independently shippable as a Spacedrive Server upgrade.
2. **Spacebot config + flag exposure.** `SpacedriveIntegrationConfig` lands per the integration contract (with the `web_url` addition), the global settings response gains the `spacedrive` block. Runtime is otherwise unchanged.
3. **Spacebot interface route + sidebar button.** A small `imageSrc` extension to `CircleButton` for the brand PNG. New `/spacedrive` route renders the iframe. Footer gets the third button, gated on the flag.
4. **Unified Docker image.** Multi-stage build, both binaries, default paired config, supervised process startup, two-port exposure.
5. **Reverse proxy** (optional polish). Single-port deployment, same-origin auth, no public-URL footgun.

---

## What this isn't

- **Not a Spacedrive client library inside Spacebot.** Spacebot doesn't gain a typed `SpacedriveClient`, doesn't query the device graph, doesn't run remote execution tools. That work is described in the integration contract and is independent of this design. Embedding the UI is a UI concern and doesn't pull RPC plumbing along with it.
- **Not a port of the Explorer to SpaceUI.** The `@spacedrive/explorer` package is intentionally small (TagPill, RenameInput, FileThumb) and grows organically as stable pieces are extracted. This design doesn't accelerate that — the iframe sidesteps the question entirely.
- **Not deep linking.** Spacebot's route can't tell the embedded Explorer to open a specific folder, jump to a Space, or run a search. Cross-frame messaging is possible later if there's a real reason. V1 ships with the iframe as a fully self-contained surface.
- **Not desktop-specific.** This works the same way in Spacebot's web interface whether it's served from a server, a desktop binary, or anywhere else. The Spacedrive desktop app stays its own thing.
- **Not a permanent shape.** Once `@spacedrive/explorer` matures enough to render the actual Files view as a component, the iframe can be replaced with a native React surface and the same `/spacedrive` route. The button and the URL stay; the implementation underneath them can swap. The iframe is a bridge, not a destination.
