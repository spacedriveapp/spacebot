//! Task tracking data model and storage.

pub mod migration;
pub mod store;

pub use store::{
    CreateTaskInput, DEFAULT_FAILURE_LIMIT, FailureDisposition, Task, TaskListFilter, TaskPriority,
    TaskProjectBinding, TaskRun, TaskRunOutcome, TaskStatus, TaskStore, TaskSubtask,
    TaskUpdateResult, UpdateTaskInput, WorkerTaskUpdateResult, can_transition, legal_transitions,
};
