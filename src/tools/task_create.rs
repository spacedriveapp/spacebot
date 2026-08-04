//! Task creation tool for branch processes.

use crate::notifications::{NewNotification, NotificationKind, NotificationSeverity};
use crate::tasks::{CreateTaskInput, TaskPriority, TaskStatus, TaskStore, TaskSubtask};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct TaskCreateTool {
    task_store: Arc<TaskStore>,
    agent_id: String,
    created_by: String,
    /// Set when this tool belongs to a worker executing a task. The worker
    /// files cards *on behalf of* that task, which is what makes fan-out
    /// bounded, provenance real, and completion claims checkable.
    ///
    /// The task number is resolved from the worker at call time rather than
    /// captured at construction, so it cannot go stale and a worker that is
    /// not bound to a task simply cannot file.
    filing_worker_id: Option<crate::WorkerId>,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
    api_state: Option<Arc<crate::api::ApiState>>,
}

impl std::fmt::Debug for TaskCreateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCreateTool")
            .field("agent_id", &self.agent_id)
            .field("created_by", &self.created_by)
            .finish()
    }
}

impl TaskCreateTool {
    pub fn new(
        task_store: Arc<TaskStore>,
        agent_id: impl Into<String>,
        created_by: impl Into<String>,
    ) -> Self {
        Self {
            task_store,
            agent_id: agent_id.into(),
            created_by: created_by.into(),
            filing_worker_id: None,
            working_memory: None,
            api_state: None,
        }
    }

    /// Scope this tool to a worker filing cards for the task it is executing.
    ///
    /// This is how a worker decomposes without spawning sub-workers: it files
    /// cards the existing pickup loop schedules. Naturally bounded, observable,
    /// and crash-safe, because the scheduler already handles all three.
    pub fn for_task_worker(
        task_store: Arc<TaskStore>,
        agent_id: impl Into<String>,
        worker_id: crate::WorkerId,
    ) -> Self {
        Self {
            task_store,
            agent_id: agent_id.into(),
            // Overwritten per call once the worker's task is known.
            created_by: "worker".to_string(),
            filing_worker_id: Some(worker_id),
            working_memory: None,
            api_state: None,
        }
    }

    pub fn with_working_memory(mut self, store: Arc<crate::memory::WorkingMemoryStore>) -> Self {
        self.working_memory = Some(store);
        self
    }

    pub fn with_api_state(mut self, state: Arc<crate::api::ApiState>) -> Self {
        self.api_state = Some(state);
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("task_create failed: {0}")]
pub struct TaskCreateError(String);

impl TaskCreateTool {
    /// Which task this worker is executing.
    ///
    /// A worker not bound to a task has nothing to file on behalf of, and
    /// letting it create cards anyway would produce work with no provenance
    /// and no fan-out budget.
    async fn resolve_filing_task(
        &self,
        worker_id: crate::WorkerId,
    ) -> Result<i64, TaskCreateError> {
        self.task_store
            .task_number_for_worker(&worker_id.to_string())
            .await
            .map_err(|error| TaskCreateError(format!("{error}")))?
            .ok_or_else(|| {
                TaskCreateError(
                    "you are not executing a task, so there is nothing to file cards for"
                        .to_string(),
                )
            })
    }

