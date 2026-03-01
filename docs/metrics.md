# Metrics

Prometheus-compatible metrics endpoint. Opt-in via the `metrics` cargo feature flag.

## Building with Metrics

```bash
cargo build --release --features metrics
```

Without the feature flag, all telemetry code is compiled out. The `[metrics]` config block is parsed regardless but has no effect.

### Docker

The Docker image always includes metrics — `--features metrics` is hardcoded in the Dockerfile.

## Configuration

```toml
[metrics]
enabled = true
port = 9090
bind = "0.0.0.0"
```

| Key       | Default     | Description                          |
| --------- | ----------- | ------------------------------------ |
| `enabled` | `false`     | Enable the /metrics HTTP endpoint    |
| `port`    | `9090`      | Port for the metrics server          |
| `bind`    | `"0.0.0.0"` | Address to bind the metrics server  |

The metrics server runs as a separate tokio task alongside the main API server. It shuts down gracefully with the rest of the process.

## Endpoints

| Path       | Description                              |
| ---------- | ---------------------------------------- |
| `/metrics` | Prometheus text exposition format (0.0.4)|
| `/health`  | Returns 200 OK (for liveness probes)     |

## Exposed Metrics

All metrics are prefixed with `spacebot_`. For detailed per-metric documentation, see `METRICS.md`.

### LLM Metrics

| Metric                                  | Type      | Labels                                     | Description                        |
| --------------------------------------- | --------- | ------------------------------------------ | ---------------------------------- |
| `spacebot_llm_requests_total`           | Counter   | agent_id, model, tier, worker_type         | Total LLM completion requests      |
| `spacebot_llm_request_duration_seconds` | Histogram | agent_id, model, tier, worker_type         | LLM request duration               |
| `spacebot_llm_tokens_total`             | Counter   | agent_id, model, tier, direction, worker_type | Token counts (input/output/cached) |
| `spacebot_llm_estimated_cost_dollars`   | Counter   | agent_id, model, tier, worker_type         | Estimated cost in USD              |

The `tier` label corresponds to the process type making the request: `channel`, `branch`, `worker`, `compactor`, or `cortex`. The `worker_type` label identifies the worker variant: `builtin`, `opencode`, or `ingestion`.

### Tool Metrics

| Metric                                    | Type      | Labels                              | Description                         |
| ----------------------------------------- | --------- | ----------------------------------- | ----------------------------------- |
| `spacebot_tool_calls_total`               | Counter   | agent_id, tool_name, process_type   | Total tool calls executed           |
| `spacebot_tool_call_duration_seconds`     | Histogram | agent_id, tool_name, process_type   | Tool call execution duration        |

### MCP Metrics

| Metric                                            | Type      | Labels                 | Description                         |
| ------------------------------------------------- | --------- | ---------------------- | ----------------------------------- |
| `spacebot_mcp_connections`                        | Gauge     | server_name, state     | Active MCP connections by state     |
| `spacebot_mcp_tools_registered`                   | Gauge     | server_name            | Tools registered per MCP server     |
| `spacebot_mcp_connection_attempts_total`          | Counter   | server_name, result    | MCP connection attempts             |
| `spacebot_mcp_tool_calls_total`                   | Counter   | server_name, tool_name | MCP tool calls                      |
| `spacebot_mcp_reconnects_total`                   | Counter   | server_name            | MCP reconnection attempts           |
| `spacebot_mcp_connection_duration_seconds`        | Histogram | server_name            | MCP connection establishment time   |
| `spacebot_mcp_tool_call_duration_seconds`         | Histogram | server_name, tool_name | MCP tool call duration              |

### Channel / Messaging Metrics

| Metric                                            | Type      | Labels                              | Description                         |
| ------------------------------------------------- | --------- | ----------------------------------- | ----------------------------------- |
| `spacebot_messages_received_total`                | Counter   | agent_id, channel_type              | Total messages received             |
| `spacebot_messages_sent_total`                    | Counter   | agent_id, channel_type              | Total messages sent (replies)       |
| `spacebot_message_handling_duration_seconds`      | Histogram | agent_id, channel_type              | Message handling duration           |
| `spacebot_channel_errors_total`                   | Counter   | agent_id, channel_type, error_type  | Channel-level errors                |

### Agent & Worker Metrics

