# Docker

Run Spacebot in a container. Two image variants: `slim` (no browser) and `full` (includes Chromium for browser workers).

## Quick Start

```bash
docker run -d \
  --name spacebot \
  -e ANTHROPIC_API_KEY="sk-ant-..." \
  -v spacebot-data:/data \
  -p 19898:19898 \
  ghcr.io/spacedriveapp/spacebot:slim
```

The web UI is available at `http://localhost:19898`.

## Image Variants

### `spacebot:slim`

Minimal runtime. Everything works except the browser tool.

- Base: `debian:bookworm-slim`
- Size: ~150MB
- Includes: Spacebot binary, CA certs, SQLite libs, embedded frontend

### `spacebot:full`

Includes Chromium for browser workers (headless Chrome automation via CDP).

- Base: `debian:bookworm-slim` + Chromium
- Size: ~800MB
- Includes: everything in slim + Chromium + browser dependencies

## Data Volume

All persistent data lives at `/data` inside the container. Mount a volume here.

```
/data/
├── config.toml              # optional, can use env vars instead
├── embedding_cache/         # FastEmbed model cache (~100MB, downloaded on first run)
├── agents/
│   └── main/
│       ├── workspace/       # identity files (SOUL.md, IDENTITY.md, USER.md)
│       ├── data/            # SQLite, LanceDB, redb databases
│       └── archives/        # compaction transcripts
└── logs/                    # log files (daily rotation)
```

On first launch with no config, Spacebot creates a default `main` agent with template identity files. The FastEmbed model (~100MB) downloads on first memory operation -- subsequent starts use the cache.

## Configuration

### Environment Variables

The simplest approach. No config file needed.

```bash
docker run -d \
  --name spacebot \
  -e ANTHROPIC_API_KEY="sk-ant-..." \
  -e DISCORD_BOT_TOKEN="..." \
  -v spacebot-data:/data \
  -p 19898:19898 \
  ghcr.io/spacedriveapp/spacebot:slim
```

Available environment variables:

| Variable                 | Description            |
| ------------------------ | ---------------------- |
| `ANTHROPIC_API_KEY`      | Anthropic API key      |
| `OPENAI_API_KEY`         | OpenAI API key         |
| `OPENROUTER_API_KEY`     | OpenRouter API key     |
| `DISCORD_BOT_TOKEN`      | Discord bot token      |
| `SLACK_BOT_TOKEN`        | Slack bot token        |
| `SLACK_APP_TOKEN`        | Slack app token        |
| `BRAVE_SEARCH_API_KEY`   | Brave Search API key   |
| `SPACEBOT_CHANNEL_MODEL` | Override channel model |
| `SPACEBOT_WORKER_MODEL`  | Override worker model  |

### Config File

Mount a config file into the volume for full control:

```bash
docker run -d \
  --name spacebot \
  -v spacebot-data:/data \
  -v ./config.toml:/data/config.toml:ro \
  -p 19898:19898 \
  ghcr.io/spacedriveapp/spacebot:slim
```

Config values can reference environment variables with `env:VAR_NAME`:

```toml
[llm]
anthropic_key = "env:ANTHROPIC_API_KEY"
```

See [config.md](config.md) for the full config reference.

## Docker Compose

```yaml
services:
  spacebot:
    image: ghcr.io/spacedriveapp/spacebot:slim
    container_name: spacebot
    restart: unless-stopped
    ports:
      - "19898:19898"
    volumes:
      - spacebot-data:/data
    environment:
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      # Optional:
      # - DISCORD_BOT_TOKEN=${DISCORD_BOT_TOKEN}

volumes:
  spacebot-data:
```

### With Browser Workers

```yaml
services:
  spacebot:
    image: ghcr.io/spacedriveapp/spacebot:full
    container_name: spacebot
    restart: unless-stopped
    ports:
      - "19898:19898"
    volumes:
      - spacebot-data:/data
    environment:
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
    # Chromium needs these for headless operation
    security_opt:
      - seccomp=unconfined
    shm_size: 1gb

volumes:
  spacebot-data:
```