    /// Refuse to file another card once this task has fanned out far enough,
    /// or once the filing chain is deep enough.
    async fn enforce_filing_limits(&self, filing_task_number: i64) -> Result<(), TaskCreateError> {
        match check_filing_limits(&self.task_store, filing_task_number).await {
            Ok(()) => Ok(()),
            Err(FilingRefusal::Storage(error)) => Err(TaskCreateError(error)),
            Err(FilingRefusal::FanOut { filed, limit }) => Err(TaskCreateError(format!(
                "task #{filing_task_number} has already filed {filed} cards, the limit is {limit}. \
                 Do the remaining work yourself, or file one card that decomposes further."
            ))),
            Err(FilingRefusal::Depth { depth, limit }) => Err(TaskCreateError(format!(
                "task #{filing_task_number} is {depth} filing hops deep and the limit is {limit}. \
                 Do this work directly rather than filing another card."
            ))),
        }
    }
}

/// Why a task may not generate more work.
///
/// Structured rather than a formatted string because the two bounds have
/// different remediations for different callers: a worker that cannot file
/// another card should do the work itself, and a worker that cannot launch
/// another workflow should run the steps itself. Same policy, different advice,
/// and the advice belongs with the tool that gives it.
#[derive(Debug, Clone)]
pub(crate) enum FilingRefusal {
    /// This task has already caused this many cards to exist.
    FanOut { filed: i64, limit: i64 },
    /// This task is this many filing hops from a human or an agent.
    Depth { depth: i64, limit: i64 },
    /// The bounds could not be read, which is not the task's fault — but a
    /// bound that cannot be checked must not be assumed satisfied.
    Storage(String),
}

/// The shared bound on work a task may generate, whether by filing a card or by
/// launching a pipeline.
///
/// Both halves are needed and neither substitutes for the other: a per-task cap
/// with unbounded depth still permits `cap^depth` tasks, and a depth bound alone
/// permits one task to generate thousands of siblings. One definition, because
/// two tools enforcing "the same" limit separately is how they stop being the
/// same limit.
pub(crate) async fn check_filing_limits(
    task_store: &TaskStore,
    filing_task_number: i64,
) -> Result<(), FilingRefusal> {
    let filer = crate::tasks::filer_id(filing_task_number);

    let filed = task_store
        .count_tasks_filed_by(&filer)
        .await
        .map_err(|error| FilingRefusal::Storage(format!("{error}")))?;
    if filed >= crate::tasks::MAX_TASKS_FILED_PER_TASK {
        return Err(FilingRefusal::FanOut {
            filed,
            limit: crate::tasks::MAX_TASKS_FILED_PER_TASK,
        });
    }

    let depth = task_store
        .filing_depth(filing_task_number)
        .await
        .map_err(|error| FilingRefusal::Storage(format!("{error}")))?;
    if depth >= crate::tasks::MAX_FILING_DEPTH {
        return Err(FilingRefusal::Depth {
            depth,
            limit: crate::tasks::MAX_FILING_DEPTH,
        });
    }

    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskCreateArgs {
    pub title: String,
    pub description: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default)]
    pub subtasks: Vec<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Project this task acts on. Scopes the task to a codebase.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Repo within the project. A project can hold several repos — set this so
    /// the task is about one of them specifically.
    #[serde(default)]
    pub repo_id: Option<String>,
    /// Worktree to execute in.
    #[serde(default)]
    pub worktree_id: Option<String>,
    /// Task numbers that must finish before this one may run.
    #[serde(default)]
    pub depends_on: Vec<i64>,
    /// Agent to assign the card to. Defaults to the filing agent.
    #[serde(default)]
    pub assigned_agent_id: Option<String>,
    /// JSON Schema the card must satisfy when it completes.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    /// Where this card's inputs come from.
    #[serde(default)]
    pub input_bindings: Vec<TaskCreateBinding>,
}

/// One input of a filed card, wired to an upstream task or a literal.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskCreateBinding {
    pub input_key: String,
    /// Upstream task to read from. Omit for a literal.
    #[serde(default)]
    pub source_task_number: Option<i64>,
    /// RFC 6901 JSON Pointer into that task's outputs, e.g. `/image/tag`.
    #[serde(default)]
    pub source_pointer: Option<String>,
    /// Literal JSON value, used when no source task is given.
    #[serde(default)]
    pub literal_value: Option<serde_json::Value>,
}

fn default_priority() -> String {
    "medium".to_string()
}

#[derive(Debug, Serialize)]
pub struct TaskCreateOutput {
    pub success: bool,
    pub task_number: i64,
    pub status: String,
    pub message: String,
}

impl Tool for TaskCreateTool {
    const NAME: &'static str = "task_create";

