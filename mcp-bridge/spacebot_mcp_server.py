#!/usr/bin/env python3
"""
Spacebot MCP Server Bridge

Exposes Spacebot's HTTP API as MCP tools for CLI agents like Claude Code, Codex, etc.

Usage:
    python spacebot_mcp_server.py [--url http://127.0.0.1:19898] [--token YOUR_TOKEN]

Add to Claude Code's .claude/settings.json:
    {
        "mcpServers": {
            "spacebot": {
                "command": "python3",
                "args": ["/path/to/spacebot_mcp_server.py"],
                "env": {
                    "SPACEBOT_URL": "http://127.0.0.1:19898"
                }
            }
        }
    }
"""

import json
import os
import sys
import urllib.request
import urllib.error
from typing import Any

SPACEBOT_URL = os.environ.get("SPACEBOT_URL", "http://127.0.0.1:19898")
SPACEBOT_TOKEN = os.environ.get("SPACEBOT_TOKEN", "")


def api_request(method: str, path: str, body: dict | None = None) -> dict:
    """Make an HTTP request to the Spacebot API."""
    url = f"{SPACEBOT_URL}/api{path}"
    data = json.dumps(body).encode() if body else None
    headers = {"Content-Type": "application/json"}
    if SPACEBOT_TOKEN:
        headers["Authorization"] = f"Bearer {SPACEBOT_TOKEN}"

    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        error_body = e.read().decode() if e.fp else str(e)
        return {"error": f"HTTP {e.code}: {error_body}"}
    except urllib.error.URLError as e:
        return {"error": f"Connection failed: {e.reason}. Is Spacebot running at {SPACEBOT_URL}?"}


# -- MCP Tool Definitions --

TOOLS = [
    {
        "name": "spacebot_list_agents",
        "description": "List all Spacebot agents and their status",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "spacebot_list_tasks",
        "description": "List tasks for a Spacebot agent (Kanban board)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
                "status": {
                    "type": "string",
                    "description": "Filter by status: pending_approval, backlog, ready, in_progress, done",
                    "enum": ["pending_approval", "backlog", "ready", "in_progress", "done"],
                },
            },
        },
    },
    {
        "name": "spacebot_create_task",
        "description": "Create a new task on the Spacebot Kanban board",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
                "title": {"type": "string", "description": "Task title"},
                "description": {
                    "type": "string",
                    "description": "Task description (markdown supported)",
                },
                "priority": {
                    "type": "string",
                    "enum": ["critical", "high", "medium", "low"],
                    "default": "medium",
                },
                "status": {
                    "type": "string",
                    "enum": ["pending_approval", "backlog", "ready"],
                    "default": "backlog",
                },
                "depends_on": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Task numbers this task depends on",
                },
            },
            "required": ["title"],
        },
    },
    {
        "name": "spacebot_update_task",
        "description": "Update a task's status or priority",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
                "task_number": {
                    "type": "integer",
                    "description": "Task number to update",
                },
                "status": {
                    "type": "string",
                    "enum": ["pending_approval", "backlog", "ready", "in_progress", "done"],
                },
                "priority": {
                    "type": "string",
                    "enum": ["critical", "high", "medium", "low"],
                },
            },
            "required": ["task_number"],
        },
    },
    {
        "name": "spacebot_execute_task",
        "description": "Execute a task (spawns a Spacebot worker to work on it)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
                "task_number": {
                    "type": "integer",
                    "description": "Task number to execute",
                },
            },
            "required": ["task_number"],
        },
    },
    {
        "name": "spacebot_list_workers",
        "description": "List active workers and their status",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
            },
        },
    },
    {
        "name": "spacebot_send_message",
        "description": "Send a message to a Spacebot channel (triggers agent response)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
                "channel_id": {
                    "type": "string",
                    "description": "Channel ID to send to (use spacebot_list_channels to find)",
                },
                "message": {"type": "string", "description": "Message content"},
            },
            "required": ["channel_id", "message"],
        },
    },
    {
        "name": "spacebot_list_channels",
        "description": "List all channels for an agent",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Agent ID (default: 'main')",
                    "default": "main",
                },
            },
        },
    },
    {
        "name": "spacebot_status",
        "description": "Get Spacebot system status and version info",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
]


