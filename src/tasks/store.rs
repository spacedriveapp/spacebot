//! Global task CRUD storage (SQLite).
//!
//! Operates against the instance-level `tasks.db` database with globally
//! unique task numbers and explicit owner/assigned agent relationships.

use crate::error::Result;

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    PendingApproval,
    Backlog,
    Ready,
    InProgress,
    /// Parked and not eligible for pickup. Today this is only reached by
    /// exhausting the failure budget; `block_kind` in a later change will
    /// distinguish dependency waits from human gates.
    Blocked,
    Done,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 6] = [
        TaskStatus::PendingApproval,
        TaskStatus::Backlog,
        TaskStatus::Ready,
        TaskStatus::InProgress,
        TaskStatus::Blocked,
        TaskStatus::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::PendingApproval => "pending_approval",
            TaskStatus::Backlog => "backlog",
            TaskStatus::Ready => "ready",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending_approval" => Some(TaskStatus::PendingApproval),
            "backlog" => Some(TaskStatus::Backlog),
            "ready" => Some(TaskStatus::Ready),
            "in_progress" => Some(TaskStatus::InProgress),
            "blocked" => Some(TaskStatus::Blocked),
            "done" => Some(TaskStatus::Done),
            _ => None,
        }
    }

    /// Whether a task in this status is eligible for the pickup loop.
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done)
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Critical,
    High,
    Medium,
    Low,
}

impl TaskPriority {
    pub const ALL: [TaskPriority; 4] = [
        TaskPriority::Critical,
        TaskPriority::High,
        TaskPriority::Medium,
        TaskPriority::Low,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskPriority::Critical => "critical",
            TaskPriority::High => "high",
            TaskPriority::Medium => "medium",
            TaskPriority::Low => "low",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "critical" => Some(TaskPriority::Critical),
            "high" => Some(TaskPriority::High),
            "medium" => Some(TaskPriority::Medium),
            "low" => Some(TaskPriority::Low),
            _ => None,
        }
    }
}

impl std::fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct TaskSubtask {
    pub title: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Task {
    pub id: String,
    pub task_number: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub owner_agent_id: String,
    pub assigned_agent_id: String,
    pub subtasks: Vec<TaskSubtask>,
    pub metadata: Value,
    pub source_memory_id: Option<String>,
    pub worker_id: Option<String>,
    pub created_by: String,
    pub approved_at: Option<String>,
    pub approved_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    /// Failures since the last success. Reset to 0 on completion and on an
    /// operator-initiated retry.
    pub consecutive_failures: i64,
    /// Per-task override of [`DEFAULT_FAILURE_LIMIT`].
    pub max_retries: Option<i64>,
    /// Text of the most recent failure, kept on the task so the board can
    /// show why it is parked without joining `task_runs`.
    pub last_error: Option<String>,
    /// Project this task acts on, if any.
    pub project_id: Option<String>,
    /// Specific repo within the project. A project holds many repos, so this is
    /// what makes a task about `api-gateway` distinguishable from one about
    /// `web` in the same project.
    pub repo_id: Option<String>,
    /// Worktree to execute in. When set, the worker's working directory is
    /// resolved from it rather than from the repo or project root.
    pub worktree_id: Option<String>,
}

/// The codebase a task acts on. Every field is optional and independently
/// meaningful: a project alone scopes the task, a repo narrows it, a worktree
/// pins the exact checkout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskProjectBinding {
    pub project_id: Option<String>,
    pub repo_id: Option<String>,
    pub worktree_id: Option<String>,
}

impl TaskProjectBinding {
    pub fn is_empty(&self) -> bool {
        self.project_id.is_none() && self.repo_id.is_none() && self.worktree_id.is_none()
    }
}

/// How many consecutive failures a task may accumulate before it is parked in
/// [`TaskStatus::Blocked`] instead of being requeued.
pub const DEFAULT_FAILURE_LIMIT: i64 = 2;

/// Outcome of a single task execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunOutcome {
    Completed,
    Failed,
    Timeout,
    Cancelled,
    Blocked,
    /// Provider rate limit. Deliberately does **not** count against the
    /// failure budget — a quota outage is not the task's fault.
    RateLimited,
}

impl TaskRunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskRunOutcome::Completed => "completed",
            TaskRunOutcome::Failed => "failed",
            TaskRunOutcome::Timeout => "timeout",
            TaskRunOutcome::Cancelled => "cancelled",
            TaskRunOutcome::Blocked => "blocked",
            TaskRunOutcome::RateLimited => "rate_limited",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(TaskRunOutcome::Completed),
            "failed" => Some(TaskRunOutcome::Failed),
            "timeout" => Some(TaskRunOutcome::Timeout),
            "cancelled" => Some(TaskRunOutcome::Cancelled),
            "blocked" => Some(TaskRunOutcome::Blocked),
            "rate_limited" => Some(TaskRunOutcome::RateLimited),
            _ => None,
        }
    }

    /// Whether this outcome should increment `consecutive_failures`.
    ///
    /// `RateLimited` is excluded on purpose: a long provider quota outage must
    /// not trip the circuit breaker on otherwise-healthy tasks.
    pub fn counts_as_failure(self) -> bool {
        matches!(
            self,
            TaskRunOutcome::Failed | TaskRunOutcome::Timeout | TaskRunOutcome::Blocked
        )
    }
}

