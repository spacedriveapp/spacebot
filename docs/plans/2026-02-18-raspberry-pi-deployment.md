# Raspberry Pi Deployment Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Cross-compile Spacebot on Mac M4 Pro, deploy the binary to Raspberry Pi 4B, and run it as a systemd daemon with ZhipuAI/GLM models on Discord.

**Architecture:** Build a release binary for `aarch64-unknown-linux-gnu` on Mac using `cross` (Docker-based cross-compilation via OrbStack). Transfer binary to Pi, set SPACEBOT_DIR to the existing repo dir, and run as a systemd service that sources `.env` for all secrets.

**Tech Stack:** Rust/cross + OrbStack Docker, systemd, SSH/SCP, ZhipuAI GLM API, Discord via Serenity.

---

## Pre-flight notes

- Pi OS: `aarch64 GNU/Linux` (Debian, kernel 6.12)
- Pi has full repo cloned at `/home/tomas/spacebot/` (source + config.toml there)
- Pi has **no `.env` file** — must be created before starting service
- Docker (OrbStack) available on Mac — using `cross` for cross-compilation
- Existing `config.toml` on Pi has correct GLM routing; Slack/Telegram bindings need placeholder cleanup
- `SPACEBOT_DIR=/home/tomas/spacebot` → config at `/home/tomas/spacebot/config.toml` ✓

---

### Task 1: Install `cross` and add the Rust target

**Files:**
- No file changes (toolchain setup only)

**Step 1: Add the aarch64-linux target to rustup**

```bash
rustup target add aarch64-unknown-linux-gnu
```

Expected: `info: component 'rust-std' for target 'aarch64-unknown-linux-gnu' installed`

**Step 2: Install `cross`**

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

Expected: `Installed package \`cross\`` — takes 1-2 min.

**Step 3: Verify cross and Docker are available**

```bash
cross --version
docker info | grep -E "Server|Context"
```

Expected: `cross 0.2.x` and Docker context showing `orbstack`.

---

### Task 2: Create `Cross.toml` for build dependencies

`cross` uses a Docker image for the build environment. LanceDB needs `protobuf-compiler` + `cmake`; reqwest needs `libssl-dev`.

**Files:**
- Create: `/Users/tomasmach/Code/spacebot/Cross.toml`

**Step 1: Create the file**

```toml
[target.aarch64-unknown-linux-gnu]
pre-build = [
    "dpkg --add-architecture arm64",
    "apt-get update",
    "apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev cmake libssl-dev:arm64 pkg-config",
]
```

> **Note:** `libssl-dev:arm64` installs the OpenSSL headers for the target architecture. `protobuf-compiler` and `cmake` run on the host (build machine) architecture.

**Step 2: Commit**

```bash
git add Cross.toml
git commit -m "chore: add Cross.toml for aarch64-linux cross-compilation"
```

---

### Task 3: Cross-compile the release binary

**Files:**
- None — build artifact only

**Step 1: Run the cross build (skip frontend to avoid bun in Docker)**

From `/Users/tomasmach/Code/spacebot/`:

```bash
SPACEBOT_SKIP_FRONTEND_BUILD=1 cross build \
  --target aarch64-unknown-linux-gnu \
  --release \
  2>&1 | tee /tmp/cross-build.log
```

Expected: `Compiling spacebot v0.1.7` ... `Finished release [optimized]` — takes 10-30 min first time.

If the build fails due to missing host packages inside the Docker image, check `/tmp/cross-build.log` for the specific error, then update `Cross.toml` pre-build commands accordingly.

**Step 2: Verify the binary exists and is the right architecture**

```bash
file target/aarch64-unknown-linux-gnu/release/spacebot
```

Expected: `ELF 64-bit LSB pie executable, ARM aarch64, version 1 (SYSV), dynamically linked`

**Step 3: Check binary size (sanity check)**

```bash
ls -lh target/aarch64-unknown-linux-gnu/release/spacebot
```

Expected: roughly 50-150 MB.

---

### Task 4: Create `.env` file on the Pi

The Pi has no `.env` file at `/home/tomas/spacebot/.env` even though the systemd service will reference it. Create it now with the required variables.

**Step 1: SSH into the Pi and create the .env file**

```bash
ssh tomas@tomas-pi.local
```

Then on the Pi:

```bash
cat > /home/tomas/spacebot/.env << 'EOF'
# ZhipuAI / GLM
ZHIPU_API_KEY=your_zhipu_api_key_here

# Discord
DISCORD_BOT_TOKEN=your_discord_bot_token_here
EOF
chmod 600 /home/tomas/spacebot/.env
```

Replace `your_zhipu_api_key_here` and `your_discord_bot_token_here` with real values.

**Step 2: Verify it's readable**

```bash
cat /home/tomas/spacebot/.env
# should show both vars
```

---

### Task 5: Update `config.toml` on the Pi — disable Slack and Telegram

The existing config.toml has Slack and Telegram sections with placeholder workspace IDs and chat IDs. Disable them so the bot only uses Discord.

**Files:**
- Modify: `/home/tomas/spacebot/config.toml` (on the Pi)

**Step 1: Replace the config**

SSH into the Pi and overwrite config.toml:

