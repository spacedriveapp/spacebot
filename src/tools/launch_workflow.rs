//! Launching a stored pipeline from inside an agent.
//!
//! The sharpest of the three triggers, because it is the one whose absence made
//! workflows a reusable procedure that the autonomous part of the system could
//! not reuse. There was a `task_create` tool and no way to run a pipeline, so an
//! agent deciding "this needs the full release process" had to re-derive the
//! steps by hand, every time, and get them slightly different every time.
//!
//! Shaped like `task_create` deliberately, down to the bounds. A worker filing
//! cards is bounded by how many it may file and how deep the filing chain may
//! go; a worker *launching* is bounded by exactly the same two numbers, read
//! through exactly the same walk, because a launch is filing by another name —
//! it makes cards exist that the same scheduler will pick up.
//!
//! ## The recursion guard, and where the depth lives
//!
//! A workflow can contain a step whose worker calls this tool. That worker can
//! launch a workflow containing a step whose worker calls this tool. Without a
//! bound that is a self-sustaining tree, and it is a tree of *live model calls*.
//!
//! The bound is `MAX_FILING_DEPTH`, and the depth is not tracked in a new
//! column. `TaskStore::filing_depth` walks the `created_by` chain, following
//! `task:<n>` back to whoever filed it. So this tool launches with
//! `LaunchIdentity::triggered_by(agent, filer_id(task))`: the run's tasks get
//! `created_by = "task:<n>"`, and are therefore one filing hop deeper than the
//! task that launched them, by the definition already in use. A worker running
//! one of those steps that launches again is two hops deep, and at
//! `MAX_FILING_DEPTH` the walk refuses and names the limit.
//!
//! Choosing that over a `depth` column on `workflow_runs` is the whole point:
//! a second notion of depth would have to be kept in step with the first, and
//! the two would drift the first time somebody filed a card from inside a run.

use crate::tasks::TaskStore;
use crate::tools::task_create::{FilingRefusal, check_filing_limits};
use crate::workflows::{LaunchError, LaunchIdentity, WorkflowStore};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct LaunchWorkflowTool {
    workflows: Arc<WorkflowStore>,
    task_store: Arc<TaskStore>,
    agent_id: String,
    /// Set when this tool belongs to a worker executing a task. The worker
    /// launches *on behalf of* that task, which is what makes the chain
    /// bounded and the provenance real.
    ///
    /// Resolved from the worker at call time rather than captured at
    /// construction, so it cannot go stale and a worker not bound to a task
    /// simply cannot launch — exactly as `task_create` resolves its filer.
    launching_worker_id: Option<crate::WorkerId>,
    api_state: Option<Arc<crate::api::ApiState>>,
}

impl std::fmt::Debug for LaunchWorkflowTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchWorkflowTool")
            .field("agent_id", &self.agent_id)
            .finish()
    }
}

impl LaunchWorkflowTool {
    pub fn new(
        workflows: Arc<WorkflowStore>,
        task_store: Arc<TaskStore>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            workflows,
            task_store,
            agent_id: agent_id.into(),
            launching_worker_id: None,
            api_state: None,
        }
    }

    /// Scope this tool to a worker launching on behalf of the task it is
    /// executing. The only construction that is subject to the depth bound,
    /// because it is the only one that is part of a chain.
    pub fn for_task_worker(
        workflows: Arc<WorkflowStore>,
        task_store: Arc<TaskStore>,
        agent_id: impl Into<String>,
        worker_id: crate::WorkerId,
    ) -> Self {
        Self {
            workflows,
            task_store,
            agent_id: agent_id.into(),
            launching_worker_id: Some(worker_id),
            api_state: None,
        }
    }

    pub fn with_api_state(mut self, state: Arc<crate::api::ApiState>) -> Self {
        self.api_state = Some(state);
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("launch_workflow failed: {0}")]
pub struct LaunchWorkflowError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchWorkflowArgs {
    /// The workflow to run, by name or by id.
    pub workflow: String,
    /// The run input, matching the workflow's declared input schema.
    #[serde(default)]
    pub inputs: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct LaunchWorkflowOutput {
    pub success: bool,
    pub run_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    /// Emitted task numbers, keyed by the step they came from.
    pub task_numbers: HashMap<String, i64>,
    pub message: String,
}

impl LaunchWorkflowTool {
    /// Which task this worker is executing.
    async fn resolve_launching_task(
        &self,
        worker_id: crate::WorkerId,
    ) -> Result<i64, LaunchWorkflowError> {
        self.task_store
            .task_number_for_worker(&worker_id.to_string())
            .await
            .map_err(|error| LaunchWorkflowError(format!("{error}")))?
            .ok_or_else(|| {
                LaunchWorkflowError(
                    "you are not executing a task, so there is nothing to launch a workflow on \
                     behalf of"
                        .to_string(),
                )
            })
    }

