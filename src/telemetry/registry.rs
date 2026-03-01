//! Global metrics registry and metric handle definitions.

use prometheus::{
    CounterVec, Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry,
};

use std::sync::LazyLock;

/// Global metrics instance. Initialized once, accessed from any call site.
static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::new);

/// All Prometheus metric handles for the Spacebot process.
///
/// Access via `Metrics::global()`. Metric handles are cheap to clone (Arc
/// internally) so call sites can grab references without threading state.
pub struct Metrics {
    pub(crate) registry: Registry,

    // -- Counters --
    /// Total LLM completion requests.
    /// Labels: agent_id, model, tier, worker_type.
    pub llm_requests_total: IntCounterVec,

    /// Total tool calls executed across all processes.
    /// Labels: agent_id, tool_name, process_type.
    pub tool_calls_total: IntCounterVec,

    /// Total memory recall (read) operations.
    /// Labels: agent_id.
    pub memory_reads_total: IntCounterVec,

    /// Total memory save (write) operations.
    /// Labels: agent_id.
    pub memory_writes_total: IntCounterVec,

    // -- Histograms --
    /// LLM request duration in seconds.
    /// Labels: agent_id, model, tier, worker_type.
    pub llm_request_duration_seconds: HistogramVec,

    /// Tool call duration in seconds.
    /// Labels: agent_id, tool_name, process_type.
    pub tool_call_duration_seconds: HistogramVec,

    // -- Gauges --
    /// Currently active workers per agent.
    /// Label: agent_id.
    pub active_workers: IntGaugeVec,

    /// Total memory entries per agent.
    /// Label: agent_id.
    pub memory_entry_count: IntGaugeVec,

    // -- Token & cost tracking --
    /// Total LLM tokens consumed.
    /// Labels: agent_id, model, tier, direction, worker_type.
    pub llm_tokens_total: IntCounterVec,

    /// Estimated LLM cost in USD.
    /// Labels: agent_id, model, tier, worker_type.
    pub llm_estimated_cost_dollars: CounterVec,

    // -- Worker visibility --
    /// Currently active branches per agent.
    /// Label: agent_id.
    pub active_branches: IntGaugeVec,

    /// Worker lifetime duration in seconds.
    /// Labels: agent_id, worker_type.
    pub worker_duration_seconds: HistogramVec,

    /// Process errors by type.
    /// Labels: agent_id, process_type, error_type, worker_type.
    pub process_errors_total: IntCounterVec,

    // -- Memory audit --
    /// Memory mutation operations.
    /// Labels: agent_id, operation (save/update/delete/forget).
    pub memory_updates_total: IntCounterVec,

    /// Dispatch attempts while readiness contract is not satisfied.
    /// Labels: agent_id, dispatch_type, reason.
    pub dispatch_while_cold_count: IntCounterVec,

    /// Total broadcast events dropped because a receiver lagged.
    /// Labels: agent_id, receiver.
    pub event_receiver_lagged_events_total: IntCounterVec,

    /// Time-to-recovery for forced warmup passes kicked by dispatch paths, in ms.
    /// Labels: agent_id, dispatch_type.
    pub warmup_recovery_latency_ms: HistogramVec,

    // =====================================================================
    // NEW METRICS
    // =====================================================================

    // -- MCP --
    /// Active MCP connections by server and state.
    /// Labels: server_name, state.
    pub mcp_connections: IntGaugeVec,

    /// Number of tools registered per MCP server.
    /// Labels: server_name.
    pub mcp_tools_registered: IntGaugeVec,

    /// MCP connection attempts (success/failure).
    /// Labels: server_name, result.
    pub mcp_connection_attempts_total: IntCounterVec,

    /// MCP tool calls by server and tool.
    /// Labels: server_name, tool_name.
    pub mcp_tool_calls_total: IntCounterVec,

    /// MCP reconnection attempts.
    /// Labels: server_name.
    pub mcp_reconnects_total: IntCounterVec,

    /// MCP connection establishment duration.
    /// Labels: server_name.
    pub mcp_connection_duration_seconds: HistogramVec,

    /// MCP tool call duration.
    /// Labels: server_name, tool_name.
    pub mcp_tool_call_duration_seconds: HistogramVec,

    // -- Channel / Messaging --
    /// Total messages received.
    /// Labels: agent_id, channel_type.
    pub messages_received_total: IntCounterVec,

    /// Total messages sent.
    /// Labels: agent_id, channel_type.
    pub messages_sent_total: IntCounterVec,

