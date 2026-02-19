# syntax=docker/dockerfile:1.6
# ---- Builder stage ----
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

WORKDIR /build

# 1. Fetch and cache Rust dependencies.
#    cargo fetch needs a valid target, so we create stubs that get replaced later.
COPY Cargo.toml Cargo.lock ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs \
    && cargo build --release \
    && rm -rf src

# 2. Build the frontend.
COPY interface/package.json interface/
RUN --mount=type=cache,target=/root/.bun/install/cache \
    cd interface && bun install
COPY interface/ interface/
RUN --mount=type=cache,target=/root/.bun/install/cache \
    cd interface && bun run build

# 3. Copy source and compile the real binary.
#    build.rs runs the frontend build (already done above, node_modules present).
#    prompts/ is needed for include_str! in src/prompts/text.rs.
#    migrations/ is needed for sqlx::migrate! in src/db.rs.
COPY build.rs ./
COPY prompts/ prompts/
COPY migrations/ migrations/
COPY src/ src/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    SPACEBOT_SKIP_FRONTEND_BUILD=1 cargo build --release \
    && cp /build/target/release/spacebot /usr/local/bin/spacebot

# ---- Slim stage ----
# Minimal runtime with just the binary. No browser.
FROM debian:bookworm-slim AS slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libsqlite3-0 \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/spacebot /usr/local/bin/spacebot
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

ENV SPACEBOT_DIR=/data
ENV SPACEBOT_DEPLOYMENT=docker
EXPOSE 19898 18789

VOLUME /data

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -f http://localhost:19898/api/health || exit 1

ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["spacebot", "start", "--foreground"]

# ---- Full stage ----
# Slim + Chromium for browser workers.
FROM slim AS full

RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium \
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
    libxss1 \
    libxtst6 \
    && rm -rf /var/lib/apt/lists/*

ENV CHROME_PATH=/usr/bin/chromium
ENV CHROME_FLAGS="--no-sandbox --disable-dev-shm-usage --disable-gpu"