```bash
cat > /home/tomas/spacebot/config.toml << 'EOF'
[llm]
zhipu_key = "env:ZHIPU_API_KEY"

[defaults.routing]
channel   = "zhipu/glm-4-plus"
branch    = "zhipu/glm-4-plus"
worker    = "zhipu/glm-4-flash"
compactor = "zhipu/glm-4-flash"
cortex    = "zhipu/glm-4-flash"

[defaults.routing.task_overrides]
coding = "zhipu/glm-4-plus"

[defaults.routing.fallbacks]
"zhipu/glm-4-plus" = ["zhipu/glm-4-flash"]

[[agents]]
id = "machmonstrum"
default = true

[messaging.discord]
enabled = true
token = "env:DISCORD_BOT_TOKEN"

[[bindings]]
agent_id = "machmonstrum"
channel  = "discord"
guild_id = "533023589096488980"
EOF
```

**Step 2: Verify the config is valid**

```bash
cat /home/tomas/spacebot/config.toml
```

Expected: clean TOML with only Discord, no Slack/Telegram.

---

### Task 6: Transfer binary to Pi

**Step 1: SCP binary from Mac to Pi (run on Mac)**

```bash
scp target/aarch64-unknown-linux-gnu/release/spacebot \
    tomas@tomas-pi.local:/home/tomas/spacebot/spacebot
```

Expected: progress bar, completes without error.

**Step 2: Make it executable on Pi**

```bash
ssh tomas@tomas-pi.local "chmod +x /home/tomas/spacebot/spacebot"
```

**Step 3: Quick smoke test on Pi**

```bash
ssh tomas@tomas-pi.local "
  export \$(cat /home/tomas/spacebot/.env | xargs)
  SPACEBOT_DIR=/home/tomas/spacebot /home/tomas/spacebot/spacebot --version
"
```

Expected: `spacebot 0.1.7` (or similar version line).

---

### Task 7: Create systemd service unit

**Files:**
- Create: `/etc/systemd/system/spacebot.service` (on the Pi, via sudo)

**Step 1: SSH into the Pi and create the service file**

```bash
sudo tee /etc/systemd/system/spacebot.service << 'EOF'
[Unit]
Description=Spacebot AI Discord daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=tomas
WorkingDirectory=/home/tomas/spacebot
EnvironmentFile=/home/tomas/spacebot/.env
Environment=SPACEBOT_DIR=/home/tomas/spacebot
ExecStart=/home/tomas/spacebot/spacebot
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=spacebot

[Install]
WantedBy=multi-user.target
EOF
```

**Step 2: Reload systemd**

```bash
sudo systemctl daemon-reload
```

Expected: no output (silent success).

---

### Task 8: Enable and start the service

**Step 1: Enable service (auto-start on boot)**

```bash
sudo systemctl enable spacebot
```

Expected: `Created symlink /etc/systemd/system/multi-user.target.wants/spacebot.service → /etc/systemd/system/spacebot.service.`

**Step 2: Start the service**

```bash
sudo systemctl start spacebot
```

**Step 3: Check service status**

```bash
sudo systemctl status spacebot
```

Expected: `Active: active (running)` — wait ~10 seconds for startup.

**Step 4: Tail the logs for startup output**

```bash
sudo journalctl -u spacebot -f --no-pager
```

Watch for:
- `Spacebot vX.X.X starting` or similar initialization message
- `Discord gateway connected` or `Connected to Discord`
- No `ERROR` lines about missing API keys

Press Ctrl+C to stop tailing.

---

### Task 9: Verify Discord bot is online

**Step 1: Check Discord**

Open Discord and navigate to your server (guild ID `533023589096488980`). The bot should appear online in the member list.

**Step 2: Send a test message in any channel**

Type a message mentioning the bot or just send a message in a channel the bot can read. Expect a response from the `machmonstrum` agent.

**Step 3: If bot is offline — check logs**

```bash
sudo journalctl -u spacebot -n 100 --no-pager
```

Common issues:
- `Invalid token` → check `DISCORD_BOT_TOKEN` in `.env`
- `invalid api key` → check `ZHIPU_API_KEY` in `.env`
- `No such file or directory: spacebot` → binary not executable or wrong path
- Crash on startup → check if binary was built correctly (re-run Task 3)

---

## Quick Reference: Useful Pi Commands After Setup

```bash
# Status
sudo systemctl status spacebot

# Restart
sudo systemctl restart spacebot

# View last 50 log lines
sudo journalctl -u spacebot -n 50 --no-pager

# Follow live logs
sudo journalctl -u spacebot -f

# Stop
sudo systemctl stop spacebot

# Update binary (after rebuilding on Mac)
scp target/aarch64-unknown-linux-gnu/release/spacebot tomas@tomas-pi.local:/home/tomas/spacebot/spacebot
ssh tomas@tomas-pi.local "sudo systemctl restart spacebot"
```

## Data Location

Once running, agent data is stored at:
```
/home/tomas/spacebot/agents/machmonstrum/
├── workspace/
│   ├── SOUL.md          # Bot personality (edit to customize)
│   ├── IDENTITY.md      # Bot name/nature
│   └── USER.md          # Info about you
└── data/
    ├── spacebot.db      # SQLite (conversations, memory graph)
    ├── lancedb/         # Vector database (semantic memory)
    └── config.redb      # Runtime key-value settings
```