    /// Message handling duration.
    /// Labels: agent_id, channel_type.
    pub message_handling_duration_seconds: HistogramVec,

    /// Channel-level errors.
    /// Labels: agent_id, channel_type, error_type.
    pub channel_errors_total: IntCounterVec,

    // -- Memory operations --
    /// Memory operation duration.
    /// Labels: agent_id, operation.
    pub memory_operation_duration_seconds: HistogramVec,

    /// Number of search results returned.
    /// Labels: agent_id.
    pub memory_search_results: HistogramVec,

    /// Embedding generation duration.
    pub memory_embedding_duration_seconds: Histogram,

    // -- API --
    /// Total HTTP requests.
    /// Labels: method, handler, status.
    pub http_requests_total: IntCounterVec,

    /// HTTP request duration.
    /// Labels: method, handler.
    pub http_request_duration_seconds: HistogramVec,

    // -- Agent lifecycle --
    /// Branches spawned.
    /// Labels: agent_id.
    pub branches_spawned_total: IntCounterVec,

    /// Context overflow events.
    /// Labels: agent_id, process_type.
    pub context_overflow_total: IntCounterVec,

    // -- Cost --
    /// Worker cost tracking in USD.
    /// Labels: agent_id, worker_type.
    pub worker_cost_dollars: CounterVec,

    // -- Cron --
    /// Cron task executions.
    /// Labels: agent_id, task_type, result.
    pub cron_executions_total: IntCounterVec,

    // -- Ingestion --
    /// Ingestion files processed.
    /// Labels: agent_id, result.
    pub ingestion_files_processed_total: IntCounterVec,
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();

        // === UPGRADED existing metrics ===

