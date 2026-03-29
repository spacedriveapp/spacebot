# Spacebot MCP Bridge

Connect CLI agents like **Claude Code**, **Codex**, and **VS Code** to your Spacebot instance via MCP (Model Context Protocol).

## Quick Start

```bash
# 1. Start Spacebot
spacebot local

# 2. Add to Claude Code settings (~/.claude/settings.json)
```

```json
{
  "mcpServers": {
    "spacebot": {
      "command": "python3",
      "args": ["/path/to/spacebot/mcp-bridge/spacebot_mcp_server.py"],
      "env": {
        "SPACEBOT_URL": "http://127.0.0.1:19898"
      }
    }
  }
}
```

## Available Tools

| Tool | Description |
|------|-------------|
| `spacebot_list_agents` | List all agents |
| `spacebot_list_tasks` | List Kanban board tasks |
| `spacebot_create_task` | Create a task (with dependencies) |
| `spacebot_update_task` | Update task status/priority |
| `spacebot_execute_task` | Execute task (spawns a worker) |
| `spacebot_list_workers` | List active workers |
| `spacebot_send_message` | Send message to a channel |
| `spacebot_list_channels` | List agent channels |
| `spacebot_status` | System status |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `SPACEBOT_URL` | `http://127.0.0.1:19898` | Spacebot API URL |
| `SPACEBOT_TOKEN` | _(empty)_ | API auth token (if configured) |

## Requirements

- Python 3.8+ (no external dependencies)
- Running Spacebot instance
