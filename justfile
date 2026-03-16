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

# Cross-compile for aarch64 Linux (Raspberry Pi, ARM servers).
# Requires: cargo install cross --locked
# Output: target/aarch64-unknown-linux-gnu/release/spacebot
build-aarch64:
    SPACEBOT_SKIP_FRONTEND_BUILD=1 cross build --target aarch64-unknown-linux-gnu --release
