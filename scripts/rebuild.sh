#!/usr/bin/env bash
# rebuild.sh — Rebuild and launch spacebot for development.
#
# Builds the backend (cargo), frontend (vite), and Tauri desktop app,
# then launches Spacebot.exe which automatically starts the backend
# server (with logs in the console) and opens devtools.
#
# Usage:
#   ./scripts/rebuild.sh                    # full rebuild + launch
#   ./scripts/rebuild.sh --skip-frontend    # backend only
#   ./scripts/rebuild.sh --skip-backend     # frontend only
#   ./scripts/rebuild.sh --build-only       # build without launching

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INTERFACE_DIR="$REPO_ROOT/interface"
DESKTOP_DIR="$REPO_ROOT/desktop"
BINARIES_DIR="$REPO_ROOT/desktop/src-tauri/binaries"

# ── Defaults ──────────────────────────────────────────────────────────────────
SKIP_FRONTEND=false
SKIP_BACKEND=false
BUILD_ONLY=false

# ── Parse arguments ───────────────────────────────────────────────────────────
while (( $# > 0 )); do
    case "$1" in
        --skip-frontend)
            SKIP_FRONTEND=true
            shift
            ;;
        --skip-backend)
            SKIP_BACKEND=true
            shift
            ;;
        --build-only)
            BUILD_ONLY=true
            shift
            ;;
        -h|--help)
            sed -n '2,/^$/s/^# //p' "$0"
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

log()  { echo "[rebuild] $*"; }
fail() { echo "[rebuild] ERROR: $*" >&2; exit 1; }

SECONDS=0

BACKEND_BIN="$REPO_ROOT/target/debug/spacebot"
TAURI_BIN="$REPO_ROOT/desktop/src-tauri/target/debug/Spacebot"
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        BACKEND_BIN="${BACKEND_BIN}.exe"
        TAURI_BIN="${TAURI_BIN}.exe"
        ;;
esac

# ── Frontend rebuild ─────────────────────────────────────────────────────────
if [[ "$SKIP_FRONTEND" == false ]]; then
    log "rebuilding frontend..."

    if ! command -v bun >/dev/null 2>&1; then
        fail "bun is not installed. Install it from https://bun.sh"
    fi

    if [[ ! -d "$INTERFACE_DIR/node_modules" ]]; then
        log "installing frontend dependencies..."
        (cd "$INTERFACE_DIR" && bun install)
    fi

    (cd "$INTERFACE_DIR" && bun run build)
    log "frontend ready -> $INTERFACE_DIR/dist/"
else
    log "skipping frontend"
fi

# ── Backend rebuild ──────────────────────────────────────────────────────────
if [[ "$SKIP_BACKEND" == false ]]; then
    log "rebuilding backend..."

    if ! command -v cargo >/dev/null 2>&1; then
        fail "cargo is not installed. Install Rust from https://rustup.rs"
    fi

    export SPACEBOT_SKIP_FRONTEND_BUILD=1
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml"

    log "backend ready -> $BACKEND_BIN"
else
    log "skipping backend"
fi

# ── Bundle sidecar ───────────────────────────────────────────────────────────
if [[ "$SKIP_BACKEND" == false ]]; then
    log "bundling sidecar..."

    HOST_TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
    mkdir -p "$BINARIES_DIR"

    SUFFIX=""
    case "$HOST_TRIPLE" in
        *windows*) SUFFIX=".exe" ;;
    esac

    DEST_BIN="$BINARIES_DIR/spacebot-${HOST_TRIPLE}${SUFFIX}"
    cp "$BACKEND_BIN" "$DEST_BIN"
    log "sidecar bundled -> $DEST_BIN"
fi

# ── Tauri desktop rebuild ────────────────────────────────────────────────────
log "rebuilding Tauri desktop app..."

# Kill any running instance so the linker can overwrite the exe
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        taskkill //f //im Spacebot.exe 2>/dev/null || true
        ;;
    *)
        pkill -f "Spacebot" 2>/dev/null || true
        ;;
esac
"$BACKEND_BIN" stop 2>/dev/null || true

cargo build --manifest-path "$REPO_ROOT/desktop/src-tauri/Cargo.toml" --features tauri/custom-protocol

log "desktop ready -> $TAURI_BIN"

# ── Build summary ────────────────────────────────────────────────────────────
elapsed=$SECONDS
log "rebuild finished in ${elapsed}s"
if [[ "$SKIP_FRONTEND" == false ]]; then log "  frontend: $INTERFACE_DIR/dist/"; fi
if [[ "$SKIP_BACKEND" == false ]];  then log "  backend:  $BACKEND_BIN"; fi
log "  desktop:  $TAURI_BIN"

if [[ "$BUILD_ONLY" == true ]]; then
    log "build-only mode, skipping launch"
    exit 0
fi

# ══════════════════════════════════════════════════════════════════════════════
# LAUNCH
# ══════════════════════════════════════════════════════════════════════════════
# Spacebot.exe handles everything:
#   - Spawns the backend server and pipes logs to the console window
#   - Opens the Tauri frontend (webview)
#   - Opens Chrome DevTools (debug build)

log "launching Spacebot..."
log "  console  -> backend server logs (localhost:19898)"
log "  window   -> Tauri frontend app"
log "  devtools -> opens automatically (debug build)"

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        WIN_BIN="$(cygpath -w "$TAURI_BIN")"
        powershell -Command "Start-Process -FilePath '$WIN_BIN'"
        ;;
    Darwin*)
        open "$TAURI_BIN"
        ;;
    *)
        "$TAURI_BIN" &
        ;;
esac
