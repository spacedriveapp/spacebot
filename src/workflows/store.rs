//! Workflow template storage and instantiation.

use crate::error::Result;
use crate::tasks::{
    ContractProblem, ContractResolution, ContractSide, CreateTaskInput, GateConfigError,
    GateDisposition, GateKind, Task, TaskInputBinding, TaskPriority, TaskProjectBinding,
    TaskStatus, TaskStore,
};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};
use std::collections::{HashMap, HashSet};

/// A reusable pipeline definition.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// JSON Schema for the input a whole run is launched with.
    pub input_schema: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// One step of a pipeline. Becomes exactly one task per launch.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowStep {
    pub workflow_id: String,
    /// Stable name that edges and bindings reference.
    pub step_key: String,
    pub title: String,
    pub description: Option<String>,
    /// `None` means the agent that launched the run.
    pub assigned_agent_id: Option<String>,
    pub priority: TaskPriority,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    /// Per-step instructions appended to the worker prompt at pickup.
    pub system_prompt: Option<String>,
    pub repo_id: Option<String>,
    pub position: i64,
    /// Which upstream step produces the collection this step iterates.
    ///
    /// Set, and the step is a fan-out: it becomes one task per item rather than
    /// one task, and the width is not known until that step finishes.
    pub for_each_step_key: Option<String>,
    /// RFC 6901 pointer into that step's outputs. Must select an array.
    pub for_each_pointer: Option<String>,
    /// Pointer *within each item* naming its branch, e.g. `/name` over
    /// `{"name": "repo-a"}` labels the branch `repo-a`.
    ///
    /// This is what makes a fan-in keyed rather than positional. Without it the
    /// index is used and the keys come out `0`, `1`, `2` — honest, but far less
    /// useful in a report.
    pub for_each_key: Option<String>,
    /// Which loop body this step belongs to.
    ///
    /// A loop is one or more steps sharing this name. A body of one step is the
    /// degenerate case and needs no special handling.
    pub loop_group: Option<String>,
    /// How many passes the body may run before the loop gives up.
    ///
    /// `None` means [`crate::tasks::DEFAULT_LOOP_MAX_ITERATIONS`]. Read from the
    /// body's exit step only; set anywhere else it would be a number nothing
    /// consumes, so launch refuses that rather than letting it sit in a row.
    pub loop_max_iterations: Option<i64>,
    /// The exit predicate, as the same object a `task_output` gate takes:
    /// `{"pointer": "/tests/passed", "equals": true}`.
    ///
    /// Deliberately not a second predicate language — conditional steps,
    /// external gating, and loop exit are one question asked in three places.
    /// Required on the body's exit step: a loop with no exit condition always
    /// burns its whole budget.
    pub loop_until: Option<Value>,
}

/// What an edge means, now that "the loop finished" is two outcomes.
///
/// Converging and giving up are opposite results, and routing both into the
/// same downstream step is the single-label-two-conditions mistake this
/// codebase has already paid for three times. A pipeline that merges after
/// three successful attempts must not also merge after three failed ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepEdgeKind {
    /// An ordinary wait. Followed when the parent finished, and — for a loop —
    /// when it converged.
    Normal,
    /// Followed only when the loop that ends at the parent ran out of attempts.
    OnExhausted,
}

impl StepEdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepEdgeKind::Normal => "normal",
            StepEdgeKind::OnExhausted => "on_exhausted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(StepEdgeKind::Normal),
            "on_exhausted" => Some(StepEdgeKind::OnExhausted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct StepEdge {
    pub parent_step_key: String,
    pub child_step_key: String,
    pub kind: StepEdgeKind,
}

/// Where a step's input comes from.
///
/// Named rather than inferred from which column is populated. The task-level
/// table infers "literal" from a NULL source, which works for rows the store
/// writes and is a trap for rows a human edits: a malformed binding is
/// indistinguishable from a deliberate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BindingSource {
    /// Read another step's output at `source_pointer`.
    Step,
    /// A value baked into the template.
    Literal,
    /// Read the run's own launch input at `source_pointer`.
    ///
    /// This is what lets one input drive a whole pipeline: a step binds
    /// straight into the launch payload, so the entry point is declarative and
    /// there is no special-cased first step.
    RunInput,
    /// Collect every branch of a fan-out step, keyed by branch key.
    ///
    /// `Step` cannot express this: it addresses one upstream task, and a
    /// fan-out produces a number of them that does not exist until it expands.
    FanIn,
    /// Read a body step's output from the iteration *before* this one.
    ///
    /// Binding from the previous pass is the point of looping rather than
    /// retrying: `Step` inside a body resolves to the current iteration, which
    /// for the step being fed has not run yet. On iteration 1 there is no
    /// previous pass, so this resolves to the loop's entry — the single step
    /// outside the body that feeds it — at the same pointer, which is what
    /// lets the body need no special first-pass wiring.
    PreviousIteration,
}

impl BindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingSource::Step => "step",
            BindingSource::Literal => "literal",
            BindingSource::RunInput => "run_input",
            BindingSource::FanIn => "fan_in",
            BindingSource::PreviousIteration => "previous_iteration",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "step" => Some(BindingSource::Step),
            "literal" => Some(BindingSource::Literal),
            "run_input" => Some(BindingSource::RunInput),
            "fan_in" => Some(BindingSource::FanIn),
            "previous_iteration" => Some(BindingSource::PreviousIteration),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct StepBinding {
    pub workflow_id: String,
    pub step_key: String,
    pub input_key: String,
    pub source: BindingSource,
    pub source_step_key: Option<String>,
    /// RFC 6901 JSON Pointer. Empty selects the whole document.
    pub source_pointer: Option<String>,
    pub literal_value: Option<Value>,
}

/// A gate declared by a *template*, addressed by step key.
///
/// The template-level mirror of `task_gates`, exactly as [`StepBinding`] is the
/// template-level mirror of `task_input_bindings`, and for the same reason:
/// `task_gates` is keyed by task number and a template has only step keys. The
/// translation at launch is the same one bindings already do.
///
/// This is where a *condition* on a step lives. Whether the condition holds the
/// step or settles it is [`StepGate::disposition`], and that one field is the
/// whole of branching.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct StepGate {
    pub workflow_id: String,
    /// The step this gate holds back.
    pub step_key: String,
    /// Author-named, so saving the same gate twice is an edit rather than a
    /// second gate holding the step behind a duplicate of one condition.
    pub gate_key: String,
    pub kind: GateKind,
    /// For `task_output`: whose output to read, by name. Becomes
    /// `config.task_number` at launch — the entire translation.
    pub source_step_key: Option<String>,
    /// The predicate, in the shape `task_gates.config` takes: an RFC 6901
    /// pointer plus `equals` / `any_of`. No second predicate language.
    pub config: Value,
    /// What the board should call this. "needs legal review" beats a pointer.
    pub label: Option<String>,
    pub poll_interval_secs: i64,
    /// `None` means derive it when the gate is polled. See
    /// [`crate::tasks::TaskGate::disposition_for`].
    pub disposition: Option<GateDisposition>,
}

/// Validate a template gate — a task gate with its source still named rather
/// than numbered.
///
/// Deliberately the same validator the task level uses, with the one field a
/// template cannot have filled in. A second set of rules here would drift, and
/// the failure mode of drift is a template that saves and then refuses to
/// launch.
pub fn validate_step_gate(gate: &StepGate) -> std::result::Result<(), GateConfigError> {
    match gate.kind {
        GateKind::Http => {
            crate::tasks::validate_config(GateKind::Http, &gate.config, gate.poll_interval_secs)
        }
        GateKind::TaskOutput => {
            let Some(object) = gate.config.as_object() else {
                return Err(GateConfigError::MissingField {
                    kind: "task_output",
                    field: "pointer",
                });
            };
            let mut config = object.clone();
            // The source is `source_step_key` here and is checked separately;
            // standing a number in its place lets the shared validator run
            // over everything else unchanged.
            config.insert("task_number".to_string(), Value::from(0));
            crate::tasks::validate_config(
                GateKind::TaskOutput,
                &Value::Object(config),
                gate.poll_interval_secs,
            )
        }
    }
}

/// How a run is going.
///
/// `stuck` is the value this enum exists for. The other four are reductions
/// over tasks that a caller could have computed itself; `stuck` is not
/// derivable from any single task, because every task in a wedged run looks
/// individually reasonable — a loop body parked for a person, a step behind a
/// gate that stopped polling, a placeholder that will never expand. Only the
/// run can see that none of them will ever move.
///
/// The distinction that matters most is the one this enum does *not* make:
/// `stuck` versus still `running`. A run waiting on a gate that can still open
/// is waiting, not stuck, and reporting it as stuck teaches people to ignore
/// the status — which is worse than the silence it replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Something is in flight, promotable, or waiting on a gate that can still
    /// open.
    Running,
    /// Every task settled and no failure path was taken. Includes runs where a
    /// branch was skipped: a condition that ruled a step out is the pipeline
    /// working, and `status_reason` says which steps did not run.
    Succeeded,
    /// A task used its whole failure budget, or a loop ran out of attempts and
    /// took its `on_exhausted` edge.
    Failed,
    /// Nothing in flight, nothing promotable, and no gate that can still open —
    /// and not finished. `status_reason` says which task is holding it and why.
    Stuck,
    /// A person stopped it.
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::Stuck => "stuck",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(RunStatus::Running),
            "succeeded" => Some(RunStatus::Succeeded),
            "failed" => Some(RunStatus::Failed),
            "stuck" => Some(RunStatus::Stuck),
            "cancelled" => Some(RunStatus::Cancelled),
            _ => None,
        }
    }

    /// Whether the run has stopped for good. Every terminal status carries a
    /// `finished_at`, and no terminal run is ever assessed again.
    pub fn is_terminal(self) -> bool {
        !matches!(self, RunStatus::Running)
    }

    /// Whether reaching this status is worth telling somebody about, once.
    ///
    /// `succeeded` is not: a pipeline that worked is what was asked for, and an
    /// inbox that fills up with successes is an inbox nobody reads, which would
    /// bury the two states that do need a person. `cancelled` is not either —
    /// the person who cancelled it already knows.
    pub fn warrants_notice(self) -> bool {
        matches!(self, RunStatus::Stuck | RunStatus::Failed)
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// One launch of a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_id: String,
    pub inputs: Value,
    pub launched_by: String,
    pub status: RunStatus,
    /// When the run stopped, in any terminal sense. `None` exactly while
    /// `status` is `running`.
    pub finished_at: Option<String>,
    /// Why the run reached its current status, in words.
    pub status_reason: Option<String>,
    pub created_at: String,
}

/// The result of a successful launch.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstantiatedRun {
    pub run: WorkflowRun,
    /// Emitted task numbers, keyed by the step they came from.
    pub task_numbers: HashMap<String, i64>,
}

/// Why a launch was refused.
///
/// Every variant is something a person can act on. A launch that fails with
/// "invalid workflow" sends someone reading rows; naming the step and the key
/// points at the line to change.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LaunchError {
    #[error("workflow {id} does not exist")]
    UnknownWorkflow { id: String },
    #[error("workflow {id} has no steps")]
    NoSteps { id: String },
    #[error(
        "step `{step_key}` binds `{input_key}` to step `{missing}`, which is not in this workflow"
    )]
    UnknownStepReference {
        step_key: String,
        input_key: String,
        missing: String,
    },
    #[error("edge {parent} -> {child} references a step that is not in this workflow")]
    UnknownEdgeReference { parent: String, child: String },
    #[error(
        "the steps form a cycle and cannot be ordered: {}",
        .cycle.join(" -> ")
    )]
    Cycle { cycle: Vec<String> },
    #[error("launch input does not match the workflow's schema: {details}")]
    InvalidInput { details: String },
    #[error(
        "step `{step_key}` binds `{input_key}` to run input at `{pointer}`, which the launch input does not contain"
    )]
    MissingRunInput {
        step_key: String,
        input_key: String,
        pointer: String,
    },
    #[error(
        "step `{step_key}` declares `{input_key}` as required but nothing binds it — \
         add a binding, or drop it from the step's input schema"
    )]
    UnboundRequiredInput { step_key: String, input_key: String },
    #[error("step `{step_key}` iterates over step `{missing}`, which is not in this workflow")]
    UnknownForEachStep { step_key: String, missing: String },
    #[error(
        "step `{step_key}` iterates over step `{source_step_key}` but does not wait for it — \
         add an edge {source_step_key} -> {step_key}"
    )]
    ForEachNotWaiting {
        step_key: String,
        source_step_key: String,
    },
    #[error(
        "step `{step_key}` binds `{input_key}` to every branch of step `{source_step_key}`, which \
         is not a fan-out — give `{source_step_key}` a for_each_step_key, or bind with source \
         `step` instead"
    )]
    FanInNotFanOut {
        step_key: String,
        input_key: String,
        source_step_key: String,
    },
    #[error(
        "step `{step_key}` binds `{input_key}` to every branch of step `{source_step_key}` but \
         does not wait for it — add an edge {source_step_key} -> {step_key}"
    )]
    FanInNotWaiting {
        step_key: String,
        input_key: String,
        source_step_key: String,
    },
    #[error(
        "step `{step_key}` binds `{input_key}` to step `{source_step_key}`'s output, but \
         `{source_step_key}` is a fan-out and produces one output per branch — bind with source \
         `fan_in` instead"
    )]
    StepBindingOnFanOut {
        step_key: String,
        input_key: String,
        source_step_key: String,
    },
    #[error(
        "loop `{loop_group}` needs exactly one exit step — a body step with nothing after it \
         inside the body, whose output decides whether to go round again — but {} qualify: [{}]",
        .candidates.len(),
        .candidates.join(", ")
    )]
    LoopBodyNotSingleExit {
        loop_group: String,
        candidates: Vec<String>,
    },
    #[error(
        "step `{step_key}` is in loop `{loop_group}` but is not its exit step `{exit_step_key}`, \
         so `loop_until` and `loop_max_iterations` set here would never be read — move them to \
         `{exit_step_key}`"
    )]
    LoopSettingOffExitStep {
        step_key: String,
        loop_group: String,
        exit_step_key: String,
    },
    #[error(
        "loop `{loop_group}` has no `loop_until` on its exit step `{step_key}` — a loop with no \
         exit condition always runs its full budget, which is a retry, not a loop"
    )]
    LoopWithoutExitCondition {
        loop_group: String,
        step_key: String,
    },
    #[error(
        "loop `{loop_group}` has an unusable `loop_until`: {details} — it takes the same shape as \
         a task_output gate, e.g. {{\"pointer\": \"/tests/passed\", \"equals\": true}}"
    )]
    LoopExitConditionInvalid { loop_group: String, details: String },
    #[error(
        "loop `{loop_group}` asks for {max_iterations} iterations; it must be between 1 and {ceiling} \
         — every pass is a live model call, so the ceiling is enforced here rather than trusted \
         to a number in a row"
    )]
    LoopMaxIterationsOutOfRange {
        loop_group: String,
        max_iterations: i64,
        ceiling: i64,
    },
    #[error(
        "step `{step_key}` is both a loop body step and a fan-out — both grow the run graph after \
         launch, and one step doing both has no single answer for what an iteration contains"
    )]
    LoopStepIsAlsoFanOut { step_key: String },
    #[error(
        "edge {parent} -> {child} is an on_exhausted edge, but `{parent}` is not the exit step of \
         a loop — only a loop can run out of attempts"
    )]
    OnExhaustedNotFromLoop { parent: String, child: String },
    #[error(
        "step `{child}` waits on the outcome of both loop `{first}` and loop `{second}` — a step \
         waits on one loop's verdict, and two would silently keep only one of them"
    )]
    StepAwaitsTwoLoops {
        child: String,
        first: String,
        second: String,
    },
    #[error(
        "step `{step_key}` binds `{input_key}` to a previous iteration, but is not in a loop body \
         — outside a loop there is no previous iteration to read"
    )]
    PreviousIterationOutsideLoop { step_key: String, input_key: String },
    #[error(
        "step `{step_key}` binds `{input_key}` to the previous iteration of `{source_step_key}`, \
         which is not in the same loop body"
    )]
    PreviousIterationOutsideBody {
        step_key: String,
        input_key: String,
        source_step_key: String,
    },
    #[error(
        "loop `{loop_group}` binds from a previous iteration, so iteration 1 needs the loop's \
         entry step to read instead, and {} qualify: [{}] — a loop entered from more than one \
         place has no single first value",
        .entries.len(),
        .entries.join(", ")
    )]
    LoopEntryAmbiguous {
        loop_group: String,
        entries: Vec<String>,
    },
    #[error(
        "gate `{gate_key}` is declared on step `{step_key}`, which is not in this workflow — \
         delete the gate, or add the step back"
    )]
    UnknownGateStep { step_key: String, gate_key: String },
    #[error(
        "gate `{gate_key}` on step `{step_key}` reads the output of step `{missing}`, which is \
         not in this workflow"
    )]
    UnknownGateSource {
        step_key: String,
        gate_key: String,
        missing: String,
    },
    #[error(
        "gate `{gate_key}` on step `{step_key}` reads that step's own output — a step cannot be \
         the condition for whether it runs"
    )]
    GateReadsItsOwnStep { step_key: String, gate_key: String },
    #[error(
        "this workflow has {steps} steps and one run may hold at most {ceiling} tasks (limit: \
         MAX_RUN_TASKS) — a template already over the ceiling at launch would be parked the \
         moment it started, so it is refused here where the message can name what to change"
    )]
    RunTaskCeiling { steps: usize, ceiling: i64 },
    #[error("gate `{gate_key}` on step `{step_key}` is not usable: {details}")]
    GateConfigInvalid {
        step_key: String,
        gate_key: String,
        details: String,
    },
    #[error("workflow storage error: {0}")]
    Storage(String),
}

pub struct WorkflowStore {
    pool: SqlitePool,
}

