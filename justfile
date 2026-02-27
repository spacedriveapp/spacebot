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