    /// Refuse to launch once this task has generated enough work, or once the
    /// chain of launches and filings behind it is deep enough.
    ///
    /// The fan-out half counts *cards this task has caused to exist*, and the
    /// steps of a launched run are counted by it, because they carry this
    /// task's filer id. One launch may therefore overshoot the cap by the size
    /// of the workflow — the check is made before the run is compiled, and
    /// there is no partial launch to fall back to. That overshoot is bounded by
    /// `MAX_RUN_TASKS`, which `launch` enforces on its own, so the worst case
    /// is one template's worth rather than an unbounded one; what the check
    /// stops is the *next* launch after that.
    async fn enforce_launch_limits(
        &self,
        launching_task_number: i64,
    ) -> Result<(), LaunchWorkflowError> {
        match check_filing_limits(&self.task_store, launching_task_number).await {
            Ok(()) => Ok(()),
            Err(FilingRefusal::Storage(error)) => Err(LaunchWorkflowError(error)),
            Err(FilingRefusal::FanOut { filed, limit }) => Err(LaunchWorkflowError(format!(
                "task #{launching_task_number} has already generated {filed} cards, the limit is \
                 {limit}. Run the remaining steps yourself rather than launching another workflow."
            ))),
            Err(FilingRefusal::Depth { depth, limit }) => Err(LaunchWorkflowError(format!(
                "task #{launching_task_number} is {depth} launch/filing hops deep and the limit \
                 is {limit}. A workflow that launches a workflow that launches a workflow stops \
                 here — do this work directly."
            ))),
        }
    }

    /// Resolve a name-or-id to a workflow.
    ///
    /// By name first in the message when it fails, because a model naming a
    /// pipeline it half-remembers is the common mistake and a list of what
    /// actually exists is the one response it can act on. `UnknownWorkflow`
    /// alone would send it guessing again.
    async fn resolve_workflow(
        &self,
        reference: &str,
    ) -> Result<crate::workflows::Workflow, LaunchWorkflowError> {
        let workflows = self
            .workflows
            .list_workflows()
            .await
            .map_err(|error| LaunchWorkflowError(format!("{error}")))?;

        if let Some(found) = workflows
            .iter()
            .find(|workflow| workflow.id == reference || workflow.name == reference)
        {
            return Ok(found.clone());
        }

        let known = workflows
            .iter()
            .map(|workflow| workflow.name.as_str())
            .collect::<Vec<_>>();
        Err(LaunchWorkflowError(if known.is_empty() {
            format!("there is no workflow `{reference}`, and no workflows are defined at all")
        } else {
            format!(
                "there is no workflow `{reference}`. Defined workflows: {}",
                known.join(", ")
            )
        }))
    }
}

impl Tool for LaunchWorkflowTool {
    const NAME: &'static str = "launch_workflow";

