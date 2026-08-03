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
    ContractSide, CreateTaskInput, DEFAULT_FAILURE_LIMIT, DependencyError, FAN_OUT_ITEM_INPUT_KEY,
    FAN_OUT_METADATA_KEY, FILED_BY_TASK_PREFIX, FailureDisposition, FanOutOutcome, FanOutSpec,
    MAX_FILING_DEPTH, MAX_TASKS_FILED_PER_TASK, OutputSubmission, PendingTask, ReadySweep, Task,
    TaskBindingPatch, TaskEdgeSummary, TaskInputBinding, TaskListFilter, TaskPriority,
    TaskProjectBinding, TaskRun, TaskRunOutcome, TaskStatus, TaskStore, TaskSubtask,
    TaskUpdateResult, UpdateTaskInput, WorkerOutputSubmission, WorkerTaskUpdateResult,
    can_transition, filer_id, legal_transitions, parse_filer_task_number,
};
