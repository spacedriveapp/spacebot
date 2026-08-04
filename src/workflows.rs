//! Workflow templates: a pipeline defined once, launched with one input.
//!
//! Nothing in here executes anything, and that is the point. A graph of tasks
//! with dependency edges and input bindings already runs itself: the ready
//! sweep decides what is eligible, the claim re-checks the parent invariant and
//! assembles inputs, completion validates outputs against the declared schema,
//! and the reaper and failure budget handle the ways a step can die. Launching
//! a workflow *compiles* a template into those rows. The scheduler does the
//! rest, unchanged.
//!
//! The single thing the task-level schema cannot express is a binding by name.
//! `task_input_bindings.source_task_number` points at a task number; a template
//! has only step keys, and numbers exist only after a launch. Instantiation is
//! the translation between the two.

pub mod store;
pub mod triggers;
pub mod worktrees;

pub use store::{
    BindingSource, CancelOutcome, DeleteRunOutcome, InstantiatedRun, LaunchError, LaunchIdentity,
    RunAssessment, RunStatus, RunTransition, StepBinding, StepEdge, StepEdgeKind, StepGate,
    StepKind, Workflow, WorkflowRun, WorkflowStep, WorkflowStore, validate_step_gate,
};
pub use triggers::{
    DeliveryOutcome, ScheduleFire, ScheduleOutcome, WEBHOOK_SECRET_HEADER, WebhookRejection,
    WorkflowSchedule, WorkflowTriggerStore, WorkflowWebhook,
};
pub use worktrees::{
    MAX_RUN_WORKTREES, OrphanWorktree, ReapOutcome, ReapedWorktree, WorktreeError, WorktreeMode,
};
