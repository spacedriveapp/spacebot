#!/usr/bin/env bash
# Build the OpenCode embed bundle from a pinned upstream commit.
#
# Clones opencode at the pinned commit, copies our embed entry points
# into the tree, builds with Vite, and copies the output into
# interface/public/opencode-embed/ for the Spacebot interface to serve.
#
# Requirements:
#   - git, node (v22+), bun
#   - fnm (optional, used to switch to node 24 if available)
#
# Usage:
#   ./scripts/build-opencode-embed.sh
#
# The OpenCode commit is pinned in OPENCODE_COMMIT below. Update it
# when pulling in a new upstream version.

set -euo pipefail

OPENCODE_REPO="https://github.com/anomalyco/opencode"
OPENCODE_COMMIT="114eb4244"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE_DIR="${REPO_ROOT}/.opencode-build-cache"
EMBED_SRC="${REPO_ROOT}/interface/opencode-embed-src"
OUTPUT_DIR="${REPO_ROOT}/interface/public/opencode-embed"

# ---------------------------------------------------------------------------
# 0. Pre-flight: verify Node 22+ is available
# ---------------------------------------------------------------------------
# Try fnm first (non-interactive shells don't source shell init files)
if command -v fnm &>/dev/null; then
  eval "$(fnm env)" 2>/dev/null || true
  fnm use v24.14.0 2>/dev/null || true
fi

if ! command -v node &>/dev/null; then
  echo "[opencode-embed] ERROR: node not found."
  echo ""
  echo "  This build requires Node 22+. Install via fnm:"
  echo "    curl -fsSL https://fnm.vercel.app/install | bash"
  echo "    fnm install v24.14.0"
  echo ""
  echo "  Then re-run:"
  echo "    eval \"\$(fnm env)\" && fnm use v24.14.0 && ./scripts/build-opencode-embed.sh"
  exit 1
fi

NODE_MAJOR="$(node -v | sed 's/^v//' | cut -d. -f1)"
if [ "${NODE_MAJOR}" -lt 22 ]; then
  echo "[opencode-embed] ERROR: Node 22+ required (got $(node -v))."
  echo ""
  echo "  If you use fnm:"
  echo "    fnm install v24.14.0 && fnm use v24.14.0"
  echo ""
  echo "  Then re-run:"
  echo "    eval \"\$(fnm env)\" && fnm use v24.14.0 && ./scripts/build-opencode-embed.sh"
  exit 1
fi

echo "[opencode-embed] Using node $(node -v)"

# ---------------------------------------------------------------------------
# 1. Clone or fetch OpenCode at the pinned commit
# ---------------------------------------------------------------------------
if [ -d "${CACHE_DIR}/.git" ]; then
  echo "[opencode-embed] Fetching updates..."
  # If the commit is already available locally, skip the fetch entirely.
  if ! git -C "${CACHE_DIR}" cat-file -e "${OPENCODE_COMMIT}^{commit}" 2>/dev/null; then
    if [ -f "${CACHE_DIR}/.git/shallow" ]; then
      git -C "${CACHE_DIR}" fetch --progress --unshallow origin
    else
      git -C "${CACHE_DIR}" fetch --progress origin
    fi
  fi
  git -C "${CACHE_DIR}" checkout "${OPENCODE_COMMIT}" --force
else
  echo "[opencode-embed] Cloning opencode (this may take a minute)..."
  # Use --single-branch to skip other branches. --progress forces output
  # even when stderr is not a TTY (fixes apparent hang on non-interactive
  # terminals).
  git clone --progress --single-branch "${OPENCODE_REPO}" "${CACHE_DIR}"
  git -C "${CACHE_DIR}" checkout "${OPENCODE_COMMIT}" --force
fi

# ---------------------------------------------------------------------------
# 2. Copy embed source files into the OpenCode tree
# ---------------------------------------------------------------------------
APP_DIR="${CACHE_DIR}/packages/app"
echo "[opencode-embed] Copying embed source files..."
cp "${EMBED_SRC}/embed.tsx"            "${APP_DIR}/src/embed.tsx"
cp "${EMBED_SRC}/embed-entry.tsx"      "${APP_DIR}/src/embed-entry.tsx"
cp "${EMBED_SRC}/spacebot-theme.json"  "${APP_DIR}/src/spacebot-theme.json"
cp "${EMBED_SRC}/vite.config.embed.ts" "${APP_DIR}/vite.config.embed.ts"
cp "${EMBED_SRC}/index-embed.html"     "${APP_DIR}/index-embed.html"

# ---------------------------------------------------------------------------
# 2b. Apply compatibility patches for the running OpenCode binary version.
#     The embed is built from anomalyco/opencode@OPENCODE_COMMIT (1.2.15-based)
#     but spacebot runs against the latest installed OpenCode binary which may
#     have a different agent schema (no `mode`/`hidden` fields).
#
#     Root cause: when the OpenCode server restarts, the embed re-bootstraps
#     and calls GET /agent. If the server responds with an empty body during
#     startup, the SDK returns {data: {}} (empty object). The original code
#     uses `?? []` which only catches null/undefined — `{}` is truthy and
#     passes through, causing `.filter is not a function` on the object.
#
#     Fix: use Array.isArray() guards instead of nullish coalescing.
# ---------------------------------------------------------------------------
echo "[opencode-embed] Applying compatibility patches..."
LOCAL_TSX="${APP_DIR}/src/context/local.tsx"
BOOTSTRAP_TS="${APP_DIR}/src/context/global-sync/bootstrap.ts"