def handle_tool_call(name: str, arguments: dict) -> Any:
    """Route MCP tool calls to Spacebot API."""
    agent_id = arguments.get("agent_id", "main")

    if name == "spacebot_list_agents":
        return api_request("GET", "/agents")

    elif name == "spacebot_list_tasks":
        params = f"?limit=100"
        if "status" in arguments:
            params += f"&status={arguments['status']}"
        return api_request("GET", f"/agents/{agent_id}/tasks{params}")

    elif name == "spacebot_create_task":
        body: dict[str, Any] = {
            "title": arguments["title"],
            "priority": arguments.get("priority", "medium"),
            "status": arguments.get("status", "backlog"),
        }
        if "description" in arguments:
            body["description"] = arguments["description"]
        if "depends_on" in arguments:
            body["metadata"] = {"depends_on": arguments["depends_on"]}
        return api_request("POST", f"/agents/{agent_id}/tasks", body)

    elif name == "spacebot_update_task":
        task_number = arguments["task_number"]
        body = {}
        if "status" in arguments:
            body["status"] = arguments["status"]
        if "priority" in arguments:
            body["priority"] = arguments["priority"]
        return api_request("PATCH", f"/agents/{agent_id}/tasks/{task_number}", body)

    elif name == "spacebot_execute_task":
        task_number = arguments["task_number"]
        return api_request("POST", f"/agents/{agent_id}/tasks/{task_number}/execute")

    elif name == "spacebot_list_workers":
        return api_request("GET", f"/agents/{agent_id}/workers")

    elif name == "spacebot_send_message":
        body = {
            "content": arguments["message"],
            "author": "mcp-bridge",
        }
        return api_request(
            "POST", f"/agents/{agent_id}/channels/{arguments['channel_id']}/messages", body
        )

    elif name == "spacebot_list_channels":
        return api_request("GET", f"/agents/{agent_id}/channels")

    elif name == "spacebot_status":
        return api_request("GET", "/system/status")

    return {"error": f"Unknown tool: {name}"}


# -- MCP Protocol (JSON-RPC over stdio) --


def write_message(msg: dict):
    """Write a JSON-RPC message to stdout."""
    content = json.dumps(msg)
    header = f"Content-Length: {len(content)}\r\n\r\n"
    sys.stdout.write(header)
    sys.stdout.write(content)
    sys.stdout.flush()


def read_message() -> dict | None:
    """Read a JSON-RPC message from stdin."""
    # Read headers
    content_length = 0
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.startswith("Content-Length:"):
            content_length = int(line.split(":")[1].strip())

    if content_length == 0:
        return None

    content = sys.stdin.read(content_length)
    return json.loads(content)


def main():
    """Run the MCP server loop."""
    while True:
        msg = read_message()
        if msg is None:
            break

        method = msg.get("method", "")
        msg_id = msg.get("id")

        if method == "initialize":
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {"listChanged": False}},
                        "serverInfo": {
                            "name": "spacebot-mcp-bridge",
                            "version": "0.1.0",
                        },
                    },
                }
            )

        elif method == "notifications/initialized":
            pass  # No response needed

        elif method == "tools/list":
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {"tools": TOOLS},
                }
            )

        elif method == "tools/call":
            params = msg.get("params", {})
            tool_name = params.get("name", "")
            arguments = params.get("arguments", {})

            result = handle_tool_call(tool_name, arguments)
            is_error = "error" in result if isinstance(result, dict) else False

            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": json.dumps(result, indent=2, default=str),
                            }
                        ],
                        "isError": is_error,
                    },
                }
            )

        elif msg_id is not None:
            write_message(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {
                        "code": -32601,
                        "message": f"Method not found: {method}",
                    },
                }
            )


if __name__ == "__main__":
    main()