    type Error = TaskCreateError;
    type Args = TaskCreateArgs;
    type Output = TaskCreateOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/task_create").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short task title" },
                    "description": { "type": "string", "description": "Optional detailed description" },
                    "priority": {
                        "type": "string",
                        "enum": crate::tasks::TaskPriority::ALL.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                        "description": "Task priority"
                    },
                    "subtasks": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional checklist items"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Optional metadata object"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project this task acts on. Scopes the task to a codebase."
                    },
                    "repo_id": {
                        "type": "string",
                        "description": "Repo within the project. A project can hold several repos — set this when the task is about one of them specifically."
                    },
                    "worktree_id": {
                        "type": "string",
                        "description": "Worktree to execute in."
                    },
                    "depends_on": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Task numbers that must all finish before this task becomes eligible. The task waits in the backlog until then."
                    }
                },
                "required": ["title"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let priority = TaskPriority::parse(&args.priority)
            .ok_or_else(|| TaskCreateError(format!("invalid priority: {}", args.priority)))?;

        // A worker's cards go straight to the queue rather than sitting in
        // pending_approval: filing them *is* the decomposition, and a human
        // approving each one would make the mechanism useless. The fan-out and
        // depth caps below are what keeps that safe, not an approval gate.
        let filing_task_number = match self.filing_worker_id {
            Some(worker_id) => Some(self.resolve_filing_task(worker_id).await?),
            None => None,
        };

        let status = if filing_task_number.is_some() {
            TaskStatus::Ready
        } else {
            TaskStatus::PendingApproval
        };

        let created_by = match filing_task_number {
            Some(number) => {
                self.enforce_filing_limits(number).await?;
                crate::tasks::filer_id(number)
            }
            None => self.created_by.clone(),
        };

        let subtasks = args
            .subtasks
            .into_iter()
            .map(|title| TaskSubtask {
                title,
                completed: false,
            })
            .collect::<Vec<_>>();

        let assigned_agent_id = args
            .assigned_agent_id
            .unwrap_or_else(|| self.agent_id.clone());

        let task = self
            .task_store
            .create(CreateTaskInput {
                owner_agent_id: self.agent_id.clone(),
                assigned_agent_id,
                title: args.title,
                description: args.description,
                status,
                priority,
                subtasks,
                metadata: args.metadata.unwrap_or_else(|| serde_json::json!({})),
                source_memory_id: None,
                created_by,
                binding: crate::tasks::TaskProjectBinding {
                    project_id: args.project_id,
                    repo_id: args.repo_id,
                    worktree_id: args.worktree_id,
                },
                depends_on: args.depends_on,
            })
            .await
            .map_err(|error| TaskCreateError(format!("{error}")))?;

        // Contract and bindings are applied after the row exists. A failure
        // here leaves a task that will not resolve, which the claim path parks
        // as a dependency block with the reason — visible rather than silent.
        if let Some(output_schema) = &args.output_schema
            && let Err(error) = self
                .task_store
                .set_contract(task.task_number, None, Some(output_schema))
                .await
        {
            tracing::warn!(%error, task_number = task.task_number, "failed to set contract on filed task");
        }

        for binding in args.input_bindings {
            if let Err(error) = self
                .task_store
                .set_input_binding(&crate::tasks::TaskInputBinding {
                    child_task_number: task.task_number,
                    input_key: binding.input_key,
                    source_task_number: binding.source_task_number,
                    source_pointer: binding.source_pointer,
                    literal_value: binding.literal_value,
                    // Fan-in is a workflow-template construct: it names a step
                    // key, and a hand-filed card is not part of a run.
                    fan_in_step_key: None,
                })
                .await
            {
                tracing::warn!(%error, task_number = task.task_number, "failed to bind input on filed task");
            }
        }

        // Emit SSE event + notification so the dashboard updates in real time.
        if let Some(api_state) = &self.api_state {
            api_state
                .event_tx
                .send(crate::api::ApiEvent::TaskUpdated {
                    agent_id: task.assigned_agent_id.clone(),
                    task_number: task.task_number,
                    status: task.status.to_string(),
                    action: "created".to_string(),
                })
                .ok();
            if task.status == TaskStatus::PendingApproval {
                api_state.emit_notification(NewNotification {
                    kind: NotificationKind::TaskApproval,
                    severity: NotificationSeverity::Info,
                    title: task.title.clone(),
                    body: task.description.clone(),
                    agent_id: Some(task.assigned_agent_id.clone()),
                    related_entity_type: Some("task".to_string()),
                    related_entity_id: Some(task.task_number.to_string()),
                    action_url: Some(format!("/tasks/{}", task.task_number)),
                    metadata: None,
                });
            }
        }

        if let Some(working_memory) = &self.working_memory {
            let (event_type, summary, importance) = if task.status == TaskStatus::Done {
                (
                    crate::memory::WorkingMemoryEventType::Outcome,
                    format!("Task #{} completed: {}", task.task_number, task.title),
                    0.7,
                )
            } else {
                (
                    crate::memory::WorkingMemoryEventType::TaskUpdate,
                    format!(
                        "Task created #{}: {} (status: {})",
                        task.task_number, task.title, task.status
                    ),
                    0.5,
                )
            };
            working_memory
                .emit(event_type, summary)
                .importance(importance)
                .record();
        }

        Ok(TaskCreateOutput {
            success: true,
            task_number: task.task_number,
            status: task.status.to_string(),
            message: format!("Created task #{}: {}", task.task_number, task.title),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::memory::working::WorkingMemoryEvent;
    use crate::memory::{WorkingMemoryEventType, WorkingMemoryStore};
    use crate::tasks::store::setup_test_store;
    use chrono_tz::Tz;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::time::Duration;

    async fn wait_for_single_event(store: &WorkingMemoryStore) -> WorkingMemoryEvent {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = store
                    .get_recent_events(10, 0.0)
                    .await
                    .expect("working memory query");
                if let Some(event) = events.into_iter().next() {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for working memory event")
    }

    #[tokio::test]
    async fn task_create_emits_task_update_for_new_tasks() {
        let task_store = Arc::new(setup_test_store().await);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite connect");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        let working_memory = WorkingMemoryStore::new(pool, Tz::UTC);

        let tool = TaskCreateTool::new(task_store, "agent-test", "branch")
            .with_working_memory(working_memory.clone());

        let output = tool
            .call(TaskCreateArgs {
                title: "Ship observation MVP".to_string(),
                description: Some("land the first packet".to_string()),
                priority: "medium".to_string(),
                subtasks: Vec::new(),
                metadata: None,
                project_id: None,
                repo_id: None,
                worktree_id: None,
                depends_on: Vec::new(),
                assigned_agent_id: None,
                output_schema: None,
                input_bindings: Vec::new(),
            })
            .await
            .expect("task create should succeed");

        assert_eq!(output.status, "pending_approval");

        let event = wait_for_single_event(&working_memory).await;
        assert_eq!(event.event_type, WorkingMemoryEventType::TaskUpdate);
        assert_eq!(
            event.summary,
            "Task created #1: Ship observation MVP (status: pending_approval)"
        );
    }
    // -- Worker-filed cards -------------------------------------------------

    async fn worker_filing_fixture() -> (Arc<TaskStore>, crate::WorkerId, i64) {
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
        let parent = store
            .create(CreateTaskInput {
                owner_agent_id: "agent-1".into(),
                assigned_agent_id: "agent-1".into(),
                title: "decompose me".into(),
                status: TaskStatus::InProgress,
                created_by: "human".into(),
                ..Default::default()
            })
            .await
            .expect("create parent");

        let worker_id = uuid::Uuid::new_v4();
        store
            .update(
                parent.task_number,
                crate::tasks::UpdateTaskInput {
                    worker_id: Some(worker_id.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("bind worker")
            .expect("exists");

        (store, worker_id, parent.task_number)
    }

    fn filing_args(title: &str) -> TaskCreateArgs {
        TaskCreateArgs {
            title: title.to_string(),
            description: None,
            priority: "medium".to_string(),
            subtasks: Vec::new(),
            metadata: None,
            project_id: None,
            repo_id: None,
            worktree_id: None,
            depends_on: Vec::new(),
            assigned_agent_id: None,
            output_schema: None,
            input_bindings: Vec::new(),
        }
    }

    /// A filed card goes straight to the queue. Filing *is* the decomposition,
    /// so routing each one through approval would make the mechanism useless.
    #[tokio::test]
    async fn a_worker_files_a_card_that_is_immediately_schedulable() {
        let (store, worker_id, parent) = worker_filing_fixture().await;
        let tool = TaskCreateTool::for_task_worker(store.clone(), "agent-1", worker_id);

        let output = tool
            .call(filing_args("regenerate clients"))
            .await
            .expect("worker should be able to file a card");

        assert_eq!(output.status, "ready");
        let filed = store
            .get_by_number(output.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            filed.created_by,
            crate::tasks::filer_id(parent),
            "provenance must name the filing task, not just 'worker'"
        );
    }

    /// The runaway bound. Hermes leaves kanban fan-out unlimited and their own
    /// docs flag it; a confused model can otherwise file cards until something
    /// else breaks.
    #[tokio::test]
    async fn fan_out_is_capped_per_task() {
        let (store, worker_id, _) = worker_filing_fixture().await;
        let tool = TaskCreateTool::for_task_worker(store.clone(), "agent-1", worker_id);

        for index in 0..crate::tasks::MAX_TASKS_FILED_PER_TASK {
            tool.call(filing_args(&format!("child {index}")))
                .await
                .expect("within the cap");
        }

        let error = tool
            .call(filing_args("one too many"))
            .await
            .expect_err("the cap must hold");
        let message = error.to_string();
        assert!(
            message.contains(&crate::tasks::MAX_TASKS_FILED_PER_TASK.to_string()),
            "the refusal must name the limit so the worker can adapt: {message}"
        );
    }

    /// Fan-out and depth need separate bounds — a cap of ten with unbounded
    /// depth still permits ten to the power of the depth.
    #[tokio::test]
    async fn filing_depth_is_capped() {
        let (store, _, root) = worker_filing_fixture().await;

        // Build a chain of filed tasks as deep as the limit allows.
        let mut current = root;
        for _ in 0..crate::tasks::MAX_FILING_DEPTH {
            let child = store
                .create(CreateTaskInput {
                    owner_agent_id: "agent-1".into(),
                    assigned_agent_id: "agent-1".into(),
                    title: "chained".into(),
                    status: TaskStatus::InProgress,
                    created_by: crate::tasks::filer_id(current),
                    ..Default::default()
                })
                .await
                .expect("create chained task");
            current = child.task_number;
        }

        let deep_worker = uuid::Uuid::new_v4();
        store
            .update(
                current,
                crate::tasks::UpdateTaskInput {
                    worker_id: Some(deep_worker.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("bind")
            .expect("exists");

        let tool = TaskCreateTool::for_task_worker(store.clone(), "agent-1", deep_worker);
        let error = tool
            .call(filing_args("one hop too far"))
            .await
            .expect_err("the depth limit must hold");
        assert!(
            error.to_string().contains("hops deep"),
            "the refusal must explain why: {error}"
        );
    }

    /// A worker with no task has nothing to file on behalf of. Letting it
    /// create cards anyway would produce work with no provenance and no budget.
    #[tokio::test]
    async fn a_worker_not_executing_a_task_cannot_file() {
        let (store, _, _) = worker_filing_fixture().await;
        let tool = TaskCreateTool::for_task_worker(store, "agent-1", uuid::Uuid::new_v4());

        let error = tool
            .call(filing_args("orphan"))
            .await
            .expect_err("an unbound worker must not file");
        assert!(error.to_string().contains("not executing a task"));
    }

    /// The point of F4 meeting F5: a decomposing worker wires the pipeline it
    /// files, so the contract machinery is exercised by the running system
    /// rather than only by tests.
    #[tokio::test]
    async fn a_filed_card_can_carry_a_contract_and_bindings() {
        let (store, worker_id, parent) = worker_filing_fixture().await;
        let tool = TaskCreateTool::for_task_worker(store.clone(), "agent-1", worker_id);

        let mut args = filing_args("deploy the tag");
        args.output_schema = Some(serde_json::json!({
            "type": "object",
            "required": ["deployment_url"],
            "properties": {"deployment_url": {"type": "string"}},
        }));
        args.input_bindings = vec![TaskCreateBinding {
            input_key: "tag".into(),
            source_task_number: Some(parent),
            source_pointer: Some("/image/tag".into()),
            literal_value: None,
        }];

        let output = tool.call(args).await.expect("file with a contract");

        let filed = store
            .get_by_number(output.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert!(filed.output_schema.is_some());

        let bindings = store
            .list_input_bindings(output.task_number)
            .await
            .expect("list bindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].source_task_number, Some(parent));
        assert_eq!(bindings[0].source_pointer.as_deref(), Some("/image/tag"));
    }

    #[tokio::test]
    async fn a_filed_card_can_be_assigned_to_another_agent() {
        let (store, worker_id, _) = worker_filing_fixture().await;
        let tool = TaskCreateTool::for_task_worker(store.clone(), "agent-1", worker_id);

        let mut args = filing_args("regenerate the web client");
        args.assigned_agent_id = Some("agent-web".into());
        let output = tool.call(args).await.expect("file cross-agent");

        let filed = store
            .get_by_number(output.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(filed.assigned_agent_id, "agent-web");
        assert_eq!(
            filed.owner_agent_id, "agent-1",
            "the filing agent stays the owner so provenance survives reassignment"
        );
    }
}