# Patch 1: local.tsx — guard the agent .filter() call with Array.isArray.
# The original line:  sync.data.agent.filter(...)
# After patch:        (Array.isArray(sync.data.agent) ? sync.data.agent : []).filter(...)
perl -pi -e \
  's/sync\.data\.agent\.filter\(/(Array.isArray(sync.data.agent) ? sync.data.agent : []).filter(/g' \
  "${LOCAL_TSX}"
if grep -q 'Array\.isArray(sync\.data\.agent)' "${LOCAL_TSX}"; then
  echo "[opencode-embed] Patched local.tsx: Array.isArray guard on agent filter"
else
  echo "[opencode-embed] ERROR: local.tsx patch did not apply — upstream may have changed."
  exit 1
fi

# Patch 2: bootstrap.ts — guard the setStore("agent", ...) call.
# The original line:  input.setStore("agent", x.data ?? [])
# After patch:        input.setStore("agent", Array.isArray(x.data) ? x.data : [])
perl -pi -e \
  's/input\.setStore\("agent",\s*x\.data\s*\?\?\s*\[\]\)/input.setStore("agent", Array.isArray(x.data) ? x.data : [])/g' \
  "${BOOTSTRAP_TS}"
if grep -q 'Array\.isArray(x\.data)' "${BOOTSTRAP_TS}"; then
  echo "[opencode-embed] Patched bootstrap.ts: Array.isArray guard on agent store"
else
  echo "[opencode-embed] ERROR: bootstrap.ts patch did not apply — upstream may have changed."
  exit 1
fi

# ---------------------------------------------------------------------------
# 3. Install dependencies
# ---------------------------------------------------------------------------
echo "[opencode-embed] Installing dependencies..."
if [ "${CI:-}" = "true" ]; then
  # In CI the lockfile must be up-to-date; fail loudly on drift.
  (cd "${CACHE_DIR}" && bun install --frozen-lockfile) || {
    echo "[opencode-embed] ERROR: bun install --frozen-lockfile failed."
    echo "  The lockfile is out of sync. Run 'bun install' locally and commit the updated lockfile."
    exit 1
  }
else
  # Locally, try frozen first but fall back to a regular install.
  (cd "${CACHE_DIR}" && bun install --frozen-lockfile 2>/dev/null || bun install)
fi

# ---------------------------------------------------------------------------
# 4. Build the embed bundle
# ---------------------------------------------------------------------------
echo "[opencode-embed] Building embed bundle..."
(cd "${APP_DIR}" && ./node_modules/.bin/vite build --config vite.config.embed.ts)

# ---------------------------------------------------------------------------
# 5. Copy output to interface/public/opencode-embed/
# ---------------------------------------------------------------------------
echo "[opencode-embed] Copying build output..."
rm -rf "${OUTPUT_DIR}"
mkdir -p "${OUTPUT_DIR}"

cp -r "${APP_DIR}/dist-embed/assets" "${OUTPUT_DIR}/assets"

# Generate a simple manifest.json with the hashed entry filenames.
# The frontend fetches /opencode-embed/manifest.json to discover them.
ENTRY_JS="$(cd "${OUTPUT_DIR}/assets" && ls index-embed-*.js 2>/dev/null | head -1)"
ENTRY_CSS="$(cd "${OUTPUT_DIR}/assets" && ls index-embed-*.css 2>/dev/null | head -1)"

if [ -z "${ENTRY_JS}" ] || [ -z "${ENTRY_CSS}" ]; then
  echo "[opencode-embed] ERROR: Could not find entry JS/CSS in build output."
  echo "  JS: ${ENTRY_JS:-not found}"
  echo "  CSS: ${ENTRY_CSS:-not found}"
  echo "  Contents of assets/:"
  ls "${OUTPUT_DIR}/assets/" | grep index-embed || echo "  (none)"
  exit 1
fi

printf '{"js":"assets/%s","css":"assets/%s"}\n' "${ENTRY_JS}" "${ENTRY_CSS}" > "${OUTPUT_DIR}/manifest.json"
echo "[opencode-embed] Manifest: js=assets/${ENTRY_JS}, css=assets/${ENTRY_CSS}"

# Count output size
TOTAL_SIZE="$(du -sh "${OUTPUT_DIR}" | cut -f1)"
JS_COUNT="$(find "${OUTPUT_DIR}" -name '*.js' | wc -l | tr -d ' ')"
CSS_COUNT="$(find "${OUTPUT_DIR}" -name '*.css' | wc -l | tr -d ' ')"

echo "[opencode-embed] Done! ${TOTAL_SIZE} total (${JS_COUNT} JS, ${CSS_COUNT} CSS)"
echo "[opencode-embed] Output: ${OUTPUT_DIR}"