impl WorkflowStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // -- Templates ----------------------------------------------------------

    pub async fn create_workflow(
        &self,
        name: &str,
        description: Option<&str>,
        input_schema: Option<&Value>,
    ) -> Result<Workflow> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO workflows (id, name, description, input_schema) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(description)
        .bind(input_schema.map(|value| value.to_string()))
        .execute(&self.pool)
        .await
        .context("failed to create workflow")?;

        self.get_workflow(&id)
            .await?
            .context("workflow inserted but not found")
            .map_err(Into::into)
    }

    /// Replace a template's own fields. Steps, edges, and bindings are
    /// addressed separately and are untouched by this.
    pub async fn update_workflow(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        input_schema: Option<&Value>,
    ) -> Result<Option<Workflow>> {
        let result = sqlx::query(
            "UPDATE workflows SET name = ?, description = ?, input_schema = ?, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?",
        )
        .bind(name)
        .bind(description)
        .bind(input_schema.map(|value| value.to_string()))
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to update workflow")?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_workflow(id).await
    }

    pub async fn get_workflow(&self, id: &str) -> Result<Option<Workflow>> {
        let row = sqlx::query(
            "SELECT id, name, description, input_schema, created_at, updated_at \
             FROM workflows WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch workflow")?;

        row.map(workflow_from_row).transpose()
    }

    pub async fn list_workflows(&self) -> Result<Vec<Workflow>> {
        let rows = sqlx::query(
            "SELECT id, name, description, input_schema, created_at, updated_at \
             FROM workflows ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflows")?;

        rows.into_iter().map(workflow_from_row).collect()
    }

    pub async fn delete_workflow(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM workflows WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete workflow")?;
        Ok(result.rows_affected() > 0)
    }

    /// Add or replace a step. Keyed on `step_key`, so editing a step is one
    /// call and cannot leave a duplicate behind.
    pub async fn put_step(&self, step: &WorkflowStep) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_steps \
                 (workflow_id, step_key, title, description, assigned_agent_id, priority, \
                  input_schema, output_schema, system_prompt, repo_id, position, \
                  for_each_step_key, for_each_pointer, for_each_key, \
                  loop_group, loop_max_iterations, loop_until) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (workflow_id, step_key) DO UPDATE SET \
                 title = excluded.title, description = excluded.description, \
                 assigned_agent_id = excluded.assigned_agent_id, priority = excluded.priority, \
                 input_schema = excluded.input_schema, output_schema = excluded.output_schema, \
                 system_prompt = excluded.system_prompt, repo_id = excluded.repo_id, \
                 position = excluded.position, \
                 for_each_step_key = excluded.for_each_step_key, \
                 for_each_pointer = excluded.for_each_pointer, \
                 for_each_key = excluded.for_each_key, \
                 loop_group = excluded.loop_group, \
                 loop_max_iterations = excluded.loop_max_iterations, \
                 loop_until = excluded.loop_until",
        )
        .bind(&step.workflow_id)
        .bind(&step.step_key)
        .bind(&step.title)
        .bind(&step.description)
        .bind(&step.assigned_agent_id)
        .bind(step.priority.as_str())
        .bind(step.input_schema.as_ref().map(|v| v.to_string()))
        .bind(step.output_schema.as_ref().map(|v| v.to_string()))
        .bind(&step.system_prompt)
        .bind(&step.repo_id)
        .bind(step.position)
        .bind(&step.for_each_step_key)
        .bind(&step.for_each_pointer)
        .bind(&step.for_each_key)
        .bind(&step.loop_group)
        .bind(step.loop_max_iterations)
        .bind(step.loop_until.as_ref().map(|value| value.to_string()))
        .execute(&self.pool)
        .await
        .context("failed to write workflow step")?;
        Ok(())
    }

    pub async fn list_steps(&self, workflow_id: &str) -> Result<Vec<WorkflowStep>> {
        let rows = sqlx::query(
            "SELECT workflow_id, step_key, title, description, assigned_agent_id, priority, \
                    input_schema, output_schema, system_prompt, repo_id, position, \
                    for_each_step_key, for_each_pointer, for_each_key, \
                    loop_group, loop_max_iterations, loop_until \
             FROM workflow_steps WHERE workflow_id = ? ORDER BY position ASC, step_key ASC",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow steps")?;

        rows.into_iter().map(step_from_row).collect()
    }

    /// Remove a step, and everything that referenced it.
    ///
    /// The cascade is the point. An edge or binding naming a step that no
    /// longer exists makes *every* future launch fail validation, and the
    /// dangling row is invisible in a step-oriented editor — so the template
    /// would be permanently unlaunchable with nothing on screen to explain it.
    /// Deleting a step is the only moment we can be sure those rows are stale.
    pub async fn delete_step(&self, workflow_id: &str, step_key: &str) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM workflow_steps WHERE workflow_id = ? AND step_key = ?")
                .bind(workflow_id)
                .bind(step_key)
                .execute(&self.pool)
                .await
                .context("failed to delete workflow step")?;

        sqlx::query(
            "DELETE FROM workflow_step_edges WHERE workflow_id = ? \
             AND (parent_step_key = ? OR child_step_key = ?)",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(step_key)
        .execute(&self.pool)
        .await
        .context("failed to delete edges of a removed step")?;

        // Both directions: bindings *on* the step, and bindings elsewhere that
        // read *from* it.
        sqlx::query(
            "DELETE FROM workflow_step_bindings WHERE workflow_id = ? \
             AND (step_key = ? OR source_step_key = ?)",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(step_key)
        .execute(&self.pool)
        .await
        .context("failed to delete bindings of a removed step")?;

        // Gates cascade the same way and for the same reason, both directions
        // included: a gate reading the output of a step that no longer exists
        // is a condition nothing can ever answer, and launch would refuse the
        // whole template over a row the step editor cannot show.
        sqlx::query(
            "DELETE FROM workflow_step_gates WHERE workflow_id = ? \
             AND (step_key = ? OR source_step_key = ?)",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(step_key)
        .execute(&self.pool)
        .await
        .context("failed to delete gates of a removed step")?;

        Ok(result.rows_affected() > 0)
    }

    /// Make one step wait for another, the ordinary way.
    pub async fn link_steps(&self, workflow_id: &str, parent: &str, child: &str) -> Result<()> {
        self.link_steps_with_kind(workflow_id, parent, child, StepEdgeKind::Normal)
            .await
    }

    /// Make one step wait for another, saying which outcome it follows.
    ///
    /// Upsert rather than `INSERT OR IGNORE`: two steps have one relationship,
    /// so re-linking an existing pair changes what that relationship *is*
    /// rather than silently keeping the old kind. A pair wired both ways is
    /// exactly the merge the kind exists to prevent.
    pub async fn link_steps_with_kind(
        &self,
        workflow_id: &str,
        parent: &str,
        child: &str,
        kind: StepEdgeKind,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_step_edges \
                 (workflow_id, parent_step_key, child_step_key, kind) VALUES (?, ?, ?, ?) \
             ON CONFLICT (workflow_id, parent_step_key, child_step_key) DO UPDATE SET \
                 kind = excluded.kind",
        )
        .bind(workflow_id)
        .bind(parent)
        .bind(child)
        .bind(kind.as_str())
        .execute(&self.pool)
        .await
        .context("failed to link workflow steps")?;
        Ok(())
    }

    /// Would adding `parent -> child` close a loop? If so, the path that does.
    ///
    /// Checked when the edge is *saved*, not only at launch. A template that
    /// accepts a cycle and refuses to run is a trap: the author gets a success,
    /// walks away, and finds out much later — possibly from someone else. The
    /// path is returned rather than a bare yes so the refusal can name the loop.
    pub async fn cycle_from_edge(
        &self,
        workflow_id: &str,
        parent: &str,
        child: &str,
    ) -> Result<Option<Vec<String>>> {
        if parent == child {
            return Ok(Some(vec![parent.to_string(), child.to_string()]));
        }

        let edges = self.list_edges(workflow_id).await?;

        // Walk forward from `child`. Reaching `parent` means `parent` is
        // already downstream of `child`, so the new edge would close the ring.
        // Iterative: the graph is hand-built and a deep chain must not blow the
        // stack.
        let mut frontier = vec![vec![child.to_string()]];
        let mut seen: HashSet<String> = HashSet::from([child.to_string()]);

        while let Some(path) = frontier.pop() {
            let tip = path.last().expect("a path always has a tip").clone();
            for (from, to) in &edges {
                if from != &tip {
                    continue;
                }
                if to == parent {
                    let mut cycle = path.clone();
                    cycle.push(to.clone());
                    return Ok(Some(cycle));
                }
                if seen.insert(to.clone()) {
                    let mut next = path.clone();
                    next.push(to.clone());
                    frontier.push(next);
                }
            }
        }

        Ok(None)
    }

    /// The next free display position, so steps added without one do not stack.
    pub async fn next_step_position(&self, workflow_id: &str) -> Result<i64> {
        let highest: Option<i64> =
            sqlx::query_scalar("SELECT MAX(position) FROM workflow_steps WHERE workflow_id = ?")
                .bind(workflow_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to read the highest step position")?;
        Ok(highest.map(|value| value + 1).unwrap_or(0))
    }

    pub async fn unlink_steps(&self, workflow_id: &str, parent: &str, child: &str) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workflow_step_edges WHERE workflow_id = ? \
             AND parent_step_key = ? AND child_step_key = ?",
        )
        .bind(workflow_id)
        .bind(parent)
        .bind(child)
        .execute(&self.pool)
        .await
        .context("failed to unlink workflow steps")?;
        Ok(result.rows_affected() > 0)
    }

    /// Every edge as a plain pair.
    ///
    /// Ordering and reachability do not care which outcome an edge follows — an
    /// `on_exhausted` child still comes after the loop — so cycle detection and
    /// the "does this wait for that" checks read the graph through this.
    pub async fn list_edges(&self, workflow_id: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .list_step_edges(workflow_id)
            .await?
            .into_iter()
            .map(|edge| (edge.parent_step_key, edge.child_step_key))
            .collect())
    }

    /// Every edge with the outcome it follows.
    pub async fn list_step_edges(&self, workflow_id: &str) -> Result<Vec<StepEdge>> {
        let rows = sqlx::query(
            "SELECT parent_step_key, child_step_key, kind FROM workflow_step_edges \
             WHERE workflow_id = ? ORDER BY parent_step_key, child_step_key",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow edges")?;

        rows.into_iter()
            .map(|row| {
                let kind: String = row.try_get("kind").unwrap_or_else(|_| "normal".into());
                Ok(StepEdge {
                    parent_step_key: row
                        .try_get("parent_step_key")
                        .context("failed to read parent_step_key")?,
                    child_step_key: row
                        .try_get("child_step_key")
                        .context("failed to read child_step_key")?,
                    kind: StepEdgeKind::parse(&kind).unwrap_or(StepEdgeKind::Normal),
                })
            })
            .collect()
    }

    pub async fn put_binding(&self, binding: &StepBinding) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_step_bindings \
                 (workflow_id, step_key, input_key, source, source_step_key, source_pointer, \
                  literal_value) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (workflow_id, step_key, input_key) DO UPDATE SET \
                 source = excluded.source, source_step_key = excluded.source_step_key, \
                 source_pointer = excluded.source_pointer, \
                 literal_value = excluded.literal_value",
        )
        .bind(&binding.workflow_id)
        .bind(&binding.step_key)
        .bind(&binding.input_key)
        .bind(binding.source.as_str())
        .bind(&binding.source_step_key)
        .bind(&binding.source_pointer)
        .bind(binding.literal_value.as_ref().map(|v| v.to_string()))
        .execute(&self.pool)
        .await
        .context("failed to write workflow binding")?;
        Ok(())
    }

    pub async fn delete_binding(
        &self,
        workflow_id: &str,
        step_key: &str,
        input_key: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workflow_step_bindings WHERE workflow_id = ? \
             AND step_key = ? AND input_key = ?",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(input_key)
        .execute(&self.pool)
        .await
        .context("failed to delete workflow binding")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_bindings(&self, workflow_id: &str) -> Result<Vec<StepBinding>> {
        let rows = sqlx::query(
            "SELECT workflow_id, step_key, input_key, source, source_step_key, source_pointer, \
                    literal_value \
             FROM workflow_step_bindings WHERE workflow_id = ? ORDER BY step_key, input_key",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow bindings")?;

        rows.into_iter().map(binding_from_row).collect()
    }

    /// Add or replace a gate on a step.
    ///
    /// Keyed by `gate_key` rather than a generated id, so an editor saving the
    /// same condition twice edits it. The alternative is a step held behind two
    /// copies of one gate, which reads as the condition being unsatisfiable.
    pub async fn put_gate(&self, gate: &StepGate) -> Result<()> {
        sqlx::query(
            "INSERT INTO workflow_step_gates \
                 (workflow_id, step_key, gate_key, kind, source_step_key, config, label, \
                  poll_interval_secs, disposition) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (workflow_id, step_key, gate_key) DO UPDATE SET \
                 kind = excluded.kind, source_step_key = excluded.source_step_key, \
                 config = excluded.config, label = excluded.label, \
                 poll_interval_secs = excluded.poll_interval_secs, \
                 disposition = excluded.disposition",
        )
        .bind(&gate.workflow_id)
        .bind(&gate.step_key)
        .bind(&gate.gate_key)
        .bind(gate.kind.as_str())
        .bind(&gate.source_step_key)
        .bind(gate.config.to_string())
        .bind(&gate.label)
        .bind(gate.poll_interval_secs)
        .bind(gate.disposition.map(GateDisposition::as_str))
        .execute(&self.pool)
        .await
        .context("failed to write workflow gate")?;
        Ok(())
    }

    pub async fn delete_gate(
        &self,
        workflow_id: &str,
        step_key: &str,
        gate_key: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM workflow_step_gates WHERE workflow_id = ? \
             AND step_key = ? AND gate_key = ?",
        )
        .bind(workflow_id)
        .bind(step_key)
        .bind(gate_key)
        .execute(&self.pool)
        .await
        .context("failed to delete workflow gate")?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_gates(&self, workflow_id: &str) -> Result<Vec<StepGate>> {
        let rows = sqlx::query(
            "SELECT workflow_id, step_key, gate_key, kind, source_step_key, config, label, \
                    poll_interval_secs, disposition \
             FROM workflow_step_gates WHERE workflow_id = ? ORDER BY step_key, gate_key",
        )
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow gates")?;

        rows.into_iter().map(gate_from_row).collect()
    }

    // -- Launch -------------------------------------------------------------

    /// Compile a workflow into tasks, edges, and bindings, and record the run.
    ///
    /// Everything is validated before anything is written, because a half-built
    /// graph is worse than none: the scheduler would immediately start running
    /// the part that exists. Validation cannot be the whole story though —
    /// writing spans two stores and can still fail on its own (a closed pool, a
    /// disk error), so a failure part-way through unwinds what it emitted
    /// before returning. See `rollback_run`.
    pub async fn launch(
        &self,
        task_store: &TaskStore,
        workflow_id: &str,
        inputs: &Value,
        launched_by: &str,
    ) -> std::result::Result<InstantiatedRun, LaunchError> {
        let workflow = self
            .get_workflow(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?
            .ok_or_else(|| LaunchError::UnknownWorkflow {
                id: workflow_id.to_string(),
            })?;

        let steps = self
            .list_steps(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        if steps.is_empty() {
            return Err(LaunchError::NoSteps {
                id: workflow_id.to_string(),
            });
        }
        // The third and outermost place the run task ceiling is enforced. Both
        // of the others — fan-out expansion and loop iteration — park a run
        // that has already started; this one refuses while a person is still
        // watching, which is the only place the answer can be a corrected
        // template rather than an incident.
        if steps.len() as i64 > crate::tasks::MAX_RUN_TASKS {
            return Err(LaunchError::RunTaskCeiling {
                steps: steps.len(),
                ceiling: crate::tasks::MAX_RUN_TASKS,
            });
        }

        let step_edges = self
            .list_step_edges(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        // Ordering and reachability do not care which outcome an edge follows.
        let edges: Vec<(String, String)> = step_edges
            .iter()
            .map(|edge| (edge.parent_step_key.clone(), edge.child_step_key.clone()))
            .collect();
        let bindings = self
            .list_bindings(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        let gates = self
            .list_gates(workflow_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;

        // 1. The launch input, checked once, here — where a person is still
        //    watching. Deferring it means the failure surfaces as an
        //    unresolvable binding inside step three, a long way from the cause.
        if let Some(schema) = &workflow.input_schema {
            let problems = validate(schema, inputs);
            if !problems.is_empty() {
                return Err(LaunchError::InvalidInput {
                    details: problems.join("; "),
                });
            }
        }

        let known: HashSet<&str> = steps.iter().map(|step| step.step_key.as_str()).collect();

        // 2. Every reference resolves. Checked before the sort so a typo is
        //    reported as a typo rather than as a missing graph node.
        for (parent, child) in &edges {
            if !known.contains(parent.as_str()) || !known.contains(child.as_str()) {
                return Err(LaunchError::UnknownEdgeReference {
                    parent: parent.clone(),
                    child: child.clone(),
                });
            }
        }
        for binding in &bindings {
            if matches!(
                binding.source,
                BindingSource::Step | BindingSource::FanIn | BindingSource::PreviousIteration
            ) {
                let target = binding.source_step_key.as_deref().unwrap_or("");
                if !known.contains(target) {
                    return Err(LaunchError::UnknownStepReference {
                        step_key: binding.step_key.clone(),
                        input_key: binding.input_key.clone(),
                        missing: target.to_string(),
                    });
                }
            }
        }

        // 2a. Gates. Same shape of check as bindings, and for the same reason:
        //     a gate naming a step that is not here can never be answered, and
        //     a gate whose config will not evaluate would error once a minute
        //     forever with nobody reading the log. Both are knowable now.
        for gate in &gates {
            if !known.contains(gate.step_key.as_str()) {
                return Err(LaunchError::UnknownGateStep {
                    step_key: gate.step_key.clone(),
                    gate_key: gate.gate_key.clone(),
                });
            }
            if gate.kind == GateKind::TaskOutput {
                let target = gate.source_step_key.as_deref().unwrap_or("");
                if !known.contains(target) {
                    return Err(LaunchError::UnknownGateSource {
                        step_key: gate.step_key.clone(),
                        gate_key: gate.gate_key.clone(),
                        missing: target.to_string(),
                    });
                }
                // A step gated on its own output can never run, so it can never
                // produce the output — a deadlock that reads like a typo
                // because it is one.
                if target == gate.step_key {
                    return Err(LaunchError::GateReadsItsOwnStep {
                        step_key: gate.step_key.clone(),
                        gate_key: gate.gate_key.clone(),
                    });
                }
            }
            if let Err(error) = validate_step_gate(gate) {
                return Err(LaunchError::GateConfigInvalid {
                    step_key: gate.step_key.clone(),
                    gate_key: gate.gate_key.clone(),
                    details: error.to_string(),
                });
            }
        }

        // 2b. Fan-out and fan-in wiring.
        //
        //     A missing edge is *refused* rather than quietly created. The edge
        //     could be synthesized here — iterating a step's output does imply
        //     waiting for it — but then the run graph would carry an edge the
        //     template does not, and the canvas would draw a pipeline that is
        //     not the one that ran. A refusal naming the edge to add keeps the
        //     picture and the run the same object.
        //
        //     Reachability rather than a direct edge: a fan-out that waits on
        //     its source through an intervening step is wired correctly, and
        //     demanding the shortcut edge would reject a legitimate template.
        let fan_outs: HashSet<&str> = steps
            .iter()
            .filter(|step| step.for_each_step_key.is_some())
            .map(|step| step.step_key.as_str())
            .collect();

        for step in &steps {
            let Some(source) = step.for_each_step_key.as_deref() else {
                continue;
            };
            if !known.contains(source) {
                return Err(LaunchError::UnknownForEachStep {
                    step_key: step.step_key.clone(),
                    missing: source.to_string(),
                });
            }
            if !precedes(&edges, source, &step.step_key) {
                return Err(LaunchError::ForEachNotWaiting {
                    step_key: step.step_key.clone(),
                    source_step_key: source.to_string(),
                });
            }
        }

        for binding in &bindings {
            let target = binding.source_step_key.as_deref().unwrap_or("");
            match binding.source {
                // A fan-in over an ordinary step would resolve to a single
                // entry keyed by a task number — plausible-looking nonsense
                // rather than an error, which is the worst kind of default.
                BindingSource::FanIn => {
                    if !fan_outs.contains(target) {
                        return Err(LaunchError::FanInNotFanOut {
                            step_key: binding.step_key.clone(),
                            input_key: binding.input_key.clone(),
                            source_step_key: target.to_string(),
                        });
                    }
                    if !precedes(&edges, target, &binding.step_key) {
                        return Err(LaunchError::FanInNotWaiting {
                            step_key: binding.step_key.clone(),
                            input_key: binding.input_key.clone(),
                            source_step_key: target.to_string(),
                        });
                    }
                }
                // The mirror image: a plain step binding onto a fan-out points
                // at the placeholder, which expansion deletes. It would resolve
                // at launch and dangle the moment the fan-out widened.
                BindingSource::Step => {
                    if fan_outs.contains(target) {
                        return Err(LaunchError::StepBindingOnFanOut {
                            step_key: binding.step_key.clone(),
                            input_key: binding.input_key.clone(),
                            source_step_key: target.to_string(),
                        });
                    }
                }
                BindingSource::Literal
                | BindingSource::RunInput
                | BindingSource::PreviousIteration => {}
            }
        }

        // 3. Every input a step says it needs is actually wired.
        //
        //    Without this the mistake surfaces at run time as an unresolvable
        //    contract, one step into a pipeline someone thought was fine — the
        //    same class of failure as an unknown step reference, and equally
        //    knowable at launch. Only `required` keys are checked: an optional
        //    input with no binding is a deliberate default, not an omission.
        for step in &steps {
            let Some(required) = step
                .input_schema
                .as_ref()
                .and_then(|schema| schema.get("required"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for key in required.iter().filter_map(Value::as_str) {
                let bound = bindings
                    .iter()
                    .any(|binding| binding.step_key == step.step_key && binding.input_key == key);
                if !bound {
                    return Err(LaunchError::UnboundRequiredInput {
                        step_key: step.step_key.clone(),
                        input_key: key.to_string(),
                    });
                }
            }
        }

        // 4. Acyclic. A cycle here would deadlock silently at run time: every
        //    step waiting on a parent that is itself waiting.
        //
        //    Unchanged by loops, and deliberately so: a loop is *declared* by
        //    steps sharing a `loop_group`, never inferred from an edge pointing
        //    backwards, so the template stays a DAG and an accidental cycle is
        //    still an accidental cycle.
        topologically_ordered(&steps, &edges)?;

        // 4b. Loop bodies. After the sort, so a body with an internal cycle is
        //     reported as the cycle it is rather than as a loop with no exit.
        let loops = loop_wiring(&steps, &step_edges, &bindings)?;

        // 5. Resolve run-input bindings now, while the payload is in hand.
        //    Frozen to literals rather than kept as live references so a step
        //    retried an hour later sees exactly what the run was launched with.
        let mut frozen: HashMap<(String, String), Value> = HashMap::new();
        for binding in &bindings {
            if binding.source != BindingSource::RunInput {
                continue;
            }
            let pointer = binding.source_pointer.as_deref().unwrap_or("");
            let value = if pointer.is_empty() {
                Some(inputs)
            } else {
                inputs.pointer(pointer)
            };
            let value = value.ok_or_else(|| LaunchError::MissingRunInput {
                step_key: binding.step_key.clone(),
                input_key: binding.input_key.clone(),
                pointer: pointer.to_string(),
            })?;
            frozen.insert(
                (binding.step_key.clone(), binding.input_key.clone()),
                value.clone(),
            );
        }

        // 6. Emit. Everything above passed, so this should not fail on content.
        let run_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO workflow_runs (id, workflow_id, inputs, launched_by) VALUES (?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(workflow_id)
        .bind(inputs.to_string())
        .bind(launched_by)
        .execute(&self.pool)
        .await
        .map_err(|error| LaunchError::Storage(error.to_string()))?;

        let mut task_numbers: HashMap<String, i64> = HashMap::new();
        if let Err(error) = self
            .emit_graph(
                task_store,
                &run_id,
                &steps,
                &edges,
                &bindings,
                &gates,
                &frozen,
                &loops,
                launched_by,
                &mut task_numbers,
            )
            .await
        {
            self.rollback_run(task_store, &run_id, &task_numbers).await;
            return Err(error);
        }

        let run = self
            .get_run(&run_id)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?
            .ok_or_else(|| LaunchError::Storage("run inserted but not found".to_string()))?;

        Ok(InstantiatedRun { run, task_numbers })
    }

    /// Write the tasks, edges, and bindings for a validated launch.
    ///
    /// `task_numbers` is an out-parameter rather than a return value so the
    /// caller can clean up whatever was emitted before a failure.
    #[allow(clippy::too_many_arguments)]
    async fn emit_graph(
        &self,
        task_store: &TaskStore,
        run_id: &str,
        steps: &[WorkflowStep],
        edges: &[(String, String)],
        bindings: &[StepBinding],
        gates: &[StepGate],
        frozen: &HashMap<(String, String), Value>,
        loops: &LoopWiring,
        launched_by: &str,
        task_numbers: &mut HashMap<String, i64>,
    ) -> std::result::Result<(), LaunchError> {
        for step in steps {
            let task = task_store
                .create(CreateTaskInput {
                    owner_agent_id: launched_by.to_string(),
                    assigned_agent_id: step
                        .assigned_agent_id
                        .clone()
                        .unwrap_or_else(|| launched_by.to_string()),
                    title: step.title.clone(),
                    description: step.description.clone(),
                    // Every step starts in backlog, entry steps included. The
                    // sweep decides what is eligible by the same rule it uses
                    // for everything else, rather than the instantiator
                    // guessing which step is first. A pipeline whose first step
                    // has an unsatisfiable binding then stalls visibly instead
                    // of being claimed and immediately blocked.
                    status: TaskStatus::Backlog,
                    priority: step.priority,
                    created_by: launched_by.to_string(),
                    binding: TaskProjectBinding {
                        repo_id: step.repo_id.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;

            sqlx::query(
                "UPDATE tasks SET workflow_run_id = ?, workflow_step_key = ?, \
                 input_schema = ?, output_schema = ?, system_prompt = ? WHERE task_number = ?",
            )
            .bind(run_id)
            .bind(&step.step_key)
            .bind(step.input_schema.as_ref().map(|v| v.to_string()))
            .bind(step.output_schema.as_ref().map(|v| v.to_string()))
            .bind(&step.system_prompt)
            .bind(task.task_number)
            .execute(&self.pool)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;

            task_numbers.insert(step.step_key.clone(), task.task_number);
        }

        // A fan-out step becomes exactly one task, marked as a placeholder.
        //
        // Its width is unknown until the step it iterates finishes, but the
        // steps downstream still need something to wait on right now: emitted
        // with no parent they would be promoted on the first sweep and run the
        // report before anything was built. So the placeholder carries exactly
        // the edges the branches will inherit and is never claimed.
        //
        // Done as a second pass because a step may iterate one that comes after
        // it in display order, and the source's task number has to exist first.
        for step in steps {
            let Some(source_key) = step.for_each_step_key.as_deref() else {
                continue;
            };
            let Some(source_task_number) = task_numbers.get(source_key).copied() else {
                continue;
            };
            let spec = crate::tasks::FanOutSpec {
                source_task_number,
                pointer: step.for_each_pointer.clone().unwrap_or_default(),
                key: step.for_each_key.clone(),
            };
            let Some(task_number) = task_numbers.get(&step.step_key) else {
                continue;
            };

            sqlx::query(
                "UPDATE tasks SET fan_out_placeholder = 1, metadata = ? WHERE task_number = ?",
            )
            .bind(spec.to_metadata().to_string())
            .bind(task_number)
            .execute(&self.pool)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        // A loop body becomes iteration 1, and its exit step carries the spec
        // every later iteration is decided by.
        //
        // Frozen onto the task rather than read back from `workflow_steps` when
        // the body finishes, for the same reason a fan-out spec is: a template
        // edited mid-run must not change what a run already in flight does, and
        // a run whose template was deleted still has to finish.
        for plan in &loops.plans {
            let spec = crate::tasks::LoopSpec {
                group: plan.group.clone(),
                max_iterations: plan.max_iterations,
                until: plan.until.clone(),
                previous_iteration: plan.previous_iteration.clone(),
            };

            for member in &plan.members {
                let Some(task_number) = task_numbers.get(member) else {
                    continue;
                };
                let is_exit = member == &plan.exit_step_key;
                sqlx::query(
                    "UPDATE tasks SET loop_group = ?, loop_iteration = 1, loop_terminal = ?, \
                     metadata = COALESCE(?, metadata) WHERE task_number = ?",
                )
                .bind(&plan.group)
                .bind(i64::from(is_exit))
                .bind(is_exit.then(|| spec.to_metadata().to_string()))
                .bind(task_number)
                .execute(&self.pool)
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;
            }
        }

        // Both arms of a loop's exit are emitted held.
        //
        // Neither can simply wait on the body — the body finishes whether the
        // loop converged or gave up, so completion alone would release both.
        // Nor can either be left parentless, which would have the first sweep
        // run it before the loop had run at all. So the edges below are wired
        // exactly as they are for any other step, and these columns are what
        // the sweep checks; only the iteration boundary clears them.
        for (step_key, (group, arm)) in &loops.awaiting {
            let Some(task_number) = task_numbers.get(step_key) else {
                continue;
            };
            let condition = match arm {
                crate::tasks::LoopArm::Normal => "converges",
                crate::tasks::LoopArm::OnExhausted => "runs out of attempts",
            };
            sqlx::query(
                "UPDATE tasks SET awaiting_loop_group = ?, awaiting_loop_arm = ?, \
                 block_kind = 'dependency', block_reason = ? WHERE task_number = ?",
            )
            .bind(group)
            .bind(arm.as_str())
            .bind(format!(
                "runs only if loop `{group}` {condition}; waiting to see whether it does"
            ))
            .bind(task_number)
            .execute(&self.pool)
            .await
            .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        // Every edge becomes a dependency, `on_exhausted` included. The kind
        // decides *whether* the child runs, not whether it waits — and an edge
        // left out here would be an edge the canvas draws and the run does not
        // have.
        for (parent, child) in edges {
            let (Some(parent_number), Some(child_number)) =
                (task_numbers.get(parent), task_numbers.get(child))
            else {
                continue;
            };
            task_store
                .link_tasks(*parent_number, *child_number)
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        for binding in bindings {
            let Some(child_number) = task_numbers.get(&binding.step_key) else {
                continue;
            };

            let translated = match binding.source {
                // The whole translation: a step key becomes the task number it
                // was compiled into.
                BindingSource::Step => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: binding
                        .source_step_key
                        .as_ref()
                        .and_then(|key| task_numbers.get(key))
                        .copied(),
                    source_pointer: binding.source_pointer.clone(),
                    literal_value: None,
                    fan_in_step_key: None,
                },
                BindingSource::Literal => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: None,
                    source_pointer: None,
                    literal_value: binding.literal_value.clone(),
                    fan_in_step_key: None,
                },
                BindingSource::RunInput => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: None,
                    source_pointer: None,
                    literal_value: frozen
                        .get(&(binding.step_key.clone(), binding.input_key.clone()))
                        .cloned(),
                    fan_in_step_key: None,
                },
                // Iteration 1 has no previous iteration, so it reads the loop's
                // entry step instead — the single step outside the body that
                // feeds it, at the same pointer. That is what lets the body
                // need no special first-pass wiring: every later iteration is
                // repointed at the pass it is replacing when it is emitted.
                BindingSource::PreviousIteration => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: loops
                        .plans
                        .iter()
                        .find(|plan| plan.members.iter().any(|key| key == &binding.step_key))
                        .and_then(|plan| plan.entry_step_key.as_ref())
                        .and_then(|key| task_numbers.get(key))
                        .copied(),
                    source_pointer: binding.source_pointer.clone(),
                    literal_value: None,
                    fan_in_step_key: None,
                },
                // Kept as a step key rather than translated to task numbers:
                // the tasks it names do not exist yet, and the whole point is
                // that how many of them there will be is not yet known.
                BindingSource::FanIn => TaskInputBinding {
                    child_task_number: *child_number,
                    input_key: binding.input_key.clone(),
                    source_task_number: None,
                    source_pointer: None,
                    literal_value: None,
                    fan_in_step_key: binding.source_step_key.clone(),
                },
            };

            task_store
                .set_input_binding(&translated)
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        // Gates compile last, once every step has a number to point at.
        //
        // The translation is the same one bindings do: `source_step_key`
        // becomes `config.task_number`. That is the whole mechanism — the
        // evaluator, the poller, the backoff, and the four result states are
        // the ones `task_gates` already had, and a compiled gate is
        // indistinguishable from one added by hand against a live run.
        //
        // `disposition` is copied through unchanged, `None` included. `None`
        // means "derive at poll time", and it has to stay `None` here rather
        // than being resolved now: at launch every source step is still in the
        // backlog, so deriving it here would freeze every gate as `wait` and
        // no branch would ever route.
        let gate_store = crate::tasks::GateStore::new(task_store.pool().clone());
        for gate in gates {
            let Some(task_number) = task_numbers.get(&gate.step_key) else {
                continue;
            };

            let config = match gate.kind {
                GateKind::Http => gate.config.clone(),
                GateKind::TaskOutput => {
                    let Some(source_number) = gate
                        .source_step_key
                        .as_ref()
                        .and_then(|key| task_numbers.get(key))
                        .copied()
                    else {
                        // Validation already refused this, so reaching it means
                        // a step failed to emit — the launch is unwinding
                        // anyway and a gate pointing nowhere must not be left
                        // behind.
                        continue;
                    };
                    let mut object = match gate.config.as_object() {
                        Some(object) => object.clone(),
                        None => serde_json::Map::new(),
                    };
                    object.insert("task_number".to_string(), Value::from(source_number));
                    Value::Object(object)
                }
            };

            gate_store
                .create(
                    *task_number,
                    gate.kind,
                    &config,
                    gate.label.as_deref(),
                    gate.poll_interval_secs,
                    gate.disposition,
                )
                .await
                .map_err(|error| LaunchError::Storage(error.to_string()))?;
        }

        Ok(())
    }

    /// Undo a launch that failed part-way through emitting.
    ///
    /// Best-effort by necessity — the reason we are here is that writes are
    /// failing. Every failure is logged rather than propagated, because the
    /// caller already has the error that matters and replacing it with a
    /// cleanup error would hide the cause.
    ///
    /// Tasks go first. A leftover `workflow_runs` row is an orphan nobody
    /// reads; a leftover *task* gets picked up and run, which is the outcome
    /// this whole path exists to prevent.
    async fn rollback_run(
        &self,
        task_store: &TaskStore,
        run_id: &str,
        task_numbers: &HashMap<String, i64>,
    ) {
        // Gates first, then the tasks they hang off. A gate outliving its task
        // is a stored, repeating, outbound request against a card that no
        // longer exists — the one piece of a rolled-back launch that would keep
        // costing something.
        let gate_store = crate::tasks::GateStore::new(task_store.pool().clone());
        for (step_key, number) in task_numbers {
            if let Err(error) = gate_store.delete_for_task(*number).await {
                tracing::error!(
                    %error, run_id, step_key, task_number = number,
                    "failed to remove the gates of a rolled-back workflow launch"
                );
            }
        }

        for (step_key, number) in task_numbers {
            if let Err(error) = task_store.delete(*number).await {
                tracing::error!(
                    %error, run_id, step_key, task_number = number,
                    "failed to remove a task from a rolled-back workflow launch; \
                     it may run on its own"
                );
            }
        }

        if let Err(error) = sqlx::query("DELETE FROM workflow_runs WHERE id = ?")
            .bind(run_id)
            .execute(&self.pool)
            .await
        {
            tracing::error!(%error, run_id, "failed to remove a rolled-back workflow run");
        }
    }

    pub async fn get_run(&self, run_id: &str) -> Result<Option<WorkflowRun>> {
        let row = sqlx::query(&format!(
            "{RUN_SELECT_COLUMNS} FROM workflow_runs WHERE id = ?"
        ))
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch workflow run")?;

        row.map(run_from_row).transpose()
    }

    pub async fn list_runs(&self, workflow_id: &str) -> Result<Vec<WorkflowRun>> {
        let rows = sqlx::query(&format!(
            "{RUN_SELECT_COLUMNS} FROM workflow_runs WHERE workflow_id = ? \
             ORDER BY created_at DESC"
        ))
        .bind(workflow_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list workflow runs")?;

        rows.into_iter().map(run_from_row).collect()
    }

    // -- Run state ----------------------------------------------------------

    /// Runs still worth judging, oldest first.
    ///
    /// `grace_secs` is the reaper's floor, for the reaper's reason. A launch
    /// inserts the run row and then emits its graph, so a run examined in that
    /// window has fewer tasks than it will have — possibly none — and would be
    /// read as finished before it had started. The reaper solved the same
    /// shape of race (a claim and its worker registration are separate writes)
    /// with a grace period rather than a lock, and a second mechanism for one
    /// problem is how the two drift apart.
    pub async fn list_assessable_runs(&self, grace_secs: i64) -> Result<Vec<WorkflowRun>> {
        let rows = sqlx::query(&format!(
            "{RUN_SELECT_COLUMNS} FROM workflow_runs \
             WHERE status = 'running' \
               AND created_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?) \
             ORDER BY created_at ASC"
        ))
        .bind(format!("-{} seconds", grace_secs.max(0)))
        .fetch_all(&self.pool)
        .await
        .context("failed to list runs to assess")?;

        rows.into_iter().map(run_from_row).collect()
    }

    /// Move a running run to a terminal status, once.
    ///
    /// `false` means somebody else got there first — another agent's tick, or a
    /// person cancelling — and the caller must not act on the transition it
    /// thought it was making.
    ///
    /// This conditional UPDATE is the whole once-only story behind the
    /// notification. Every agent's cortex assesses every running run, so two
    /// processes reaching the same verdict on the same tick is normal; the
    /// `status = 'running'` guard means exactly one of them observes the
    /// transition, and it is the same pattern `claim_next_ready` uses to hand a
    /// task to exactly one worker. Notifying on *state* instead would repeat
    /// every tick for as long as the run stayed stuck, which is a status nobody
    /// reads dressed up as a status somebody might.
    pub async fn settle_run(&self, run_id: &str, status: RunStatus, reason: &str) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE workflow_runs SET status = ?, status_reason = ?, \
             finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE id = ? AND status = 'running'",
        )
        .bind(status.as_str())
        .bind(reason)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .context("failed to settle a workflow run")?;

        Ok(updated.rows_affected() > 0)
    }

    /// Judge every assessable run and record what changed.
    ///
    /// The periodic pass, shaped like the reaper: it asks one question of each
    /// run, writes only transitions, and returns them so the caller can log and
    /// notify. One run that cannot be assessed does not stop the others —
    /// a storage error while reading one run's tasks is our problem, and
    /// abandoning the pass over it would let every other run go unwatched.
    pub async fn sweep_runs(
        &self,
        task_store: &TaskStore,
        grace_secs: i64,
    ) -> Result<Vec<RunTransition>> {
        let runs = self.list_assessable_runs(grace_secs).await?;
        let mut transitions = Vec::new();

        for run in runs {
            let assessment = match self.assess_run(task_store, &run).await {
                Ok(assessment) => assessment,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        run_id = %run.id,
                        "failed to assess a workflow run — leaving it running"
                    );
                    continue;
                }
            };

            let RunAssessment::Settled { status, reason } = assessment else {
                continue;
            };

            if self.settle_run(&run.id, status, &reason).await? {
                transitions.push(RunTransition {
                    run_id: run.id.clone(),
                    workflow_id: run.workflow_id.clone(),
                    launched_by: run.launched_by.clone(),
                    status,
                    reason,
                });
            }
        }

        Ok(transitions)
    }

    /// Decide whether one run has stopped, and if so how.
    ///
    /// Three questions, in the order that makes the cheap answer the common
    /// one: is anything in flight, is anything at the frontier able to move,
    /// and is anything still waiting on the outside world. None of the three
    /// and not finished means stuck.
    ///
    /// The frontier — unsettled tasks whose every parent has settled — is the
    /// only part of the graph worth interrogating, and that is not an
    /// optimisation. A task waiting on an unfinished parent tells you nothing
    /// about whether the run can advance; its parent does. Judging it too would
    /// make every downstream step of a wedged run report "waiting on a parent"
    /// and the run would never be called stuck. The graph is acyclic, so an
    /// unsettled task set always has a non-empty frontier, which is what makes
    /// the reduction complete rather than merely cheaper.
    pub async fn assess_run(
        &self,
        task_store: &TaskStore,
        run: &WorkflowRun,
    ) -> Result<RunAssessment> {
        let tasks = task_store.list_by_workflow_run(&run.id).await?;

        if tasks.is_empty() {
            // Past the grace period, so this is not a launch mid-emit. Either a
            // rollback left the row behind or somebody deleted the cards; both
            // want a person, and neither is success.
            return Ok(RunAssessment::Settled {
                status: RunStatus::Stuck,
                reason: "this run has no tasks — its launch left nothing behind, so nothing \
                         will ever run"
                    .to_string(),
            });
        }

        // A loop that ran out of attempts and took its `on_exhausted` edge is
        // the run reporting failure through a path its author declared. The
        // give-up branch may still be running, so this only decides the verdict
        // once everything settles — a rollback step in flight is a run that is
        // still going.
        let exhausted = tasks.iter().find(|task| {
            task.loop_resolution == Some(crate::tasks::LoopResolution::ExhaustedRouted)
        });

        let unsettled: Vec<&Task> = tasks
            .iter()
            .filter(|task| !task.status.is_terminal())
            .collect();

        if unsettled.is_empty() {
            if let Some(task) = exhausted {
                return Ok(RunAssessment::Settled {
                    status: RunStatus::Failed,
                    reason: format!(
                        "loop `{}` ran out of attempts at task #{} and the run took its \
                         on_exhausted path",
                        task.loop_group.as_deref().unwrap_or("?"),
                        task.task_number
                    ),
                });
            }

            let skipped: Vec<&Task> = tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Skipped)
                .collect();

            // Succeeded either way. A skipped branch is a condition that ruled
            // a step out, which is the pipeline working — but "everything ran"
            // and "four steps never ran" are different things to have happened,
            // and the reason is where that difference lives rather than in a
            // sixth status every caller would have to treat as success anyway.
            let reason = if skipped.is_empty() {
                format!("all {} task(s) finished", tasks.len())
            } else {
                format!(
                    "{} of {} task(s) finished; {} did not run: {}",
                    tasks.len() - skipped.len(),
                    tasks.len(),
                    skipped.len(),
                    skipped
                        .iter()
                        .map(|task| format!(
                            "#{} ({})",
                            task.task_number,
                            task.skip_reason.as_deref().unwrap_or("no reason recorded")
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            };

            return Ok(RunAssessment::Settled {
                status: RunStatus::Succeeded,
                reason,
            });
        }

        // Anything in flight settles it without a single further query. This is
        // the overwhelmingly common answer for a healthy run, and it comes
        // before every verdict below on purpose: a terminal status stamps
        // `finished_at` and retires the run from ever being looked at again, so
        // declaring one while a worker is still writing would leave the last
        // thing that happened unrecorded. A run with one permanently failed
        // step and three still building has failed; it has not *finished*
        // failing until they stop.
        if unsettled.iter().any(|task| in_flight(task)) {
            return Ok(RunAssessment::Advancing);
        }

        // A task that used its whole failure budget is `failed`, not `stuck`,
        // and the difference is the recovery: the pipeline ran, something in it
        // does not work, and retrying it unchanged will not help. Checked
        // before the frontier walk so it wins over the generic "blocked"
        // reading of the same row — one label covering both is exactly the bug
        // this codebase keeps paying for.
        for task in &unsettled {
            let limit = task
                .max_retries
                .unwrap_or(crate::tasks::DEFAULT_FAILURE_LIMIT);
            if task.consecutive_failures >= limit {
                return Ok(RunAssessment::Settled {
                    status: RunStatus::Failed,
                    reason: format!(
                        "task #{} ({}) used its whole failure budget ({}/{}): {}",
                        task.task_number,
                        task.title,
                        task.consecutive_failures,
                        limit,
                        task.last_error.as_deref().unwrap_or("no error recorded")
                    ),
                });
            }
        }

        let gates = crate::tasks::GateStore::new(self.pool.clone());
        let mut held: Vec<String> = Vec::new();

        for task in &unsettled {
            if !task_store
                .unfinished_parents(task.task_number)
                .await?
                .is_empty()
            {
                // Not at the frontier. Whatever it is waiting for is judged on
                // its own row.
                continue;
            }

            match frontier_hold(task_store, &gates, task).await? {
                // One task that can still move is the whole answer.
                None => return Ok(RunAssessment::Advancing),
                Some(reason) => {
                    held.push(format!("#{} ({}) {reason}", task.task_number, task.title))
                }
            }
        }

        if held.is_empty() {
            // Unsettled tasks, none at the frontier: every one of them waits on
            // another that is not finished. Only a cycle in `task_dependencies`
            // produces this, and it deadlocks exactly as thoroughly as anything
            // below — so it is reported rather than silently read as healthy.
            return Ok(RunAssessment::Settled {
                status: RunStatus::Stuck,
                reason: format!(
                    "all {} unfinished task(s) are waiting on another unfinished task, so none \
                     of them can start — the dependency edges of this run form a cycle",
                    unsettled.len()
                ),
            });
        }

        Ok(RunAssessment::Settled {
            status: RunStatus::Stuck,
            reason: format!("nothing in this run can advance: {}", held.join("; ")),
        })
    }

    /// Stop a run, and settle the work it had not started.
    ///
    /// Unstarted tasks are settled as `skipped`; running ones are left alone to
    /// finish or be reaped, because killing work mid-flight throws away
    /// whatever it had already done and leaves a worker writing into a task
    /// nobody will read.
    ///
    /// `skipped` rather than a new `cancelled` task status, deliberately.
    /// `skipped` already means precisely "settled, and will never run" — it is
    /// terminal, it satisfies a dependency edge, it carries its own
    /// `skip_reason` field, and every promote, claim, fan-out and loop
    /// predicate in the scheduler already accounts for it. An eighth status
    /// would have to be taught to all of them, and a single missed predicate is
    /// a task waiting forever on a parent that will never run. Nothing recovers
    /// differently between a branch that was ruled out and a card cancelled
    /// with the run, which is the test for whether two conditions may share a
    /// label; what differs is *why*, and that is on the card in `skip_reason`
    /// and on the run in `status` and `status_reason`.
    ///
    /// One transaction over both tables — they are one database — so a run
    /// cannot end up cancelled with claimable cards, or emptied without being
    /// cancelled.
    pub async fn cancel_run(&self, run_id: &str, cancelled_by: &str) -> Result<CancelOutcome> {
        let Some(run) = self.get_run(run_id).await? else {
            return Ok(CancelOutcome::NotFound);
        };

        // Cancellable while anything of the run might still be sitting on a
        // board: `running` obviously, and also `stuck` and `failed`, which are
        // terminal for the *run* while leaving parked cards behind. Cancelling
        // one of those is how a person clears them. A `succeeded` run has
        // nothing left to settle and a `cancelled` one has already been through
        // here.
        if matches!(run.status, RunStatus::Succeeded | RunStatus::Cancelled) {
            return Ok(CancelOutcome::AlreadyFinished { status: run.status });
        }

        let reason = format!("cancelled by {cancelled_by}");
        let skip_reason = format!(
            "workflow run {run_id} was cancelled by {cancelled_by} before this task started"
        );

        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open a run cancellation transaction")?;

        sqlx::query(
            "UPDATE workflow_runs SET status = 'cancelled', status_reason = ?, \
             finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
        )
        .bind(&reason)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("failed to cancel a workflow run")?;

        // The claim race, solved the way `claim_next_ready` solves it rather
        // than with a second mechanism. A task being claimed as this runs is
        // either still `ready`, in which case this UPDATE wins and the claim's
        // own `WHERE status = 'ready'` then matches nothing and hands back no
        // task, or it is already `in_progress`, in which case it is not in the
        // list below and is left to finish. There is no ordering of the two in
        // which a cancelled task is also handed to a worker.
        let settled = sqlx::query(
            "UPDATE tasks SET status = 'skipped', skip_reason = ?, worker_id = NULL, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE workflow_run_id = ? \
               AND status IN ('backlog', 'ready', 'pending_approval', 'blocked')",
        )
        .bind(&skip_reason)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .context("failed to settle the unstarted tasks of a cancelled run")?
        .rows_affected();

        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE workflow_run_id = ? AND status = 'in_progress'",
        )
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to count the in-flight tasks of a cancelled run")?;

        tx.commit()
            .await
            .context("failed to commit a run cancellation")?;

        Ok(CancelOutcome::Cancelled {
            settled: settled as i64,
            left_running: running,
        })
    }

    /// Remove a run and everything it emitted.
    ///
    /// The endpoint that did not exist, which is why two agents left empty run
    /// rows behind: cleanup had nothing to call.
    ///
    /// Refused while the run is `running` — cancel it first. A delete is not a
    /// stop: it would take the cards out from under a pipeline that is still
    /// being scheduled and leave the scheduler mid-promote on rows that no
    /// longer exist. Refused too while any task is `in_progress`, whatever the
    /// run says, because a live worker writing into a deleted task is the one
    /// outcome with no recovery at all.
    ///
    /// Gates go before tasks, exactly as in the launch rollback: a gate
    /// outliving its task is a stored, repeating, outbound request against a
    /// card that no longer exists.
    pub async fn delete_run(
        &self,
        task_store: &TaskStore,
        run_id: &str,
    ) -> Result<DeleteRunOutcome> {
        let Some(run) = self.get_run(run_id).await? else {
            return Ok(DeleteRunOutcome::NotFound);
        };

        if run.status == RunStatus::Running {
            return Ok(DeleteRunOutcome::Refused {
                reason: "this run is still running — cancel it first, so its unstarted tasks are \
                         settled and anything in flight is allowed to finish"
                    .to_string(),
            });
        }

        let tasks = task_store.list_by_workflow_run(run_id).await?;
        if let Some(task) = tasks
            .iter()
            .find(|task| task.status == TaskStatus::InProgress)
        {
            return Ok(DeleteRunOutcome::Refused {
                reason: format!(
                    "task #{} is still in progress — a worker is writing to it, so the run \
                     cannot be removed until it finishes or is reaped",
                    task.task_number
                ),
            });
        }

        let gates = crate::tasks::GateStore::new(self.pool.clone());
        for task in &tasks {
            gates.delete_for_task(task.task_number).await?;

            // `delete` only touches `tasks`, so the edges and bindings that
            // mention this number would outlive it — present in the table and
            // invisible to every join the scheduler makes, which is the worst
            // of both. The expansion path removes a placeholder's rows for
            // exactly this reason.
            sqlx::query(
                "DELETE FROM task_dependencies \
                 WHERE parent_task_number = ? OR child_task_number = ?",
            )
            .bind(task.task_number)
            .bind(task.task_number)
            .execute(&self.pool)
            .await
            .context("failed to remove a deleted run task's edges")?;

            sqlx::query("DELETE FROM task_input_bindings WHERE child_task_number = ?")
                .bind(task.task_number)
                .execute(&self.pool)
                .await
                .context("failed to remove a deleted run task's input bindings")?;

            task_store.delete(task.task_number).await?;
        }

        sqlx::query("DELETE FROM workflow_runs WHERE id = ?")
            .bind(run_id)
            .execute(&self.pool)
            .await
            .context("failed to delete a workflow run")?;

        Ok(DeleteRunOutcome::Deleted {
            tasks_removed: tasks.len(),
        })
    }
}

/// What a run assessment concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum RunAssessment {
    /// Something can still happen without anyone intervening. Includes waiting
    /// on a gate that can still open, which is the case a stuck detector must
    /// not flag.
    Advancing,
    /// The run has stopped, in the named sense and for the stated reason.
    Settled { status: RunStatus, reason: String },
}

/// A run that just changed status. One per transition, never per tick.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTransition {
    pub run_id: String,
    pub workflow_id: String,
    pub launched_by: String,
    pub status: RunStatus,
    pub reason: String,
}

/// What cancelling did.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    Cancelled {
        /// Unstarted tasks settled as `skipped`.
        settled: i64,
        /// Tasks left in flight to finish or be reaped.
        left_running: i64,
    },
    /// Nothing to cancel. The status says which kind of finished it already is.
    AlreadyFinished {
        status: RunStatus,
    },
    NotFound,
}

/// What deleting did.
#[derive(Debug, Clone, PartialEq)]
pub enum DeleteRunOutcome {
    Deleted {
        tasks_removed: usize,
    },
    /// Refused, with a sentence naming what to do instead.
    Refused {
        reason: String,
    },
    NotFound,
}

/// Whether this task is doing something, or about to be.
///
/// `pending_approval` counts. A run holding a card that asks a person to
/// approve it is *waiting*, not stuck: it has already said what it needs, in
/// the one place a person is looking, and calling that stuck would double-report
/// a healthy pause.
fn in_flight(task: &Task) -> bool {
    matches!(
        task.status,
        TaskStatus::InProgress | TaskStatus::Ready | TaskStatus::PendingApproval
    )
}

/// Why this frontier task cannot move, or `None` if it can.
///
/// Only asked of tasks whose every parent has settled, so "waiting on an
/// upstream task" is never the answer — the graph has already handed this task
/// everything it was owed and the question is whether anything else is in the
/// way.
///
/// Every `None` is a claim that this run will change on its own without anyone
/// touching it, so each one is a chance to park a healthy run by mistake. They
/// are therefore deliberately generous: anything the scheduler might still act
/// on, and anything we could not determine, reads as "can move".
async fn frontier_hold(
    task_store: &TaskStore,
    gates: &crate::tasks::GateStore,
    task: &Task,
) -> Result<Option<String>> {
    // Parked for a person. `dependency` blocks rest in `backlog` rather than
    // here, so this is a sticky kind or a transient one, and neither clears
    // itself — the sweep will not touch it and no upstream event will either.
    if task.status == TaskStatus::Blocked {
        return Ok(Some(format!(
            "is blocked ({}) and only a person can release it: {}",
            task.block_kind
                .map(|kind| kind.to_string())
                .unwrap_or_else(|| "no kind recorded".to_string()),
            task.block_reason.as_deref().unwrap_or("no reason recorded")
        )));
    }

    // The loop boundary owns this card and clears the column when it decides.
    // Its verdict depends on the body, whose own rows are assessed like any
    // other — so a wedged loop is caught at the body task that is actually
    // stuck rather than here, where the reason would be "waiting for a loop".
    if task.awaiting_loop_group.is_some() {
        return Ok(None);
    }

    // A placeholder is shape, not work: expansion replaces it. Parked
    // placeholders are the exception and the reason this check is not just
    // `return Ok(None)` — `expand_fan_outs` skips anything with a `block_kind`,
    // so a placeholder that failed to expand once will never be tried again.
    // That is where a refused fan-out width and a refused run task ceiling both
    // land, and the ceiling that refused is named on the card.
    if task.fan_out_placeholder {
        return match &task.block_reason {
            Some(reason) => Ok(Some(format!("is a fan-out that will not expand: {reason}"))),
            None => Ok(None),
        };
    }

    // Gates, and the distinction the whole detector turns on.
    let blocking = gates.blocking_gates(task.task_number).await?;
    let dead: Vec<&crate::tasks::TaskGate> = blocking
        .iter()
        .filter(|gate| !gate.can_still_open())
        .collect();
    if !dead.is_empty() {
        return Ok(Some(format!(
            "is held by a gate that will never open: {}",
            dead.iter()
                .map(|gate| gate.explain())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    if !blocking.is_empty() {
        // Waiting on the world, which has not answered yet and still can. This
        // is the false positive that would matter most: park a run for having
        // asked CI a question and nobody trusts the status again.
        return Ok(None);
    }

    // Parents done, nothing gating it, and its inputs still will not resolve.
    // The sweep reports this every tick and cannot fix it — a missing pointer
    // is not repaired by an upstream task finishing — so the task will sit in
    // the backlog until a person changes the graph.
    match task_store.resolve_inputs(task.task_number).await {
        Ok(ContractResolution::Unresolved { problems }) => Ok(Some(format!(
            "has every upstream task finished but its inputs still do not resolve: {}",
            problems
                .iter()
                .map(|problem| problem.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        ))),
        // Everything else can still change, or is ours to fix rather than the
        // graph's. `Unreachable` becomes a skip on the next sweep, which is
        // progress; an error here is our failure and must not park a run.
        Ok(_) => Ok(None),
        Err(error) => {
            tracing::warn!(
                %error,
                task_number = task.task_number,
                "failed to check a task's inputs while assessing its run — treating it as able \
                 to advance"
            );
            Ok(None)
        }
    }
}

/// Everything launch worked out about one loop body.
///
/// Computed once, before anything is written, so the emitter never has to
/// re-derive which step is the exit or where iteration 1 reads from — deriving
/// it twice is how the two answers drift apart.
struct LoopPlan {
    group: String,
    /// Every step in the body.
    members: Vec<String>,
    /// The one step with nothing after it inside the body. Its output is what
    /// `loop_until` reads, and its completion is the iteration boundary.
    exit_step_key: String,
    max_iterations: i64,
    until: Value,
    previous_iteration: Vec<crate::tasks::PreviousIterationBinding>,
    /// The single step outside the body that feeds it, when the body reads a
    /// previous iteration. `None` when nothing does, because then there is
    /// nothing for iteration 1 to fall back to and no reason to demand one.
    entry_step_key: Option<String>,
}

/// The loop shape of a whole template.
#[derive(Default)]
struct LoopWiring {
    plans: Vec<LoopPlan>,
    /// Step key -> the loop whose verdict it waits on, and which arm it is on.
    awaiting: HashMap<String, (String, crate::tasks::LoopArm)>,
}

/// Work out every loop body, and refuse the templates that cannot be run.
///
/// Every refusal here names the step to change. The alternative is a loop that
/// launches and then behaves in a way nobody declared — a body with two exits
/// would iterate on whichever step happened to finish last, which is a coin
/// toss dressed up as a pipeline.
fn loop_wiring(
    steps: &[WorkflowStep],
    edges: &[StepEdge],
    bindings: &[StepBinding],
) -> std::result::Result<LoopWiring, LaunchError> {
    let group_of = |key: &str| -> Option<&str> {
        steps
            .iter()
            .find(|step| step.step_key == key)
            .and_then(|step| step.loop_group.as_deref())
            .filter(|group| !group.is_empty())
    };

    // Checked before the bodies, so a previous-iteration binding on a step that
    // is in no loop at all is reported as that rather than going unmentioned
    // because there were no groups to check it against.
    for binding in bindings
        .iter()
        .filter(|binding| binding.source == BindingSource::PreviousIteration)
    {
        if group_of(&binding.step_key).is_none() {
            return Err(LaunchError::PreviousIterationOutsideLoop {
                step_key: binding.step_key.clone(),
                input_key: binding.input_key.clone(),
            });
        }
    }

    let mut groups: Vec<(String, Vec<&WorkflowStep>)> = Vec::new();
    for step in steps {
        let Some(group) = step.loop_group.as_deref().filter(|name| !name.is_empty()) else {
            continue;
        };
        match groups.iter_mut().find(|(name, _)| name == group) {
            Some((_, members)) => members.push(step),
            None => groups.push((group.to_string(), vec![step])),
        }
    }

    let mut wiring = LoopWiring::default();

    for (group, members) in &groups {
        let keys: HashSet<&str> = members.iter().map(|step| step.step_key.as_str()).collect();

        // Two mechanisms that both grow the run graph after launch, on one
        // step. There is no single answer for what an iteration of a body that
        // is also n branches wide contains, so this is refused rather than
        // guessed at.
        for step in members {
            if step.for_each_step_key.is_some() {
                return Err(LaunchError::LoopStepIsAlsoFanOut {
                    step_key: step.step_key.clone(),
                });
            }
        }

        // The exit step is the one nothing inside the body waits on. Ambiguity
        // here is worse than a refusal: with two exits the loop would turn over
        // on whichever finished last.
        let candidates: Vec<String> = members
            .iter()
            .filter(|step| {
                !edges.iter().any(|edge| {
                    edge.parent_step_key == step.step_key
                        && keys.contains(edge.child_step_key.as_str())
                })
            })
            .map(|step| step.step_key.clone())
            .collect();
        if candidates.len() != 1 {
            return Err(LaunchError::LoopBodyNotSingleExit {
                loop_group: group.clone(),
                candidates,
            });
        }
        let exit_step_key = candidates[0].clone();
        let exit = members
            .iter()
            .find(|step| step.step_key == exit_step_key)
            .expect("the exit step came from this body");

        // A setting nothing reads is worse than a missing one: it looks
        // configured.
        for step in members {
            if step.step_key != exit_step_key
                && (step.loop_until.is_some() || step.loop_max_iterations.is_some())
            {
                return Err(LaunchError::LoopSettingOffExitStep {
                    step_key: step.step_key.clone(),
                    loop_group: group.clone(),
                    exit_step_key,
                });
            }
        }

        let Some(until) = exit.loop_until.clone() else {
            return Err(LaunchError::LoopWithoutExitCondition {
                loop_group: group.clone(),
                step_key: exit_step_key,
            });
        };
        if !until.is_object() {
            return Err(LaunchError::LoopExitConditionInvalid {
                loop_group: group.clone(),
                details: "it is not an object".to_string(),
            });
        }
        if until.get("pointer").and_then(Value::as_str).is_none() {
            return Err(LaunchError::LoopExitConditionInvalid {
                loop_group: group.clone(),
                details: "it has no `pointer` saying what to read in the exit step's output"
                    .to_string(),
            });
        }

        let max_iterations = exit
            .loop_max_iterations
            .unwrap_or(crate::tasks::DEFAULT_LOOP_MAX_ITERATIONS);
        if !(1..=crate::tasks::MAX_LOOP_ITERATIONS).contains(&max_iterations) {
            return Err(LaunchError::LoopMaxIterationsOutOfRange {
                loop_group: group.clone(),
                max_iterations,
                ceiling: crate::tasks::MAX_LOOP_ITERATIONS,
            });
        }

        let mut previous_iteration = Vec::new();
        for binding in bindings
            .iter()
            .filter(|binding| binding.source == BindingSource::PreviousIteration)
        {
            if group_of(&binding.step_key) != Some(group.as_str()) {
                continue;
            }
            let target = binding.source_step_key.as_deref().unwrap_or("");
            if !keys.contains(target) {
                return Err(LaunchError::PreviousIterationOutsideBody {
                    step_key: binding.step_key.clone(),
                    input_key: binding.input_key.clone(),
                    source_step_key: target.to_string(),
                });
            }
            previous_iteration.push(crate::tasks::PreviousIterationBinding {
                step_key: binding.step_key.clone(),
                input_key: binding.input_key.clone(),
                source_step_key: target.to_string(),
            });
        }

        // Only demanded when something actually reads a previous iteration. A
        // body that does not is free to be entered from anywhere, or from
        // nowhere at all.
        let entry_step_key = if previous_iteration.is_empty() {
            None
        } else {
            let mut entries: Vec<String> = edges
                .iter()
                .filter(|edge| {
                    keys.contains(edge.child_step_key.as_str())
                        && !keys.contains(edge.parent_step_key.as_str())
                })
                .map(|edge| edge.parent_step_key.clone())
                .collect();
            entries.sort();
            entries.dedup();
            if entries.len() != 1 {
                return Err(LaunchError::LoopEntryAmbiguous {
                    loop_group: group.clone(),
                    entries,
                });
            }
            Some(entries[0].clone())
        };

        wiring.plans.push(LoopPlan {
            group: group.clone(),
            members: members.iter().map(|step| step.step_key.clone()).collect(),
            exit_step_key,
            max_iterations,
            until,
            previous_iteration,
            entry_step_key,
        });
    }

    // Every edge out of a loop's exit is conditional, both arms of it.
    //
    // The `on_exhausted` half is obvious. The *normal* half is the one that
    // gets missed: the body finishes whether the loop converged or gave up, so
    // a step wired downstream the ordinary way would run after three failed
    // attempts exactly as it does after three successful ones — which is the
    // merge the edge kind exists to prevent, arriving by the back door.
    //
    // And an `on_exhausted` edge whose parent is not a loop's exit is a promise
    // the run cannot keep: that step can never run out of attempts.
    for edge in edges {
        let plan = wiring
            .plans
            .iter()
            .find(|plan| plan.exit_step_key == edge.parent_step_key);

        let arm = match (plan, edge.kind) {
            (Some(_), StepEdgeKind::Normal) => crate::tasks::LoopArm::Normal,
            (Some(_), StepEdgeKind::OnExhausted) => crate::tasks::LoopArm::OnExhausted,
            (None, StepEdgeKind::OnExhausted) => {
                return Err(LaunchError::OnExhaustedNotFromLoop {
                    parent: edge.parent_step_key.clone(),
                    child: edge.child_step_key.clone(),
                });
            }
            (None, StepEdgeKind::Normal) => continue,
        };
        let group = plan.expect("an arm implies a plan").group.clone();

        if let Some((first, _)) = wiring
            .awaiting
            .insert(edge.child_step_key.clone(), (group.clone(), arm))
            && first != group
        {
            return Err(LaunchError::StepAwaitsTwoLoops {
                child: edge.child_step_key.clone(),
                first,
                second: group,
            });
        }
    }

    Ok(wiring)
}

/// Kahn's algorithm. Returns the order, or the steps left over in a cycle.
///
/// The order itself is not used for scheduling — the dependency edges do that —
/// but a workflow that cannot be ordered cannot be run, and finding out at
/// launch is far better than finding out never.
fn topologically_ordered(
    steps: &[WorkflowStep],
    edges: &[(String, String)],
) -> std::result::Result<Vec<String>, LaunchError> {
    let mut indegree: HashMap<&str, usize> = steps
        .iter()
        .map(|step| (step.step_key.as_str(), 0usize))
        .collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();

    for (parent, child) in edges {
        children
            .entry(parent.as_str())
            .or_default()
            .push(child.as_str());
        *indegree.entry(child.as_str()).or_insert(0) += 1;
    }

    let mut queue: Vec<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(key, _)| *key)
        .collect();
    queue.sort_unstable();

    let mut ordered = Vec::new();
    while let Some(key) = queue.pop() {
        ordered.push(key.to_string());
        for child in children.get(key).into_iter().flatten() {
            let degree = indegree.entry(child).or_insert(0);
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                queue.push(child);
            }
        }
    }

    if ordered.len() != steps.len() {
        // Whatever never reached indegree zero is in or downstream of a cycle.
        let mut cycle: Vec<String> = indegree
            .iter()
            .filter(|(_, degree)| **degree > 0)
            .map(|(key, _)| (*key).to_string())
            .collect();
        cycle.sort();
        return Err(LaunchError::Cycle { cycle });
    }

    Ok(ordered)
}

/// Does `parent` run before `child`, directly or through other steps?
///
/// Reachability rather than "is there an edge", because a step that waits on
/// its source through an intermediate step waits on it just as surely, and a
/// check that demanded the shortcut edge would refuse templates that are wired
/// correctly. Iterative: the graph is hand-built and a deep chain must not blow
/// the stack.
fn precedes(edges: &[(String, String)], parent: &str, child: &str) -> bool {
    let mut frontier = vec![parent];
    let mut seen: HashSet<&str> = HashSet::from([parent]);

    while let Some(node) = frontier.pop() {
        for (from, to) in edges {
            if from != node {
                continue;
            }
            if to == child {
                return true;
            }
            if seen.insert(to.as_str()) {
                frontier.push(to.as_str());
            }
        }
    }

    false
}

fn validate(schema: &Value, value: &Value) -> Vec<String> {
    match jsonschema::validator_for(schema) {
        Ok(validator) => validator
            .iter_errors(value)
            .map(|error| {
                ContractProblem::SchemaViolation {
                    side: ContractSide::Input,
                    path: error.instance_path().to_string(),
                    message: error.to_string(),
                }
                .to_string()
            })
            .collect(),
        Err(error) => vec![format!("schema is not valid JSON Schema: {error}")],
    }
}

fn read_json(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<Value> {
    row.try_get::<Option<String>, _>(column)
        .ok()
        .flatten()
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn workflow_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Workflow> {
    Ok(Workflow {
        id: row.try_get("id").context("failed to read workflow id")?,
        name: row
            .try_get("name")
            .context("failed to read workflow name")?,
        description: row.try_get("description").ok().flatten(),
        input_schema: read_json(&row, "input_schema"),
        created_at: row
            .try_get("created_at")
            .context("failed to read workflow created_at")?,
        updated_at: row
            .try_get("updated_at")
            .context("failed to read workflow updated_at")?,
    })
}

fn step_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkflowStep> {
    let priority: String = row.try_get("priority").unwrap_or_else(|_| "medium".into());
    Ok(WorkflowStep {
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read step workflow_id")?,
        step_key: row.try_get("step_key").context("failed to read step_key")?,
        title: row.try_get("title").context("failed to read step title")?,
        description: row.try_get("description").ok().flatten(),
        assigned_agent_id: row.try_get("assigned_agent_id").ok().flatten(),
        priority: TaskPriority::parse(&priority).unwrap_or(TaskPriority::Medium),
        input_schema: read_json(&row, "input_schema"),
        output_schema: read_json(&row, "output_schema"),
        system_prompt: row.try_get("system_prompt").ok().flatten(),
        repo_id: row.try_get("repo_id").ok().flatten(),
        position: row.try_get("position").unwrap_or(0),
        for_each_step_key: row.try_get("for_each_step_key").ok().flatten(),
        for_each_pointer: row.try_get("for_each_pointer").ok().flatten(),
        for_each_key: row.try_get("for_each_key").ok().flatten(),
        loop_group: row.try_get("loop_group").ok().flatten(),
        loop_max_iterations: row.try_get("loop_max_iterations").ok().flatten(),
        loop_until: read_json(&row, "loop_until"),
    })
}

fn binding_from_row(row: sqlx::sqlite::SqliteRow) -> Result<StepBinding> {
    let source: String = row
        .try_get("source")
        .context("failed to read binding source")?;
    Ok(StepBinding {
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read binding workflow_id")?,
        step_key: row
            .try_get("step_key")
            .context("failed to read binding step_key")?,
        input_key: row
            .try_get("input_key")
            .context("failed to read binding input_key")?,
        source: BindingSource::parse(&source).unwrap_or(BindingSource::Literal),
        source_step_key: row.try_get("source_step_key").ok().flatten(),
        source_pointer: row.try_get("source_pointer").ok().flatten(),
        literal_value: read_json(&row, "literal_value"),
    })
}

fn gate_from_row(row: sqlx::sqlite::SqliteRow) -> Result<StepGate> {
    let kind: String = row.try_get("kind").context("failed to read gate kind")?;
    Ok(StepGate {
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read gate workflow_id")?,
        step_key: row
            .try_get("step_key")
            .context("failed to read gate step_key")?,
        gate_key: row
            .try_get("gate_key")
            .context("failed to read gate gate_key")?,
        // An unreadable kind falls back to `http`, which cannot silently route
        // anything: `http` never derives `route`, so the worst case is a gate
        // that waits and is visible on the board.
        kind: GateKind::parse(&kind).unwrap_or(GateKind::Http),
        source_step_key: row.try_get("source_step_key").ok().flatten(),
        config: read_json(&row, "config").unwrap_or(Value::Null),
        label: row.try_get("label").ok().flatten(),
        poll_interval_secs: row.try_get("poll_interval_secs").unwrap_or(60),
        disposition: row
            .try_get::<Option<String>, _>("disposition")
            .ok()
            .flatten()
            .as_deref()
            .and_then(GateDisposition::parse),
    })
}

/// The columns every run read selects, in the order [`run_from_row`] expects.
const RUN_SELECT_COLUMNS: &str = "SELECT id, workflow_id, inputs, launched_by, status, \
     finished_at, status_reason, created_at";

fn run_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkflowRun> {
    Ok(WorkflowRun {
        id: row.try_get("id").context("failed to read run id")?,
        workflow_id: row
            .try_get("workflow_id")
            .context("failed to read run workflow_id")?,
        inputs: read_json(&row, "inputs").unwrap_or(Value::Null),
        launched_by: row
            .try_get("launched_by")
            .context("failed to read run launched_by")?,
        // An unreadable status reads as `running`, which is the only safe
        // default: it keeps the run under the supervisor's eye instead of
        // silently retiring it as finished.
        status: row
            .try_get::<String, _>("status")
            .ok()
            .as_deref()
            .and_then(RunStatus::parse)
            .unwrap_or(RunStatus::Running),
        finished_at: row.try_get("finished_at").ok().flatten(),
        status_reason: row.try_get("status_reason").ok().flatten(),
        created_at: row
            .try_get("created_at")
            .context("failed to read run created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::ContractResolution;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn fixture() -> (WorkflowStore, TaskStore, String) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::tasks::store::create_task_schema(&pool).await;
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("seed sequence");
        create_workflow_schema(&pool).await;

        let workflows = WorkflowStore::new(pool.clone());
        let tasks = TaskStore::new(pool);
        let workflow = workflows
            .create_workflow(
                "deploy",
                None,
                Some(&serde_json::json!({
                    "type": "object",
                    "required": ["tag"],
                    "properties": {"tag": {"type": "string"}},
                })),
            )
            .await
            .expect("create workflow");

        (workflows, tasks, workflow.id)
    }

    fn step(workflow_id: &str, key: &str, position: i64) -> WorkflowStep {
        WorkflowStep {
            workflow_id: workflow_id.to_string(),
            step_key: key.to_string(),
            title: format!("step {key}"),
            description: None,
            assigned_agent_id: None,
            priority: TaskPriority::Medium,
            input_schema: None,
            output_schema: None,
            system_prompt: None,
            repo_id: None,
            position,
            for_each_step_key: None,
            for_each_pointer: None,
            for_each_key: None,
            loop_group: None,
            loop_max_iterations: None,
            loop_until: None,
        }
    }

    /// The headline case: one input, a chain of steps, everything wired.
    #[tokio::test]
    async fn a_launch_compiles_a_template_into_a_runnable_graph() {
        let (workflows, tasks, id) = fixture().await;

        for (index, key) in ["build", "test", "deploy"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("put step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");
        workflows
            .link_steps(&id, "test", "deploy")
            .await
            .expect("link");

        // The entry step reads straight from the run's launch payload.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "build".into(),
                input_key: "tag".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind run input");
        // A later step reads an earlier step's output, by name.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "deploy".into(),
                input_key: "artifact".into(),
                source: BindingSource::Step,
                source_step_key: Some("build".into()),
                source_pointer: Some("/artifact".into()),
                literal_value: None,
            })
            .await
            .expect("bind step output");

        let launched = workflows
            .launch(
                &tasks,
                &id,
                &serde_json::json!({"tag": "v2.0.0"}),
                "agent-1",
            )
            .await
            .expect("launch should succeed");

        assert_eq!(launched.task_numbers.len(), 3);

        // Every step lands in backlog — the sweep decides what is eligible,
        // not the instantiator.
        for number in launched.task_numbers.values() {
            let task = tasks
                .get_by_number(*number)
                .await
                .expect("fetch")
                .expect("exists");
            assert_eq!(task.status, TaskStatus::Backlog);
            assert_eq!(
                task.workflow_run_id.as_deref(),
                Some(launched.run.id.as_str())
            );
        }

        // Edges were translated from step keys to task numbers.
        let build = launched.task_numbers["build"];
        let test = launched.task_numbers["test"];
        let deploy = launched.task_numbers["deploy"];
        assert_eq!(
            tasks.list_parents(test).await.expect("parents"),
            vec![build]
        );
        assert_eq!(
            tasks.list_parents(deploy).await.expect("parents"),
            vec![test]
        );

        // The step binding points at the compiled task number, not a name.
        let deploy_bindings = tasks.list_input_bindings(deploy).await.expect("bindings");
        assert_eq!(deploy_bindings.len(), 1);
        assert_eq!(deploy_bindings[0].source_task_number, Some(build));
        assert_eq!(
            deploy_bindings[0].source_pointer.as_deref(),
            Some("/artifact")
        );

        // The run input was frozen into a literal at launch.
        let build_bindings = tasks.list_input_bindings(build).await.expect("bindings");
        assert_eq!(
            build_bindings[0].literal_value,
            Some(serde_json::json!("v2.0.0"))
        );
        assert_eq!(
            build_bindings[0].source_task_number, None,
            "a frozen run input must not depend on anything at run time"
        );
    }

    /// The entry step must be immediately runnable: no parents, and its inputs
    /// already resolve. If this fails the pipeline never starts.
    #[tokio::test]
    async fn the_entry_step_is_promotable_immediately_after_launch() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        workflows
            .put_step(&step(&id, "deploy", 1))
            .await
            .expect("step");
        workflows
            .link_steps(&id, "build", "deploy")
            .await
            .expect("link");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "build".into(),
                input_key: "tag".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![launched.task_numbers["build"]],
            "the entry step should be the only thing the sweep promotes"
        );
        assert!(sweep.stalled.is_empty(), "a fresh launch must not stall");
    }

    #[tokio::test]
    async fn a_launch_input_that_misses_the_schema_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": 42}), "agent-1")
            .await
            .expect_err("a number where a string was declared must be refused");
        assert!(
            matches!(error, LaunchError::InvalidInput { .. }),
            "{error:?}"
        );

        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty(),
            "a refused launch must not leave tasks behind"
        );
    }

    /// A cycle would deadlock silently at run time — every step waiting on a
    /// parent that is itself waiting. Refuse at launch and name the steps.
    #[tokio::test]
    async fn a_cyclic_template_is_refused_before_anything_is_written() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["a", "b", "c"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows.link_steps(&id, "a", "b").await.expect("link");
        workflows.link_steps(&id, "b", "c").await.expect("link");
        workflows.link_steps(&id, "c", "a").await.expect("link");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a cycle must be refused");
        match error {
            LaunchError::Cycle { cycle } => assert_eq!(cycle, vec!["a", "b", "c"]),
            other => panic!("expected Cycle, got {other:?}"),
        }
        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_binding_naming_an_unknown_step_is_refused_with_the_name() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "deploy", 0))
            .await
            .expect("step");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "deploy".into(),
                input_key: "artifact".into(),
                source: BindingSource::Step,
                source_step_key: Some("buidl".into()), // typo on purpose
                source_pointer: Some("/artifact".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("an unknown step reference must be refused");
        match error {
            LaunchError::UnknownStepReference { missing, .. } => assert_eq!(missing, "buidl"),
            other => panic!("expected UnknownStepReference, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_run_input_pointer_that_misses_is_refused_at_launch() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "build".into(),
                input_key: "digest".into(),
                source: BindingSource::RunInput,
                source_step_key: None,
                source_pointer: Some("/image/digest".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a pointer the input does not contain must be refused");
        assert!(
            matches!(error, LaunchError::MissingRunInput { .. }),
            "{error:?}"
        );
    }

    /// Per-step instructions have to reach the task, or the field is decoration.
    #[tokio::test]
    async fn a_step_stamps_its_system_prompt_onto_the_task_it_becomes() {
        let (workflows, tasks, id) = fixture().await;
        let mut build = step(&id, "build", 0);
        build.system_prompt = Some("Answer in British English.".into());
        workflows.put_step(&build).await.expect("step");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");

        let task = tasks
            .get_by_number(launched.task_numbers["build"])
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            task.system_prompt.as_deref(),
            Some("Answer in British English."),
        );
    }

    /// Deleting a step must not leave an edge or binding pointing at it.
    ///
    /// A dangling reference fails *every* future launch, and it is invisible in
    /// a step-oriented editor — the template would be permanently unlaunchable
    /// with nothing on screen to explain why.
    #[tokio::test]
    async fn deleting_a_step_takes_its_edges_and_bindings_with_it() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["build", "test", "deploy"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");
        workflows
            .link_steps(&id, "test", "deploy")
            .await
            .expect("link");
        // deploy reads from test — a reference *into* the step being removed.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "deploy".into(),
                input_key: "report".into(),
                source: BindingSource::Step,
                source_step_key: Some("test".into()),
                source_pointer: Some("/report".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        assert!(workflows.delete_step(&id, "test").await.expect("delete"));

        assert!(
            workflows.list_edges(&id).await.expect("edges").is_empty(),
            "both edges touched the removed step"
        );
        assert!(
            workflows
                .list_bindings(&id)
                .await
                .expect("bindings")
                .is_empty(),
            "the binding read from the removed step"
        );

        // The real assertion: the template still launches.
        workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("a workflow with a step removed must still launch");
    }

    /// A launch that dies part-way through must not leave runnable tasks.
    ///
    /// Validation catches bad templates, but emitting spans two stores and can
    /// fail on its own. Whatever is left behind gets *picked up and run*, which
    /// is worse than the failure itself.
    #[tokio::test]
    async fn a_launch_that_fails_while_emitting_leaves_nothing_runnable() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["build", "test"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");

        // Emit half a graph by hand, then roll it back — the same call the
        // launch path makes when a write fails under it.
        let run_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO workflow_runs (id, workflow_id, inputs, launched_by) VALUES (?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(&id)
        .bind("{}")
        .bind("agent-1")
        .execute(workflows.pool())
        .await
        .expect("insert run");

        let mut emitted = HashMap::new();
        workflows
            .emit_graph(
                &tasks,
                &run_id,
                &workflows.list_steps(&id).await.expect("steps"),
                &workflows.list_edges(&id).await.expect("edges"),
                &[],
                &[],
                &HashMap::new(),
                &LoopWiring::default(),
                "agent-1",
                &mut emitted,
            )
            .await
            .expect("emit");
        assert_eq!(emitted.len(), 2);

        workflows.rollback_run(&tasks, &run_id, &emitted).await;

        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty(),
            "a rolled-back launch must leave no task the sweep could promote"
        );
        assert!(
            workflows.get_run(&run_id).await.expect("run").is_none(),
            "the run row goes too"
        );
    }

    /// A step that says it needs an input, with nothing wired to it, is a
    /// mistake that is fully knowable at launch. Letting it through means
    /// finding out one step into a pipeline someone thought was fine.
    #[tokio::test]
    async fn a_required_input_with_no_binding_is_refused_at_launch() {
        let (workflows, tasks, id) = fixture().await;
        let mut deploy = step(&id, "deploy", 0);
        deploy.input_schema = Some(serde_json::json!({
            "type": "object",
            "required": ["artifact"],
            "properties": {"artifact": {"type": "string"}},
        }));
        workflows.put_step(&deploy).await.expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a declared, unbound input must be refused");
        match error {
            LaunchError::UnboundRequiredInput {
                step_key,
                input_key,
            } => {
                assert_eq!(step_key, "deploy");
                assert_eq!(input_key, "artifact");
            }
            other => panic!("expected UnboundRequiredInput, got {other:?}"),
        }
    }

    /// An optional input with no binding is a default, not an omission.
    #[tokio::test]
    async fn an_optional_input_needs_no_binding() {
        let (workflows, tasks, id) = fixture().await;
        let mut deploy = step(&id, "deploy", 0);
        deploy.input_schema = Some(serde_json::json!({
            "type": "object",
            "properties": {"artifact": {"type": "string"}},
        }));
        workflows.put_step(&deploy).await.expect("step");

        workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("an optional input is not a missing one");
    }

    /// A cycle must be refused when the edge is *saved*, not only at launch.
    /// A template that accepts one and then refuses to run is a trap.
    #[tokio::test]
    async fn an_edge_that_would_close_a_loop_is_detectable_before_it_is_saved() {
        let (workflows, _tasks, id) = fixture().await;
        for (index, key) in ["a", "b", "c"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows.link_steps(&id, "a", "b").await.expect("link");
        workflows.link_steps(&id, "b", "c").await.expect("link");

        let cycle = workflows
            .cycle_from_edge(&id, "c", "a")
            .await
            .expect("check")
            .expect("c -> a closes the ring a -> b -> c");
        assert_eq!(cycle, vec!["a", "b", "c"], "the path should name the loop");

        assert!(
            workflows
                .cycle_from_edge(&id, "a", "c")
                .await
                .expect("check")
                .is_none(),
            "a shortcut along the existing direction is not a cycle"
        );
        assert!(
            workflows
                .cycle_from_edge(&id, "a", "a")
                .await
                .expect("check")
                .is_some(),
            "a step cannot wait for itself"
        );
    }

    /// Steps added without an explicit position must not all land on zero.
    #[tokio::test]
    async fn positions_do_not_collide_when_left_unspecified() {
        let (workflows, _tasks, id) = fixture().await;
        assert_eq!(workflows.next_step_position(&id).await.expect("next"), 0);

        workflows.put_step(&step(&id, "a", 0)).await.expect("step");
        assert_eq!(workflows.next_step_position(&id).await.expect("next"), 1);

        workflows.put_step(&step(&id, "b", 1)).await.expect("step");
        assert_eq!(workflows.next_step_position(&id).await.expect("next"), 2);
    }

    /// Two launches must not share tasks. A run is the unit of "this happened".
    #[tokio::test]
    async fn two_launches_produce_independent_graphs() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");

        let first = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("first launch");
        let second = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v2"}), "agent-1")
            .await
            .expect("second launch");

        assert_ne!(first.run.id, second.run.id);
        assert_ne!(first.task_numbers["build"], second.task_numbers["build"]);
    }

    // -- Fan-out -----------------------------------------------------------

    /// scan -> build (one task per repo scan found) -> report.
    async fn fan_out_template(workflows: &WorkflowStore, id: &str) {
        workflows
            .put_step(&step(id, "scan", 0))
            .await
            .expect("step");
        let mut build = step(id, "build", 1);
        build.for_each_step_key = Some("scan".into());
        build.for_each_pointer = Some("/repos".into());
        build.for_each_key = Some("/name".into());
        workflows.put_step(&build).await.expect("step");
        workflows
            .put_step(&step(id, "report", 2))
            .await
            .expect("step");
        workflows
            .link_steps(id, "scan", "build")
            .await
            .expect("link");
        workflows
            .link_steps(id, "build", "report")
            .await
            .expect("link");
    }

    /// Walk a task through to `done` with the outputs it produced.
    async fn complete(tasks: &TaskStore, task_number: i64, outputs: serde_json::Value) {
        for status in [TaskStatus::Ready, TaskStatus::InProgress] {
            tasks
                .update(
                    task_number,
                    crate::tasks::UpdateTaskInput {
                        status: Some(status),
                        ..Default::default()
                    },
                )
                .await
                .expect("advance")
                .expect("exists");
        }
        tasks
            .submit_outputs(task_number, &outputs)
            .await
            .expect("submit outputs");
        tasks
            .update(
                task_number,
                crate::tasks::UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("finish")
            .expect("exists");
    }

    /// At launch the width is unknown, but the steps downstream still need
    /// something to wait on. Emitted with no parent they would be promoted on
    /// the first sweep and run the report before anything was built.
    #[tokio::test]
    async fn a_fan_out_step_launches_as_one_placeholder_carrying_the_edges_its_branches_inherit() {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        assert_eq!(
            launched.task_numbers.len(),
            3,
            "a fan-out step is one task at launch, however wide it becomes"
        );

        let build = tasks
            .get_by_number(launched.task_numbers["build"])
            .await
            .expect("fetch")
            .expect("exists");
        assert!(build.fan_out_placeholder);

        // Frozen at launch: the template can be edited or deleted while this
        // run is in flight, and the run still has to finish the way it started.
        let spec = crate::tasks::FanOutSpec::from_metadata(&build.metadata)
            .expect("the placeholder carries its own fan-out spec");
        assert_eq!(spec.source_task_number, launched.task_numbers["scan"]);
        assert_eq!(spec.pointer, "/repos");
        assert_eq!(spec.key.as_deref(), Some("/name"));

        assert_eq!(
            tasks
                .list_parents(build.task_number)
                .await
                .expect("parents"),
            vec![launched.task_numbers["scan"]]
        );
        assert_eq!(
            tasks
                .list_children(build.task_number)
                .await
                .expect("children"),
            vec![launched.task_numbers["report"]]
        );

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![launched.task_numbers["scan"]],
            "only the entry step is eligible; the placeholder is not work"
        );
    }

    #[tokio::test]
    async fn a_for_each_naming_an_unknown_step_is_refused_with_the_name() {
        let (workflows, tasks, id) = fixture().await;
        let mut build = step(&id, "build", 0);
        build.for_each_step_key = Some("scn".into()); // typo on purpose
        build.for_each_pointer = Some("/repos".into());
        workflows.put_step(&build).await.expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("iterating a step that does not exist must be refused");
        match error {
            LaunchError::UnknownForEachStep { step_key, missing } => {
                assert_eq!(step_key, "build");
                assert_eq!(missing, "scn");
            }
            other => panic!("expected UnknownForEachStep, got {other:?}"),
        }
    }

    /// A step cannot iterate output it does not wait for: the collection would
    /// be read before it exists. Refused rather than silently wired up, so the
    /// graph that runs is the graph the canvas draws.
    #[tokio::test]
    async fn a_fan_out_that_does_not_wait_for_the_step_it_iterates_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;
        assert!(
            workflows
                .unlink_steps(&id, "scan", "build")
                .await
                .expect("unlink")
        );

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a fan-out with no edge from its source must be refused");
        match error {
            LaunchError::ForEachNotWaiting {
                step_key,
                source_step_key,
            } => {
                assert_eq!(step_key, "build");
                assert_eq!(source_step_key, "scan");
            }
            other => panic!("expected ForEachNotWaiting, got {other:?}"),
        }

        // An indirect wait is still a wait — demanding the shortcut edge would
        // refuse templates that are wired correctly.
        workflows
            .put_step(&step(&id, "prepare", 3))
            .await
            .expect("step");
        workflows
            .link_steps(&id, "scan", "prepare")
            .await
            .expect("link");
        workflows
            .link_steps(&id, "prepare", "build")
            .await
            .expect("link");
        workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("waiting through an intermediate step is waiting");
    }

    /// The whole pipeline, and the distinction the fan-in turns on: a branch
    /// that has not finished is a *wait*, and reporting it as an unresolvable
    /// contract parks the report on the first sweep of a healthy run.
    #[tokio::test]
    async fn a_fan_in_waits_for_its_branches_and_then_resolves_keyed_by_branch_key() {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "report".into(),
                input_key: "results".into(),
                source: BindingSource::FanIn,
                source_step_key: Some("build".into()),
                source_pointer: None,
                literal_value: None,
            })
            .await
            .expect("bind fan-in");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let report = launched.task_numbers["report"];

        // Before anything has run at all. This is the case that must not be an
        // unresolvable contract — nothing is wrong, the run just started.
        match tasks.resolve_inputs(report).await.expect("resolve") {
            ContractResolution::Pending { waiting_on } => assert!(
                waiting_on.iter().any(|reason| reason.contains("build")),
                "the wait should name the step it is waiting on: {waiting_on:?}"
            ),
            other => panic!("a fan-in before its fan-out expanded must be pending, got {other:?}"),
        }

        complete(
            &tasks,
            launched.task_numbers["scan"],
            serde_json::json!({"repos": [{"name": "api"}, {"name": "web"}]}),
        )
        .await;

        // The sweep expands before it promotes, so the branches exist and are
        // eligible in the same pass that the fan-out widened.
        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(sweep.promoted.len(), 2, "both branches: {sweep:?}");
        // Held by the edges the branches inherited, and not reported as a
        // problem: nothing about a report waiting on running branches is wrong.
        assert!(!sweep.promoted.contains(&report), "{sweep:?}");
        assert!(sweep.stalled.is_empty(), "{sweep:?}");

        // One branch done is still not enough.
        complete(&tasks, sweep.promoted[0], serde_json::json!({"ok": true})).await;
        assert!(
            matches!(
                tasks.resolve_inputs(report).await.expect("resolve"),
                ContractResolution::Pending { .. }
            ),
            "one branch left unfinished is still a wait"
        );

        complete(&tasks, sweep.promoted[1], serde_json::json!({"ok": false})).await;

        match tasks.resolve_inputs(report).await.expect("resolve") {
            ContractResolution::Resolved { inputs } => assert_eq!(
                inputs,
                serde_json::json!({"results": {"api": {"ok": true}, "web": {"ok": false}}}),
                "keyed by branch, not positional"
            ),
            other => panic!("expected the fan-in to resolve, got {other:?}"),
        }

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(sweep.promoted, vec![report]);
    }

    /// A pipeline that scanned and found nothing has succeeded. The report
    /// still runs, and the fan-in it reads is an empty object rather than an
    /// error — "it did nothing" and "it iterated an empty list" must not look
    /// alike from the outside.
    #[tokio::test]
    async fn an_empty_fan_out_lets_the_report_run_with_an_empty_collection() {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "report".into(),
                input_key: "results".into(),
                source: BindingSource::FanIn,
                source_step_key: Some("build".into()),
                source_pointer: None,
                literal_value: None,
            })
            .await
            .expect("bind fan-in");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        complete(
            &tasks,
            launched.task_numbers["scan"],
            serde_json::json!({"repos": []}),
        )
        .await;

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![launched.task_numbers["report"]],
            "the report runs: {sweep:?}"
        );

        match tasks
            .resolve_inputs(launched.task_numbers["report"])
            .await
            .expect("resolve")
        {
            ContractResolution::Resolved { inputs } => {
                assert_eq!(inputs, serde_json::json!({"results": {}}));
            }
            other => panic!("an empty fan-in resolves to nothing, not to a problem: {other:?}"),
        }
    }

    /// A fan-in over an ordinary step would resolve to a single entry keyed by
    /// a task number — plausible-looking nonsense rather than an error.
    #[tokio::test]
    async fn a_fan_in_over_a_step_that_is_not_a_fan_out_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        workflows
            .put_step(&step(&id, "report", 1))
            .await
            .expect("step");
        workflows
            .link_steps(&id, "build", "report")
            .await
            .expect("link");
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "report".into(),
                input_key: "results".into(),
                source: BindingSource::FanIn,
                source_step_key: Some("build".into()),
                source_pointer: None,
                literal_value: None,
            })
            .await
            .expect("bind");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("collecting branches from a step that has none must be refused");
        assert!(
            matches!(error, LaunchError::FanInNotFanOut { .. }),
            "{error:?}"
        );
    }

    /// The mirror image: a plain step binding onto a fan-out points at the
    /// placeholder, which expansion deletes. It would resolve at launch and
    /// dangle the moment the fan-out widened.
    #[tokio::test]
    async fn a_plain_step_binding_onto_a_fan_out_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.clone(),
                step_key: "report".into(),
                input_key: "artifact".into(),
                source: BindingSource::Step,
                source_step_key: Some("build".into()),
                source_pointer: Some("/artifact".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("one output cannot be selected from a step that has many");
        assert!(
            matches!(error, LaunchError::StepBindingOnFanOut { .. }),
            "{error:?}"
        );
    }

    // -- Bounded loops ------------------------------------------------------

    /// analyze -> [ patch -> test ] -> ship, with `test` the loop's exit step.
    ///
    /// The shape every assertion below needs: a body of two steps so the
    /// degenerate one-step case cannot hide a bug, an entry step outside it, a
    /// step after it, and a binding of each kind that iteration has to rewire.
    async fn loop_template(workflows: &WorkflowStore, id: &str, max_iterations: Option<i64>) {
        workflows
            .put_step(&step(id, "analyze", 0))
            .await
            .expect("step");

        let mut patch = step(id, "patch", 1);
        patch.loop_group = Some("fix".into());
        workflows.put_step(&patch).await.expect("step");

        let mut test = step(id, "test", 2);
        test.loop_group = Some("fix".into());
        test.loop_max_iterations = max_iterations;
        test.loop_until = Some(serde_json::json!({"pointer": "/green", "equals": true}));
        workflows.put_step(&test).await.expect("step");

        workflows
            .put_step(&step(id, "ship", 3))
            .await
            .expect("step");

        for (parent, child) in [("analyze", "patch"), ("patch", "test"), ("test", "ship")] {
            workflows.link_steps(id, parent, child).await.expect("link");
        }

        // Reaches back one pass; on iteration 1 there is none, so it reads the
        // loop's entry step instead.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.to_string(),
                step_key: "patch".into(),
                input_key: "failures".into(),
                source: BindingSource::PreviousIteration,
                source_step_key: Some("test".into()),
                source_pointer: Some("/failures".into()),
                literal_value: None,
            })
            .await
            .expect("bind");
        // Inside the body, an ordinary step binding is the *current* pass.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.to_string(),
                step_key: "test".into(),
                input_key: "patched".into(),
                source: BindingSource::Step,
                source_step_key: Some("patch".into()),
                source_pointer: Some("/patched".into()),
                literal_value: None,
            })
            .await
            .expect("bind");
        // Downstream of the loop: must end up reading whichever pass finished.
        workflows
            .put_binding(&StepBinding {
                workflow_id: id.to_string(),
                step_key: "ship".into(),
                input_key: "report".into(),
                source: BindingSource::Step,
                source_step_key: Some("test".into()),
                source_pointer: Some("/report".into()),
                literal_value: None,
            })
            .await
            .expect("bind");
    }

    /// The task a step compiled into on a given pass.
    async fn pass(
        tasks: &TaskStore,
        run_id: &str,
        step_key: &str,
        iteration: i64,
    ) -> crate::tasks::Task {
        tasks
            .list_by_workflow_run(run_id)
            .await
            .expect("run tasks")
            .into_iter()
            .find(|task| {
                task.workflow_step_key.as_deref() == Some(step_key)
                    && task.loop_iteration == Some(iteration)
            })
            .unwrap_or_else(|| panic!("no `{step_key}` at iteration {iteration}"))
    }

    /// Sweep, then run one whole pass of the body, ending with the verdict the
    /// exit step reports.
    async fn run_pass(tasks: &TaskStore, run_id: &str, iteration: i64, green: bool) {
        tasks.recompute_ready("agent-1").await.expect("sweep");
        let patch = pass(tasks, run_id, "patch", iteration).await;
        complete(
            tasks,
            patch.task_number,
            serde_json::json!({"patched": format!("pass {iteration}")}),
        )
        .await;

        tasks.recompute_ready("agent-1").await.expect("sweep");
        let test = pass(tasks, run_id, "test", iteration).await;
        complete(
            tasks,
            test.task_number,
            serde_json::json!({
                "green": green,
                "failures": [format!("failure from pass {iteration}")],
                "report": format!("report from pass {iteration}"),
            }),
        )
        .await;
    }

    /// A loop that converges on the first pass is a loop that ran once. Emitting
    /// a second body anyway is the difference between "iterate until it works"
    /// and "always do it three times".
    #[tokio::test]
    async fn a_body_that_satisfies_loop_until_on_the_first_pass_never_runs_again() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();

        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;
        run_pass(&tasks, &run_id, 1, true).await;

        let outcomes = tasks.advance_loops("agent-1").await.expect("advance");
        assert!(
            matches!(
                outcomes.as_slice(),
                [crate::tasks::LoopOutcome::Converged { iteration: 1, .. }]
            ),
            "{outcomes:?}"
        );

        let body: Vec<_> = tasks
            .list_by_workflow_run(&run_id)
            .await
            .expect("tasks")
            .into_iter()
            .filter(|task| task.loop_group.is_some())
            .collect();
        assert_eq!(body.len(), 2, "the body ran once: {body:?}");

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![launched.task_numbers["ship"]],
            "the step after a converged loop runs: {sweep:?}"
        );
    }

    /// The point of looping rather than retrying: pass two reads what pass one
    /// produced. If this regresses the body re-runs on stale inputs and the loop
    /// is an expensive way to do the same thing three times.
    #[tokio::test]
    async fn a_body_that_fails_loop_until_emits_a_second_pass_reading_the_first() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();

        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["from analyze"]}),
        )
        .await;

        // Iteration 1 falls back to the loop's entry step, so the body needs no
        // special first-pass wiring.
        tasks.recompute_ready("agent-1").await.expect("sweep");
        match tasks
            .resolve_inputs(launched.task_numbers["patch"])
            .await
            .expect("resolve")
        {
            ContractResolution::Resolved { inputs } => assert_eq!(
                inputs,
                serde_json::json!({"failures": ["from analyze"]}),
                "iteration 1 reads the loop's entry"
            ),
            other => panic!("expected the first pass to resolve, got {other:?}"),
        }

        run_pass(&tasks, &run_id, 1, false).await;
        let outcomes = tasks.advance_loops("agent-1").await.expect("advance");
        assert!(
            matches!(
                outcomes.as_slice(),
                [crate::tasks::LoopOutcome::Iterated { iteration: 2, .. }]
            ),
            "{outcomes:?}"
        );

        let first_test = pass(&tasks, &run_id, "test", 1).await;
        assert_eq!(
            first_test.loop_resolution,
            Some(crate::tasks::LoopResolution::Iterated)
        );

        let second_patch = pass(&tasks, &run_id, "patch", 2).await;
        assert_eq!(second_patch.status, TaskStatus::Backlog);
        assert_eq!(second_patch.loop_group.as_deref(), Some("fix"));
        assert!(
            second_patch.title.contains("(iteration 2)"),
            "a second pass must be tellable apart on a board: {}",
            second_patch.title
        );
        assert_eq!(
            tasks
                .list_parents(second_patch.task_number)
                .await
                .expect("parents"),
            vec![launched.task_numbers["analyze"]],
            "the new pass still hangs off the loop's entry"
        );

        // The rewiring that matters: reading pass one, not itself.
        match tasks
            .resolve_inputs(second_patch.task_number)
            .await
            .expect("resolve")
        {
            ContractResolution::Resolved { inputs } => assert_eq!(
                inputs,
                serde_json::json!({"failures": ["failure from pass 1"]}),
                "the second pass reads what the first produced"
            ),
            other => panic!("expected the second pass to resolve, got {other:?}"),
        }

        // And inside the body, a plain step binding is the *current* pass.
        let second_test = pass(&tasks, &run_id, "test", 2).await;
        let bindings = tasks
            .list_input_bindings(second_test.task_number)
            .await
            .expect("bindings");
        assert_eq!(
            bindings[0].source_task_number,
            Some(second_patch.task_number),
            "`patched` must read pass two's patch, not pass one's"
        );
    }

    /// Three passes and no more, from an unset `loop_max_iterations`. The
    /// default is the whole safety story for a template that never converges.
    #[tokio::test]
    async fn an_unset_max_iterations_stops_the_body_after_three_passes() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();
        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;

        for iteration in 1..=3 {
            run_pass(&tasks, &run_id, iteration, false).await;
            tasks.advance_loops("agent-1").await.expect("advance");
        }

        let passes = tasks
            .list_by_workflow_run(&run_id)
            .await
            .expect("tasks")
            .into_iter()
            .filter(|task| task.workflow_step_key.as_deref() == Some("test"))
            .count();
        assert_eq!(passes, 3, "three passes, and the fourth is never emitted");

        // Nothing is left that a further sweep could turn over.
        let outcomes = tasks.advance_loops("agent-1").await.expect("advance");
        assert!(outcomes.is_empty(), "{outcomes:?}");
    }

    /// Converging and giving up are opposite results. A pipeline that merges
    /// after three successful attempts must not also merge after three failed
    /// ones — which is the same bug this codebase has now shipped three times.
    #[tokio::test]
    async fn a_loop_that_runs_out_of_attempts_takes_its_on_exhausted_edge_and_not_the_normal_one() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, Some(2)).await;
        workflows
            .put_step(&step(&id, "escalate", 4))
            .await
            .expect("step");
        workflows
            .link_steps_with_kind(&id, "test", "escalate", StepEdgeKind::OnExhausted)
            .await
            .expect("link");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();
        let escalate = launched.task_numbers["escalate"];
        let ship = launched.task_numbers["ship"];

        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;

        // Held from the first sweep. The body finishes either way, so mere
        // completion cannot be what releases the give-up path.
        let held = tasks
            .get_by_number(escalate)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(held.awaiting_loop_group.as_deref(), Some("fix"));

        run_pass(&tasks, &run_id, 1, false).await;
        tasks.advance_loops("agent-1").await.expect("advance");
        assert!(
            !tasks
                .recompute_ready("agent-1")
                .await
                .expect("sweep")
                .promoted
                .contains(&escalate),
            "the give-up path must not run while the loop still has attempts left"
        );

        run_pass(&tasks, &run_id, 2, false).await;
        let outcomes = tasks.advance_loops("agent-1").await.expect("advance");
        assert!(
            matches!(
                outcomes.as_slice(),
                [crate::tasks::LoopOutcome::ExhaustedRouted { iteration: 2, .. }]
            ),
            "{outcomes:?}"
        );

        let released = tasks
            .get_by_number(escalate)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(released.awaiting_loop_group, None);
        assert_eq!(
            tasks.list_parents(escalate).await.expect("parents"),
            vec![pass(&tasks, &run_id, "test", 2).await.task_number],
            "released off the pass that actually gave up"
        );

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert!(sweep.promoted.contains(&escalate), "{sweep:?}");
        assert!(
            !sweep.promoted.contains(&ship),
            "the whole point: a pipeline that merges after three successful attempts must not \
             also merge after three failed ones — {sweep:?}"
        );

        let held = tasks
            .get_by_number(ship)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(held.awaiting_loop_arm, Some(crate::tasks::LoopArm::Normal));
        assert!(
            held.block_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("ran out of attempts")),
            "the branch that was not taken says which way the loop went: {held:?}"
        );
    }

    /// The mirror image, and the half that is easy to forget: a loop that
    /// converged must not also run the step written for it giving up. Held
    /// rather than deleted, and told why — a card held with no explanation is
    /// indistinguishable from a deadlock.
    #[tokio::test]
    async fn a_converged_loop_leaves_its_give_up_path_held_and_says_which_way_it_went() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;
        workflows
            .put_step(&step(&id, "escalate", 4))
            .await
            .expect("step");
        workflows
            .link_steps_with_kind(&id, "test", "escalate", StepEdgeKind::OnExhausted)
            .await
            .expect("link");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();
        let escalate = launched.task_numbers["escalate"];

        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;
        run_pass(&tasks, &run_id, 1, true).await;
        tasks.advance_loops("agent-1").await.expect("advance");

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![launched.task_numbers["ship"]],
            "the success path runs and nothing else does: {sweep:?}"
        );

        let held = tasks
            .get_by_number(escalate)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            held.awaiting_loop_arm,
            Some(crate::tasks::LoopArm::OnExhausted)
        );
        assert!(
            held.block_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("converged")),
            "{held:?}"
        );
    }

    /// No `on_exhausted` edge is a real state, not an error: the loop has
    /// nowhere to go and a person has to decide. Sticky, so no sweep resurrects
    /// it, and off `done`, so the steps after the loop do not proceed as though
    /// it had succeeded.
    #[tokio::test]
    async fn a_loop_with_nowhere_to_go_parks_for_a_person_and_the_sweep_leaves_it_alone() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, Some(1)).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();
        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;
        run_pass(&tasks, &run_id, 1, false).await;

        let outcomes = tasks.advance_loops("agent-1").await.expect("advance");
        let [crate::tasks::LoopOutcome::ExhaustedBlocked { reason, .. }] = outcomes.as_slice()
        else {
            panic!("expected the loop to park, got {outcomes:?}");
        };
        assert!(
            reason.contains("ran out of attempts"),
            "the card has to say why: {reason}"
        );

        let exit = pass(&tasks, &run_id, "test", 1).await;
        assert_eq!(exit.status, TaskStatus::Blocked);
        assert_eq!(exit.block_kind, Some(crate::tasks::BlockKind::NeedsInput));

        for _ in 0..3 {
            let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
            assert!(
                !sweep.promoted.contains(&launched.task_numbers["ship"]),
                "a loop that gave up must not release the step after it: {sweep:?}"
            );
        }
        let exit = pass(&tasks, &run_id, "test", 1).await;
        assert_eq!(
            exit.status,
            TaskStatus::Blocked,
            "a sticky block is not for the sweep to undo"
        );
    }

    /// The edge and the binding have to move together. Moved apart, the
    /// pipeline waits on the newest pass and reads the oldest — a graph that is
    /// correct and an answer that is stale, which no test of the edges alone
    /// would catch.
    #[tokio::test]
    async fn the_step_after_a_loop_reads_the_pass_that_finished_it_not_a_superseded_one() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();
        let ship = launched.task_numbers["ship"];
        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;

        run_pass(&tasks, &run_id, 1, false).await;
        tasks.advance_loops("agent-1").await.expect("advance");
        run_pass(&tasks, &run_id, 2, true).await;
        tasks.advance_loops("agent-1").await.expect("advance");

        let second = pass(&tasks, &run_id, "test", 2).await;
        assert_eq!(
            tasks.list_parents(ship).await.expect("parents"),
            vec![second.task_number],
            "downstream waits on the newest pass only"
        );

        match tasks.resolve_inputs(ship).await.expect("resolve") {
            ContractResolution::Resolved { inputs } => assert_eq!(
                inputs,
                serde_json::json!({"report": "report from pass 2"}),
                "and reads it too"
            ),
            other => panic!("expected ship to resolve, got {other:?}"),
        }

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert_eq!(sweep.promoted, vec![ship], "{sweep:?}");
    }

    /// Ambiguity here is worse than a refusal: with two exits the loop would
    /// turn over on whichever step happened to finish last, which is a coin toss
    /// dressed up as a pipeline.
    #[tokio::test]
    async fn a_body_without_a_single_exit_step_is_refused_and_names_the_candidates() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        // A second step with nothing after it inside the body.
        let mut lint = step(&id, "lint", 5);
        lint.loop_group = Some("fix".into());
        workflows.put_step(&lint).await.expect("step");
        workflows
            .link_steps(&id, "patch", "lint")
            .await
            .expect("link");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a body with two exits must be refused");
        match error {
            LaunchError::LoopBodyNotSingleExit {
                loop_group,
                candidates,
            } => {
                assert_eq!(loop_group, "fix");
                assert_eq!(candidates, vec!["test".to_string(), "lint".to_string()]);
            }
            other => panic!("expected LoopBodyNotSingleExit, got {other:?}"),
        }
        assert!(
            tasks
                .list(crate::tasks::TaskListFilter::default())
                .await
                .expect("list")
                .is_empty(),
            "a refused launch must not leave tasks behind"
        );
    }

    /// The most expensive mistake available here. The sweep runs every tick and
    /// completion fires independently, so an emit path that can fire twice for
    /// one pass creates bodies without bound against a live model.
    #[tokio::test]
    async fn the_emit_path_cannot_fire_twice_for_one_iteration() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let run_id = launched.run.id.clone();
        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;
        run_pass(&tasks, &run_id, 1, false).await;

        let exit = pass(&tasks, &run_id, "test", 1).await;
        let first = tasks.advance_loops("agent-1").await.expect("advance");
        assert_eq!(first.len(), 1, "{first:?}");

        // Both entry points, twice each. Every one of them re-reads a boundary
        // that has already been decided.
        for _ in 0..2 {
            assert!(
                tasks
                    .advance_loops("agent-1")
                    .await
                    .expect("advance")
                    .is_empty()
            );
            assert!(
                tasks
                    .advance_loops_for(exit.task_number)
                    .await
                    .expect("advance")
                    .is_empty()
            );
        }

        let second_pass = tasks
            .list_by_workflow_run(&run_id)
            .await
            .expect("tasks")
            .into_iter()
            .filter(|task| task.loop_iteration == Some(2))
            .count();
        assert_eq!(second_pass, 2, "one body, not two");

        // And the guard underneath the guard: the database itself refuses a
        // second task for the same step of the same pass.
        let duplicate = sqlx::query(
            "INSERT INTO tasks (id, task_number, title, status, priority, owner_agent_id, \
             assigned_agent_id, created_by, workflow_run_id, workflow_step_key, loop_iteration) \
             VALUES ('dupe', 9999, 't', 'backlog', 'medium', 'a', 'a', 'a', ?, 'test', 2)",
        )
        .bind(&run_id)
        .execute(tasks.pool())
        .await;
        assert!(
            duplicate.is_err(),
            "a unique index, not a check-then-act, is what makes double emission impossible"
        );
    }

    /// A setting nothing reads is worse than a missing one: it looks configured.
    #[tokio::test]
    async fn a_loop_setting_on_a_step_that_is_not_the_exit_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;

        let mut patch = step(&id, "patch", 1);
        patch.loop_group = Some("fix".into());
        patch.loop_max_iterations = Some(9);
        workflows.put_step(&patch).await.expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a number nothing reads must be refused");
        match error {
            LaunchError::LoopSettingOffExitStep {
                step_key,
                exit_step_key,
                ..
            } => {
                assert_eq!(step_key, "patch");
                assert_eq!(exit_step_key, "test");
            }
            other => panic!("expected LoopSettingOffExitStep, got {other:?}"),
        }
    }

    /// Every pass is a live model call, so the ceiling is enforced where a
    /// person is still watching rather than trusted to a number in a row.
    #[tokio::test]
    async fn a_loop_asking_for_more_iterations_than_the_ceiling_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, Some(crate::tasks::MAX_LOOP_ITERATIONS + 1)).await;

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("an unbounded loop must be refused");
        assert!(
            matches!(error, LaunchError::LoopMaxIterationsOutOfRange { .. }),
            "{error:?}"
        );
    }

    /// A loop with no exit condition always runs its full budget, which is a
    /// retry with extra steps rather than a loop.
    #[tokio::test]
    async fn a_body_with_no_exit_condition_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;
        let mut test = step(&id, "test", 2);
        test.loop_group = Some("fix".into());
        test.loop_until = None;
        workflows.put_step(&test).await.expect("step");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a loop with no exit condition must be refused");
        assert!(
            matches!(error, LaunchError::LoopWithoutExitCondition { .. }),
            "{error:?}"
        );
    }

    /// An `on_exhausted` edge from a step that cannot run out of attempts is a
    /// promise the run has no way to keep.
    #[tokio::test]
    async fn an_on_exhausted_edge_from_a_step_that_is_not_a_loop_exit_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        workflows
            .put_step(&step(&id, "rollback", 1))
            .await
            .expect("step");
        workflows
            .link_steps_with_kind(&id, "build", "rollback", StepEdgeKind::OnExhausted)
            .await
            .expect("link");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("only a loop can give up");
        assert!(
            matches!(error, LaunchError::OnExhaustedNotFromLoop { .. }),
            "{error:?}"
        );
    }

    /// Iteration 1 has nothing to reach back to, so it reads the loop's entry.
    /// A body entered from two places has no single first value, and guessing
    /// one would make the first pass silently different from the rest.
    #[tokio::test]
    async fn a_previous_iteration_binding_with_no_single_loop_entry_is_refused() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, None).await;
        workflows
            .put_step(&step(&id, "survey", 5))
            .await
            .expect("step");
        workflows
            .link_steps(&id, "survey", "patch")
            .await
            .expect("link");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("two entries means no single first value");
        match error {
            LaunchError::LoopEntryAmbiguous { entries, .. } => {
                assert_eq!(entries, vec!["analyze".to_string(), "survey".to_string()]);
            }
            other => panic!("expected LoopEntryAmbiguous, got {other:?}"),
        }
    }

    #[cfg(test)]
    async fn create_workflow_schema(pool: &SqlitePool) {
        for statement in [
            "CREATE TABLE workflows (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL UNIQUE, \
             description TEXT, input_schema TEXT, \
             created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')), \
             updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')))",
            "CREATE TABLE workflow_steps (workflow_id TEXT NOT NULL, step_key TEXT NOT NULL, \
             title TEXT NOT NULL, description TEXT, assigned_agent_id TEXT, \
             priority TEXT NOT NULL DEFAULT 'medium', input_schema TEXT, output_schema TEXT, \
             system_prompt TEXT, repo_id TEXT, position INTEGER NOT NULL DEFAULT 0, \
             for_each_step_key TEXT, for_each_pointer TEXT, for_each_key TEXT, \
             loop_group TEXT, loop_max_iterations INTEGER, loop_until TEXT, \
             PRIMARY KEY (workflow_id, step_key))",
            "CREATE TABLE workflow_step_edges (workflow_id TEXT NOT NULL, \
             parent_step_key TEXT NOT NULL, child_step_key TEXT NOT NULL, \
             kind TEXT NOT NULL DEFAULT 'normal', \
             PRIMARY KEY (workflow_id, parent_step_key, child_step_key))",
            "CREATE TABLE workflow_step_bindings (workflow_id TEXT NOT NULL, \
             step_key TEXT NOT NULL, input_key TEXT NOT NULL, source TEXT NOT NULL, \
             source_step_key TEXT, source_pointer TEXT, literal_value TEXT, \
             PRIMARY KEY (workflow_id, step_key, input_key))",
            "CREATE TABLE workflow_step_gates (workflow_id TEXT NOT NULL, \
             step_key TEXT NOT NULL, gate_key TEXT NOT NULL, kind TEXT NOT NULL, \
             source_step_key TEXT, config TEXT NOT NULL, label TEXT, \
             poll_interval_secs INTEGER NOT NULL DEFAULT 60, disposition TEXT, \
             PRIMARY KEY (workflow_id, step_key, gate_key))",
            // `status`, `finished_at` and `status_reason` are as much a part of
            // this table as the columns it was created with — a pool without
            // them passes tests the real database would refuse.
            "CREATE TABLE workflow_runs (id TEXT PRIMARY KEY NOT NULL, workflow_id TEXT NOT NULL, \
             inputs TEXT NOT NULL, launched_by TEXT NOT NULL, \
             status TEXT NOT NULL DEFAULT 'running', finished_at TEXT, status_reason TEXT, \
             created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')))",
        ] {
            sqlx::query(statement)
                .execute(pool)
                .await
                .expect("workflow schema should be created");
        }
    }

    // -- Conditions ---------------------------------------------------------

    fn condition(id: &str, step_key: &str, source: &str, equals: &str) -> StepGate {
        StepGate {
            workflow_id: id.to_string(),
            step_key: step_key.to_string(),
            gate_key: "runs-when".to_string(),
            kind: GateKind::TaskOutput,
            source_step_key: Some(source.to_string()),
            config: serde_json::json!({"pointer": "/state", "equals": equals}),
            label: Some(format!("deploy went {equals}")),
            poll_interval_secs: 60,
            disposition: None,
        }
    }

    /// The translation, and the only genuinely new mechanism in template-level
    /// gates: a step key becomes the task number that step compiled into.
    ///
    /// Without it a template cannot declare a condition at all — `task_gates`
    /// is keyed by task number, and a template has only names — so branching
    /// would remain a property of one run assembled by a script after launch,
    /// which is how it had to be done before this existed.
    #[tokio::test]
    async fn a_condition_on_a_template_compiles_to_the_task_number_its_source_step_became() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["check", "rollback"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("put step");
        }
        workflows
            .link_steps(&id, "check", "rollback")
            .await
            .expect("link");
        workflows
            .put_gate(&condition(&id, "rollback", "check", "red"))
            .await
            .expect("put gate");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");

        let gates = crate::tasks::GateStore::new(tasks.pool().clone());
        let compiled = gates
            .list_for_task(launched.task_numbers["rollback"])
            .await
            .expect("gates");
        assert_eq!(compiled.len(), 1, "one declared condition, one real gate");

        let gate = &compiled[0];
        assert_eq!(gate.kind, crate::tasks::GateKind::TaskOutput);
        assert_eq!(
            gate.config.get("task_number").and_then(|v| v.as_i64()),
            Some(launched.task_numbers["check"]),
            "the source step key has to resolve to the task that step became"
        );
        assert_eq!(
            gate.config.get("pointer").and_then(|v| v.as_str()),
            Some("/state"),
            "the predicate is carried through untouched"
        );
        assert_eq!(
            gate.disposition, None,
            "`None` has to survive the compile: deriving it now would freeze \
             every condition as `wait`, because at launch no source has run"
        );

        // Launching again produces a second, independent set — the condition
        // belongs to the template, not to one run.
        let again = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v2"}), "agent-1")
            .await
            .expect("relaunch");
        let second = gates
            .list_for_task(again.task_numbers["rollback"])
            .await
            .expect("gates");
        assert_eq!(
            second[0].config.get("task_number").and_then(|v| v.as_i64()),
            Some(again.task_numbers["check"]),
            "the second run's condition points at the second run's task"
        );
    }

    /// The shape this whole feature exists to make possible, end to end.
    ///
    /// Two mutually exclusive conditions off one decision step: one branch
    /// runs, the other is settled as skipped, and the step below *both* still
    /// merges. Before this, the branch that was not taken sat in the backlog
    /// forever and the merge waited on it — the pipeline deadlocked outright
    /// while looking like it was still working.
    #[tokio::test]
    async fn two_exclusive_conditions_run_one_branch_settle_the_other_and_still_merge_below() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["check", "rollback", "announce", "notify"]
            .iter()
            .enumerate()
        {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("put step");
        }
        for (parent, child) in [
            ("check", "rollback"),
            ("check", "announce"),
            ("rollback", "notify"),
            ("announce", "notify"),
        ] {
            workflows
                .link_steps(&id, parent, child)
                .await
                .expect("link");
        }
        workflows
            .put_gate(&condition(&id, "rollback", "check", "red"))
            .await
            .expect("put gate");
        workflows
            .put_gate(&condition(&id, "announce", "check", "green"))
            .await
            .expect("put gate");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        let number = |key: &str| launched.task_numbers[key];

        // The decision is a task, and a model answered it. The routing
        // predicate only ever reads a value something smarter computed.
        complete(
            &tasks,
            number("check"),
            serde_json::json!({"state": "green"}),
        )
        .await;

        let gates = crate::tasks::GateStore::new(tasks.pool().clone());
        let now = chrono::Utc::now().timestamp();
        let polled = crate::tasks::poll_gates_once(&tasks, &gates, now)
            .await
            .expect("poll");
        assert_eq!(polled.len(), 2, "both conditions are asked");

        let rollback = tasks
            .get_by_number(number("rollback"))
            .await
            .expect("read")
            .expect("exists");
        assert_eq!(
            rollback.status,
            TaskStatus::Skipped,
            "the branch whose condition does not hold is settled, not left waiting"
        );
        assert!(
            rollback
                .skip_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("/state") && reason.contains("green")),
            "the card says which pointer decided it and what was there: {:?}",
            rollback.skip_reason
        );

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert!(
            sweep.promoted.contains(&number("announce")),
            "the branch whose condition holds runs"
        );
        assert!(
            !sweep.promoted.contains(&number("notify")),
            "the merge still waits for the branch that is actually running"
        );

        complete(
            &tasks,
            number("announce"),
            serde_json::json!({"sent": true}),
        )
        .await;

        let sweep = tasks.recompute_ready("agent-1").await.expect("sweep");
        assert!(
            sweep.promoted.contains(&number("notify")),
            "the merge runs with one parent done and one skipped — this is the \
             deadlock the feature removes, and it must stay removed"
        );
    }

    /// A condition reading a step that is not in the workflow can never be
    /// answered, so it is refused at launch by name.
    ///
    /// The alternative is a step held forever by a gate whose source does not
    /// exist, which on the board is indistinguishable from one still waiting.
    #[tokio::test]
    async fn a_condition_naming_an_unknown_step_is_refused_with_the_name() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "rollback", 0))
            .await
            .expect("put step");
        workflows
            .put_gate(&condition(&id, "rollback", "check", "red"))
            .await
            .expect("put gate");

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a condition nothing can answer is not launchable");
        match &error {
            LaunchError::UnknownGateSource {
                step_key,
                gate_key,
                missing,
            } => {
                assert_eq!(step_key, "rollback");
                assert_eq!(gate_key, "runs-when");
                assert_eq!(missing, "check");
            }
            other => panic!("expected UnknownGateSource, got {other:?}"),
        }
        assert!(
            error.to_string().contains("rollback") && error.to_string().contains("check"),
            "the refusal names both steps: {error}"
        );
    }

    /// A condition whose config will not evaluate would error once a minute
    /// forever with nobody reading the log. Refused while the author is still
    /// looking at it, by the same validator the task level uses.
    #[test]
    fn a_condition_with_no_predicate_is_refused_by_the_shared_validator() {
        let mut gate = condition("w", "rollback", "check", "red");
        gate.config = serde_json::json!({"equals": "red"});
        let error = validate_step_gate(&gate).expect_err("a task_output condition needs a pointer");
        assert!(
            matches!(
                error,
                crate::tasks::GateConfigError::MissingField {
                    field: "pointer",
                    ..
                }
            ),
            "{error:?}"
        );

        // And the source is *not* what the task-level validator asks for here:
        // a template has a step key, not a number, so demanding `task_number`
        // would make every template gate unsavable.
        let mut usable = condition("w", "rollback", "check", "red");
        usable.config = serde_json::json!({"pointer": "/state", "equals": "red"});
        assert!(validate_step_gate(&usable).is_ok());
    }

    /// Deleting a step takes its conditions with it, in both directions.
    ///
    /// A gate on a step that is gone, or reading one that is gone, makes every
    /// future launch fail validation over a row a step-oriented editor cannot
    /// show.
    #[tokio::test]
    async fn deleting_a_step_takes_the_conditions_that_mention_it() {
        let (workflows, _tasks, id) = fixture().await;
        for (index, key) in ["check", "rollback"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("put step");
        }
        workflows
            .put_gate(&condition(&id, "rollback", "check", "red"))
            .await
            .expect("put gate");

        assert_eq!(workflows.list_gates(&id).await.expect("gates").len(), 1);
        workflows
            .delete_step(&id, "check")
            .await
            .expect("delete step");
        assert!(
            workflows.list_gates(&id).await.expect("gates").is_empty(),
            "a condition reading a deleted step goes with it"
        );
    }

    // -- Spend ceilings -----------------------------------------------------

    /// The only live hazard on this board: fan-out width was uncapped, and the
    /// width is decided at run time by model output. A scan step that
    /// hallucinates a nine-hundred-element array is nine hundred tasks, each of
    /// them a live model call, on an instance that runs unattended on a timer.
    ///
    /// If this regresses, the cap is either gone or — far worse — has become a
    /// truncation. The assertion that no branch exists is the one that matters:
    /// a fan-out that emitted the first fifty would feed the report step a
    /// subset and the report would present it as the whole collection, which is
    /// a wrong answer delivered confidently rather than a run that stopped.
    #[tokio::test]
    async fn a_fan_out_wider_than_the_branch_cap_is_refused_naming_the_pointer_and_the_count_and_emits_no_branches()
     {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");

        let repos: Vec<serde_json::Value> = (0..crate::tasks::MAX_FAN_OUT_BRANCHES + 1)
            .map(|index| serde_json::json!({"name": format!("repo-{index}")}))
            .collect();
        let found = repos.len();
        complete(
            &tasks,
            launched.task_numbers["scan"],
            serde_json::json!({"repos": repos}),
        )
        .await;

        let outcomes = tasks
            .expand_fan_outs("agent-1")
            .await
            .expect("expansion pass");
        let placeholder = launched.task_numbers["build"];
        match &outcomes[..] {
            [
                crate::tasks::FanOutOutcome::Blocked {
                    placeholder_task_number,
                    reason,
                },
            ] => {
                assert_eq!(*placeholder_task_number, placeholder);
                assert!(
                    reason.contains("/repos"),
                    "the refusal must name the pointer, or nobody knows which collection was \
                     too wide: {reason}"
                );
                assert!(
                    reason.contains(&found.to_string()),
                    "the refusal must name the count found, which is what separates a pointer \
                     aimed at the wrong level from a genuinely enormous collection: {reason}"
                );
                assert!(
                    reason.contains("MAX_FAN_OUT_BRANCHES"),
                    "a run stopped by a ceiling that does not say which ceiling is \
                     indistinguishable from a bug: {reason}"
                );
            }
            other => panic!("expected the fan-out to be refused, got {other:?}"),
        }

        let run_tasks = tasks
            .list_by_workflow_run(&launched.run.id)
            .await
            .expect("run tasks");
        assert_eq!(
            run_tasks.len(),
            3,
            "the refusal must emit nothing at all — scan, the parked placeholder, and report"
        );
        assert!(
            run_tasks
                .iter()
                .all(|task| task.fan_out_branch_key.is_none()),
            "not one branch may exist: a truncated fan-out feeding a fan-in reports part of the \
             collection as the whole of it"
        );
    }

    /// A template already past the run ceiling is refused at launch, where the
    /// answer is a corrected template rather than an incident.
    ///
    /// If this regresses, the ceiling is only enforced on the paths that grow a
    /// run *after* launch, and a large enough template starts a run that is
    /// parked the instant it begins.
    #[tokio::test]
    async fn a_template_with_more_steps_than_one_run_may_hold_is_refused_at_launch_naming_the_ceiling()
     {
        let (workflows, tasks, id) = fixture().await;
        for index in 0..=crate::tasks::MAX_RUN_TASKS {
            workflows
                .put_step(&step(&id, &format!("s{index}"), index))
                .await
                .expect("step");
        }

        let error = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect_err("a template over the run ceiling must be refused");
        match error {
            LaunchError::RunTaskCeiling { steps, ceiling } => {
                assert_eq!(steps as i64, crate::tasks::MAX_RUN_TASKS + 1);
                assert_eq!(ceiling, crate::tasks::MAX_RUN_TASKS);
                assert!(
                    error.to_string().contains("MAX_RUN_TASKS"),
                    "the refusal names the limit it was"
                );
            }
            other => panic!("expected RunTaskCeiling, got {other:?}"),
        }
    }

    /// Fill a run out to `total` tasks with settled filler, so a ceiling can be
    /// reached without emitting two hundred real cards.
    ///
    /// The numbers start well above the sequence so they cannot collide with
    /// anything the run allocates afterwards.
    async fn pad_run_to(pool: &SqlitePool, run_id: &str, existing: i64, total: i64) {
        for index in 0..(total - existing) {
            sqlx::query(
                "INSERT INTO tasks (id, task_number, title, status, priority, owner_agent_id, \
                 assigned_agent_id, created_by, workflow_run_id) \
                 VALUES (?, ?, ?, 'done', 'medium', 'agent-1', 'agent-1', 'test', ?)",
            )
            .bind(format!("filler-{index}"))
            .bind(100_000 + index)
            .bind(format!("filler {index}"))
            .bind(run_id)
            .execute(pool)
            .await
            .expect("filler task");
        }
    }

    /// The second ceiling, and the one the width cap cannot cover: fifty
    /// branches is within the width cap every time, and a loop body containing
    /// one reaches fifty again on every pass.
    ///
    /// If this regresses, a run's size is once again decided entirely by model
    /// output, and the only thing standing between a wedged pipeline and an
    /// unbounded bill is that no single step happened to fan out too wide.
    #[tokio::test]
    async fn a_fan_out_that_would_cross_the_run_task_ceiling_parks_the_run_stuck_and_names_the_ceiling()
     {
        let (workflows, tasks, id) = fixture().await;
        fan_out_template(&workflows, &id).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        // Three real tasks plus filler, two short of the ceiling: a ten-branch
        // fan-out is well within the width cap and still cannot fit.
        pad_run_to(
            tasks.pool(),
            &launched.run.id,
            3,
            crate::tasks::MAX_RUN_TASKS - 2,
        )
        .await;

        let repos: Vec<serde_json::Value> = (0..10)
            .map(|index| serde_json::json!({"name": format!("repo-{index}")}))
            .collect();
        complete(
            &tasks,
            launched.task_numbers["scan"],
            serde_json::json!({"repos": repos}),
        )
        .await;

        let outcomes = tasks
            .expand_fan_outs("agent-1")
            .await
            .expect("expansion pass");
        match &outcomes[..] {
            [crate::tasks::FanOutOutcome::Blocked { reason, .. }] => assert!(
                reason.contains("MAX_RUN_TASKS")
                    && reason.contains(&crate::tasks::MAX_RUN_TASKS.to_string()),
                "the refusal must name the ceiling and its value: {reason}"
            ),
            other => panic!("expected the run ceiling to refuse the expansion, got {other:?}"),
        }

        let run = workflows
            .get_run(&launched.run.id)
            .await
            .expect("fetch run")
            .expect("run exists");
        match workflows
            .assess_run(&tasks, &run)
            .await
            .expect("assess the run")
        {
            RunAssessment::Settled { status, reason } => {
                assert_eq!(status, RunStatus::Stuck);
                assert!(
                    reason.contains("MAX_RUN_TASKS"),
                    "the run must carry the ceiling that stopped it, not just the word stuck: \
                     {reason}"
                );
            }
            other => panic!("a run that cannot expand cannot advance, got {other:?}"),
        }
    }

    /// The other post-launch growth path, and the one the fan-out check cannot
    /// cover: a loop with attempts left, whose next pass will not fit.
    ///
    /// It must not take its `on_exhausted` edge here. That edge is a declared
    /// give-up path that emits tasks of its own, so routing to it would spend
    /// past the very ceiling that stopped the loop. If this regresses, hitting
    /// the run ceiling inside a loop either does nothing — and the loop keeps
    /// spending — or fires the give-up branch and spends anyway.
    #[tokio::test]
    async fn a_loop_that_cannot_fit_another_pass_parks_at_the_run_ceiling_instead_of_giving_up() {
        let (workflows, tasks, id) = fixture().await;
        loop_template(&workflows, &id, Some(5)).await;

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        complete(
            &tasks,
            launched.task_numbers["analyze"],
            serde_json::json!({"failures": ["boom"]}),
        )
        .await;
        // One task short of the ceiling, so the two-task body cannot fit even
        // though the loop still has four attempts left.
        pad_run_to(
            tasks.pool(),
            &launched.run.id,
            4,
            crate::tasks::MAX_RUN_TASKS - 1,
        )
        .await;

        run_pass(&tasks, &launched.run.id, 1, false).await;

        let outcomes = tasks.advance_loops("agent-1").await.expect("boundary");
        match &outcomes[..] {
            [crate::tasks::LoopOutcome::ExhaustedBlocked { reason, .. }] => assert!(
                reason.contains("MAX_RUN_TASKS") && reason.contains("give-up path"),
                "the loop must park naming the ceiling, and say why it did not route: {reason}"
            ),
            other => panic!("expected the run ceiling to stop the loop, got {other:?}"),
        }

        let run_tasks = tasks
            .list_by_workflow_run(&launched.run.id)
            .await
            .expect("run tasks");
        assert!(
            run_tasks.iter().all(|task| task.loop_iteration != Some(2)),
            "not one task of the next pass may exist — the ceiling refuses, it does not \
             half-emit"
        );

        let run = workflows
            .get_run(&launched.run.id)
            .await
            .expect("fetch run")
            .expect("run exists");
        match workflows
            .assess_run(&tasks, &run)
            .await
            .expect("assess the run")
        {
            RunAssessment::Settled { status, reason } => {
                assert_eq!(
                    status,
                    RunStatus::Stuck,
                    "out of the run's budget is a person raising a limit; out of the loop's own \
                     attempts is a person fixing a template — different recoveries, so not the \
                     same status"
                );
                assert!(reason.contains("MAX_RUN_TASKS"), "{reason}");
            }
            other => panic!("expected the run to be parked, got {other:?}"),
        }
    }

    // -- Run state ----------------------------------------------------------

    /// Launch `build -> deploy` and finish `build`, leaving `deploy` at the
    /// frontier for whatever the caller wants to do to it.
    async fn frontier_fixture() -> (WorkflowStore, TaskStore, WorkflowRun, i64) {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["build", "deploy"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows
            .link_steps(&id, "build", "deploy")
            .await
            .expect("link");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        complete(
            &tasks,
            launched.task_numbers["build"],
            serde_json::json!({"artifact": "a.tar"}),
        )
        .await;

        let run = workflows
            .get_run(&launched.run.id)
            .await
            .expect("fetch run")
            .expect("run exists");
        let deploy = launched.task_numbers["deploy"];
        (workflows, tasks, run, deploy)
    }

    /// The ordinary ending, and the one thing it must not do is call a skipped
    /// branch a failure.
    ///
    /// If this regresses, either every finished run stays `running` forever —
    /// so nothing ever reports and the status is decoration — or a pipeline
    /// whose condition correctly ruled a branch out is reported as having
    /// failed, which sends someone to debug a run that did exactly what it was
    /// designed to do.
    #[tokio::test]
    async fn a_run_whose_tasks_have_all_settled_succeeds_and_a_skipped_branch_is_not_a_failure() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;

        tasks
            .skip_task(deploy, "the rollback branch was not taken")
            .await
            .expect("skip");

        match workflows
            .assess_run(&tasks, &run)
            .await
            .expect("assess the run")
        {
            RunAssessment::Settled { status, reason } => {
                assert_eq!(
                    status,
                    RunStatus::Succeeded,
                    "a branch that was not taken is the pipeline working"
                );
                assert!(
                    reason.contains("did not run")
                        && reason.contains("the rollback branch was not taken"),
                    "succeeded-with-a-skip and succeeded-outright have the same recovery and so \
                     share a status, but not saying which happened is how the difference gets \
                     lost: {reason}"
                );
            }
            other => panic!("expected the run to have settled, got {other:?}"),
        }
    }

    /// The false positive that would matter most.
    ///
    /// A gate that has not opened yet is the world not having answered, and it
    /// answers on its own. If this regresses, every pipeline waiting on CI is
    /// reported as stuck, people learn the status is noise, and the two states
    /// that genuinely need somebody get ignored along with it — strictly worse
    /// than the silence this feature replaced.
    #[tokio::test]
    async fn a_run_waiting_on_a_gate_that_can_still_open_is_waiting_rather_than_stuck() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;

        crate::tasks::GateStore::new(tasks.pool().clone())
            .create(
                deploy,
                GateKind::Http,
                &serde_json::json!({"url": "https://ci.example/status", "expect_status": 200}),
                Some("waiting for CI on main"),
                60,
                None,
            )
            .await
            .expect("gate");

        assert_eq!(
            workflows
                .assess_run(&tasks, &run)
                .await
                .expect("assess the run"),
            RunAssessment::Advancing,
            "a run waiting on a pollable gate is waiting, not stuck"
        );
    }

    /// The gate case that *is* stuck, and the reason the two need separating.
    ///
    /// A gate that has errored its way past `GATE_ERROR_LIMIT` has stopped
    /// being polled, so its `erroring` no longer means "still trying" — it
    /// means "gave up trying", and the branch it guards will never be decided.
    /// If this regresses, a run behind an unreachable endpoint is
    /// indistinguishable from one waiting on a build, which is the silence this
    /// whole feature exists to remove.
    #[tokio::test]
    async fn a_gate_that_has_stopped_polling_leaves_its_run_stuck_rather_than_waiting() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;

        let gate = crate::tasks::GateStore::new(tasks.pool().clone())
            .create(
                deploy,
                GateKind::Http,
                &serde_json::json!({"url": "https://ci.example/status", "expect_status": 200}),
                Some("waiting for CI on main"),
                60,
                Some(GateDisposition::Route),
            )
            .await
            .expect("gate");
        sqlx::query(
            "UPDATE task_gates SET last_result = 'erroring', consecutive_errors = ?, \
             last_detail = 'dns failure' WHERE id = ?",
        )
        .bind(crate::tasks::GATE_ERROR_LIMIT)
        .bind(&gate.id)
        .execute(tasks.pool())
        .await
        .expect("exhaust the gate's error budget");

        match workflows
            .assess_run(&tasks, &run)
            .await
            .expect("assess the run")
        {
            RunAssessment::Settled { status, reason } => {
                assert_eq!(status, RunStatus::Stuck);
                assert!(
                    reason.contains(&deploy.to_string()) && reason.contains("never open"),
                    "the reason must name the task and say the gate will not open: {reason}"
                );
            }
            other => panic!("a gate that stopped polling wedges its run, got {other:?}"),
        }
    }

    /// The case the doc calls out: nothing is running, nothing is promotable,
    /// and every individual card looks reasonable.
    ///
    /// If this regresses, a run parked behind a task only a person can release
    /// sits silently forever, which is precisely the state that made run status
    /// necessary — and the assertion on the reason is half the point, because
    /// "stuck" with no reason sends someone reading rows.
    #[tokio::test]
    async fn a_run_whose_only_unfinished_task_is_blocked_forever_is_stuck_and_says_which_and_why() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;

        tasks
            .block_task(
                deploy,
                crate::tasks::BlockKind::NeedsInput,
                "needs a production credential nobody has issued",
            )
            .await
            .expect("block");

        match workflows
            .assess_run(&tasks, &run)
            .await
            .expect("assess the run")
        {
            RunAssessment::Settled { status, reason } => {
                assert_eq!(status, RunStatus::Stuck);
                assert!(
                    reason.contains(&deploy.to_string())
                        && reason.contains("needs_input")
                        && reason.contains("production credential"),
                    "the reason carries the task, the kind, and the card's own words: {reason}"
                );
            }
            other => panic!("expected a stuck run, got {other:?}"),
        }
    }

    /// `failed` and `stuck` are one label away from being the same bug this
    /// codebase has now paid for four times.
    ///
    /// A task that used its whole failure budget has run, repeatedly, and does
    /// not work; a stuck run has not run at all and is waiting on a person.
    /// They recover differently — fix the step versus release the block — so
    /// they must not share a status. If this regresses, every exhausted
    /// pipeline is reported as stuck and the recovery advice is wrong.
    #[tokio::test]
    async fn a_task_that_used_its_whole_failure_budget_makes_its_run_failed_rather_than_stuck() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;

        for _ in 0..crate::tasks::DEFAULT_FAILURE_LIMIT {
            for status in [TaskStatus::Ready, TaskStatus::InProgress] {
                tasks
                    .update(
                        deploy,
                        crate::tasks::UpdateTaskInput {
                            status: Some(status),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("advance")
                    .expect("exists");
            }
            tasks
                .record_failure(
                    deploy,
                    crate::tasks::TaskRunOutcome::Failed,
                    "the deploy host refused the connection",
                )
                .await
                .expect("record failure");
        }

        match workflows
            .assess_run(&tasks, &run)
            .await
            .expect("assess the run")
        {
            RunAssessment::Settled { status, reason } => {
                assert_eq!(status, RunStatus::Failed);
                assert!(
                    reason.contains("failure budget") && reason.contains("refused the connection"),
                    "the reason says which task ran out and what it said: {reason}"
                );
            }
            other => panic!("expected a failed run, got {other:?}"),
        }
    }

    /// Cancelling settles what has not started and leaves what has.
    ///
    /// Killing a task mid-flight throws away whatever it had already done, and
    /// leaves a worker writing into a card nobody will read. If this regresses,
    /// either cancel stops nothing — the graph carries on scheduling — or it
    /// stops too much and destroys work in progress.
    #[tokio::test]
    async fn cancelling_a_run_settles_its_unstarted_tasks_and_leaves_the_running_one_alone() {
        let (workflows, tasks, id) = fixture().await;
        for (index, key) in ["build", "test", "deploy"].iter().enumerate() {
            workflows
                .put_step(&step(&id, key, index as i64))
                .await
                .expect("step");
        }
        workflows
            .link_steps(&id, "build", "test")
            .await
            .expect("link");
        workflows
            .link_steps(&id, "test", "deploy")
            .await
            .expect("link");

        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        complete(
            &tasks,
            launched.task_numbers["build"],
            serde_json::json!({"artifact": "a.tar"}),
        )
        .await;

        // `test` is claimed and running; `deploy` has not started.
        let test = launched.task_numbers["test"];
        let deploy = launched.task_numbers["deploy"];
        tasks.recompute_ready("agent-1").await.expect("sweep");
        let claimed = tasks
            .claim_next_ready("agent-1")
            .await
            .expect("claim")
            .expect("something was ready");
        assert_eq!(claimed.task_number, test);

        let outcome = workflows
            .cancel_run(&launched.run.id, "pat")
            .await
            .expect("cancel");
        assert_eq!(
            outcome,
            CancelOutcome::Cancelled {
                settled: 1,
                left_running: 1
            }
        );

        let run = workflows
            .get_run(&launched.run.id)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.finished_at.is_some(), "a terminal run is stamped");

        let running = tasks
            .get_by_number(test)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            running.status,
            TaskStatus::InProgress,
            "work already in flight finishes or is reaped; cancelling must not throw it away"
        );

        let unstarted = tasks
            .get_by_number(deploy)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(unstarted.status, TaskStatus::Skipped);
        assert!(
            unstarted
                .skip_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("cancelled by pat")),
            "the card says why it will never run, and who decided"
        );

        // The claim race, from the other side: a cancelled task is no longer
        // ready, so nothing can be handed out after the cancel.
        assert!(
            tasks
                .claim_next_ready("agent-1")
                .await
                .expect("claim")
                .is_none(),
            "cancelling must close the window `claim_next_ready` reads"
        );
    }

    /// Delete is not a stop, and refusing to conflate them is the whole
    /// content of this test.
    ///
    /// If this regresses, deleting a live run pulls the cards out from under
    /// the scheduler mid-promote, or worse, out from under a worker that is
    /// still writing to one.
    #[tokio::test]
    async fn deleting_a_run_is_refused_while_it_is_live_and_removes_everything_once_it_is_not() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;

        match workflows
            .delete_run(&tasks, &run.id)
            .await
            .expect("delete attempt")
        {
            DeleteRunOutcome::Refused { reason } => assert!(
                reason.contains("cancel it first"),
                "the refusal says what to do instead: {reason}"
            ),
            other => panic!("a running run must not be deletable, got {other:?}"),
        }

        workflows.cancel_run(&run.id, "pat").await.expect("cancel");

        match workflows.delete_run(&tasks, &run.id).await.expect("delete") {
            DeleteRunOutcome::Deleted { tasks_removed } => assert_eq!(tasks_removed, 2),
            other => panic!("expected the run to be deleted, got {other:?}"),
        }

        assert!(
            workflows.get_run(&run.id).await.expect("fetch").is_none(),
            "the row an agent had no way to clean up"
        );
        assert!(
            tasks.get_by_number(deploy).await.expect("fetch").is_none(),
            "the cards go with it, or the next board view shows work from a run that is gone"
        );
    }

    /// A run that stopped says so once.
    ///
    /// The supervisor runs on every tick of every agent, so "notify on state"
    /// would repeat the same alert every thirty seconds for as long as the run
    /// stayed stuck — a status nobody reads, which is what having no status
    /// was. The transition is a conditional UPDATE for exactly this reason, and
    /// if it regresses the inbox fills up until people mute it.
    #[tokio::test]
    async fn a_terminal_transition_is_reported_once_however_many_times_the_pass_runs() {
        let (workflows, tasks, run, deploy) = frontier_fixture().await;
        tasks
            .block_task(
                deploy,
                crate::tasks::BlockKind::NeedsInput,
                "needs a production credential nobody has issued",
            )
            .await
            .expect("block");

        let first = workflows.sweep_runs(&tasks, 0).await.expect("first pass");
        assert_eq!(first.len(), 1, "the transition is reported when it happens");
        assert_eq!(first[0].run_id, run.id);
        assert_eq!(first[0].status, RunStatus::Stuck);
        assert!(
            first[0].status.warrants_notice(),
            "stuck is one of the two states a person is told about"
        );

        let second = workflows.sweep_runs(&tasks, 0).await.expect("second pass");
        assert!(
            second.is_empty(),
            "the run is still stuck and there is nothing new to say — a terminal run is never \
             assessed again"
        );

        let settled = workflows
            .get_run(&run.id)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(settled.status, RunStatus::Stuck);
        assert!(settled.status_reason.is_some());
        assert!(settled.finished_at.is_some());
    }

    /// The grace period, which is the reaper's and exists for the reaper's
    /// reason: a launch writes the run row and then emits its graph, and a run
    /// judged inside that window has fewer tasks than it will have.
    ///
    /// If this regresses, a run is declared stuck between its own two writes
    /// and every launch races the supervisor.
    #[tokio::test]
    async fn a_run_younger_than_the_grace_period_is_not_judged_at_all() {
        let (workflows, tasks, id) = fixture().await;
        workflows
            .put_step(&step(&id, "build", 0))
            .await
            .expect("step");
        let launched = workflows
            .launch(&tasks, &id, &serde_json::json!({"tag": "v1"}), "agent-1")
            .await
            .expect("launch");
        // Every task of this run, gone — the shape of a launch caught between
        // inserting its run row and emitting its graph.
        tasks
            .delete(launched.task_numbers["build"])
            .await
            .expect("delete");

        assert!(
            workflows
                .sweep_runs(&tasks, 60)
                .await
                .expect("sweep")
                .is_empty(),
            "a run this young is still being written"
        );
        assert_eq!(
            workflows.sweep_runs(&tasks, 0).await.expect("sweep").len(),
            1,
            "past the grace period the same run is judged, and a run with no tasks is not a \
             success"
        );
    }
}
