# Docker

Run Spacebot in a container with the unified Spacebot image.

## Quick Start

```bash
docker run -d \
  --name spacebot \
  -e ANTHROPIC_API_KEY="sk-ant-..." \
  -v spacebot-data:/data \
  -p 19898:19898 \
  ghcr.io/spacedriveapp/spacebot:latest
```

The web UI is available at `http://localhost:19898`.

## Image Behavior

There is one published image: `ghcr.io/spacedriveapp/spacebot`.

- `latest` tracks the rolling release
- `vX.Y.Z` pins a specific release
- Browser support is built in: Chromium is downloaded on first browser-tool use and cached under `/data`
- Legacy `-slim` / `-full` tags are deprecated

## Data Volume

All persistent data lives at `/data` inside the container. Mount a volume here.

```
/data/
├── config.toml              # optional, can use env vars instead
├── embedding_cache/         # FastEmbed model cache (~100MB, downloaded on first run)
├── agents/
│   └── main/
│       ├── SOUL.md          # agent personality and voice
│       ├── IDENTITY.md      # agent purpose and scope
│       ├── ROLE.md          # agent responsibilities
│       ├── workspace/       # working directory (sandbox boundary)
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
  ghcr.io/spacedriveapp/spacebot:latest
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
  ghcr.io/spacedriveapp/spacebot:latest
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
    image: ghcr.io/spacedriveapp/spacebot:latest
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

### Browser Workers

No alternate image is needed. The browser tool downloads Chromium on demand and
caches it on the mounted `/data` volume.

## Building the Image

From the spacebot repo root:

```bash
docker build -t spacebot:local .
```

The multi-stage Dockerfile has two stages:

1. **Builder stage** -- Rust toolchain + Bun. Compiles the React frontend, then builds the Rust binary with the frontend embedded.
2. **Runtime stage** -- Minimal Debian runtime with Chrome's shared-library dependencies. The browser binary itself is fetched on demand.

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

The web dashboard also includes **Settings → Updates** with status details, one-click controls (Docker), and manual command snippets.

`latest` is supported and continues to receive updates. Use explicit version tags only when you want controlled rollouts.

### Manual Update

```bash
docker compose pull spacebot
docker compose up -d --force-recreate spacebot
```

### One-Click Update

Mount `/var/run/docker.sock` into the Spacebot container to enable the **Update now** button in the UI. Without the socket mount, update checks still work but apply is manual.

One-click updates are intended for containers running Spacebot release tags. If you're running a custom/self-built image, rebuild your image and recreate the container.

### Native / Source Builds

If Spacebot is installed from source (`cargo install --path .` or a local release build), updates are manual: pull latest source, rebuild/reinstall, then restart.

## CI / Releases

Images are built and pushed to `ghcr.io/spacedriveapp/spacebot` via GitHub Actions (`.github/workflows/release.yml`).

**Triggers:**

- Push a `v*` tag (recommended: `cargo bump patch`)
- Manual dispatch from the Actions tab

**Tags pushed per release:**

| Tag      | Description       |
| -------- | ----------------- |
| `v0.1.0` | Versioned release |
| `latest` | Rolling release   |
