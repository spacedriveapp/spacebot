//! Append a durable finding to a task.
//!
//! Comments are the enrichment loop's output: what was investigated, what was
//! found, what was decided. They are append-only and never touch the task
//! description, which stays the stable statement of the work itself.
//!
//! The tool is also where autonomous ownership is settled. When the agent
//! comments on an unassigned task and `claim_unowned` is on, the claim happens
//! first and atomically — one winner, a clean skip for everyone else.

use crate::agent::autonomy::EnrichmentBudget;
use crate::tasks::{
    CreateTaskCommentInput, TaskClaim, TaskCommentAuthor, TaskStore, normalize_comment_body,
};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct AddTaskCommentTool {
    task_store: Arc<TaskStore>,
    agent_id: String,
    author_type: TaskCommentAuthor,
    author_id: Option<String>,
    /// Worker id stamped on every comment this tool writes. Set for the worker
    /// toolset so a worker's findings link back to its own run without the
    /// model having to know its id.
    bound_worker_id: Option<String>,
    claim_unowned: bool,
    budget: Option<EnrichmentBudget>,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
    api_state: Option<Arc<crate::api::ApiState>>,
}

impl std::fmt::Debug for AddTaskCommentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddTaskCommentTool")
            .field("agent_id", &self.agent_id)
            .field("author_type", &self.author_type)
            .field("claim_unowned", &self.claim_unowned)
            .finish()
    }
}

impl AddTaskCommentTool {
    /// Comments authored by an agent process — the autonomy channel or a branch.
    pub fn for_agent(task_store: Arc<TaskStore>, agent_id: impl Into<String>) -> Self {
        let agent_id = agent_id.into();
        Self {
            task_store,
            author_id: Some(agent_id.clone()),
            agent_id,
            author_type: TaskCommentAuthor::Agent,
            bound_worker_id: None,
            claim_unowned: false,
            budget: None,
            working_memory: None,
            api_state: None,
        }
    }

    /// Comments authored by a worker, attributed to its own run.
    pub fn for_worker(
        task_store: Arc<TaskStore>,
        agent_id: impl Into<String>,
        worker_id: crate::WorkerId,
    ) -> Self {
        let worker_id = worker_id.to_string();
        Self {
            author_type: TaskCommentAuthor::Worker,
            author_id: Some(worker_id.clone()),
            bound_worker_id: Some(worker_id),
            ..Self::for_agent(task_store, agent_id)
        }
    }

    /// Whether commenting on an unassigned task claims it for this agent.
    pub fn with_claim_unowned(mut self, claim_unowned: bool) -> Self {
        self.claim_unowned = claim_unowned;
        self
    }