impl std::fmt::Display for TaskRunOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A single execution attempt against a task.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskRun {
    pub id: String,
    pub task_number: i64,
    pub attempt: i64,
    pub worker_id: Option<String>,
    pub outcome: Option<TaskRunOutcome>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateTaskInput {
    pub owner_agent_id: String,
    pub assigned_agent_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub subtasks: Vec<TaskSubtask>,
    pub metadata: Value,
    pub source_memory_id: Option<String>,
    pub created_by: String,
    /// Codebase this task acts on. Empty for tasks that aren't about code.
    pub binding: TaskProjectBinding,
}

/// Defaults exist so callers can use `..Default::default()` and stay source
/// compatible as fields are added. Every field that must be set for the task to
/// make sense (agents, title) defaults to empty and is expected to be provided.
impl Default for CreateTaskInput {
    fn default() -> Self {
        Self {
            owner_agent_id: String::new(),
            assigned_agent_id: String::new(),
            title: String::new(),
            description: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            subtasks: Vec::new(),
            metadata: Value::Object(serde_json::Map::new()),
            source_memory_id: None,
            created_by: String::new(),
            binding: TaskProjectBinding::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UpdateTaskInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<TaskPriority>,
    pub subtasks: Option<Vec<TaskSubtask>>,
    pub metadata: Option<Value>,
    pub worker_id: Option<String>,
    pub clear_worker_id: bool,
    pub approved_by: Option<String>,
    pub complete_subtask: Option<usize>,
    /// Reassign the task to a different agent.
    pub assigned_agent_id: Option<String>,
    /// Rebind the task to a different codebase. `None` leaves each field as-is;
    /// use `clear_binding` to unset.
    pub binding: Option<TaskProjectBinding>,
    /// Clear all three binding columns.
    pub clear_binding: bool,
}

#[derive(Debug, Clone)]
pub struct TaskUpdateResult {
    pub previous_status: TaskStatus,
    pub task: Task,
}

#[derive(Debug, Clone)]
pub enum WorkerTaskUpdateResult {
    Updated(Box<TaskUpdateResult>),
    NotAssigned,
    WrongTask { assigned_task_number: i64 },
}

/// Filters for listing tasks from the global store.
#[derive(Debug, Clone, Default)]
pub struct TaskListFilter {
    /// Convenience: matches tasks where `owner_agent_id` OR `assigned_agent_id`
    /// equals this value. Mutually exclusive with the individual fields below.
    pub agent_id: Option<String>,
    pub owner_agent_id: Option<String>,
    pub assigned_agent_id: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<TaskPriority>,
    pub created_by: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct TaskStore {
    pool: SqlitePool,
}

impl TaskStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Maximum number of retries when a concurrent create races on the
    /// `task_number` UNIQUE constraint.
    const MAX_CREATE_RETRIES: usize = 3;

    pub async fn create(&self, input: CreateTaskInput) -> Result<Task> {
        let subtasks_json =
            serde_json::to_string(&input.subtasks).context("failed to serialize subtasks")?;
        let metadata_json = input.metadata.to_string();

        for attempt in 0..Self::MAX_CREATE_RETRIES {
            let mut tx = self
                .pool
                .begin()
                .await
                .context("failed to open task create transaction")?;

            // Atomically allocate the next task number from the high-water-mark
            // sequence. This avoids number reuse after hard deletes.
            let task_number: i64 = sqlx::query_scalar(
                "UPDATE task_number_seq SET next_number = next_number + 1 \
                 WHERE id = 1 RETURNING next_number - 1",
            )
            .fetch_one(&mut *tx)
            .await
            .context("failed to allocate next task number")?;

            let task_id = uuid::Uuid::new_v4().to_string();

            let insert_result = sqlx::query(
                r#"
                INSERT INTO tasks (
                    id, task_number, title, description, status, priority,
                    owner_agent_id, assigned_agent_id,
                    subtasks, metadata, source_memory_id, created_by,
                    project_id, repo_id, worktree_id
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&task_id)
            .bind(task_number)
            .bind(&input.title)
            .bind(&input.description)
            .bind(input.status.as_str())
            .bind(input.priority.as_str())
            .bind(&input.owner_agent_id)
            .bind(&input.assigned_agent_id)
            .bind(&subtasks_json)
            .bind(&metadata_json)
            .bind(&input.source_memory_id)
            .bind(&input.created_by)
            .bind(&input.binding.project_id)
            .bind(&input.binding.repo_id)
            .bind(&input.binding.worktree_id)
            .execute(&mut *tx)
            .await;

            match insert_result {
                Ok(_) => {
                    tx.commit()
                        .await
                        .context("failed to commit task create transaction")?;

                    return self
                        .get_by_number(task_number)
                        .await?
                        .context("task inserted but not found")
                        .map_err(Into::into);
                }
                Err(sqlx::Error::Database(ref db_error))
                    if db_error.code().as_deref() == Some("2067") =>
                {
                    // UNIQUE constraint violation — another concurrent create won the
                    // race for this task_number. Roll back and retry.
                    tracing::debug!(attempt, task_number, "task_number collision, retrying");
                    // tx is dropped here which rolls back automatically.
                    continue;
                }
                Err(error) => {
                    return Err(anyhow::anyhow!("failed to insert task: {error}").into());
                }
            }
        }

        Err(anyhow::anyhow!(
            "failed to create task after {} retries due to concurrent task_number collisions",
            Self::MAX_CREATE_RETRIES
        )
        .into())
    }

    /// List tasks with optional filters. Uses the global store — no agent_id
    /// is required, but callers can filter by owner or assigned agent.
    pub async fn list(&self, filter: TaskListFilter) -> Result<Vec<Task>> {
        let mut query = String::from(SELECT_COLUMNS);
        query.push_str(" FROM tasks WHERE 1=1");

        if filter.agent_id.is_some() {
            query.push_str(" AND (owner_agent_id = ? OR assigned_agent_id = ?)");
        }
        if filter.owner_agent_id.is_some() {
            query.push_str(" AND owner_agent_id = ?");
        }
        if filter.assigned_agent_id.is_some() {
            query.push_str(" AND assigned_agent_id = ?");
        }
        if filter.status.is_some() {
            query.push_str(" AND status = ?");
        }
        if filter.priority.is_some() {
            query.push_str(" AND priority = ?");
        }
        if filter.created_by.is_some() {
            query.push_str(" AND created_by = ?");
        }
        query.push_str(" ORDER BY task_number DESC LIMIT ?");

        let mut sql = sqlx::query(&query);
        if let Some(ref agent) = filter.agent_id {
            sql = sql.bind(agent).bind(agent);
        }
        if let Some(ref owner) = filter.owner_agent_id {
            sql = sql.bind(owner);
        }
        if let Some(ref assigned) = filter.assigned_agent_id {
            sql = sql.bind(assigned);
        }
        if let Some(status) = filter.status {
            sql = sql.bind(status.as_str());
        }
        if let Some(priority) = filter.priority {
            sql = sql.bind(priority.as_str());
        }
        if let Some(ref created_by) = filter.created_by {
            sql = sql.bind(created_by);
        }
        sql = sql.bind(filter.limit.unwrap_or(100).clamp(1, 500));

        let rows = sql
            .fetch_all(&self.pool)
            .await
            .context("failed to list tasks")?;

        rows.into_iter().map(task_from_row).collect()
    }

    /// List ready tasks assigned to the given agent.
    pub async fn list_ready(&self, assigned_agent_id: &str, limit: i64) -> Result<Vec<Task>> {
        self.list(TaskListFilter {
            assigned_agent_id: Some(assigned_agent_id.to_string()),
            status: Some(TaskStatus::Ready),
            limit: Some(limit),
            ..Default::default()
        })
        .await
    }

    /// Fetch a single task by its globally unique number.
    pub async fn get_by_number(&self, task_number: i64) -> Result<Option<Task>> {
        let row = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE task_number = ?"
        ))
        .bind(task_number)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch task by number")?;

        row.map(task_from_row).transpose()
    }

    pub async fn update(&self, task_number: i64, input: UpdateTaskInput) -> Result<Option<Task>> {
        Ok(self
            .update_with_status_transition(task_number, input)
            .await?
            .map(|result| result.task))
    }

    pub async fn update_with_status_transition(
        &self,
        task_number: i64,
        input: UpdateTaskInput,
    ) -> Result<Option<TaskUpdateResult>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open task update transaction")?;

        let row = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE task_number = ?"
        ))
        .bind(task_number)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to fetch task by number for update")?;

