//! Task tracking data model and storage.

pub mod gates;
pub mod migration;
pub mod store;

pub use gates::{
    Evaluation, GATE_ERROR_LIMIT, GateConfigError, GateKind, GateResult, GateStore,
    MIN_POLL_INTERVAL_SECS, TaskGate, evaluate_http, evaluate_task_output, validate_config,
};

pub use store::{
    BLOCK_RECURRENCE_LIMIT, BlockKind, BlockOutcome, ContractProblem, ContractResolution,
    ContractSide, CreateTaskInput, DEFAULT_FAILURE_LIMIT, DEFAULT_LOOP_MAX_ITERATIONS,
    DependencyError, FAN_OUT_ITEM_INPUT_KEY, FAN_OUT_METADATA_KEY, FILED_BY_TASK_PREFIX,
    FailureDisposition, FanOutOutcome, FanOutSpec, LOOP_METADATA_KEY, LoopArm, LoopOutcome,
    LoopResolution, LoopSpec, MAX_FILING_DEPTH, MAX_GRAPH_TASKS, MAX_LOOP_ITERATIONS,
    MAX_TASKS_FILED_PER_TASK, OutputSubmission, PendingTask, PreviousIterationBinding, ReadySweep,
    Task, TaskBindingPatch, TaskEdgeSummary, TaskGraph, TaskGraphEdge, TaskInputBinding,
    TaskListFilter, TaskPriority, TaskProjectBinding, TaskRun, TaskRunOutcome, TaskStatus,
    TaskStore, TaskSubtask, TaskUpdateResult, UpdateTaskInput, WorkerOutputSubmission,
    WorkerTaskUpdateResult, can_transition, filer_id, legal_transitions, parse_filer_task_number,
};
