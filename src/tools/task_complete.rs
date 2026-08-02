//! Structured completion for task workers.
//!
//! `task_update` carries prose and `set_outcome` carries a verdict; neither
//! carries a value a downstream task can read. This does: the worker submits an
//! `outputs` object, it is checked against the task's declared `output_schema`,
//! and a mismatch is rejected with the reasons rather than accepted and
//! discovered later by whatever consumes it.
//!
//! Rejection returns an error the worker can act on, so it corrects and retries
//! inside its own segment budget. That is deliberately cheaper than failing the
//! task: a wrong shape is usually a formatting slip, not a failed job.

use crate::WorkerId;
use crate::tasks::{OutputSubmission, TaskStore, WorkerOutputSubmission};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TaskCompleteTool {
    task_store: Arc<TaskStore>,
    worker_id: WorkerId,
}

impl TaskCompleteTool {
    pub fn new(task_store: Arc<TaskStore>, worker_id: WorkerId) -> Self {
        Self {
            task_store,
            worker_id,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task_complete failed: {0}")]
pub struct TaskCompleteError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskCompleteArgs {
    pub task_number: i64,
    /// Human-readable account of what was done.
    pub summary: String,
    /// Machine-readable result, validated against the task's output schema.
    pub outputs: serde_json::Value,
    /// Task numbers you filed while working on this. Verified server-side.
    #[serde(default)]
    pub created_tasks: Vec<i64>,
}

#[derive(Debug, Serialize)]
pub struct TaskCompleteOutput {
    pub success: bool,
    pub task_number: i64,
    pub message: String,
}

impl Tool for TaskCompleteTool {
    const NAME: &'static str = "task_complete";

    type Error = TaskCompleteError;
    type Args = TaskCompleteArgs;
    type Output = TaskCompleteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Submit the structured result of your assigned task. The `outputs` \
                object is validated against the task's declared output schema and read by \
                downstream tasks, so every value must be one you actually produced. If it does \
                not match the schema you will be told what is wrong and can call this again."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_number": {
                        "type": "integer",
                        "description": "Your assigned task number."
                    },
                    "summary": {
                        "type": "string",
                        "description": "What you did, for a human reading the board."
                    },
                    "outputs": {
                        "type": "object",
                        "description": "The task's result, matching the output schema given in your instructions."
                    },
                    "created_tasks": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Task numbers you filed while working on this. Verified against what was actually created — reporting a card you did not file rejects the completion."
                    }
                },
                "required": ["task_number", "summary", "outputs"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Check the claim about filed cards before accepting the completion.
        //
        // A worker reporting children it never created leaves whoever reads the
        // board believing work is scheduled when it is not — worse than the
        // worker failing outright, because the failure is invisible until
        // someone wonders why nothing happened. Verified against `created_by`,
        // which only the store writes.
        if !args.created_tasks.is_empty() {
            let filer = crate::tasks::filer_id(args.task_number);
            let unverified = self
                .task_store
                .unverified_filed_tasks(&filer, &args.created_tasks)
                .await
                .map_err(|error| TaskCompleteError(format!("{error}")))?;

            if !unverified.is_empty() {
                let list = unverified
                    .iter()
                    .map(|number| format!("#{number}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(TaskCompleteError(format!(
                    "you reported filing {list}, but this task did not create them. \
                     File them with task_create, or drop them from created_tasks."
                )));
            }
        }

        let submission = self
            .task_store
            .submit_worker_outputs(&self.worker_id.to_string(), args.task_number, &args.outputs)
            .await
            .map_err(|error| TaskCompleteError(format!("{error}")))?;