| Metric                                  | Type      | Labels                                          | Description                        |
| --------------------------------------- | --------- | ----------------------------------------------- | ---------------------------------- |
| `spacebot_active_workers`               | Gauge     | agent_id                                        | Currently active workers           |
| `spacebot_active_branches`              | Gauge     | agent_id                                        | Currently active branches          |
| `spacebot_branches_spawned_total`       | Counter   | agent_id                                        | Total branches spawned             |
| `spacebot_worker_duration_seconds`      | Histogram | agent_id, worker_type                           | Worker lifetime duration           |
| `spacebot_context_overflow_total`       | Counter   | agent_id, process_type                          | Context overflow events            |
| `spacebot_process_errors_total`         | Counter   | agent_id, process_type, error_type, worker_type | Process errors by type             |

### Memory Metrics

| Metric                                          | Type      | Labels                | Description                         |
| ----------------------------------------------- | --------- | --------------------- | ----------------------------------- |
| `spacebot_memory_reads_total`                   | Counter   | agent_id              | Total memory recall operations      |
| `spacebot_memory_writes_total`                  | Counter   | agent_id              | Total memory save operations        |
| `spacebot_memory_entry_count`                   | Gauge     | agent_id              | Memory entries per agent            |
| `spacebot_memory_updates_total`                 | Counter   | agent_id, operation   | Memory mutations (save/delete/forget) |
| `spacebot_memory_operation_duration_seconds`    | Histogram | agent_id, operation   | Memory operation duration           |
| `spacebot_memory_search_results`                | Histogram | agent_id              | Search results per recall query     |
| `spacebot_memory_embedding_duration_seconds`    | Histogram |                       | Embedding generation duration       |

### Cost Metrics

| Metric                                  | Type      | Labels                 | Description                        |
| --------------------------------------- | --------- | ---------------------- | ---------------------------------- |
| `spacebot_worker_cost_dollars`          | Counter   | agent_id, worker_type  | Worker-specific cost in USD        |

### API Metrics

| Metric                                          | Type      | Labels                   | Description                         |
| ----------------------------------------------- | --------- | ------------------------ | ----------------------------------- |
| `spacebot_http_requests_total`                  | Counter   | method, path, status     | Total HTTP API requests             |
| `spacebot_http_request_duration_seconds`        | Histogram | method, path             | HTTP request duration               |

### Cron & Ingestion Metrics

| Metric                                          | Type      | Labels                        | Description                         |
| ----------------------------------------------- | --------- | ----------------------------- | ----------------------------------- |
| `spacebot_cron_executions_total`                | Counter   | agent_id, task_type, result   | Cron task executions                |
| `spacebot_ingestion_files_processed_total`      | Counter   | agent_id, result              | Ingestion files processed           |

## Useful PromQL Queries

**Total estimated spend by agent:**
```promql
sum(spacebot_llm_estimated_cost_dollars) by (agent_id)
```

**Cost per worker type (daily rate):**
```promql
sum by (worker_type) (rate(spacebot_worker_cost_dollars[1d]))
```

**Top 5 expensive workers:**
```promql
topk(5, sum by (worker_type, agent_id) (spacebot_llm_estimated_cost_dollars{tier="worker"}))
```

**Hourly spend rate by model:**
```promql
sum(rate(spacebot_llm_estimated_cost_dollars[1h])) by (agent_id, model) * 3600
```

**Token throughput:**
```promql
sum(rate(spacebot_llm_tokens_total[5m])) by (direction)
```

**MCP server health:**
```promql
spacebot_mcp_connections{state="connected"}
```

**Channel throughput by type:**
```promql
sum by (channel_type) (rate(spacebot_messages_received_total[5m]))
```

**Memory operation latency p99:**
```promql
histogram_quantile(0.99, rate(spacebot_memory_operation_duration_seconds_bucket[5m]))
```

**API latency by endpoint (p95):**
```promql
histogram_quantile(0.95, sum by (path, le) (rate(spacebot_http_request_duration_seconds_bucket[5m])))
```

**Active branches and workers:**
```promql
spacebot_active_branches
spacebot_active_workers
```

**Context overflow rate:**
```promql
sum by (agent_id, process_type) (rate(spacebot_context_overflow_total[1h]))
```

## Prometheus Scrape Config

```yaml
scrape_configs:
  - job_name: spacebot
    scrape_interval: 15s
    static_configs:
      - targets: ["localhost:9090"]
```

## Docker

Expose the metrics port alongside the API port:

```bash
docker run -d \
  --name spacebot \
  -e ANTHROPIC_API_KEY="sk-ant-..." \
  -v spacebot-data:/data \
  -p 19898:19898 \
  -p 9090:9090 \
  ghcr.io/spacedriveapp/spacebot:full
```

The Docker image always includes metrics. For local builds without metrics, omit the `--features metrics` flag.