        let llm_requests_total = IntCounterVec::new(
            Opts::new(
                "spacebot_llm_requests_total",
                "Total LLM completion requests",
            ),
            &["agent_id", "model", "tier", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        let tool_calls_total = IntCounterVec::new(
            Opts::new("spacebot_tool_calls_total", "Total tool calls executed"),
            &["agent_id", "tool_name", "process_type"],
        )
        .expect("hardcoded metric descriptor");

        let memory_reads_total = IntCounterVec::new(
            Opts::new(
                "spacebot_memory_reads_total",
                "Total memory recall operations",
            ),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let memory_writes_total = IntCounterVec::new(
            Opts::new(
                "spacebot_memory_writes_total",
                "Total memory save operations",
            ),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let llm_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_llm_request_duration_seconds",
                "LLM request duration in seconds",
            )
            .buckets(vec![
                0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0,
            ]),
            &["agent_id", "model", "tier", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        let tool_call_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_tool_call_duration_seconds",
                "Tool call duration in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &["agent_id", "tool_name", "process_type"],
        )
        .expect("hardcoded metric descriptor");

        let active_workers = IntGaugeVec::new(
            Opts::new("spacebot_active_workers", "Currently active workers"),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let memory_entry_count = IntGaugeVec::new(
            Opts::new(
                "spacebot_memory_entry_count",
                "Total memory entries per agent",
            ),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let llm_tokens_total = IntCounterVec::new(
            Opts::new("spacebot_llm_tokens_total", "Total LLM tokens consumed"),
            &["agent_id", "model", "tier", "direction", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        let llm_estimated_cost_dollars = CounterVec::new(
            Opts::new(
                "spacebot_llm_estimated_cost_dollars",
                "Estimated LLM cost in USD",
            ),
            &["agent_id", "model", "tier", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        let active_branches = IntGaugeVec::new(
            Opts::new("spacebot_active_branches", "Currently active branches"),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let worker_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_worker_duration_seconds",
                "Worker lifetime duration in seconds",
            )
            .buckets(vec![
                1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
            ]),
            &["agent_id", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        let process_errors_total = IntCounterVec::new(
            Opts::new("spacebot_process_errors_total", "Process errors by type"),
            &["agent_id", "process_type", "error_type", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        let memory_updates_total = IntCounterVec::new(
            Opts::new(
                "spacebot_memory_updates_total",
                "Memory mutation operations",
            ),
            &["agent_id", "operation"],
        )
        .expect("hardcoded metric descriptor");

        let dispatch_while_cold_count = IntCounterVec::new(
            Opts::new(
                "spacebot_dispatch_while_cold_count",
                "Dispatch attempts while readiness contract is unsatisfied",
            ),
            &["agent_id", "dispatch_type", "reason"],
        )
        .expect("hardcoded metric descriptor");

        let event_receiver_lagged_events_total = IntCounterVec::new(
            Opts::new(
                "spacebot_event_receiver_lagged_events_total",
                "Total broadcast events dropped because a receiver lagged",
            ),
            &["agent_id", "receiver"],
        )
        .expect("hardcoded metric descriptor");

        let warmup_recovery_latency_ms = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_warmup_recovery_latency_ms",
                "Forced warmup recovery latency in milliseconds",
            )
            .buckets(vec![
                25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10_000.0, 30_000.0,
            ]),
            &["agent_id", "dispatch_type"],
        )
        .expect("hardcoded metric descriptor");

        // === NEW metrics ===

        // MCP (7)
        let mcp_connections = IntGaugeVec::new(
            Opts::new("spacebot_mcp_connections", "Active MCP connections"),
            &["server_name", "state"],
        )
        .expect("hardcoded metric descriptor");

        let mcp_tools_registered = IntGaugeVec::new(
            Opts::new(
                "spacebot_mcp_tools_registered",
                "Number of tools registered per MCP server",
            ),
            &["server_name"],
        )
        .expect("hardcoded metric descriptor");

        let mcp_connection_attempts_total = IntCounterVec::new(
            Opts::new(
                "spacebot_mcp_connection_attempts_total",
                "MCP connection attempts",
            ),
            &["server_name", "result"],
        )
        .expect("hardcoded metric descriptor");

        let mcp_tool_calls_total = IntCounterVec::new(
            Opts::new(
                "spacebot_mcp_tool_calls_total",
                "MCP tool calls by server and tool",
            ),
            &["server_name", "tool_name"],
        )
        .expect("hardcoded metric descriptor");

        let mcp_reconnects_total = IntCounterVec::new(
            Opts::new(
                "spacebot_mcp_reconnects_total",
                "MCP reconnection attempts",
            ),
            &["server_name"],
        )
        .expect("hardcoded metric descriptor");

        let mcp_connection_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_mcp_connection_duration_seconds",
                "MCP connection establishment duration",
            )
            .buckets(vec![0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &["server_name"],
        )
        .expect("hardcoded metric descriptor");

        let mcp_tool_call_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_mcp_tool_call_duration_seconds",
                "MCP tool call duration",
            )
            .buckets(vec![
                0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
            ]),
            &["server_name", "tool_name"],
        )
        .expect("hardcoded metric descriptor");

        // Channel/Messaging (4)
        let messages_received_total = IntCounterVec::new(
            Opts::new(
                "spacebot_messages_received_total",
                "Total messages received",
            ),
            &["agent_id", "channel_type"],
        )
        .expect("hardcoded metric descriptor");

        let messages_sent_total = IntCounterVec::new(
            Opts::new("spacebot_messages_sent_total", "Total messages sent"),
            &["agent_id", "channel_type"],
        )
        .expect("hardcoded metric descriptor");

        let message_handling_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_message_handling_duration_seconds",
                "Message handling duration",
            )
            .buckets(vec![
                0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0,
            ]),
            &["agent_id", "channel_type"],
        )
        .expect("hardcoded metric descriptor");

        let channel_errors_total = IntCounterVec::new(
            Opts::new("spacebot_channel_errors_total", "Channel-level errors"),
            &["agent_id", "channel_type", "error_type"],
        )
        .expect("hardcoded metric descriptor");

        // Memory (3)
        let memory_operation_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_memory_operation_duration_seconds",
                "Memory operation duration",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0,
            ]),
            &["agent_id", "operation"],
        )
        .expect("hardcoded metric descriptor");

        let memory_search_results = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_memory_search_results",
                "Number of search results returned",
            )
            .buckets(vec![0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0]),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let memory_embedding_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "spacebot_memory_embedding_duration_seconds",
                "Embedding generation duration",
            )
            .buckets(vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5]),
        )
        .expect("hardcoded metric descriptor");

        // API (2)
        let http_requests_total = IntCounterVec::new(
            Opts::new("spacebot_http_requests_total", "Total HTTP requests"),
            &["method", "handler", "status"],
        )
        .expect("hardcoded metric descriptor");

        let http_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "spacebot_http_request_duration_seconds",
                "HTTP request duration",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0,
            ]),
            &["method", "handler"],
        )
        .expect("hardcoded metric descriptor");

        // Agent Lifecycle (2)
        let branches_spawned_total = IntCounterVec::new(
            Opts::new("spacebot_branches_spawned_total", "Branches spawned"),
            &["agent_id"],
        )
        .expect("hardcoded metric descriptor");

        let context_overflow_total = IntCounterVec::new(
            Opts::new(
                "spacebot_context_overflow_total",
                "Context overflow events",
            ),
            &["agent_id", "process_type"],
        )
        .expect("hardcoded metric descriptor");

        // Cost (1)
        let worker_cost_dollars = CounterVec::new(
            Opts::new(
                "spacebot_worker_cost_dollars",
                "Worker cost tracking in USD",
            ),
            &["agent_id", "worker_type"],
        )
        .expect("hardcoded metric descriptor");

        // Cron (1)
        let cron_executions_total = IntCounterVec::new(
            Opts::new("spacebot_cron_executions_total", "Cron task executions"),
            &["agent_id", "task_type", "result"],
        )
        .expect("hardcoded metric descriptor");

        // Ingestion (1)
        let ingestion_files_processed_total = IntCounterVec::new(
            Opts::new(
                "spacebot_ingestion_files_processed_total",
                "Ingestion files processed",
            ),
            &["agent_id", "result"],
        )
        .expect("hardcoded metric descriptor");

        // === Register all metrics ===

        // Existing (upgraded)
        registry
            .register(Box::new(llm_requests_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(tool_calls_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(memory_reads_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(memory_writes_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(llm_request_duration_seconds.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(tool_call_duration_seconds.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(active_workers.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(memory_entry_count.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(llm_tokens_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(llm_estimated_cost_dollars.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(active_branches.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(worker_duration_seconds.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(process_errors_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(memory_updates_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(dispatch_while_cold_count.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(event_receiver_lagged_events_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(warmup_recovery_latency_ms.clone()))
            .expect("hardcoded metric");

        // New: MCP
        registry
            .register(Box::new(mcp_connections.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(mcp_tools_registered.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(mcp_connection_attempts_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(mcp_tool_calls_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(mcp_reconnects_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(mcp_connection_duration_seconds.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(mcp_tool_call_duration_seconds.clone()))
            .expect("hardcoded metric");

        // New: Channel/Messaging
        registry
            .register(Box::new(messages_received_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(messages_sent_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(message_handling_duration_seconds.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(channel_errors_total.clone()))
            .expect("hardcoded metric");

        // New: Memory operations
        registry
            .register(Box::new(memory_operation_duration_seconds.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(memory_search_results.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(memory_embedding_duration_seconds.clone()))
            .expect("hardcoded metric");

        // New: API
        registry
            .register(Box::new(http_requests_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(http_request_duration_seconds.clone()))
            .expect("hardcoded metric");

        // New: Agent lifecycle
        registry
            .register(Box::new(branches_spawned_total.clone()))
            .expect("hardcoded metric");
        registry
            .register(Box::new(context_overflow_total.clone()))
            .expect("hardcoded metric");

        // New: Cost
        registry
            .register(Box::new(worker_cost_dollars.clone()))
            .expect("hardcoded metric");

        // New: Cron
        registry
            .register(Box::new(cron_executions_total.clone()))
            .expect("hardcoded metric");

        // New: Ingestion
        registry
            .register(Box::new(ingestion_files_processed_total.clone()))
            .expect("hardcoded metric");

        Self {
            registry,
            llm_requests_total,
            tool_calls_total,
            memory_reads_total,
            memory_writes_total,
            llm_request_duration_seconds,
            tool_call_duration_seconds,
            active_workers,
            memory_entry_count,
            llm_tokens_total,
            llm_estimated_cost_dollars,
            active_branches,
            worker_duration_seconds,
            process_errors_total,
            memory_updates_total,
            dispatch_while_cold_count,
            event_receiver_lagged_events_total,
            warmup_recovery_latency_ms,
            // New
            mcp_connections,
            mcp_tools_registered,
            mcp_connection_attempts_total,
            mcp_tool_calls_total,
            mcp_reconnects_total,
            mcp_connection_duration_seconds,
            mcp_tool_call_duration_seconds,
            messages_received_total,
            messages_sent_total,
            message_handling_duration_seconds,
            channel_errors_total,
            memory_operation_duration_seconds,
            memory_search_results,
            memory_embedding_duration_seconds,
            http_requests_total,
            http_request_duration_seconds,
            branches_spawned_total,
            context_overflow_total,
            worker_cost_dollars,
            cron_executions_total,
            ingestion_files_processed_total,
        }
    }

    /// Access the global metrics instance.
    pub fn global() -> &'static Self {
        &METRICS
    }
}