    type Error = LaunchWorkflowError;
    type Args = LaunchWorkflowArgs;
    type Output = LaunchWorkflowOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/launch_workflow").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "workflow": {
                        "type": "string",
                        "description": "The workflow to run, by name or by id."
                    },
                    "inputs": {
                        "type": "object",
                        "description": "The run input. Must match the workflow's declared input schema — a launch with the wrong input is refused, and the refusal names the step and key."
                    }
                },
                "required": ["workflow"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let workflow = self.resolve_workflow(&args.workflow).await?;

        // Bounds first, before anything is compiled. A launch that is going to
        // be refused for depth must not have created a run row on the way to
        // finding that out.
        let launching_task_number = match self.launching_worker_id {
            Some(worker_id) => Some(self.resolve_launching_task(worker_id).await?),
            None => None,
        };
        if let Some(number) = launching_task_number {
            self.enforce_launch_limits(number).await?;
        }

        // Provenance, and with it the depth. A worker launches on behalf of its
        // task; a cortex or branch launches as the agent, which is a root and
        // therefore zero hops deep.
        let identity = match launching_task_number {
            Some(number) => {
                LaunchIdentity::triggered_by(&self.agent_id, crate::tasks::filer_id(number))
            }
            None => LaunchIdentity::agent(&self.agent_id),
        };

        let inputs = args.inputs.unwrap_or_else(|| serde_json::json!({}));

        let launched = self
            .workflows
            .launch_as(&self.task_store, &workflow.id, &inputs, &identity)
            .await
            .map_err(refusal)?;

        // Every emitted task starts in backlog, so nothing runs until a sweep
        // looks at them. Running one now is what makes a launch feel like a
        // launch rather than a filing — without it the entry step waits for the
        // next tick for no reason the model could see or explain.
        if let Err(error) = self.task_store.recompute_ready(&self.agent_id).await {
            tracing::warn!(
                %error,
                run_id = %launched.run.id,
                "workflow launched but the ready sweep failed; the next tick will pick it up"
            );
        }

        if let Some(api_state) = &self.api_state {
            for number in launched.task_numbers.values() {
                api_state
                    .event_tx
                    .send(crate::api::ApiEvent::TaskUpdated {
                        agent_id: self.agent_id.clone(),
                        task_number: *number,
                        status: crate::tasks::TaskStatus::Backlog.as_str().to_string(),
                        action: "created".to_string(),
                    })
                    .ok();
            }
        }

        let mut numbers = launched.task_numbers.values().copied().collect::<Vec<_>>();
        numbers.sort_unstable();

        Ok(LaunchWorkflowOutput {
            success: true,
            run_id: launched.run.id.clone(),
            workflow_id: workflow.id,
            workflow_name: workflow.name.clone(),
            task_numbers: launched.task_numbers,
            message: format!(
                "Launched `{}` as run {} — task(s) {}",
                workflow.name,
                launched.run.id,
                numbers
                    .iter()
                    .map(|number| format!("#{number}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
    }
}

/// Turn a refused launch into something a model can act on.
///
/// Every `LaunchError` variant already names the offending step and key, which
/// is exactly what makes it usable here — the text goes through unchanged
/// rather than being collapsed into "launch failed", because a model told only
/// that will retry the identical call. A storage failure is the one variant
/// that is not the caller's to fix, and says so.
fn refusal(error: LaunchError) -> LaunchWorkflowError {
    match error {
        LaunchError::Storage(detail) => LaunchWorkflowError(format!(
            "the launch could not be attempted — this is a storage failure on our side, not a \
             problem with your call: {detail}"
        )),
        other => LaunchWorkflowError(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tasks::{CreateTaskInput, TaskStatus};
    use crate::workflows::{BindingSource, StepBinding};
    use sqlx::sqlite::SqlitePoolOptions;

    /// A store pair over one pool, plus a three-step `deploy` workflow whose
    /// entry step reads the run input.
    async fn fixture() -> (Arc<WorkflowStore>, Arc<TaskStore>, String) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::tasks::store::create_task_schema(&pool).await;
        crate::workflows::store::create_workflow_schema(&pool).await;
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("seed sequence");

        let workflows = Arc::new(WorkflowStore::new(pool.clone()));
        let tasks = Arc::new(TaskStore::new(pool));

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

        for (index, key) in ["build", "test"].iter().enumerate() {
            workflows
                .put_step(&crate::workflows::WorkflowStep {
                    workflow_id: workflow.id.clone(),
                    step_key: (*key).to_string(),
                    title: format!("step {key}"),
                    description: None,
                    assigned_agent_id: None,
                    required_capabilities: None,
                    priority: crate::tasks::TaskPriority::Medium,
                    input_schema: None,
                    output_schema: None,
                    system_prompt: None,
                    repo_id: None,
                    position: index as i64,
                    for_each_step_key: None,
                    for_each_pointer: None,
                    for_each_key: None,
                    loop_group: None,
                    loop_max_iterations: None,
                    loop_until: None,
                    kind: crate::workflows::StepKind::Agent,
                    command: None,
                    command_timeout_secs: None,
                    expect_exit_code: None,
                    worktree_mode: crate::workflows::WorktreeMode::Inherit,
                    worktree_base_ref: None,
                })
                .await
                .expect("put step");
        }
        workflows
            .link_steps(&workflow.id, "build", "test")
            .await
            .expect("link");
        workflows
            .put_binding(&StepBinding {
                workflow_id: workflow.id.clone(),
                step_key: "build".into(),
                input_key: "tag".into(),
                source: BindingSource::RunInput,
                source_pointer: Some("/tag".into()),
                source_step_key: None,
                literal_value: None,
            })
            .await
            .expect("bind run input");

        (workflows, tasks, workflow.id)
    }

    /// Bind a fresh worker to a task and hand back its id, so the tool can be
    /// built in its "launching on behalf of a task" shape.
    async fn worker_on_task(tasks: &TaskStore, task_number: i64) -> crate::WorkerId {
        let worker_id = uuid::Uuid::new_v4();
        tasks
            .update(
                task_number,
                crate::tasks::UpdateTaskInput {
                    worker_id: Some(worker_id.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("bind worker")
            .expect("task exists");
        worker_id
    }

    async fn task_in_progress(tasks: &TaskStore, created_by: &str) -> i64 {
        tasks
            .create(CreateTaskInput {
                owner_agent_id: "agent-1".into(),
                assigned_agent_id: "agent-1".into(),
                title: "decide what to do".into(),
                status: TaskStatus::InProgress,
                created_by: created_by.to_string(),
                ..Default::default()
            })
            .await
            .expect("create task")
            .task_number
    }

    fn args(workflow: &str, inputs: serde_json::Value) -> LaunchWorkflowArgs {
        LaunchWorkflowArgs {
            workflow: workflow.to_string(),
            inputs: Some(inputs),
        }
    }

    /// The capability itself: an agent can run a stored pipeline and gets back
    /// the identifiers it needs to talk about what it started. Without the run
    /// id and the task numbers the tool would be a fire-and-forget with no way
    /// to report or follow up, which is what "the cortex can file a card and
    /// cannot run a pipeline" was really about.
    #[tokio::test]
    async fn the_tool_launches_a_workflow_and_returns_its_run_id_and_task_numbers() {
        let (workflows, tasks, workflow_id) = fixture().await;
        let tool = LaunchWorkflowTool::new(workflows.clone(), tasks.clone(), "agent-1");

        let output = tool
            .call(args("deploy", serde_json::json!({"tag": "v2.0.0"})))
            .await
            .expect("launching by name should succeed");

        assert!(output.success);
        assert_eq!(output.workflow_id, workflow_id);
        assert_eq!(output.workflow_name, "deploy");
        assert_eq!(
            output.task_numbers.len(),
            2,
            "every step of the template becomes a task"
        );

        let run = workflows
            .get_run(&output.run_id)
            .await
            .expect("fetch run")
            .expect("the run exists");
        assert_eq!(
            run.launched_by, "agent-1",
            "an agent launching for itself is its own launch identity"
        );

        let emitted = tasks
            .list_by_workflow_run(&output.run_id)
            .await
            .expect("list run tasks");
        assert_eq!(emitted.len(), 2);
        for task in &emitted {
            assert_eq!(
                task.assigned_agent_id, "agent-1",
                "an unassigned step runs as the launching agent, which must be an agent that can \
                 claim a card"
            );
        }
    }

    /// A refusal has to arrive as a tool error carrying the reason, not as a
    /// success the model then reports as done. `LaunchError` already names the
    /// offending step and key; collapsing that into "launch failed" would leave
    /// a model retrying the identical call forever.
    #[tokio::test]
    async fn a_launch_refused_by_validation_comes_back_as_an_actionable_tool_error() {
        let (workflows, tasks, _) = fixture().await;
        let tool = LaunchWorkflowTool::new(workflows, tasks, "agent-1");

        let error = tool
            .call(args("deploy", serde_json::json!({"tag": 7})))
            .await
            .expect_err("an input that does not match the schema must be refused");

        let message = error.to_string();
        assert!(
            message.contains("schema"),
            "the refusal must say the input was the problem: {message}"
        );
        assert!(
            message.contains("tag"),
            "the refusal must name the offending key so the model can fix it: {message}"
        );
    }

    /// Naming a workflow that does not exist is the common model mistake, and
    /// the only response it can act on is what does exist.
    #[tokio::test]
    async fn launching_an_unknown_workflow_lists_the_workflows_that_exist() {
        let (workflows, tasks, _) = fixture().await;
        let tool = LaunchWorkflowTool::new(workflows, tasks, "agent-1");

        let error = tool
            .call(args("depoly", serde_json::json!({"tag": "v1"})))
            .await
            .expect_err("an unknown workflow must be refused");

        assert!(
            error.to_string().contains("deploy"),
            "the refusal must list what is actually defined: {error}"
        );
    }

    /// The recursion guard. A workflow can contain a step whose worker launches
    /// a workflow, and without a bound that is a self-sustaining tree of live
    /// model calls. The depth is the existing `created_by` walk, so a launched
    /// run's tasks are one hop deeper than the task that launched them.
    #[tokio::test]
    async fn the_recursion_guard_stops_a_chain_and_names_the_limit() {
        let (workflows, tasks, _) = fixture().await;

        // A chain of tasks as deep as the limit allows, each filed by the last.
        let mut current = task_in_progress(&tasks, "human").await;
        for _ in 0..crate::tasks::MAX_FILING_DEPTH {
            current = task_in_progress(&tasks, &crate::tasks::filer_id(current)).await;
        }

        let worker_id = worker_on_task(&tasks, current).await;
        let tool =
            LaunchWorkflowTool::for_task_worker(workflows, tasks.clone(), "agent-1", worker_id);

        let error = tool
            .call(args("deploy", serde_json::json!({"tag": "v1"})))
            .await
            .expect_err("the depth limit must hold across the launch boundary");

        let message = error.to_string();
        assert!(
            message.contains(&crate::tasks::MAX_FILING_DEPTH.to_string()),
            "the refusal must name the limit so the model can adapt: {message}"
        );
        assert!(
            message.contains("hops deep"),
            "the refusal must explain why it was refused: {message}"
        );
    }

    /// The link that makes the guard work at all: a launched run's tasks carry
    /// the launching task's filer id, so the depth walk crosses the launch
    /// boundary. If this regresses the chain reads as depth zero forever and
    /// the guard above never fires, however deep the recursion goes.
    #[tokio::test]
    async fn a_worker_launched_run_carries_the_filing_task_as_its_provenance() {
        let (workflows, tasks, _) = fixture().await;

        let parent = task_in_progress(&tasks, "human").await;
        let worker_id = worker_on_task(&tasks, parent).await;
        let tool = LaunchWorkflowTool::for_task_worker(
            workflows.clone(),
            tasks.clone(),
            "agent-1",
            worker_id,
        );

        let output = tool
            .call(args("deploy", serde_json::json!({"tag": "v1"})))
            .await
            .expect("a worker on a task may launch");

        let run = workflows
            .get_run(&output.run_id)
            .await
            .expect("fetch run")
            .expect("the run exists");
        assert_eq!(run.launched_by, crate::tasks::filer_id(parent));

        for number in output.task_numbers.values() {
            let task = tasks
                .get_by_number(*number)
                .await
                .expect("fetch")
                .expect("exists");
            assert_eq!(
                task.created_by,
                crate::tasks::filer_id(parent),
                "provenance must name the launching task, or the depth walk stops here"
            );
            assert_eq!(
                task.owner_agent_id, "agent-1",
                "ownership must name an agent — a filer id cannot claim a card"
            );
            assert_eq!(
                tasks.filing_depth(*number).await.expect("depth"),
                1,
                "a launched run's tasks are one filing hop below the task that launched them"
            );
        }
    }

    /// The other half of the bound. Depth alone permits one task to launch the
    /// same pipeline without limit, which is fan-out by a different route.
    #[tokio::test]
    async fn a_task_that_has_generated_its_fan_out_budget_cannot_launch_again() {
        let (workflows, tasks, _) = fixture().await;

        let parent = task_in_progress(&tasks, "human").await;
        for _ in 0..crate::tasks::MAX_TASKS_FILED_PER_TASK {
            task_in_progress(&tasks, &crate::tasks::filer_id(parent)).await;
        }

        let worker_id = worker_on_task(&tasks, parent).await;
        let tool =
            LaunchWorkflowTool::for_task_worker(workflows, tasks.clone(), "agent-1", worker_id);

        let error = tool
            .call(args("deploy", serde_json::json!({"tag": "v1"})))
            .await
            .expect_err("the fan-out cap must hold");

        assert!(
            error
                .to_string()
                .contains(&crate::tasks::MAX_TASKS_FILED_PER_TASK.to_string()),
            "the refusal must name the limit: {error}"
        );
    }

    /// A worker with no task has nothing to launch on behalf of, and letting it
    /// launch anyway would produce a run with no provenance and no depth — a
    /// hole straight through the guard.
    #[tokio::test]
    async fn a_worker_not_executing_a_task_cannot_launch() {
        let (workflows, tasks, _) = fixture().await;
        let tool =
            LaunchWorkflowTool::for_task_worker(workflows, tasks, "agent-1", uuid::Uuid::new_v4());

        let error = tool
            .call(args("deploy", serde_json::json!({"tag": "v1"})))
            .await
            .expect_err("an unbound worker must not launch");
        assert!(error.to_string().contains("not executing a task"));
    }
}
