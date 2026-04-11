# syntax=docker/dockerfile:1.7
#
# The unified spacebot image. Builds both the spacebot binary and the
# embedded spacedrive server (sd-server). The spacedrive source is pinned
# via `.spacedrive-version` at the repo root; CI checks out that ref into
# `.build/spacedrive` and `spaceui` into `.build/spaceui` before running
# `docker build` so the spacedrive apps/web Vite config can resolve its
# cross-repo aliases at source paths.

# ---- Spacebot builder stage ----
# Compiles the React frontend and the Rust binary with the frontend embedded.
FROM rust:bookworm AS builder

# Install build dependencies:
#   protobuf-compiler — LanceDB protobuf codegen
#   cmake — onig_sys (regex), lz4-sys
#   libssl-dev — openssl-sys (reqwest TLS)
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    libprotobuf-dev \
    cmake \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:${PATH}"

# Node 22+ is required for the OpenCode embed Vite build.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# 1. Fetch and cache Rust dependencies.
#    cargo fetch needs a valid target, so we create stubs that get replaced later.
COPY Cargo.toml Cargo.lock ./
COPY vendor/ vendor/
RUN mkdir -p src/bin && echo "fn main() {}" > src/main.rs && touch src/lib.rs \
    && echo "fn main() {}" > src/bin/openapi_spec.rs \
    && cargo build --release --features metrics \
    && rm -rf src

# 2. Install frontend dependencies.
COPY interface/package.json interface/
RUN cd interface && bun install

# 3. Build the OpenCode embed bundle (live coding UI in Workers tab).
#    Must run before the frontend build so the embed assets in
#    interface/public/opencode-embed/ are included in the Vite output.
COPY scripts/build-opencode-embed.sh scripts/
COPY interface/opencode-embed-src/ interface/opencode-embed-src/
RUN ./scripts/build-opencode-embed.sh

# 4. Build the frontend (includes OpenCode embed assets from step 3).
COPY interface/ interface/
RUN cd interface && bun run build

# 5. Copy source and compile the real binary.
#    build.rs is skipped (SPACEBOT_SKIP_FRONTEND_BUILD=1) since the
#    frontend is already built above with the OpenCode embed included.
#    prompts/ is needed for include_str! in src/prompts/text.rs.
#    presets/ is needed for rust-embed in src/factory/presets.rs and
#    include_str! in src/identity/files.rs.
#    migrations/ is needed for sqlx::migrate! in src/db.rs.
#    docs/ is needed for rust-embed in src/self_awareness.rs.
#    AGENTS.md, README.md, CHANGELOG.md are needed for include_str! in src/self_awareness.rs.
COPY build.rs ./
COPY prompts/ prompts/
COPY presets/ presets/
COPY migrations/ migrations/
COPY docs/ docs/
COPY AGENTS.md README.md CHANGELOG.md ./
COPY src/ src/
RUN SPACEBOT_SKIP_FRONTEND_BUILD=1 cargo build --release --features metrics \
    && mv /build/target/release/spacebot /usr/local/bin/spacebot \
    && cargo clean -p spacebot --release --target-dir /build/target

# ---- Spacedrive builder stage ----
# Builds sd-server with the apps/web bundle embedded via rust-embed. Source
# is supplied via the build context at `.build/spacedrive` (pinned by
# `.spacedrive-version`) and `.build/spaceui`. The apps/web Vite config
# has aliases expecting `../../../spaceui/packages/*` and
# `../../../spacebot/packages/api-client/src` relative to
# `spacedrive/apps/web/`, so we arrange the three checkouts as siblings
# under `/workspace` before running bun install.
FROM rust:bookworm AS spacedrive_builder

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    libprotobuf-dev \
    cmake \
    libssl-dev \
    pkg-config \
    curl \
    ca-certificates \
    nasm \
    libheif-dev \
    libavcodec-dev \
    libavformat-dev \
    libavutil-dev \
    libswscale-dev

RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:${PATH}"

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# Stage the three repos as siblings. `.build/spacedrive` and `.build/spaceui`
# are populated by CI via actions/checkout with a target path; the
# api-client subtree comes from the spacebot build context directly.
COPY .build/spacedrive ./spacedrive
COPY .build/spaceui ./spaceui
COPY packages/api-client ./spacebot/packages/api-client

# Build the web UI bundle first so rust-embed has something to embed.
WORKDIR /workspace/spacedrive/apps/web
RUN bun install && bun run build

# Build sd-server with HEIF and FFmpeg features enabled.
WORKDIR /workspace/spacedrive
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/workspace/spacedrive/target \
    cargo build --release -p sd-server --features sd-core/heif,sd-core/ffmpeg \
    && cp target/release/sd-server /usr/local/bin/sd-server

# ---- Runtime stage ----
# Minimal runtime with Chrome runtime libraries for fetcher-downloaded Chromium.
# Chrome itself is downloaded on first browser tool use and cached on the volume.
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    curl \
    gh \
    bubblewrap \
    openssh-server \
    tini \
    # sd-server runtime deps (HEIF + FFmpeg shared libraries).
    libheif1 \
    libavcodec59 \
    libavformat59 \
    libavutil57 \
    libswscale6 \
    # Chrome runtime dependencies — required whether Chrome is system-installed
    # or downloaded by the built-in fetcher. The fetcher provides the browser
    # binary; these are the shared libraries it links against.
    fonts-liberation \
    libnss3 \
    libatk-bridge2.0-0 \
    libdrm2 \
    libxcomposite1 \
    libxdamage1 \
    libxrandr2 \
    libgbm1 \
    libasound2 \
    libpango-1.0-0 \
    libcairo2 \
    libcups2 \
    libxkbcommon0 \
    libxss1 \
    libxtst6 \
    libxfixes3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/spacebot /usr/local/bin/spacebot
COPY --from=spacedrive_builder /usr/local/bin/sd-server /usr/local/bin/sd-server
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

ENV SPACEBOT_DIR=/data
ENV SPACEBOT_DEPLOYMENT=docker
# Spacedrive server runs alongside spacebot under /data/spacedrive.
# The default SD_P2P=false keeps startup fast; override via env if you
# want Iroh P2P discovery inside the container.
ENV SD_AUTH=disabled
ENV SD_P2P=false
ENV SPACEDRIVE_DATA_DIR=/data/spacedrive
ENV SPACEDRIVE_PORT=8080
EXPOSE 19898 18789 9090 8080

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -f http://localhost:19898/api/health || exit 1

# tini is PID 1 so signals are reaped and zombie processes don't pile up.
# Our entrypoint handles fanning SIGTERM out to sd-server and spacebot.
ENTRYPOINT ["tini", "--", "docker-entrypoint.sh"]
CMD ["spacebot", "start", "--foreground"]