    /// Cap on how many distinct tasks this run may enrich.
    pub fn with_budget(mut self, budget: EnrichmentBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    pub fn with_working_memory(mut self, store: Arc<crate::memory::WorkingMemoryStore>) -> Self {
        self.working_memory = Some(store);
        self
    }

    pub fn with_api_state(mut self, api_state: Arc<crate::api::ApiState>) -> Self {
        self.api_state = Some(api_state);
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("add_task_comment failed: {0}")]
pub struct AddTaskCommentError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddTaskCommentArgs {
    /// Task number reference (#N).
    pub task_number: i64,
    /// The synthesised finding — a few lines, not a transcript.
    pub body: String,
    /// Worker run this comment summarises, when it summarises one.
    #[serde(default)]
    pub worker_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AddTaskCommentOutput {
    pub success: bool,
    pub comment_id: String,
    pub task_number: i64,
    /// Set when this call took ownership of a previously unassigned task.
    pub claimed: bool,
    pub message: String,
}

impl Tool for AddTaskCommentTool {
    const NAME: &'static str = "add_task_comment";

    type Error = AddTaskCommentError;
    type Args = AddTaskCommentArgs;
    type Output = AddTaskCommentOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut properties = serde_json::json!({
            "task_number": { "type": "integer", "description": "Task number reference (#N)" },
            "body": {
                "type": "string",
                "description": format!(
                    "The synthesised finding, 2-5 lines. Maximum {} characters.",
                    crate::tasks::MAX_COMMENT_BODY_BYTES
                ),
            },
        });
        // A worker's comments are attributed to its own run automatically, so
        // the field would only ever let it misattribute.
        if self.bound_worker_id.is_none() {
            properties["worker_id"] = serde_json::json!({
                "type": "string",
                "description": "Worker whose output this comment summarises, when applicable",
            });
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/add_task_comment").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["task_number", "body"],
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let body = normalize_comment_body(&args.body).map_err(AddTaskCommentError)?;
        let task_number = args.task_number;

        let task = self
            .task_store
            .get_by_number(task_number)
            .await
            .map_err(|error| AddTaskCommentError(error.to_string()))?
            .ok_or_else(|| AddTaskCommentError(format!("task #{task_number} not found")))?;

        // Assignment is the ownership boundary: another agent's task is out of
        // reach regardless of who owns the task record.
        if let Some(assigned) = task.assigned_agent_id.as_deref()
            && assigned != self.agent_id
        {
            return Err(AddTaskCommentError(format!(
                "task #{task_number} is assigned to {assigned} — skip it"
            )));
        }

        let mut claimed = false;
        if task.assigned_agent_id.is_none() && self.claim_unowned {
            match self
                .task_store
                .claim_for_agent(task_number, &self.agent_id)
                .await
                .map_err(|error| AddTaskCommentError(error.to_string()))?
            {
                TaskClaim::Claimed(_) => claimed = true,
                TaskClaim::AlreadyOwned(_) => {}
                TaskClaim::OwnedByOther { agent_id } => {
                    return Err(AddTaskCommentError(format!(
                        "task #{task_number} was claimed by {agent_id} first — skip it"
                    )));
                }
                TaskClaim::NotFound => {
                    return Err(AddTaskCommentError(format!(
                        "task #{task_number} not found"
                    )));
                }
            }
        }

        // The run's task allowance is spent on the task, not per comment, so a
        // run can keep commenting on work it has already started.
        if let Some(budget) = &self.budget
            && !budget.try_touch(task_number)
        {
            return Err(AddTaskCommentError(format!(
                "this run's allowance of {} task{} is spent — record what you have and call autonomy_complete",
                budget.max_tasks(),
                if budget.max_tasks() == 1 { "" } else { "s" }
            )));
        }

        let worker_id = self.bound_worker_id.clone().or(args.worker_id);
        let comment = self
            .task_store
            .add_comment(CreateTaskCommentInput {
                task_number,
                author_type: self.author_type,
                author_id: self.author_id.clone(),
                body,
                worker_id,
                metadata: serde_json::json!({}),
                // Agent and worker comments are enrichment; the cadence clock
                // moves with them so the next run picks up somewhere new.
                mark_enriched: true,
            })
            .await
            .map_err(|error| AddTaskCommentError(error.to_string()))?;

        if let Some(api_state) = &self.api_state {
            crate::api::tasks::fan_out_task_comment(api_state, &task, self.author_type).await;
        }

        if let Some(working_memory) = &self.working_memory {
            working_memory
                .emit(
                    crate::memory::WorkingMemoryEventType::TaskUpdate,
                    format!(
                        "Task #{} enriched: {}",
                        task_number,
                        crate::summarize_first_non_empty_line(&comment.body, 160)
                    ),
                )
                .importance(0.5)
                .record();
        }

        let message = if claimed {
            format!("Claimed and commented on task #{task_number}")
        } else {
            format!("Commented on task #{task_number}")
        };

        Ok(AddTaskCommentOutput {
            success: true,
            comment_id: comment.id,
            task_number,
            claimed,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::store::{CreateTaskInput, TaskPriority, TaskStatus, setup_test_store};

    fn task_input(title: &str, assigned: Option<&str>) -> CreateTaskInput {
        CreateTaskInput {
            owner_agent_id: "agent-a".to_string(),
            assigned_agent_id: assigned.map(str::to_string),
            title: title.to_string(),
            description: None,
            status: TaskStatus::PendingApproval,
            priority: TaskPriority::Medium,
            subtasks: Vec::new(),
            metadata: serde_json::json!({}),
            goal_id: None,
            source_memory_id: None,
            created_by: "autonomy".to_string(),
        }
    }

    fn args(task_number: i64, body: &str) -> AddTaskCommentArgs {
        AddTaskCommentArgs {
            task_number,
            body: body.to_string(),
            worker_id: None,
        }
    }

    #[tokio::test]
    async fn comment_claims_an_unowned_task_once() {
        let store = Arc::new(setup_test_store().await);
        let task = store
            .create(task_input("unowned", None))
            .await
            .expect("create");
        let tool = AddTaskCommentTool::for_agent(store.clone(), "agent-a").with_claim_unowned(true);

        let first = tool
            .call(args(task.task_number, "Investigated the retry path."))
            .await
            .expect("comment should succeed");
        assert!(first.claimed);

        let second = tool
            .call(args(task.task_number, "Follow-up: the backoff is capped."))
            .await
            .expect("comment should succeed");
        assert!(!second.claimed, "ownership is taken once, not per comment");

        let loaded = store
            .get_by_number(task.task_number)
            .await
            .expect("load")
            .expect("task exists");
        assert_eq!(loaded.assigned_agent_id.as_deref(), Some("agent-a"));
        assert!(loaded.last_enriched_at.is_some());
    }

    #[tokio::test]
    async fn comment_leaves_unowned_tasks_unclaimed_when_disabled() {
        let store = Arc::new(setup_test_store().await);
        let task = store
            .create(task_input("unowned", None))
            .await
            .expect("create");
        let tool = AddTaskCommentTool::for_agent(store.clone(), "agent-a");

        let output = tool
            .call(args(task.task_number, "Read the issue thread."))
            .await
            .expect("comment should succeed");
        assert!(!output.claimed);

        let loaded = store
            .get_by_number(task.task_number)
            .await
            .expect("load")
            .expect("task exists");
        assert!(loaded.assigned_agent_id.is_none());
    }

    #[tokio::test]
    async fn comment_rejects_another_agents_task() {
        let store = Arc::new(setup_test_store().await);
        let task = store
            .create(task_input("theirs", Some("agent-b")))
            .await
            .expect("create");
        let tool = AddTaskCommentTool::for_agent(store.clone(), "agent-a").with_claim_unowned(true);

        let error = tool
            .call(args(
                task.task_number,
                "Trying to touch someone else's work.",
            ))
            .await
            .expect_err("wrong-agent access must fail");
        assert!(error.to_string().contains("assigned to agent-b"));
        assert_eq!(
            store.count_comments(task.task_number).await.expect("count"),
            0
        );
    }

    #[tokio::test]
    async fn concurrent_first_comments_leave_one_winner() {
        let store = Arc::new(setup_test_store().await);
        let task = store
            .create(task_input("contested", None))
            .await
            .expect("create");
        let number = task.task_number;

        let left = AddTaskCommentTool::for_agent(store.clone(), "agent-a").with_claim_unowned(true);
        let right =
            AddTaskCommentTool::for_agent(store.clone(), "agent-b").with_claim_unowned(true);
        let (first, second) = tokio::join!(
            tokio::spawn(async move { left.call(args(number, "Agent A investigated.")).await }),
            tokio::spawn(async move { right.call(args(number, "Agent B investigated.")).await }),
        );

        let results = [first.expect("join"), second.expect("join")];
        let winners = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(winners, 1, "exactly one agent may take the task");
        let skip = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("the loser reports a skip");
        assert!(skip.to_string().contains("claimed by"));
        assert_eq!(store.count_comments(number).await.expect("count"), 1);
    }

    #[tokio::test]
    async fn budget_caps_distinct_tasks_per_run() {
        let store = Arc::new(setup_test_store().await);
        let first = store
            .create(task_input("first", Some("agent-a")))
            .await
            .expect("create");
        let second = store
            .create(task_input("second", Some("agent-a")))
            .await
            .expect("create");
        let third = store
            .create(task_input("third", Some("agent-a")))
            .await
            .expect("create");

        let tool = AddTaskCommentTool::for_agent(store.clone(), "agent-a")
            .with_budget(EnrichmentBudget::new(2));

        tool.call(args(first.task_number, "First finding."))
            .await
            .expect("first task is within budget");
        tool.call(args(second.task_number, "Second finding."))
            .await
            .expect("second task is within budget");
        tool.call(args(first.task_number, "More on the first task."))
            .await
            .expect("a task already worked stays in budget");

        let error = tool
            .call(args(third.task_number, "One task too many."))
            .await
            .expect_err("the third distinct task must be refused");
        assert!(error.to_string().contains("allowance"));
    }

    #[tokio::test]
    async fn worker_comments_are_attributed_to_their_run() {
        let store = Arc::new(setup_test_store().await);
        let task = store
            .create(task_input("worker owned", Some("agent-a")))
            .await
            .expect("create");
        let worker_id = crate::WorkerId::new_v4();
        let tool = AddTaskCommentTool::for_worker(store.clone(), "agent-a", worker_id);

        tool.call(AddTaskCommentArgs {
            task_number: task.task_number,
            body: "Reproduced the failure on the second attempt.".to_string(),
            // A worker cannot misattribute: the bound id wins.
            worker_id: Some("some-other-worker".to_string()),
        })
        .await
        .expect("worker comment should succeed");

        let comments = store
            .list_comments(task.task_number, 10, None)
            .await
            .expect("list");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author_type, TaskCommentAuthor::Worker);
        assert_eq!(
            comments[0].worker_id.as_deref(),
            Some(worker_id.to_string()).as_deref()
        );
    }
}