The `shm_size` and `seccomp` settings are needed for Chromium to run properly in a container.

## Building the Image

From the spacebot repo root:

```bash
# Slim (no browser)
docker build --target slim -t spacebot:slim .

# Full (with Chromium)
docker build --target full -t spacebot:full .
```

The multi-stage Dockerfile:

1. **Builder stage** -- Rust toolchain + Bun. Compiles the React frontend, then builds the Rust binary with the frontend embedded.
2. **Slim stage** -- Minimal Debian runtime with the compiled binary.
3. **Full stage** -- Slim + Chromium and its dependencies.

Build time is ~5-10 minutes on first build (downloading and compiling Rust dependencies). Subsequent builds use the cargo cache.

## Ports

| Port  | Service                                 |
| ----- | --------------------------------------- |
| 19898 | HTTP API + Web UI                       |
| 18789 | Webhook receiver (if enabled in config) |

The API server binds to `0.0.0.0` inside the container (overriding the default `127.0.0.1` bind). The webhook port is only needed if you enable the webhook messaging adapter.

## Health Check

The API server responds to `GET /api/health`. Use this for container health checks:

```yaml
healthcheck:
  test: ["CMD", "curl", "-f", "http://localhost:19898/api/health"]
  interval: 30s
  timeout: 5s
  retries: 3
```

## Container Behavior

- Spacebot runs in **foreground mode** (`--foreground`) inside the container. No daemonization.
- Logs go to stdout/stderr. Use `docker logs` to view them.
- Graceful shutdown on `SIGTERM` (what `docker stop` sends). Drains active channels, closes database connections.
- The PID file and Unix socket (used in daemon mode) are not created.

## Updates

Spacebot checks for new releases on startup and every hour. When a new version is available, a banner appears in the web UI.

The web dashboard also includes **Settings → Updates** with status details, one-click controls (Docker / Podman), and manual command snippets.

`latest` is supported and continues to receive updates (it tracks the rolling `full` image). Use explicit version tags only when you want controlled rollouts.

### Manual Update

```bash
docker compose pull spacebot
docker compose up -d --force-recreate spacebot
```

### One-Click Update

Mount a container runtime socket into the Spacebot container to enable the **Update now** button in the UI. Without the socket mount, update checks still work but apply is manual.

- Docker: `/var/run/docker.sock:/var/run/docker.sock`
- Podman (rootful): `/run/podman/podman.sock:/run/podman/podman.sock`
- Podman (rootless): `${XDG_RUNTIME_DIR}/podman/podman.sock:/run/podman/podman.sock`

One-click updates are intended for containers running Spacebot release tags. If you're running a custom/self-built image, rebuild your image and recreate the container.

For Podman one-click updates, set `SPACEBOT_DEPLOYMENT=docker` and ensure the Podman socket service is enabled (`systemctl --user enable --now podman.socket` for rootless, `sudo systemctl enable --now podman.socket` for rootful).

On Fedora/RHEL with SELinux enforcing, include `--security-opt label=disable` when mounting the Podman socket.

### Native / Source Builds

If Spacebot is installed from source (`cargo install --path .` or a local release build), updates are manual: pull latest source, rebuild/reinstall, then restart.

## CI / Releases

Images are built and pushed to `ghcr.io/spacedriveapp/spacebot` via GitHub Actions (`.github/workflows/release.yml`).

**Triggers:**

- Push a `v*` tag (recommended: `cargo bump patch`)
- Manual dispatch from the Actions tab

**Tags pushed per release:**

| Tag           | Description                |
| ------------- | -------------------------- |
| `v0.1.0-slim` | Versioned slim             |
| `v0.1.0-full` | Versioned full             |
| `v0.1.0`      | Versioned (points to full) |
| `slim`        | Rolling slim               |
| `full`        | Rolling full               |
| `latest`      | Rolling (points to full)   |

The `latest` tag always points to the `full` variant.
