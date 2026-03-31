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

typegen:
    cargo run --bin openapi-spec > /tmp/spacebot-openapi.json
    cd interface && bunx openapi-typescript /tmp/spacebot-openapi.json -o src/api/schema.d.ts

check-typegen:
    cargo run --bin openapi-spec > /tmp/spacebot-openapi-check.json
    cd interface && bunx openapi-typescript /tmp/spacebot-openapi-check.json -o /tmp/spacebot-schema-check.d.ts
    diff interface/src/api/schema.d.ts /tmp/spacebot-schema-check.d.ts

typegen-package:
	cargo run --bin openapi-spec > /tmp/spacebot-openapi-package.json
	cd interface && bunx openapi-typescript /tmp/spacebot-openapi-package.json -o src/api/schema.d.ts

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

# Update the frontend node_modules hash in nix/default.nix after updating interface dependencies.
# Usage: Update interface/package.json or interface/bun.lock, then run: just update-frontend-hash
update-frontend-hash:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building frontend-updater to get new hash..."
    output=$(nix build .#frontend-updater 2>&1 || true)
    new_hash=$(echo "$output" | awk '/got:/ {print $2}' || true)

    if [ -z "$new_hash" ]; then
        echo "Error: Failed to extract hash from build output"
        echo "Build output:"
        echo "$output"
        exit 1
    fi

    echo "New hash: $new_hash"

    # Check if hash is already up to date
    current_hash=$(grep -E 'hash \?' nix/default.nix | head -1 | sed -E 's/.*hash \? "([^"]+)".*/\1/')
    if [ "$current_hash" = "$new_hash" ]; then
        echo "Hash is already up to date!"
        exit 0
    fi

    # Update the hash in nix/default.nix (POSIX-safe in-place edit)
    tmpfile=$(mktemp)
    sed -E "s|hash \? \"[^\"]+\"|hash ? \"$new_hash\"|" nix/default.nix > "$tmpfile"
    mv "$tmpfile" nix/default.nix
    echo "Updated nix/default.nix with new hash"
    echo ""
    echo "Next steps:"
    echo "  1. Review the changes: git diff nix/default.nix"
    echo "  2. Test the build: nix build .#frontend"
    echo "  3. Commit the changes: git add nix/default.nix && git commit -m 'update: frontend node_modules hash'"

# Update all Nix flake inputs (flake.lock).
# Use this when you want to update dependencies like nixpkgs, crane, etc.
update-flake:
    nix flake update --extra-experimental-features "nix-command flakes"