        let Some(row) = row else {
            tx.commit()
                .await
                .context("failed to commit empty task update transaction")?;
            return Ok(None);
        };

        let current = task_from_row(row)?;
        let previous_status = current.status;
        let task = Self::update_current_in_tx(&mut tx, task_number, current, input).await?;

        tx.commit()
            .await
            .context("failed to commit task update transaction")?;

        Ok(Some(TaskUpdateResult {
            previous_status,
            task,
        }))
    }

    pub async fn update_worker_task(
        &self,
        worker_id: &str,
        task_number: i64,
        input: UpdateTaskInput,
    ) -> Result<WorkerTaskUpdateResult> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open worker task update transaction")?;

        let exact_row = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE worker_id = ? AND task_number = ?"
        ))
        .bind(worker_id)
        .bind(task_number)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to fetch worker task by id and number for update")?;

        let Some(row) = exact_row else {
            let assigned_task_number = sqlx::query_scalar::<_, i64>(
                "SELECT task_number FROM tasks WHERE worker_id = ? ORDER BY task_number DESC LIMIT 1",
            )
            .bind(worker_id)
            .fetch_optional(&mut *tx)
            .await
            .context("failed to fetch any task by worker id for update")?;

            tx.commit()
                .await
                .context("failed to commit unmatched worker task update transaction")?;
            if let Some(assigned_task_number) = assigned_task_number {
                return Ok(WorkerTaskUpdateResult::WrongTask {
                    assigned_task_number,
                });
            }
            return Ok(WorkerTaskUpdateResult::NotAssigned);
        };

        let current = task_from_row(row)?;
        let previous_status = current.status;
        let task = Self::update_current_in_tx(&mut tx, task_number, current, input).await?;

        tx.commit()
            .await
            .context("failed to commit worker task update transaction")?;

        Ok(WorkerTaskUpdateResult::Updated(Box::new(
            TaskUpdateResult {
                previous_status,
                task,
            },
        )))
    }

    async fn update_current_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_number: i64,
        current: Task,
        input: UpdateTaskInput,
    ) -> Result<Task> {
        if let Some(next_status) = input.status
            && !can_transition(current.status, next_status)
        {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "invalid task status transition: {} -> {}",
                current.status,
                next_status
            )));
        }

        let mut subtasks = input.subtasks.unwrap_or(current.subtasks);
        if let Some(index) = input.complete_subtask
            && let Some(subtask) = subtasks.get_mut(index)
        {
            subtask.completed = true;
        }

        let next_status = input.status.unwrap_or(current.status);
        let next_priority = input.priority.unwrap_or(current.priority);
        let next_metadata = merge_json_object(current.metadata, input.metadata);
        let next_assigned = input
            .assigned_agent_id
            .unwrap_or(current.assigned_agent_id.clone());
        let reassigned = next_assigned != current.assigned_agent_id;

        // If the task is being reassigned to a different agent, clear the worker
        // binding so the old worker cannot keep updating it.
        let clear_worker = input.clear_worker_id || (reassigned && current.worker_id.is_some());
        let next_worker_id = if clear_worker {
            None
        } else if let Some(worker_id) = input.worker_id {
            Some(worker_id)
        } else {
            current.worker_id
        };

        let approved_at = if current.approved_at.is_none() && next_status == TaskStatus::Ready {
            Some("SET")
        } else {
            None
        };

        let completed_at = if next_status == TaskStatus::Done {
            Some("SET")
        } else if current.completed_at.is_some() && next_status != TaskStatus::Done {
            Some("NULL")
        } else {
            None
        };

        let mut query = String::from(
            "UPDATE tasks SET title = ?, description = ?, status = ?, priority = ?, \
             assigned_agent_id = ?, subtasks = ?, metadata = ?, ",
        );

        if clear_worker {
            query.push_str("worker_id = NULL, ");
        } else {
            query.push_str("worker_id = ?, ");
        }

        // Binding: clear wins over set, and an absent binding leaves the
        // existing columns untouched rather than nulling them.
        let next_binding = if input.clear_binding {
            Some(TaskProjectBinding::default())
        } else {
            input.binding.clone()
        };
        if next_binding.is_some() {
            query.push_str("project_id = ?, repo_id = ?, worktree_id = ?, ");
        }

        query.push_str(
            "approved_by = COALESCE(?, approved_by), \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        );

        if approved_at.is_some() {
            query.push_str(", approved_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')");
        }
        if let Some(value) = completed_at {
            if value == "SET" {
                query.push_str(", completed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')");
            } else {
                query.push_str(", completed_at = NULL");
            }
        }

        query.push_str(" WHERE task_number = ?");

        let mut sql = sqlx::query(&query)
            .bind(input.title.unwrap_or(current.title))
            .bind(input.description.or(current.description))
            .bind(next_status.as_str())
            .bind(next_priority.as_str())
            .bind(&next_assigned)
            .bind(serde_json::to_string(&subtasks).context("failed to serialize subtasks")?)
            .bind(next_metadata.to_string());

        if !clear_worker {
            sql = sql.bind(next_worker_id);
        }

        if let Some(binding) = &next_binding {
            sql = sql
                .bind(binding.project_id.clone())
                .bind(binding.repo_id.clone())
                .bind(binding.worktree_id.clone());
        }

        sql.bind(input.approved_by)
            .bind(task_number)
            .execute(&mut **tx)
            .await
            .context("failed to update task")?;

        let updated = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE task_number = ?"
        ))
        .bind(task_number)
        .fetch_one(&mut **tx)
        .await
        .context("failed to fetch updated task")?;

        task_from_row(updated)
    }

    pub async fn delete(&self, task_number: i64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM tasks WHERE task_number = ?")
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to delete task")?;

        Ok(result.rows_affected() > 0)
    }

    /// Atomically claim the highest-priority ready task assigned to the given
    /// agent. Moves it to `in_progress` and returns it.
    pub async fn claim_next_ready(&self, assigned_agent_id: &str) -> Result<Option<Task>> {
        let row = sqlx::query(
            "SELECT task_number FROM tasks WHERE assigned_agent_id = ? AND status = 'ready' \
             ORDER BY CASE priority \
               WHEN 'critical' THEN 0 \
               WHEN 'high' THEN 1 \
               WHEN 'medium' THEN 2 \
               WHEN 'low' THEN 3 \
               ELSE 4 END ASC, \
             task_number ASC \
             LIMIT 1",
        )
        .bind(assigned_agent_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to find ready task")?;

        let Some(row) = row else {
            return Ok(None);
        };

        let task_number: i64 = row
            .try_get("task_number")
            .context("failed to read task_number from ready task row")?;
        let result = sqlx::query(
            "UPDATE tasks SET status = 'in_progress', \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ? AND status = 'ready'",
        )
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to claim ready task")?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }

        self.get_by_number(task_number).await
    }

    pub async fn get_by_worker_id(&self, worker_id: &str) -> Result<Option<Task>> {
        let row = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE worker_id = ? ORDER BY updated_at DESC LIMIT 1"
        ))
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch task by worker id")?;

        row.map(task_from_row).transpose()
    }

    // -- Attempt log --------------------------------------------------------

    /// Open a new attempt row for a task. The attempt number is one past the
    /// highest existing attempt, allocated inside the transaction so two
    /// concurrent starts cannot collide on the unique index.
    pub async fn start_run(&self, task_number: i64, worker_id: Option<&str>) -> Result<TaskRun> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open task run transaction")?;

        let next_attempt: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(attempt), 0) + 1 FROM task_runs WHERE task_number = ?",
        )
        .bind(task_number)
        .fetch_one(&mut *tx)
        .await
        .context("failed to allocate next task run attempt")?;

        let run_id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO task_runs (id, task_number, attempt, worker_id) VALUES (?, ?, ?, ?)",
        )
        .bind(&run_id)
        .bind(task_number)
        .bind(next_attempt)
        .bind(worker_id)
        .execute(&mut *tx)
        .await
        .context("failed to insert task run")?;

        let row = sqlx::query(&format!("{RUN_SELECT_COLUMNS} FROM task_runs WHERE id = ?"))
            .bind(&run_id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to read back inserted task run")?;

        tx.commit()
            .await
            .context("failed to commit task run transaction")?;

        task_run_from_row(row)
    }

    /// Close an attempt row with its outcome. Idempotent — closing an already
    /// closed run overwrites the outcome rather than erroring, so a duplicate
    /// completion path can't fail the caller.
    pub async fn finish_run(
        &self,
        run_id: &str,
        outcome: TaskRunOutcome,
        summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE task_runs SET outcome = ?, summary = ?, error = ?, \
             ended_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
        )
        .bind(outcome.as_str())
        .bind(summary)
        .bind(error)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .context("failed to finish task run")?;

        Ok(())
    }

    /// Attach a worker to an already-open run row. Used when the run is opened
    /// before the worker id is known.
    pub async fn set_run_worker(&self, run_id: &str, worker_id: &str) -> Result<()> {
        sqlx::query("UPDATE task_runs SET worker_id = ? WHERE id = ?")
            .bind(worker_id)
            .bind(run_id)
            .execute(&self.pool)
            .await
            .context("failed to set task run worker")?;
        Ok(())
    }

    /// All attempts for a task, oldest first.
    pub async fn list_runs(&self, task_number: i64) -> Result<Vec<TaskRun>> {
        let rows = sqlx::query(&format!(
            "{RUN_SELECT_COLUMNS} FROM task_runs WHERE task_number = ? ORDER BY attempt ASC"
        ))
        .bind(task_number)
        .fetch_all(&self.pool)
        .await
        .context("failed to list task runs")?;

        rows.into_iter().map(task_run_from_row).collect()
    }

    // -- Failure budget -----------------------------------------------------

    /// Record a failed attempt and decide whether the task may be retried.
    ///
    /// Runs as one transaction so the increment and the status change cannot
    /// interleave with a concurrent claim.
    pub async fn record_failure(
        &self,
        task_number: i64,
        outcome: TaskRunOutcome,
        error: &str,
    ) -> Result<FailureDisposition> {
        if !outcome.counts_as_failure() {
            return Ok(FailureDisposition::NotCounted);
        }

        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open failure budget transaction")?;

        let row = sqlx::query(
            "SELECT consecutive_failures, max_retries FROM tasks WHERE task_number = ?",
        )
        .bind(task_number)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to read task failure budget")?;

        let Some(row) = row else {
            tx.commit()
                .await
                .context("failed to commit empty failure budget transaction")?;
            return Ok(FailureDisposition::TaskMissing);
        };

        let previous: i64 = row.try_get("consecutive_failures").unwrap_or(0);
        let limit: i64 = row
            .try_get::<Option<i64>, _>("max_retries")
            .ok()
            .flatten()
            .unwrap_or(DEFAULT_FAILURE_LIMIT);
        let failures = previous + 1;
        let exhausted = failures >= limit;

        let next_status = if exhausted {
            TaskStatus::Blocked
        } else {
            TaskStatus::Ready
        };

        sqlx::query(
            "UPDATE tasks SET consecutive_failures = ?, last_error = ?, status = ?, \
             worker_id = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ?",
        )
        .bind(failures)
        .bind(error)
        .bind(next_status.as_str())
        .bind(task_number)
        .execute(&mut *tx)
        .await
        .context("failed to persist task failure budget")?;

        tx.commit()
            .await
            .context("failed to commit failure budget transaction")?;

        Ok(if exhausted {
            FailureDisposition::Parked { failures, limit }
        } else {
            FailureDisposition::Requeued { failures, limit }
        })
    }

    /// Reset the failure budget. Called on successful completion, and on an
    /// operator-initiated retry — a human looked at it, so the budget starts
    /// over rather than immediately re-parking the task.
    pub async fn clear_failures(&self, task_number: i64) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET consecutive_failures = 0, last_error = NULL \
             WHERE task_number = ?",
        )
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to clear task failure budget")?;

        Ok(())
    }
}