        match submission {
            WorkerOutputSubmission::Submitted(OutputSubmission::Accepted) => {
                Ok(TaskCompleteOutput {
                    success: true,
                    task_number: args.task_number,
                    message: format!("Recorded outputs for task #{}.", args.task_number),
                })
            }
            WorkerOutputSubmission::Submitted(OutputSubmission::Rejected { problems }) => {
                // An error rather than a success-with-warning: a tool result the
                // model can read past is one it will read past, and the whole
                // value of the contract is that this does not silently pass.
                let detail = problems
                    .iter()
                    .map(|problem| problem.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                Err(TaskCompleteError(format!(
                    "outputs do not match the task's declared output schema: {detail}. \
                     Correct the object and call task_complete again."
                )))
            }
            WorkerOutputSubmission::Submitted(OutputSubmission::TaskMissing) => Err(
                TaskCompleteError(format!("task #{} does not exist", args.task_number)),
            ),
            WorkerOutputSubmission::WrongTask {
                assigned_task_number,
            } => Err(TaskCompleteError(format!(
                "you are assigned to task #{assigned_task_number}, not #{}",
                args.task_number
            ))),
            WorkerOutputSubmission::NotAssigned => Err(TaskCompleteError(
                "you are not assigned to a task, so there is nothing to complete".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{CreateTaskInput, TaskStatus, UpdateTaskInput};
    use sqlx::sqlite::SqlitePoolOptions;

    async fn store_with_task(worker_id: WorkerId) -> (Arc<TaskStore>, i64) {
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

        let store = Arc::new(TaskStore::new(pool));
        let task = store
            .create(CreateTaskInput {
                owner_agent_id: "agent-1".into(),
                assigned_agent_id: "agent-1".into(),
                title: "build".into(),
                status: TaskStatus::InProgress,
                created_by: "test".into(),
                ..Default::default()
            })
            .await
            .expect("create task");

        store
            .update(
                task.task_number,
                UpdateTaskInput {
                    worker_id: Some(worker_id.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("bind worker")
            .expect("exists");

        (store, task.task_number)
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["tag"],
            "properties": {"tag": {"type": "string"}},
        })
    }

    #[tokio::test]
    async fn accepts_output_matching_the_declared_schema() {
        let worker_id = uuid::Uuid::new_v4();
        let (store, task_number) = store_with_task(worker_id).await;
        store
            .set_contract(task_number, None, Some(&schema()))
            .await
            .expect("set contract");

        let tool = TaskCompleteTool::new(store.clone(), worker_id);
        let output = tool
            .call(TaskCompleteArgs {
                task_number,
                summary: "built it".into(),
                outputs: serde_json::json!({"tag": "v1.0.0"}),
                created_tasks: Vec::new(),
            })
            .await
            .expect("valid output should be accepted");

        assert!(output.success);
        let task = store
            .get_by_number(task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(task.outputs, Some(serde_json::json!({"tag": "v1.0.0"})));
    }

    /// The rejection has to reach the model as an error it must handle. A
    /// success-with-warning is something it will read past, and then the
    /// contract has bought nothing.
    #[tokio::test]
    async fn rejects_mismatched_output_and_says_what_is_wrong() {
        let worker_id = uuid::Uuid::new_v4();
        let (store, task_number) = store_with_task(worker_id).await;
        store
            .set_contract(task_number, None, Some(&schema()))
            .await
            .expect("set contract");

        let tool = TaskCompleteTool::new(store.clone(), worker_id);
        let error = tool
            .call(TaskCompleteArgs {
                task_number,
                summary: "built it".into(),
                outputs: serde_json::json!({"digest": "sha256:abc"}),
                created_tasks: Vec::new(),
            })
            .await
            .expect_err("output missing a required field must be rejected");

        let message = error.to_string();
        assert!(message.contains("tag"), "must name the problem: {message}");
        assert!(
            message.contains("task_complete again"),
            "must tell the worker it can retry: {message}"
        );

        let task = store
            .get_by_number(task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert!(
            task.outputs.is_none(),
            "a rejected output must not be visible downstream"
        );
    }

    /// A worker writing outputs onto someone else's task would be laundering
    /// invented values into a downstream task's inputs.
    #[tokio::test]
    async fn refuses_to_complete_another_workers_task() {
        let worker_id = uuid::Uuid::new_v4();
        let (store, task_number) = store_with_task(worker_id).await;

        let intruder = TaskCompleteTool::new(store.clone(), uuid::Uuid::new_v4());
        let error = intruder
            .call(TaskCompleteArgs {
                task_number,
                summary: "not mine".into(),
                outputs: serde_json::json!({"tag": "v9"}),
                created_tasks: Vec::new(),
            })
            .await
            .expect_err("a worker must not complete a task it was not given");
        assert!(error.to_string().contains("not assigned"));

        let task = store
            .get_by_number(task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert!(task.outputs.is_none());
    }

    #[tokio::test]
    async fn accepts_any_output_when_no_schema_is_declared() {
        let worker_id = uuid::Uuid::new_v4();
        let (store, task_number) = store_with_task(worker_id).await;

        let tool = TaskCompleteTool::new(store.clone(), worker_id);
        tool.call(TaskCompleteArgs {
            task_number,
            summary: "done".into(),
            outputs: serde_json::json!({"anything": [1, 2, 3]}),
            created_tasks: Vec::new(),
        })
        .await
        .expect("an undeclared contract constrains nothing");
    }
    /// Server-side verification of a model's claim about its own actions.
    ///
    /// A worker reporting children it never filed leaves whoever reads the
    /// board believing work is scheduled when it is not — worse than failing
    /// outright, because nothing looks wrong until somebody wonders why
    /// nothing happened.
    #[tokio::test]
    async fn rejects_a_completion_claiming_cards_it_did_not_file() {
        let worker_id = uuid::Uuid::new_v4();
        let (store, task_number) = store_with_task(worker_id).await;

        // A real card, but filed by somebody else.
        let other = store
            .create(CreateTaskInput {
                owner_agent_id: "agent-1".into(),
                assigned_agent_id: "agent-1".into(),
                title: "not mine".into(),
                status: TaskStatus::Backlog,
                created_by: "human".into(),
                ..Default::default()
            })
            .await
            .expect("create other task");

        let tool = TaskCompleteTool::new(store.clone(), worker_id);
        let error = tool
            .call(TaskCompleteArgs {
                task_number,
                summary: "decomposed the work".into(),
                outputs: serde_json::json!({}),
                created_tasks: vec![other.task_number, 9999],
            })
            .await
            .expect_err("claiming cards it did not file must be rejected");

        let message = error.to_string();
        assert!(message.contains(&format!("#{}", other.task_number)));
        assert!(
            message.contains("#9999"),
            "a nonexistent card counts too: {message}"
        );

        let task = store
            .get_by_number(task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert!(
            task.outputs.is_none(),
            "a rejected completion must not record outputs either"
        );
    }

    #[tokio::test]
    async fn accepts_a_completion_reporting_cards_it_really_filed() {
        let worker_id = uuid::Uuid::new_v4();
        let (store, task_number) = store_with_task(worker_id).await;

        let child = store
            .create(CreateTaskInput {
                owner_agent_id: "agent-1".into(),
                assigned_agent_id: "agent-1".into(),
                title: "filed child".into(),
                status: TaskStatus::Ready,
                created_by: crate::tasks::filer_id(task_number),
                ..Default::default()
            })
            .await
            .expect("create child");

        let tool = TaskCompleteTool::new(store.clone(), worker_id);
        tool.call(TaskCompleteArgs {
            task_number,
            summary: "decomposed the work".into(),
            outputs: serde_json::json!({"filed": 1}),
            created_tasks: vec![child.task_number],
        })
        .await
        .expect("a truthful claim should be accepted");
    }
}
