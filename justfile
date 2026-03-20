set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

preflight:
    ./scripts/preflight.sh

preflight-ci:
    ./scripts/preflight.sh --ci

fmt-check:
    cargo fmt --all -- --check

check-all:
    cargo check --all-targets

clippy-all:
    cargo clippy --all-targets

test-lib:
    cargo test --lib

test-integration-compile:
    cargo test --tests --no-run

gate-pr: preflight
    ./scripts/gate-pr.sh

gate-pr-ci: preflight-ci
    ./scripts/gate-pr.sh --ci

gate-pr-ci-fast: preflight-ci
    ./scripts/gate-pr.sh --ci --fast

# Build the OpenCode embed bundle from a pinned upstream commit.
# Clones opencode, copies embed entry points, builds with Vite,
# and outputs to interface/public/opencode-embed/.
build-opencode-embed:
    ./scripts/build-opencode-embed.sh

# Build the spacebot binary and copy it into the Tauri sidecar
# binaries directory with the correct target-triple suffix.
bundle-sidecar:
    ./scripts/bundle-sidecar.sh --release

# Run the desktop app in development mode.
# Tauri's beforeDevCommand handles sidecar bundling and Vite automatically.
desktop-dev:
    cd desktop && bun run tauri:dev

# Build the full desktop app (sidecar + frontend + Tauri bundle).
# Tauri's beforeBuildCommand handles sidecar bundling and frontend build automatically.
desktop-build:
    cd desktop && bun run tauri:build