/// What [`TaskStore::record_failure`] decided to do with a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    /// Budget remains — task returned to `ready` for another attempt.
    Requeued { failures: i64, limit: i64 },
    /// Budget exhausted — task parked in `blocked` for a human.
    Parked { failures: i64, limit: i64 },
    /// Outcome does not count against the budget (rate limits).
    NotCounted,
    /// The task row disappeared between execution and bookkeeping.
    TaskMissing,
}

/// Column list used by all SELECT queries. Kept in sync with `task_from_row`.
const SELECT_COLUMNS: &str = "SELECT id, task_number, title, description, status, priority, \
     owner_agent_id, assigned_agent_id, subtasks, metadata, source_memory_id, worker_id, \
     created_by, approved_at, approved_by, created_at, updated_at, completed_at, \
     consecutive_failures, max_retries, last_error, project_id, repo_id, worktree_id";

const RUN_SELECT_COLUMNS: &str = "SELECT id, task_number, attempt, worker_id, outcome, \
     summary, error, started_at, ended_at";

/// The single source of truth for legal status transitions.
///
/// Both the HTTP API and the dashboard's drag-and-drop consume this, so the
/// board can never render a move the API rejects.
pub fn can_transition(current: TaskStatus, next: TaskStatus) -> bool {
    if current == next {
        return true;
    }

    if next == TaskStatus::Backlog {
        return true;
    }

    matches!(
        (current, next),
        (TaskStatus::PendingApproval, TaskStatus::Ready)
            | (TaskStatus::Ready, TaskStatus::InProgress)
            | (TaskStatus::InProgress, TaskStatus::Done)
            | (TaskStatus::InProgress, TaskStatus::Ready)
            | (TaskStatus::InProgress, TaskStatus::Blocked)
            | (TaskStatus::Backlog, TaskStatus::Ready)
            | (TaskStatus::Done, TaskStatus::Ready)
            // Unblocking is always operator- or sweep-initiated.
            | (TaskStatus::Blocked, TaskStatus::Ready)
            | (TaskStatus::Blocked, TaskStatus::Done)
    )
}

