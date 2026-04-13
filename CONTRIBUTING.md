# Contributing

Contributions welcome. Read [RUST_STYLE_GUIDE.md](RUST_STYLE_GUIDE.md) before writing any code, and [AGENTS.md](AGENTS.md) for the full implementation guide.

---

## Prerequisites

- **Rust** 1.85+ with `rustfmt` and `clippy`
- **protoc** (protobuf compiler)
- **bun** (for frontend/interface work)
- **just** (`brew install just` or `cargo install just --locked`)

Optional: [Nix flakes](https://nixos.org/) for isolated dev environments (`nix develop` gives you everything).

---

## Getting Started

1. Fork the repo and create a feature branch
2. Run `./scripts/install-git-hooks.sh` (installs a pre-commit hook that runs `cargo fmt`)
3. `cargo build` to verify the backend compiles
4. For frontend work: `cd interface && bun install`
5. Make your changes
6. Run `just preflight && just gate-pr`
7. Submit a PR

---

## PR Gate

Every PR must pass `just gate-pr` before merge. This mirrors CI and checks:

1. **Migration safety** — new migrations only, never edit existing ones
2. **Formatting** — `cargo fmt --all -- --check`
3. **Compile** — `cargo check --all-targets`
4. **Lints** — `cargo clippy --all-targets -Dwarnings`
5. **Tests** — `cargo test --lib`
6. **Integration compile** — `cargo test --tests --no-run`

Use `just gate-pr --fast` to skip clippy and integration compile during iteration.

The frontend CI (`interface-ci.yml`) runs `bun ci` and `bunx tsc --noEmit` on interface changes.

---

## Project Structure

Single binary crate (no workspace). Key directories:

```
src/
├── main.rs           — CLI entry, config, startup
├── lib.rs            — re-exports
├── config.rs         — config loading/validation
├── error.rs          — top-level Error enum
├── llm/              — LlmManager, model routing, providers
├── agent/            — Channel, Branch, Worker, Compactor, Cortex
├── hooks/            — SpacebotHook, CortexHook
├── tools/            — reply, branch, spawn_worker, memory_*, etc.
├── memory/           — MemoryStore, hybrid search, graph ops
├── messaging/        — Discord, Telegram, Slack, webhook adapters
├── conversation/     — history persistence, context assembly
├── cron/             — scheduler, CRUD
├── identity/         — SOUL.md, IDENTITY.md, USER.md loading
├── secrets/          — encrypted credentials (AES-256-GCM)
├── settings/         — key-value settings
└── db/               — SQLite migrations, connection setup

interface/            — Dashboard UI (Vite + React + TypeScript)
prompts/              — LLM prompts as markdown (not Rust strings)
docs/                 — Documentation site (MDX)
desktop/              — Tauri desktop app
scripts/              — Dev tooling (hooks, gates, builds)
```

Module roots use `src/module.rs`, **not** `src/module/mod.rs`.

---

## Rust Conventions

The full guide is in [RUST_STYLE_GUIDE.md](RUST_STYLE_GUIDE.md). Key points:

**Imports** — three tiers separated by blank lines: (1) crate-local, (2) external crates, (3) std.

**Error handling** — domain errors per module, wrapped by top-level `Error` enum via `#[from]`. Use `?` and `.context()`. Never silently discard with `let _ =`.

**Async** — native RPITIT for async traits (not `#[async_trait]`). `tokio::spawn` for concurrent work. Clone before moving into async blocks.

**Logging** — `tracing` crate, never `println!`. Structured key-value fields. `#[tracing::instrument]` for spans.

**Lints** (enforced in Cargo.toml): `dbg_macro = "forbid"`, `todo = "forbid"`, `unimplemented = "forbid"`.

**Testing** — `#[cfg(test)]` at end of file. `#[tokio::test]` for async. `.unwrap()` is fine in tests only.

---

## Frontend (Interface)

Use **bun** exclusively — never npm, pnpm, or yarn.

```bash
cd interface
bun install       # install deps
bun run dev       # dev server
bun run build     # production build
```

### SpaceUI Packages

The dashboard uses `@spacedrive/*` packages published to npm from the [spaceui](https://github.com/spacedriveapp/spaceui) monorepo:

- `@spacedrive/primitives` — base UI components
- `@spacedrive/ai` — AI chat components
- `@spacedrive/forms` — form components
- `@spacedrive/explorer` — file explorer components
- `@spacedrive/tokens` — design tokens

`package.json` points to npm versions (e.g. `"^0.2.0"`). CI pulls from the registry. For local development, `bun link` overrides them with your local copies.

**Local SpaceUI development:**

Clone the spaceui repo adjacent to this one, then run the link command:

```bash
git clone https://github.com/spacedriveapp/spaceui ../spaceui
just spaceui-link
```

This builds SpaceUI, registers all packages as global links, and connects them to `interface/`. Use `bun run watch` in the SpaceUI repo for automatic rebuilds.

To unlink and restore npm versions: `just spaceui-unlink`.

---

## Useful Commands

```bash
just preflight                # validate git/remote state
just gate-pr                  # full PR gate (mirrors CI)
just gate-pr --fast           # skip clippy + integration compile
just typegen                  # generate TypeScript API types
just check-typegen            # verify types match
just build-opencode-embed     # build OpenCode embed bundle
just bundle-sidecar           # build Tauri sidecar
just desktop-dev              # run desktop app in dev mode
just update-frontend-hash     # update Nix hash after frontend dep changes
```

---

## Migrations

SQLite migrations are **immutable**. Never edit an existing migration file. Always create a new timestamped migration for schema changes.

---

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design. The short version: five process types, each with one job.

- **Channels** — user-facing LLM, stays responsive, never blocks on work
- **Branches** — fork channel context to think, return conclusion, get deleted
- **Workers** — independent task execution with focused tools, no conversation context
- **Compactor** — programmatic context monitor, triggers compaction before channels fill up
- **Cortex** — system observer, generates memory bulletins, supervises processes

Key rule: **never block the channel**. Branch to think, spawn workers to act.

---

## Release Process

Releases are triggered by git tags (`v*`). The CI workflow:

1. Verifies `Cargo.toml` version matches the tag
2. Builds multi-platform binaries (x86_64/aarch64, Linux/macOS)
3. Builds Docker images (amd64/arm64)
4. Creates a GitHub release with binaries
5. Updates the Homebrew tap

---

## License

FSL-1.1-ALv2 ([Functional Source License](https://fsl.software/)), converting to Apache 2.0 after two years.
