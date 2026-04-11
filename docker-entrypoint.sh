#!/bin/sh
set -e

mkdir -p "$SPACEBOT_DIR"
mkdir -p "$SPACEBOT_DIR/tools/bin"
mkdir -p "$SPACEDRIVE_DATA_DIR"

# Generate config.toml from environment variables when no config file exists.
# Once a config.toml is present on the volume, this is skipped entirely.
if [ ! -f "$SPACEBOT_DIR/config.toml" ]; then
    cat > "$SPACEBOT_DIR/config.toml" <<EOF
[api]
bind = "::"

[llm]
anthropic_key = "env:ANTHROPIC_API_KEY"
openai_key = "env:OPENAI_API_KEY"
openrouter_key = "env:OPENROUTER_API_KEY"

[spacedrive]
enabled = true
api_url = "http://127.0.0.1:${SPACEDRIVE_PORT}"
web_url = "${SPACEDRIVE_PUBLIC_URL:-http://localhost:${SPACEDRIVE_PORT}}"
EOF

    # Discord adapter
    if [ -n "$DISCORD_BOT_TOKEN" ]; then
        cat >> "$SPACEBOT_DIR/config.toml" <<EOF

[messaging.discord]
enabled = true
token = "env:DISCORD_BOT_TOKEN"
EOF
        if [ -n "$DISCORD_DM_ALLOWED_USERS" ]; then
            # Comma-separated user IDs -> TOML array
            DM_ARRAY=$(echo "$DISCORD_DM_ALLOWED_USERS" | sed 's/[[:space:]]//g' | sed 's/,/", "/g')
            cat >> "$SPACEBOT_DIR/config.toml" <<EOF
dm_allowed_users = ["$DM_ARRAY"]
EOF
        fi
    fi

    # Telegram adapter
    if [ -n "$TELEGRAM_BOT_TOKEN" ]; then
        cat >> "$SPACEBOT_DIR/config.toml" <<EOF

[messaging.telegram]
enabled = true
token = "env:TELEGRAM_BOT_TOKEN"
EOF
    fi

    # Webhook adapter
    if [ -n "$WEBHOOK_ENABLED" ]; then
        cat >> "$SPACEBOT_DIR/config.toml" <<EOF

[messaging.webhook]
enabled = true
bind = "0.0.0.0"
EOF
    fi

    # Default agent
    cat >> "$SPACEBOT_DIR/config.toml" <<EOF

[[agents]]
id = "main"
default = true
EOF

    # Discord binding
    if [ -n "$DISCORD_GUILD_ID" ]; then
        cat >> "$SPACEBOT_DIR/config.toml" <<EOF

[[bindings]]
agent_id = "main"
channel = "discord"
guild_id = "$DISCORD_GUILD_ID"
EOF
        if [ -n "$DISCORD_CHANNEL_IDS" ]; then
            CH_ARRAY=$(echo "$DISCORD_CHANNEL_IDS" | sed 's/[[:space:]]//g' | sed 's/,/", "/g')
            cat >> "$SPACEBOT_DIR/config.toml" <<EOF
channel_ids = ["$CH_ARRAY"]
EOF
        fi
        if [ -n "$DISCORD_DM_ALLOWED_USERS" ]; then
            DM_ARRAY=$(echo "$DISCORD_DM_ALLOWED_USERS" | sed 's/[[:space:]]//g' | sed 's/,/", "/g')
            cat >> "$SPACEBOT_DIR/config.toml" <<EOF
dm_allowed_users = ["$DM_ARRAY"]
EOF
        fi
    fi

    # Telegram binding
    if [ -n "$TELEGRAM_CHAT_ID" ]; then
        cat >> "$SPACEBOT_DIR/config.toml" <<EOF

[[bindings]]
agent_id = "main"
channel = "telegram"
chat_id = "$TELEGRAM_CHAT_ID"
EOF
    fi

    echo "Generated config.toml from environment variables"
fi

# ---- Start sd-server in the background ----
#
# sd-server holds the daemon + HTTP API + embedded web UI. It listens on
# $SPACEDRIVE_PORT (8080 by default) and serves both /rpc and the Explorer
# iframe target. Data lives at $SPACEDRIVE_DATA_DIR on the same volume as
# spacebot's config, so a single `-v /path:/data` mount persists both.

SPACEDRIVE_PID=""
SPACEBOT_PID=""

cleanup() {
    exit_code=$?
    if [ -n "$SPACEBOT_PID" ]; then
        kill -TERM "$SPACEBOT_PID" 2>/dev/null || true
    fi
    if [ -n "$SPACEDRIVE_PID" ]; then
        kill -TERM "$SPACEDRIVE_PID" 2>/dev/null || true
    fi
    wait 2>/dev/null || true
    exit "$exit_code"
}
trap cleanup INT TERM EXIT

echo "Starting sd-server on :${SPACEDRIVE_PORT} (data: ${SPACEDRIVE_DATA_DIR})"
DATA_DIR="$SPACEDRIVE_DATA_DIR" \
SD_AUTH="$SD_AUTH" \
SD_P2P="$SD_P2P" \
sd-server --port "$SPACEDRIVE_PORT" &
SPACEDRIVE_PID=$!

# Wait up to 120s for sd-server to be healthy. Iroh networking init can
# take a while on first boot; we log a warning if it's slow but don't
# block spacebot startup indefinitely.
echo "Waiting for sd-server health..."
HEALTHY=0
for i in $(seq 1 120); do
    if curl -sf "http://127.0.0.1:${SPACEDRIVE_PORT}/health" >/dev/null 2>&1; then
        HEALTHY=1
        break
    fi
    if ! kill -0 "$SPACEDRIVE_PID" 2>/dev/null; then
        echo "WARNING: sd-server exited before becoming healthy; continuing without embedded Spacedrive"
        SPACEDRIVE_PID=""
        break
    fi
    sleep 1
done

if [ "$HEALTHY" = "1" ]; then
    echo "sd-server is healthy"
elif [ -n "$SPACEDRIVE_PID" ]; then
    echo "WARNING: sd-server not healthy after 120s; continuing anyway (Files tab may be unavailable)"
fi

# ---- Start spacebot in the foreground ----
#
# Run spacebot under `wait` rather than `exec` so the trap above can still
# fire and clean up sd-server when the container is stopped.
"$@" &
SPACEBOT_PID=$!

wait "$SPACEBOT_PID"