/// Every legal `(from, to)` pair, for export to the dashboard so the UI and the
/// API agree on what a drag is allowed to do.
pub fn legal_transitions() -> Vec<(TaskStatus, TaskStatus)> {
    let mut pairs = Vec::new();
    for from in TaskStatus::ALL {
        for to in TaskStatus::ALL {
            if from != to && can_transition(from, to) {
                pairs.push((from, to));
            }
        }
    }
    pairs
}

fn merge_json_object(current: Value, patch: Option<Value>) -> Value {
    let Some(patch) = patch else {
        return current;
    };

    // Only apply object patches — ignore scalars/nulls to preserve the
    // invariant that task metadata is always an object.
    let Value::Object(patch_object) = patch else {
        return current;
    };

    let Value::Object(mut current_object) = current else {
        return Value::Object(patch_object);
    };

    for (key, patch_value) in patch_object {
        let merged_value = match current_object.remove(&key) {
            Some(current_value) => merge_json_value(current_value, patch_value),
            None => patch_value,
        };
        current_object.insert(key, merged_value);
    }

    Value::Object(current_object)
}

fn merge_json_value(current: Value, patch: Value) -> Value {
    match (current, patch) {
        (Value::Object(current_object), Value::Object(patch_object)) => merge_json_object(
            Value::Object(current_object),
            Some(Value::Object(patch_object)),
        ),
        (_, patch_value) => patch_value,
    }
}

fn parse_subtasks(value: &str) -> Vec<TaskSubtask> {
    serde_json::from_str(value).unwrap_or_default()
}

