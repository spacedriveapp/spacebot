//! Global metrics registry and metric handle definitions.

use prometheus::{
    CounterVec, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGaugeVec,
    Opts, Registry,
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
    /// Labels: agent_id, model, tier (e.g. "channel", "branch", "worker").
    pub llm_requests_total: IntCounterVec,

    /// Total tool calls executed across all processes.
    /// Labels: agent_id, tool_name.
    pub tool_calls_total: IntCounterVec,

    /// Total memory recall (read) operations.
    pub memory_reads_total: IntCounter,

    /// Total memory save (write) operations.
    pub memory_writes_total: IntCounter,

    // -- Histograms --
    /// LLM request duration in seconds.
    pub llm_request_duration_seconds: HistogramVec,

    /// Tool call duration in seconds.
    pub tool_call_duration_seconds: Histogram,

    // -- Gauges --
    /// Currently active workers per agent.
    /// Label: agent_id.
    pub active_workers: IntGaugeVec,

    /// Total memory entries per agent.
    /// Label: agent_id.
    pub memory_entry_count: IntGaugeVec,

    // -- Token & cost tracking --
    /// Total LLM tokens consumed.
    /// Labels: agent_id, model, tier, direction (input/output/cached_input).
    pub llm_tokens_total: IntCounterVec,

    /// Estimated LLM cost in USD.
    /// Labels: agent_id, model, tier.
    pub llm_estimated_cost_dollars: CounterVec,

    // -- Worker visibility --
    /// Currently active branches per agent.
    /// Label: agent_id.
    pub active_branches: IntGaugeVec,

    /// Worker lifetime duration in seconds.
    /// Labels: agent_id, worker_type.
    pub worker_duration_seconds: HistogramVec,

    /// Process errors by type.
    /// Labels: agent_id, process_type, error_type.
    pub process_errors_total: IntCounterVec,

    // -- Memory audit --
    /// Memory mutation operations.
    /// Labels: agent_id, operation (save/update/delete/forget).
    pub memory_updates_total: IntCounterVec,
}

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();

        let llm_requests_total = IntCounterVec::new(
            Opts::new(
                "spacebot_llm_requests_total",
                "Total LLM completion requests",
            ),
            &["agent_id", "model", "tier"],
        )
        .expect("hardcoded metric descriptor");

        let tool_calls_total = IntCounterVec::new(
            Opts::new("spacebot_tool_calls_total", "Total tool calls executed"),
            &["agent_id", "tool_name"],
        )
        .expect("hardcoded metric descriptor");

        let memory_reads_total = IntCounter::new(
            "spacebot_memory_reads_total",
            "Total memory recall operations",
        )
        .expect("hardcoded metric descriptor");

        let memory_writes_total = IntCounter::new(
            "spacebot_memory_writes_total",
            "Total memory save operations",
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
            &["agent_id", "model", "tier"],
        )
        .expect("hardcoded metric descriptor");

        let tool_call_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "spacebot_tool_call_duration_seconds",
                "Tool call duration in seconds",
            )
            .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
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
            &["agent_id", "model", "tier", "direction"],
        )
        .expect("hardcoded metric descriptor");

        let llm_estimated_cost_dollars = CounterVec::new(
            Opts::new(
                "spacebot_llm_estimated_cost_dollars",
                "Estimated LLM cost in USD",
            ),
            &["agent_id", "model", "tier"],
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
            &["agent_id", "process_type", "error_type"],
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
        }
    }

    /// Access the global metrics instance.
    pub fn global() -> &'static Self {
        &METRICS
    }
}