fn parse_metadata(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

fn task_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Task> {
    let status_value: String = row
        .try_get("status")
        .context("failed to read task status")?;
    let priority_value: String = row
        .try_get("priority")
        .context("failed to read task priority")?;
    let subtasks_value: String = row.try_get("subtasks").unwrap_or_else(|_| "[]".to_string());
    let metadata_value: String = row.try_get("metadata").unwrap_or_else(|_| "{}".to_string());

    let status = TaskStatus::parse(&status_value)
        .with_context(|| format!("invalid task status in database: {status_value}"))?;
    let priority = TaskPriority::parse(&priority_value)
        .with_context(|| format!("invalid task priority in database: {priority_value}"))?;

    // The global schema uses TEXT columns with ISO 8601 defaults. Read as
    // strings directly; fall back to NaiveDateTime parsing for compatibility
    // with rows that may still use SQLite TIMESTAMP format.
    let created_at = read_timestamp(&row, "created_at")?;
    let updated_at = read_timestamp(&row, "updated_at")?;

    Ok(Task {
        id: row.try_get("id").context("failed to read task id")?,
        task_number: row
            .try_get("task_number")
            .context("failed to read task_number")?,
        title: row.try_get("title").context("failed to read task title")?,
        description: row.try_get("description").ok(),
        status,
        priority,
        owner_agent_id: row
            .try_get("owner_agent_id")
            .context("failed to read owner_agent_id")?,
        assigned_agent_id: row
            .try_get("assigned_agent_id")
            .context("failed to read assigned_agent_id")?,
        subtasks: parse_subtasks(&subtasks_value),
        metadata: parse_metadata(&metadata_value),
        source_memory_id: row.try_get("source_memory_id").ok(),
        worker_id: read_optional_id(&row, "worker_id"),
        created_by: row
            .try_get("created_by")
            .context("failed to read task created_by")?,
        approved_at: read_optional_timestamp(&row, "approved_at"),
        approved_by: row.try_get("approved_by").ok(),
        created_at,
        updated_at,
        completed_at: read_optional_timestamp(&row, "completed_at"),
        consecutive_failures: row.try_get("consecutive_failures").unwrap_or(0),
        max_retries: row.try_get("max_retries").ok().flatten(),
        last_error: row
            .try_get::<Option<String>, _>("last_error")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        project_id: read_optional_id(&row, "project_id"),
        repo_id: read_optional_id(&row, "repo_id"),
        worktree_id: read_optional_id(&row, "worktree_id"),
    })
}

/// Read a nullable TEXT id, treating the empty string as absent.
fn read_optional_id(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn task_run_from_row(row: sqlx::sqlite::SqliteRow) -> Result<TaskRun> {
    let outcome = row
        .try_get::<Option<String>, _>("outcome")
        .ok()
        .flatten()
        .and_then(|value| TaskRunOutcome::parse(&value));

    Ok(TaskRun {
        id: row.try_get("id").context("failed to read task run id")?,
        task_number: row
            .try_get("task_number")
            .context("failed to read task run task_number")?,
        attempt: row
            .try_get("attempt")
            .context("failed to read task run attempt")?,
        worker_id: row.try_get::<Option<String>, _>("worker_id").ok().flatten(),
        outcome,
        summary: row.try_get::<Option<String>, _>("summary").ok().flatten(),
        error: row.try_get::<Option<String>, _>("error").ok().flatten(),
        started_at: read_timestamp(&row, "started_at")?,
        ended_at: read_optional_timestamp(&row, "ended_at"),
    })
}

/// Read a required timestamp column, trying TEXT first (ISO 8601) then falling
/// back to NaiveDateTime for legacy TIMESTAMP columns.
fn read_timestamp(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<String> {
    if let Ok(value) = row.try_get::<String, _>(column) {
        return Ok(value);
    }
    row.try_get::<chrono::NaiveDateTime, _>(column)
        .map(|v| v.and_utc().to_rfc3339())
        .with_context(|| format!("failed to read task {column}"))
        .map_err(Into::into)
}

/// Read an optional timestamp column, trying TEXT first then NaiveDateTime.
fn read_optional_timestamp(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<String> {
    if let Ok(Some(value)) = row.try_get::<Option<String>, _>(column)
        && !value.is_empty()
    {
        return Some(value);
    }
    row.try_get::<Option<chrono::NaiveDateTime>, _>(column)
        .ok()
        .flatten()
        .map(|v| v.and_utc().to_rfc3339())
}

/// Create the task tables in a test pool.
///
/// This is the single definition of the test schema — `cortex.rs` and any other
/// module that needs a bare pool with task tables calls this rather than
/// hand-rolling its own `CREATE TABLE`. Keep it in sync with
/// `migrations/global/`; when a migration adds a column, add it here too and
/// every test site picks it up.
#[cfg(test)]
pub(crate) async fn create_task_schema(pool: &SqlitePool) {
    sqlx::query(
        r#"
        CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            task_number INTEGER NOT NULL UNIQUE,
            title TEXT NOT NULL,
            description TEXT,
            status TEXT NOT NULL DEFAULT 'backlog',
            priority TEXT NOT NULL DEFAULT 'medium',
            owner_agent_id TEXT NOT NULL,
            assigned_agent_id TEXT NOT NULL,
            subtasks TEXT,
            metadata TEXT,
            source_memory_id TEXT,
            worker_id TEXT,
            created_by TEXT NOT NULL,
            approved_at TEXT,
            approved_by TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            completed_at TEXT,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            max_retries INTEGER,
            last_error TEXT,
            project_id TEXT,
            repo_id TEXT,
            worktree_id TEXT
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("tasks schema should be created");

    sqlx::query(
        r#"
        CREATE TABLE task_runs (
            id TEXT PRIMARY KEY NOT NULL,
            task_number INTEGER NOT NULL,
            attempt INTEGER NOT NULL,
            worker_id TEXT,
            outcome TEXT,
            summary TEXT,
            error TEXT,
            started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            ended_at TEXT,
            UNIQUE (task_number, attempt)
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("task_runs schema should be created");

    sqlx::query(
        "CREATE TABLE task_number_seq (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            next_number INTEGER NOT NULL DEFAULT 1
        )",
    )
    .execute(pool)
    .await
    .expect("task_number_seq should be created");
}

#[cfg(test)]
pub(crate) async fn setup_test_store() -> TaskStore {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite should connect");

    create_task_schema(&pool).await;

    sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
        .execute(&pool)
        .await
        .expect("sequence seed should be inserted");

    TaskStore::new(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_store() -> TaskStore {
        setup_test_store().await
    }

    fn self_assigned_input(title: &str, status: TaskStatus) -> CreateTaskInput {
        CreateTaskInput {
            owner_agent_id: "agent-test".to_string(),
            assigned_agent_id: "agent-test".to_string(),
            title: title.to_string(),
            status,
            created_by: "branch".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn binding_round_trips_through_create_and_read() {
        let store = setup_store().await;
        let created = store
            .create(CreateTaskInput {
                binding: TaskProjectBinding {
                    project_id: Some("proj-platform".into()),
                    repo_id: Some("repo-api".into()),
                    worktree_id: Some("wt-feature".into()),
                },
                ..self_assigned_input("bound task", TaskStatus::Backlog)
            })
            .await
            .expect("should create");

        assert_eq!(created.project_id.as_deref(), Some("proj-platform"));
        assert_eq!(created.repo_id.as_deref(), Some("repo-api"));
        assert_eq!(created.worktree_id.as_deref(), Some("wt-feature"));

        let fetched = store
            .get_by_number(created.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(fetched.repo_id.as_deref(), Some("repo-api"));
    }

    #[tokio::test]
    async fn unbound_tasks_have_no_binding() {
        let store = setup_store().await;
        let created = store
            .create(self_assigned_input("plain task", TaskStatus::Backlog))
            .await
            .expect("should create");

        assert!(created.project_id.is_none());
        assert!(created.repo_id.is_none());
        assert!(created.worktree_id.is_none());
    }

    #[tokio::test]
    async fn two_tasks_in_one_project_can_target_different_repos() {
        // The multi-repo case the board previously could not express at all:
        // one project, two repos, one task each.
        let store = setup_store().await;

        let api_task = store
            .create(CreateTaskInput {
                binding: TaskProjectBinding {
                    project_id: Some("proj-platform".into()),
                    repo_id: Some("repo-api".into()),
                    worktree_id: None,
                },
                ..self_assigned_input("change the contract", TaskStatus::Backlog)
            })
            .await
            .expect("should create");

        let web_task = store
            .create(CreateTaskInput {
                binding: TaskProjectBinding {
                    project_id: Some("proj-platform".into()),
                    repo_id: Some("repo-web".into()),
                    worktree_id: None,
                },
                ..self_assigned_input("regenerate clients", TaskStatus::Backlog)
            })
            .await
            .expect("should create");

        assert_eq!(api_task.project_id, web_task.project_id);
        assert_ne!(
            api_task.repo_id, web_task.repo_id,
            "two tasks in the same project must be able to name different repos"
        );
    }

    #[tokio::test]
    async fn update_rebinds_and_clears() {
        let store = setup_store().await;
        let created = store
            .create(CreateTaskInput {
                binding: TaskProjectBinding {
                    project_id: Some("proj-a".into()),
                    repo_id: Some("repo-1".into()),
                    worktree_id: None,
                },
                ..self_assigned_input("movable", TaskStatus::Backlog)
            })
            .await
            .expect("should create");

        // Rebind to a different repo.
        let rebound = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    binding: Some(TaskProjectBinding {
                        project_id: Some("proj-a".into()),
                        repo_id: Some("repo-2".into()),
                        worktree_id: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(rebound.repo_id.as_deref(), Some("repo-2"));

        // An update that says nothing about the binding must leave it alone.
        let untouched = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    title: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(
            untouched.repo_id.as_deref(),
            Some("repo-2"),
            "an unrelated update must not silently unbind the task"
        );

        // Explicit clear.
        let cleared = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    clear_binding: true,
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert!(cleared.project_id.is_none());
        assert!(cleared.repo_id.is_none());
    }

    #[tokio::test]
    async fn runs_are_numbered_sequentially_per_task() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("multi attempt", TaskStatus::Ready))
            .await
            .expect("should create");

        let first = store
            .start_run(task.task_number, Some("worker-1"))
            .await
            .expect("first run");
        let second = store
            .start_run(task.task_number, Some("worker-2"))
            .await
            .expect("second run");

        assert_eq!(first.attempt, 1);
        assert_eq!(second.attempt, 2);
        assert!(first.outcome.is_none(), "a fresh run has no outcome yet");

        store
            .finish_run(&first.id, TaskRunOutcome::Failed, None, Some("boom"))
            .await
            .expect("finish first");

        let runs = store.list_runs(task.task_number).await.expect("list runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].outcome, Some(TaskRunOutcome::Failed));
        assert_eq!(runs[0].error.as_deref(), Some("boom"));
        assert!(runs[0].ended_at.is_some());
        assert!(runs[1].ended_at.is_none(), "open run stays open");
    }

    #[tokio::test]
    async fn failure_budget_requeues_then_parks() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("flaky", TaskStatus::InProgress))
            .await
            .expect("should create");

        // First failure: budget remains, back to ready.
        let first = store
            .record_failure(task.task_number, TaskRunOutcome::Failed, "attempt 1 failed")
            .await
            .expect("record first failure");
        assert_eq!(
            first,
            FailureDisposition::Requeued {
                failures: 1,
                limit: DEFAULT_FAILURE_LIMIT
            }
        );
        let after_first = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after_first.status, TaskStatus::Ready);
        assert_eq!(after_first.consecutive_failures, 1);
        assert_eq!(after_first.last_error.as_deref(), Some("attempt 1 failed"));

        // Second failure hits the limit: parked, not requeued.
        let second = store
            .record_failure(task.task_number, TaskRunOutcome::Failed, "attempt 2 failed")
            .await
            .expect("record second failure");
        assert_eq!(
            second,
            FailureDisposition::Parked {
                failures: 2,
                limit: DEFAULT_FAILURE_LIMIT
            }
        );
        let after_second = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            after_second.status,
            TaskStatus::Blocked,
            "an exhausted budget must park the task instead of hot-looping"
        );
        assert_eq!(after_second.consecutive_failures, 2);
    }

    #[tokio::test]
    async fn rate_limits_do_not_spend_the_failure_budget() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("quota", TaskStatus::InProgress))
            .await
            .expect("should create");

        for _ in 0..5 {
            let disposition = store
                .record_failure(task.task_number, TaskRunOutcome::RateLimited, "429")
                .await
                .expect("record rate limit");
            assert_eq!(disposition, FailureDisposition::NotCounted);
        }

        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            after.consecutive_failures, 0,
            "a provider quota outage must not trip the circuit breaker"
        );
    }

    #[tokio::test]
    async fn clear_failures_resets_the_budget() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("retryable", TaskStatus::InProgress))
            .await
            .expect("should create");

        store
            .record_failure(task.task_number, TaskRunOutcome::Failed, "nope")
            .await
            .expect("record failure");
        store
            .clear_failures(task.task_number)
            .await
            .expect("clear failures");

        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.consecutive_failures, 0);
        assert!(after.last_error.is_none());
    }

    #[tokio::test]
    async fn max_retries_overrides_the_default_limit() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("one shot", TaskStatus::InProgress))
            .await
            .expect("should create");

        sqlx::query("UPDATE tasks SET max_retries = 1 WHERE task_number = ?")
            .bind(task.task_number)
            .execute(store.pool())
            .await
            .expect("set max_retries");

        let disposition = store
            .record_failure(task.task_number, TaskRunOutcome::Failed, "failed once")
            .await
            .expect("record failure");
        assert_eq!(
            disposition,
            FailureDisposition::Parked {
                failures: 1,
                limit: 1
            },
            "a max_retries of 1 must park on the first failure"
        );
    }

    #[tokio::test]
    async fn blocked_tasks_are_not_claimable() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("parked", TaskStatus::InProgress))
            .await
            .expect("should create");

        // Burn the budget so the task lands in Blocked.
        for _ in 0..DEFAULT_FAILURE_LIMIT {
            store
                .record_failure(task.task_number, TaskRunOutcome::Failed, "dead end")
                .await
                .expect("record failure");
        }

        let claimed = store
            .claim_next_ready("agent-test")
            .await
            .expect("claim should succeed");
        assert!(
            claimed.is_none(),
            "the pickup loop must not re-claim a task parked by the failure budget"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_status_transition() {
        let store = setup_store().await;
        let created = store
            .create(CreateTaskInput {
                created_by: "cortex".to_string(),
                ..self_assigned_input("pending task", TaskStatus::PendingApproval)
            })
            .await
            .expect("task should be created");

        let error = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .await
            .expect_err("pending_approval -> in_progress must fail");

        assert!(error.to_string().contains("invalid task status transition"));
    }

    #[tokio::test]
    async fn update_with_status_transition_returns_previous_status() {
        let store = setup_store().await;
        let created = store
            .create(self_assigned_input(
                "track status transition",
                TaskStatus::InProgress,
            ))
            .await
            .expect("task should be created");

        let result = store
            .update_with_status_transition(
                created.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");

        assert_eq!(result.previous_status, TaskStatus::InProgress);
        assert_eq!(result.task.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn update_worker_task_prefers_exact_match_with_duplicate_worker_bindings() {
        let store = setup_store().await;
        let first = store
            .create(self_assigned_input("first task", TaskStatus::InProgress))
            .await
            .expect("first task should be created");
        let second = store
            .create(self_assigned_input("second task", TaskStatus::InProgress))
            .await
            .expect("second task should be created");

        let shared_worker_id = "worker-shared";
        store
            .update(
                first.task_number,
                UpdateTaskInput {
                    worker_id: Some(shared_worker_id.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("first worker binding should update");
        store
            .update(
                second.task_number,
                UpdateTaskInput {
                    worker_id: Some(shared_worker_id.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("second worker binding should update");

        let result = store
            .update_worker_task(
                shared_worker_id,
                first.task_number,
                UpdateTaskInput {
                    metadata: Some(serde_json::json!({"target": "first"})),
                    ..Default::default()
                },
            )
            .await
            .expect("worker-scoped update should succeed");

        let WorkerTaskUpdateResult::Updated(result) = result else {
            panic!("expected exact task update despite duplicate worker bindings");
        };
        assert_eq!(result.task.task_number, first.task_number);
        assert_eq!(result.task.metadata["target"], "first");
    }

    #[tokio::test]
    async fn can_requeue_in_progress_and_clear_worker_binding() {
        let store = setup_store().await;
        let created = store
            .create(self_assigned_input("ready task", TaskStatus::Ready))
            .await
            .expect("task should be created");

        let in_progress = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::InProgress),
                    worker_id: Some("worker-1".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");

        assert_eq!(in_progress.worker_id.as_deref(), Some("worker-1"));

        let requeued = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Ready),
                    clear_worker_id: true,
                    ..Default::default()
                },
            )
            .await
            .expect("requeue should succeed")
            .expect("task should exist");

        assert_eq!(requeued.status, TaskStatus::Ready);
        assert!(
            requeued.worker_id.is_none(),
            "expected worker binding to clear, got {:?}",
            requeued.worker_id
        );
    }

    #[tokio::test]
    async fn metadata_updates_deep_merge_nested_objects() {
        let store = setup_store().await;
        let created = store
            .create(CreateTaskInput {
                metadata: serde_json::json!({
                    "github_issue": {
                        "repo": "spacedriveapp/spacebot",
                        "number": 123,
                        "labels": ["bug"],
                        "state": "open"
                    },
                    "source": "github"
                }),
                ..self_assigned_input("github-linked task", TaskStatus::Backlog)
            })
            .await
            .expect("task should be created");

        let updated = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    metadata: Some(serde_json::json!({
                        "github_issue": {
                            "url": "https://github.com/spacedriveapp/spacebot/issues/123",
                            "labels": ["bug", "tasks"]
                        },
                        "github_pr": {
                            "number": 456
                        }
                    })),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");

        assert_eq!(
            updated.metadata,
            serde_json::json!({
                "github_issue": {
                    "repo": "spacedriveapp/spacebot",
                    "number": 123,
                    "url": "https://github.com/spacedriveapp/spacebot/issues/123",
                    "labels": ["bug", "tasks"],
                    "state": "open"
                },
                "github_pr": {
                    "number": 456
                },
                "source": "github"
            })
        );
    }

    #[tokio::test]
    async fn global_task_numbers_are_unique_across_agents() {
        let store = setup_store().await;

        let task_a = store
            .create(self_assigned_input("task for agent A", TaskStatus::Backlog))
            .await
            .expect("task A should be created");

        let task_b = store
            .create(CreateTaskInput {
                owner_agent_id: "agent-other".to_string(),
                assigned_agent_id: "agent-other".to_string(),
                ..self_assigned_input("task for agent B", TaskStatus::Backlog)
            })
            .await
            .expect("task B should be created");

        assert_eq!(task_a.task_number, 1);
        assert_eq!(task_b.task_number, 2);

        // Both accessible by global number without agent scoping
        let fetched_a = store
            .get_by_number(1)
            .await
            .expect("fetch should succeed")
            .expect("task 1 should exist");
        assert_eq!(fetched_a.owner_agent_id, "agent-test");

        let fetched_b = store
            .get_by_number(2)
            .await
            .expect("fetch should succeed")
            .expect("task 2 should exist");
        assert_eq!(fetched_b.owner_agent_id, "agent-other");
    }

    #[tokio::test]
    async fn list_filters_by_assigned_agent() {
        let store = setup_store().await;

        store
            .create(self_assigned_input("my task", TaskStatus::Backlog))
            .await
            .expect("should create");

        store
            .create(CreateTaskInput {
                owner_agent_id: "agent-test".to_string(),
                assigned_agent_id: "agent-other".to_string(),
                ..self_assigned_input("delegated task", TaskStatus::Ready)
            })
            .await
            .expect("should create");

        let mine = store
            .list(TaskListFilter {
                assigned_agent_id: Some("agent-test".to_string()),
                ..Default::default()
            })
            .await
            .expect("list should succeed");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].title, "my task");

        let theirs = store
            .list(TaskListFilter {
                assigned_agent_id: Some("agent-other".to_string()),
                ..Default::default()
            })
            .await
            .expect("list should succeed");
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].title, "delegated task");

        // Unfiltered returns both
        let all = store
            .list(TaskListFilter::default())
            .await
            .expect("list should succeed");
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn claim_next_ready_scopes_by_assigned_agent() {
        let store = setup_store().await;

        // Create a ready task assigned to agent-other
        store
            .create(CreateTaskInput {
                owner_agent_id: "agent-test".to_string(),
                assigned_agent_id: "agent-other".to_string(),
                title: "not mine".to_string(),
                description: None,
                status: TaskStatus::Ready,
                priority: TaskPriority::High,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "branch".to_string(),
                ..Default::default()
            })
            .await
            .expect("should create");

        // agent-test should not be able to claim it
        let claimed = store
            .claim_next_ready("agent-test")
            .await
            .expect("claim should succeed");
        assert!(
            claimed.is_none(),
            "should not claim task assigned to other agent"
        );

        // agent-other should be able to claim it
        let claimed = store
            .claim_next_ready("agent-other")
            .await
            .expect("claim should succeed");
        assert!(claimed.is_some());
        assert_eq!(claimed.unwrap().status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn reassign_task_via_update() {
        let store = setup_store().await;
        let created = store
            .create(self_assigned_input("reassignable", TaskStatus::Backlog))
            .await
            .expect("should create");

        assert_eq!(created.assigned_agent_id, "agent-test");

        let updated = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    assigned_agent_id: Some("agent-other".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");

        assert_eq!(updated.assigned_agent_id, "agent-other");
        assert_eq!(updated.owner_agent_id, "agent-test");
    }
}
