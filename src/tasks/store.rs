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
    /// Stuck and waiting on a human. `block_kind` says why.
    ///
    /// Deliberately *not* where a task waiting on its dependencies lives — that
    /// task sits in `Backlog`, which already means "not yet eligible". Putting
    /// both in one status would make the board unable to answer the only
    /// question it exists to answer: what needs me?
    Blocked,
    Done,
}

/// Why a task is parked.
///
/// The kinds differ in how they recover, which is the entire reason the column
/// exists. `dependency` and `transient` clear themselves; `needs_input` and
/// `capability` are sticky and only an explicit unblock releases them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// Waiting on an upstream task. Cleared automatically by the ready sweep.
    /// Never a human gate, so a task carrying it stays in `Backlog`.
    Dependency,
    /// Needs a human decision.
    NeedsInput,
    /// The agent lacks a tool, credential, or permission it needs.
    Capability,
    /// Flaky failure or provider outage. Retried under the F1 failure budget.
    Transient,
}

impl BlockKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::Dependency => "dependency",
            BlockKind::NeedsInput => "needs_input",
            BlockKind::Capability => "capability",
            BlockKind::Transient => "transient",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "dependency" => Some(BlockKind::Dependency),
            "needs_input" => Some(BlockKind::NeedsInput),
            "capability" => Some(BlockKind::Capability),
            "transient" => Some(BlockKind::Transient),
            _ => None,
        }
    }

    /// Whether only an explicit unblock may release this task.
    ///
    /// The automatic sweep must skip sticky kinds. A human parked the task
    /// knowing it could not proceed; resurrecting it on a timer would throw
    /// that decision away and hand the worker the same dead end again.
    pub fn is_sticky(self) -> bool {
        matches!(self, BlockKind::NeedsInput | BlockKind::Capability)
    }

    /// The status a task takes when blocked for this reason.
    ///
    /// `dependency` is ordinary scheduling, not an incident: the task goes back
    /// to `Backlog` and the sweep promotes it when its parents land. Everything
    /// else is a stop that wants attention.
    pub fn resting_status(self) -> TaskStatus {
        match self {
            BlockKind::Dependency => TaskStatus::Backlog,
            _ => TaskStatus::Blocked,
        }
    }
}

impl std::fmt::Display for BlockKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// How many cards one task may file. Bounds a single runaway decomposition.
pub const MAX_TASKS_FILED_PER_TASK: i64 = 10;

/// How many filing hops are allowed from a human or agent to a filed card.
///
/// Fan-out and depth need separate bounds: a cap of 10 with unbounded depth
/// still permits 10^n tasks. Three hops is enough for
/// "epic -> service -> change" and stops well short of a self-sustaining tree.
pub const MAX_FILING_DEPTH: i64 = 3;

/// Hard stop for the `created_by` walk, independent of the policy limit above,
/// so a malformed chain cannot loop forever.
const MAX_FILING_DEPTH_WALK: i64 = 32;

/// `created_by` prefix marking a card filed by a task rather than by a human,
/// a branch, or the cortex. The suffix is the filing task number, which is what
/// makes provenance and the fan-out cap possible without another column.
pub const FILED_BY_TASK_PREFIX: &str = "task:";

/// Render the `created_by` value for a card filed by a task.
pub fn filer_id(task_number: i64) -> String {
    format!("{FILED_BY_TASK_PREFIX}{task_number}")
}

/// Read the filing task number back out of a `created_by` value.
pub fn parse_filer_task_number(created_by: &str) -> Option<i64> {
    created_by
        .strip_prefix(FILED_BY_TASK_PREFIX)
        .and_then(|rest| rest.parse().ok())
}

/// How many times a task may be unblocked and re-blocked for the *same* reason
/// before it escalates to a human instead of continuing to bounce.
///
/// Borrowed from Hermes, which learned it by running the system: a cron that
/// unblocks and a worker that re-blocks will trade a card forever, burning a
/// worker spawn each round, and nothing in the loop notices.
pub const BLOCK_RECURRENCE_LIMIT: i64 = 2;

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

    /// Whether this status is an end state — nothing further will move the
    /// task on its own. Used as the gate predicate for dependency edges: a
    /// child is eligible only once every parent is terminal.
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
    ///
    /// Despite the name (inherited from the column), this is a *failure* limit,
    /// not a retry count: the task is parked once `consecutive_failures`
    /// reaches it, so `max_retries = 1` allows one attempt and zero retries.
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
    /// Why this task is parked, when it is.
    pub block_kind: Option<BlockKind>,
    /// Human-readable explanation shown on the card.
    pub block_reason: Option<String>,
    /// Consecutive blocks for the same reason. See [`BLOCK_RECURRENCE_LIMIT`].
    pub block_recurrences: i64,
    /// What this task was last blocked for, kept across promotion.
    ///
    /// `block_kind` answers "why is it parked right now" and goes to NULL when
    /// it is not. The recurrence counter needs the other question — "what did
    /// it park for last time" — because the sweep clears `block_kind` every
    /// time it promotes, which would otherwise reset the counter forever.
    pub last_block_kind: Option<BlockKind>,
    /// JSON Schema this task's resolved inputs must satisfy before it runs.
    pub input_schema: Option<Value>,
    /// JSON Schema this task's outputs must satisfy to complete.
    pub output_schema: Option<Value>,
    /// Inputs as resolved at claim time. Persisted so the value the worker
    /// actually saw survives a crash and stays readable after upstream changes.
    pub inputs: Option<Value>,
    /// Validated outputs. What downstream tasks read from.
    pub outputs: Option<Value>,
    /// The workflow launch this task was compiled from, if any.
    ///
    /// Plain text rather than a foreign key: a task outlives its template, and
    /// deleting a workflow must not take the record of work that actually
    /// happened with it.
    pub workflow_run_id: Option<String>,
    /// Which step of that workflow produced this task.
    pub workflow_step_key: Option<String>,
    /// Extra instructions appended to the worker prompt at pickup. Appended,
    /// never substituted — this is task guidance, not an identity override.
    pub system_prompt: Option<String>,
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

impl Task {
    /// The codebase this task is bound to, as a single value.
    pub fn binding(&self) -> TaskProjectBinding {
        TaskProjectBinding {
            project_id: self.project_id.clone(),
            repo_id: self.repo_id.clone(),
            worktree_id: self.worktree_id.clone(),
        }
    }
}

/// A partial update to a task's binding.
///
/// Each field is independently three-valued, which [`TaskProjectBinding`] is
/// not: `None` leaves the column alone, `Some(None)` clears it, `Some(Some(id))`
/// sets it. Using the flat binding here would make "set the repo" indistinguishable
/// from "set the repo and unbind the project", which is exactly the bug this type
/// exists to prevent — a `PATCH` naming one field must not null its siblings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskBindingPatch {
    pub project_id: Option<Option<String>>,
    pub repo_id: Option<Option<String>>,
    pub worktree_id: Option<Option<String>>,
}

impl TaskBindingPatch {
    /// A patch that clears all three columns.
    pub fn clear_all() -> Self {
        Self {
            project_id: Some(None),
            repo_id: Some(None),
            worktree_id: Some(None),
        }
    }

    /// Whether this patch touches any column at all.
    pub fn is_noop(&self) -> bool {
        self.project_id.is_none() && self.repo_id.is_none() && self.worktree_id.is_none()
    }
}

impl From<TaskProjectBinding> for TaskBindingPatch {
    /// Sets every field, including the absent ones. Use this only when the
    /// caller genuinely supplied a complete binding.
    fn from(binding: TaskProjectBinding) -> Self {
        Self {
            project_id: Some(binding.project_id),
            repo_id: Some(binding.repo_id),
            worktree_id: Some(binding.worktree_id),
        }
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
    /// The worker vanished without reporting anything — process died, host
    /// restarted, cortex was killed mid-run. Recorded by the reaper, never by
    /// the worker itself, since by definition nothing was left to report it.
    Abandoned,
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
            TaskRunOutcome::Abandoned => "abandoned",
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
            "abandoned" => Some(TaskRunOutcome::Abandoned),
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
            TaskRunOutcome::Failed
                | TaskRunOutcome::Timeout
                | TaskRunOutcome::Blocked
                | TaskRunOutcome::Abandoned
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
    /// Tasks that must finish before this one may run.
    ///
    /// Applied after the row exists, so a bad edge fails the create rather than
    /// leaving an orphan task with a half-built graph.
    pub depends_on: Vec<i64>,
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
            depends_on: Vec::new(),
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
    /// Rebind the task to a different codebase, one column at a time. An
    /// untouched field is left as-is rather than nulled.
    pub binding: TaskBindingPatch,
    /// Clear all three binding columns, overriding `binding`.
    pub clear_binding: bool,
    /// Change how many failures this task tolerates before it is parked.
    ///
    /// Three-valued for the same reason the binding patch is: `None` leaves it
    /// alone, `Some(None)` returns the task to the instance default, and
    /// `Some(Some(n))` sets an explicit limit. Without the middle case there is
    /// no way to undo an override once set.
    pub max_retries: Option<Option<i64>>,
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

    /// The instance pool this store reads.
    ///
    /// Exposed so the gate store can be built alongside it without threading a
    /// second pool through every construction site: gates live in the same
    /// database as the tasks they hold, and a gate against a different database
    /// would be a gate the scheduler could not see.
    pub fn pool(&self) -> &SqlitePool {
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

                    // Edges are applied after the row exists, because
                    // `link_tasks` validates both endpoints. A rejected edge
                    // deletes the task rather than leaving it behind with a
                    // graph the caller did not ask for — a half-linked task is
                    // worse than none, since the scheduler would run it early.
                    for parent in &input.depends_on {
                        if let Err(error) = self.link_tasks(*parent, task_number).await {
                            let _ = self.delete(task_number).await;
                            return Err(anyhow::anyhow!(
                                "failed to link task #{task_number} to parent #{parent}: {error}"
                            )
                            .into());
                        }
                    }

                    // Anything waiting on a parent starts in backlog; the ready
                    // sweep promotes it once every parent lands.
                    if !input.depends_on.is_empty() && input.status == TaskStatus::Ready {
                        sqlx::query(
                            "UPDATE tasks SET status = 'backlog', block_kind = 'dependency', \
                             block_reason = 'waiting on an upstream task' \
                             WHERE task_number = ? AND status = 'ready'",
                        )
                        .bind(task_number)
                        .execute(&self.pool)
                        .await
                        .context("failed to park newly linked task")?;
                    }

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

    /// Tasks this agent left running that have not been touched for
    /// `min_age_secs`.
    ///
    /// The age floor is what keeps the reaper from eating a task that was
    /// claimed moments ago and whose worker has not finished registering yet.
    /// Scoped to one agent because the task table is instance-wide: another
    /// agent's running task is not this one's to reap.
    pub async fn list_stale_in_progress(
        &self,
        assigned_agent_id: &str,
        min_age_secs: i64,
    ) -> Result<Vec<Task>> {
        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks \
             WHERE status = 'in_progress' AND assigned_agent_id = ? \
             AND updated_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?) \
             ORDER BY task_number ASC"
        ))
        .bind(assigned_agent_id)
        .bind(format!("-{} seconds", min_age_secs.max(0)))
        .fetch_all(&self.pool)
        .await
        .context("failed to list stale in-progress tasks")?;

        rows.into_iter().map(task_from_row).collect()
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

    /// Every task one launch produced, oldest first.
    ///
    /// The join is on a plain column, not a foreign key: a task outlives the
    /// template it came from, so this returns work that really happened even
    /// when the workflow has since been deleted.
    pub async fn list_by_workflow_run(&self, run_id: &str) -> Result<Vec<Task>> {
        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE workflow_run_id = ? ORDER BY task_number ASC"
        ))
        .bind(run_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list tasks for a workflow run")?;

        rows.into_iter().map(task_from_row).collect()
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

        // Binding: clear wins over set, and each column is emitted only when
        // the patch actually names it. Emitting all three unconditionally is
        // what made a single-field PATCH silently unbind its siblings.
        let binding_patch = if input.clear_binding {
            TaskBindingPatch::clear_all()
        } else {
            input.binding.clone()
        };
        if binding_patch.project_id.is_some() {
            query.push_str("project_id = ?, ");
        }
        if binding_patch.repo_id.is_some() {
            query.push_str("repo_id = ?, ");
        }
        if binding_patch.worktree_id.is_some() {
            query.push_str("worktree_id = ?, ");
        }
        if input.max_retries.is_some() {
            query.push_str("max_retries = ?, ");
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

        // Bind order must match the fragment order pushed above.
        if let Some(project_id) = &binding_patch.project_id {
            sql = sql.bind(project_id.clone());
        }
        if let Some(repo_id) = &binding_patch.repo_id {
            sql = sql.bind(repo_id.clone());
        }
        if let Some(worktree_id) = &binding_patch.worktree_id {
            sql = sql.bind(worktree_id.clone());
        }
        if let Some(max_retries) = input.max_retries {
            sql = sql.bind(max_retries);
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
             AND NOT EXISTS (\
               SELECT 1 FROM task_dependencies d \
                 JOIN tasks p ON p.task_number = d.parent_task_number \
                WHERE d.child_task_number = tasks.task_number AND p.status <> 'done') \
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
             WHERE task_number = ? AND status = 'ready' \
             AND NOT EXISTS (\
               SELECT 1 FROM task_dependencies d \
                 JOIN tasks p ON p.task_number = d.parent_task_number \
                WHERE d.child_task_number = tasks.task_number AND p.status <> 'done')",
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

    // -- Dependency graph ---------------------------------------------------

    /// Add a `parent → child` edge.
    ///
    /// Rejects at link time rather than at execution time: a cycle discovered
    /// while scheduling is a deadlock nobody can see, whereas a cycle rejected
    /// here names the exact edge that would have caused it.
    pub async fn link_tasks(
        &self,
        parent: i64,
        child: i64,
    ) -> std::result::Result<(), DependencyError> {
        if parent == child {
            return Err(DependencyError::SelfLoop { task_number: child });
        }

        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| DependencyError::Storage(error.to_string()))?;

        for number in [parent, child] {
            let exists: Option<i64> =
                sqlx::query_scalar("SELECT task_number FROM tasks WHERE task_number = ?")
                    .bind(number)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| DependencyError::Storage(error.to_string()))?;
            if exists.is_none() {
                return Err(DependencyError::UnknownTask {
                    task_number: number,
                });
            }
        }

        // Walk down from `child`: if `parent` is reachable, this edge closes a
        // loop. Done inside the write transaction so a concurrent link cannot
        // slip an edge in between the check and the insert.
        if let Some(path) = reachable_path(&mut tx, child, parent).await? {
            return Err(DependencyError::WouldCycle { path });
        }

        sqlx::query(
            "INSERT OR IGNORE INTO task_dependencies (parent_task_number, child_task_number) \
             VALUES (?, ?)",
        )
        .bind(parent)
        .bind(child)
        .execute(&mut *tx)
        .await
        .map_err(|error| DependencyError::Storage(error.to_string()))?;

        tx.commit()
            .await
            .map_err(|error| DependencyError::Storage(error.to_string()))?;

        Ok(())
    }

    /// Remove a `parent → child` edge. Returns whether an edge was removed.
    pub async fn unlink_tasks(&self, parent: i64, child: i64) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM task_dependencies \
             WHERE parent_task_number = ? AND child_task_number = ?",
        )
        .bind(parent)
        .bind(child)
        .execute(&self.pool)
        .await
        .context("failed to unlink tasks")?;

        Ok(result.rows_affected() > 0)
    }

    /// Task numbers this task waits on.
    pub async fn list_parents(&self, child: i64) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            "SELECT parent_task_number FROM task_dependencies \
             WHERE child_task_number = ? ORDER BY parent_task_number ASC",
        )
        .bind(child)
        .fetch_all(&self.pool)
        .await
        .context("failed to list task parents")
        .map_err(Into::into)
    }

    /// Task numbers waiting on this task.
    pub async fn list_children(&self, parent: i64) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            "SELECT child_task_number FROM task_dependencies \
             WHERE parent_task_number = ? ORDER BY child_task_number ASC",
        )
        .bind(parent)
        .fetch_all(&self.pool)
        .await
        .context("failed to list task children")
        .map_err(Into::into)
    }

    /// Edge counts for every task that has any, in one query.
    ///
    /// The board draws a dependency badge on each card. Asking per card would
    /// be a request per row on a view whose whole purpose is showing many rows,
    /// so this returns the entire adjacency summary and lets the caller index
    /// into it. Tasks with no edges are absent rather than present with zeroes.
    pub async fn dependency_summaries(&self) -> Result<Vec<TaskEdgeSummary>> {
        let rows = sqlx::query(
            "SELECT task_number, \
                    SUM(is_parent_side) AS children, \
                    SUM(1 - is_parent_side) AS parents, \
                    SUM(blocking) AS blocked_by \
               FROM ( \
                 SELECT d.parent_task_number AS task_number, 1 AS is_parent_side, 0 AS blocking \
                   FROM task_dependencies d \
                 UNION ALL \
                 SELECT d.child_task_number AS task_number, 0 AS is_parent_side, \
                        CASE WHEN p.status <> 'done' THEN 1 ELSE 0 END AS blocking \
                   FROM task_dependencies d \
                   JOIN tasks p ON p.task_number = d.parent_task_number \
               ) \
              GROUP BY task_number",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to summarize task dependencies")?;

        rows.into_iter()
            .map(|row| {
                Ok(TaskEdgeSummary {
                    task_number: row
                        .try_get("task_number")
                        .context("failed to read edge summary task_number")?,
                    parents: row.try_get("parents").unwrap_or(0),
                    children: row.try_get("children").unwrap_or(0),
                    blocked_by: row.try_get("blocked_by").unwrap_or(0),
                })
            })
            .collect()
    }

    /// Parents of `child` that have not reached a terminal status.
    pub async fn unfinished_parents(&self, child: i64) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            "SELECT d.parent_task_number FROM task_dependencies d \
             JOIN tasks p ON p.task_number = d.parent_task_number \
             WHERE d.child_task_number = ? AND p.status <> 'done' \
             ORDER BY d.parent_task_number ASC",
        )
        .bind(child)
        .fetch_all(&self.pool)
        .await
        .context("failed to list unfinished parents")
        .map_err(Into::into)
    }

    // -- Blocking -----------------------------------------------------------

    /// Park a task with a typed reason.
    ///
    /// Returns the status the task came to rest in, which depends on the kind:
    /// a dependency wait is ordinary scheduling and rests in `Backlog`, while
    /// everything else rests in `Blocked` where a human will see it.
    ///
    /// Re-blocking for the same reason increments `block_recurrences`; past
    /// [`BLOCK_RECURRENCE_LIMIT`] the task escalates to `PendingApproval`
    /// instead, which already raises a notification. That breaks the loop where
    /// a sweep unblocks a card and a worker immediately re-blocks it.
    pub async fn block_task(
        &self,
        task_number: i64,
        kind: BlockKind,
        reason: &str,
    ) -> Result<Option<BlockOutcome>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open block transaction")?;

        // `last_block_kind`, not `block_kind`: the sweep clears the latter when
        // it promotes, so comparing against it made every re-block look like a
        // first offence and the recurrence limiter never fired.
        let row = sqlx::query(
            "SELECT last_block_kind, block_recurrences FROM tasks WHERE task_number = ?",
        )
        .bind(task_number)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to read current block state")?;

        let Some(row) = row else {
            tx.commit()
                .await
                .context("failed to commit empty block transaction")?;
            return Ok(None);
        };

        let previous_kind = row
            .try_get::<Option<String>, _>("last_block_kind")
            .ok()
            .flatten()
            .as_deref()
            .and_then(BlockKind::parse);
        let previous_recurrences: i64 = row.try_get("block_recurrences").unwrap_or(0);

        // Only a repeat of the *same* reason counts. Bouncing between different
        // reasons is a task making progress through different obstacles, not a
        // loop, and escalating it would be noise.
        let recurrences = if previous_kind == Some(kind) {
            previous_recurrences + 1
        } else {
            0
        };

        let escalated = recurrences >= BLOCK_RECURRENCE_LIMIT;
        let status = if escalated {
            TaskStatus::PendingApproval
        } else {
            kind.resting_status()
        };

        sqlx::query(
            "UPDATE tasks SET status = ?, block_kind = ?, last_block_kind = ?, \
             block_reason = ?, block_recurrences = ?, worker_id = NULL, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ?",
        )
        .bind(status.as_str())
        .bind(kind.as_str())
        .bind(kind.as_str())
        .bind(reason)
        .bind(recurrences)
        .bind(task_number)
        .execute(&mut *tx)
        .await
        .context("failed to block task")?;

        tx.commit()
            .await
            .context("failed to commit block transaction")?;

        Ok(Some(BlockOutcome {
            status,
            kind,
            recurrences,
            escalated,
        }))
    }

    /// Release a parked task back to the scheduler.
    ///
    /// Clears the reason but deliberately keeps `block_recurrences`: the
    /// counter exists to notice a task being unblocked and re-blocked in a
    /// loop, and resetting it here would erase the very evidence of that.
    /// A task with unfinished parents goes to `backlog`, not `ready`.
    pub async fn unblock_task(&self, task_number: i64) -> Result<Option<Task>> {
        let unfinished = self.unfinished_parents(task_number).await?;
        let status = if unfinished.is_empty() {
            TaskStatus::Ready
        } else {
            TaskStatus::Backlog
        };

        let result = sqlx::query(
            "UPDATE tasks SET status = ?, block_kind = NULL, block_reason = NULL, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ? AND status IN ('blocked', 'backlog', 'pending_approval')",
        )
        .bind(status.as_str())
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to unblock task")?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }

        self.get_by_number(task_number).await
    }

    // -- Ready sweep --------------------------------------------------------

    /// Reconcile which of an agent's tasks are eligible for pickup.
    ///
    /// Run before claiming. Three repairs, in one pass:
    ///
    /// - `backlog` whose parents are all done, with no sticky block → `ready`
    /// - `ready` with an unfinished parent → back to `backlog` (repairs drift
    ///   from a parent being reopened, or an edge added after promotion)
    /// - `blocked(dependency)` whose parents are all done → `ready`
    ///
    /// Sticky kinds are never touched. That is the whole point of typing the
    /// block: a human parked those, and a sweep that resurrects them would hand
    /// the worker the same dead end it already reported.
    /// Which of these tasks have a gate that is not open, and why.
    ///
    /// Returns nothing when nothing is gated, which is the overwhelmingly
    /// common case — the cost of this feature should be zero for the graphs
    /// that do not use it.
    async fn gate_holds(&self, candidates: &[i64]) -> Result<Vec<GatedTask>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = std::iter::repeat_n("?", candidates.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, task_number, kind, config, label, poll_interval_secs, \
                    last_checked_at, last_result, last_detail, consecutive_errors, created_at \
             FROM task_gates \
             WHERE last_result <> 'satisfied' AND task_number IN ({placeholders}) \
             ORDER BY task_number, created_at"
        );
        let mut query = sqlx::query(&sql);
        for number in candidates {
            query = query.bind(number);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .context("failed to check task gates")?;

        let mut holds: Vec<GatedTask> = Vec::new();
        for row in rows {
            let gate = crate::tasks::gates::gate_from_row_public(row)?;
            match holds
                .iter_mut()
                .find(|held| held.task_number == gate.task_number)
            {
                Some(held) => held.reasons.push(gate.explain()),
                None => holds.push(GatedTask {
                    task_number: gate.task_number,
                    reasons: vec![gate.explain()],
                }),
            }
        }
        Ok(holds)
    }

    pub async fn recompute_ready(&self, assigned_agent_id: &str) -> Result<ReadySweep> {
        let mut sweep = ReadySweep::default();

        // Promote: eligible and waiting.
        //
        // The last clause decides what "waiting" means, and it turns on who put
        // the task in the backlog. Backlog is two different situations wearing
        // one status:
        //
        //   - A person parked it. It waits until a person says otherwise, and a
        //     sweep that dragged it into the queue would defeat the point of
        //     having a backlog at all.
        //   - Something automatic put it there, or something automatic decided
        //     it should run. Then the same automation is entitled to promote it
        //     once the reason has cleared.
        //
        // Four signals say "automatic", and each is a decision already taken:
        //
        //   has parents        waiting on them; their completion is the signal
        //   workflow_run_id    a launch decided the whole pipeline should run,
        //                      which is how an entry step with no parents starts
        //   created_by task:N  a worker filed it; filing *is* the decision, so
        //                      it does not need a second one
        //   block_kind         the scheduler parked it, so the scheduler
        //                      reconsiders it
        //
        // The last two matter more than they look. A worker can file a card
        // with input bindings but no dependency edges — `depends_on` and
        // `input_bindings` are separate arguments — and if those inputs do not
        // resolve at claim time the card is parked as `dependency`. With only
        // the first two signals it would then have no parents, no run id, and
        // no way back out: stranded in the backlog permanently by the very
        // mechanism meant to reconsider it.
        let promoted: Vec<i64> = sqlx::query_scalar(
            "SELECT task_number FROM tasks t \
             WHERE t.assigned_agent_id = ? \
               AND t.status = 'backlog' \
               AND (t.block_kind IS NULL OR t.block_kind = 'dependency') \
               AND NOT EXISTS (\
                 SELECT 1 FROM task_dependencies d \
                   JOIN tasks p ON p.task_number = d.parent_task_number \
                  WHERE d.child_task_number = t.task_number AND p.status <> 'done') \
               AND (\
                 EXISTS (\
                   SELECT 1 FROM task_dependencies d \
                    WHERE d.child_task_number = t.task_number) \
                 OR t.workflow_run_id IS NOT NULL \
                 OR t.created_by LIKE 'task:%' \
                 OR t.block_kind = 'dependency')",
        )
        .bind(assigned_agent_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to find promotable tasks")?;

        // An unsatisfied gate holds a task exactly as an unfinished parent
        // does. Done as one query over the candidates rather than per task,
        // and phrased as "which of these are held" rather than filtered away in
        // SQL, so the sweep can say *why* a task did not move. A task held by
        // something invisible is the failure mode this whole feature invites.
        let gated = self.gate_holds(&promoted).await?;

        for task_number in promoted {
            if let Some(hold) = gated.iter().find(|held| held.task_number == task_number) {
                sweep.gated.push(hold.clone());
                continue;
            }
            // Parents being done is not the same as the task being runnable.
            //
            // `dependency` covers two conditions that recover differently:
            // waiting on a parent, which this sweep clears, and an input
            // binding that will not resolve, which it cannot — a missing
            // pointer is not fixed by an upstream task finishing. Promoting on
            // parent completion alone put a task with a bad binding into a loop
            // that promoted and re-blocked it every tick forever.
            //
            // So resolution is checked before promotion, not just at claim
            // time. Only tasks that actually declare a contract pay for it.
            match self.resolve_inputs(task_number).await {
                Ok(ContractResolution::Unresolved { problems }) => {
                    sweep.stalled.push(StalledTask {
                        task_number,
                        problems,
                    });
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    // Our failure, not the graph's. Let the claim path decide
                    // rather than pinning the task in the backlog over it.
                    tracing::warn!(
                        %error,
                        task_number,
                        "failed to check inputs while sweeping — promoting anyway"
                    );
                }
            }

            let updated = sqlx::query(
                "UPDATE tasks SET status = 'ready', block_kind = NULL, block_reason = NULL, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
                 WHERE task_number = ? AND status = 'backlog'",
            )
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to promote task")?;
            if updated.rows_affected() > 0 {
                sweep.promoted.push(task_number);
            }
        }

        // Demote: promoted too early, or a parent came back.
        let demoted: Vec<i64> = sqlx::query_scalar(
            "SELECT task_number FROM tasks t \
             WHERE t.assigned_agent_id = ? \
               AND t.status = 'ready' \
               AND EXISTS (\
                 SELECT 1 FROM task_dependencies d \
                   JOIN tasks p ON p.task_number = d.parent_task_number \
                  WHERE d.child_task_number = t.task_number AND p.status <> 'done')",
        )
        .bind(assigned_agent_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to find tasks to demote")?;

        for task_number in demoted {
            let updated = sqlx::query(
                "UPDATE tasks SET status = 'backlog', block_kind = 'dependency', \
                 block_reason = 'waiting on an upstream task', \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
                 WHERE task_number = ? AND status = 'ready'",
            )
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to demote task")?;
            if updated.rows_affected() > 0 {
                sweep.demoted.push(task_number);
            }
        }

        Ok(sweep)
    }

    // -- Worker-filed cards -------------------------------------------------

    /// The task a worker is currently executing, if any.
    pub async fn task_number_for_worker(&self, worker_id: &str) -> Result<Option<i64>> {
        sqlx::query_scalar(
            "SELECT task_number FROM tasks WHERE worker_id = ? ORDER BY task_number DESC LIMIT 1",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to look up the worker's task")
        .map_err(Into::into)
    }

    /// How many tasks a given filer has already created.
    ///
    /// Bounds fan-out. A worker decomposing its task into children is the
    /// mechanism that breaks the delegation depth ceiling, and it is also the
    /// mechanism by which a confused model files two hundred cards nobody asked
    /// for. Hermes leaves this unbounded and their own docs flag it as a risk.
    pub async fn count_tasks_filed_by(&self, created_by: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE created_by = ?")
            .bind(created_by)
            .fetch_one(&self.pool)
            .await
            .context("failed to count filed tasks")
            .map_err(Into::into)
    }

    /// How many filing hops separate this task from a human or an agent.
    ///
    /// A per-task fan-out cap alone still permits `cap^depth` tasks, so depth
    /// needs its own bound. Walks the `created_by` chain, which records the
    /// filing task for worker-filed cards. A cycle is impossible — a task can
    /// only be filed by one that already exists — but the walk is bounded
    /// anyway rather than trusting that.
    pub async fn filing_depth(&self, task_number: i64) -> Result<i64> {
        let mut depth = 0i64;
        let mut current = task_number;

        while depth < MAX_FILING_DEPTH_WALK {
            let created_by: Option<String> =
                sqlx::query_scalar("SELECT created_by FROM tasks WHERE task_number = ?")
                    .bind(current)
                    .fetch_optional(&self.pool)
                    .await
                    .context("failed to read task creator")?;

            let Some(parent) = created_by.as_deref().and_then(parse_filer_task_number) else {
                return Ok(depth);
            };

            depth += 1;
            current = parent;
        }

        Ok(depth)
    }

    /// Of the tasks a worker claims it filed, the ones it did not.
    ///
    /// Server-side verification of a model's claim about its own actions. A
    /// worker reporting children it never created leaves whoever reads the
    /// board believing work is scheduled when it is not — worse than the worker
    /// failing outright, because the failure is invisible.
    pub async fn unverified_filed_tasks(
        &self,
        created_by: &str,
        claimed: &[i64],
    ) -> Result<Vec<i64>> {
        let mut unverified = Vec::new();

        for task_number in claimed {
            let actual: Option<String> =
                sqlx::query_scalar("SELECT created_by FROM tasks WHERE task_number = ?")
                    .bind(task_number)
                    .fetch_optional(&self.pool)
                    .await
                    .context("failed to verify filed task")?;

            if actual.as_deref() != Some(created_by) {
                unverified.push(*task_number);
            }
        }

        Ok(unverified)
    }

    /// Tasks filed by a given filer, for the provenance view.
    pub async fn list_tasks_filed_by(&self, created_by: &str) -> Result<Vec<Task>> {
        let rows = sqlx::query(&format!(
            "{SELECT_COLUMNS} FROM tasks WHERE created_by = ? ORDER BY task_number ASC"
        ))
        .bind(created_by)
        .fetch_all(&self.pool)
        .await
        .context("failed to list filed tasks")?;

        rows.into_iter().map(task_from_row).collect()
    }

    // -- Contracts ----------------------------------------------------------

    /// Declare where one of a task's inputs comes from.
    ///
    /// Replaces any existing binding for the same key, so re-pointing an input
    /// is one call rather than delete-then-add.
    pub async fn set_input_binding(&self, binding: &TaskInputBinding) -> Result<()> {
        sqlx::query(
            "INSERT INTO task_input_bindings \
                 (child_task_number, input_key, source_task_number, source_pointer, literal_value) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (child_task_number, input_key) DO UPDATE SET \
                 source_task_number = excluded.source_task_number, \
                 source_pointer = excluded.source_pointer, \
                 literal_value = excluded.literal_value",
        )
        .bind(binding.child_task_number)
        .bind(&binding.input_key)
        .bind(binding.source_task_number)
        .bind(&binding.source_pointer)
        .bind(binding.literal_value.as_ref().map(|v| v.to_string()))
        .execute(&self.pool)
        .await
        .context("failed to set task input binding")?;

        Ok(())
    }

    pub async fn remove_input_binding(
        &self,
        child_task_number: i64,
        input_key: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM task_input_bindings WHERE child_task_number = ? AND input_key = ?",
        )
        .bind(child_task_number)
        .bind(input_key)
        .execute(&self.pool)
        .await
        .context("failed to remove task input binding")?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_input_bindings(
        &self,
        child_task_number: i64,
    ) -> Result<Vec<TaskInputBinding>> {
        let rows = sqlx::query(
            "SELECT child_task_number, input_key, source_task_number, source_pointer, literal_value \
             FROM task_input_bindings WHERE child_task_number = ? ORDER BY input_key ASC",
        )
        .bind(child_task_number)
        .fetch_all(&self.pool)
        .await
        .context("failed to list task input bindings")?;

        rows.into_iter()
            .map(|row| {
                Ok(TaskInputBinding {
                    child_task_number: row
                        .try_get("child_task_number")
                        .context("failed to read binding child_task_number")?,
                    input_key: row
                        .try_get("input_key")
                        .context("failed to read binding input_key")?,
                    source_task_number: row.try_get("source_task_number").ok().flatten(),
                    source_pointer: row.try_get("source_pointer").ok().flatten(),
                    literal_value: row
                        .try_get::<Option<String>, _>("literal_value")
                        .ok()
                        .flatten()
                        .and_then(|raw| serde_json::from_str(&raw).ok()),
                })
            })
            .collect()
    }

    /// Assemble a task's inputs from its bindings and check them against its
    /// input schema.
    ///
    /// Returns the resolved object on success. Every failure mode here is a
    /// *graph* problem, not a worker problem — the upstream task has not
    /// produced what this one was promised — which is why the caller blocks
    /// with `dependency` rather than spending the failure budget on it.
    pub async fn resolve_inputs(&self, task_number: i64) -> Result<ContractResolution> {
        let Some(task) = self.get_by_number(task_number).await? else {
            return Ok(ContractResolution::Unresolved {
                problems: vec![ContractProblem::TaskMissing { task_number }],
            });
        };

        let bindings = self.list_input_bindings(task_number).await?;

        // No contract and no bindings is the overwhelmingly common case today.
        // Skipping it entirely keeps existing tasks on exactly the old path.
        if bindings.is_empty() && task.input_schema.is_none() {
            return Ok(ContractResolution::NotRequired);
        }

        let mut resolved = serde_json::Map::new();
        let mut problems = Vec::new();

        for binding in &bindings {
            match self.resolve_one_binding(binding).await {
                Ok(value) => {
                    resolved.insert(binding.input_key.clone(), value);
                }
                Err(problem) => problems.push(problem),
            }
        }

        let inputs = Value::Object(resolved);

        if let Some(schema) = &task.input_schema {
            problems.extend(validation_problems(schema, &inputs, ContractSide::Input));
        }

        if problems.is_empty() {
            Ok(ContractResolution::Resolved { inputs })
        } else {
            Ok(ContractResolution::Unresolved { problems })
        }
    }

    async fn resolve_one_binding(
        &self,
        binding: &TaskInputBinding,
    ) -> std::result::Result<Value, ContractProblem> {
        // A literal needs no upstream task at all.
        let Some(source) = binding.source_task_number else {
            return binding
                .literal_value
                .clone()
                .ok_or_else(|| ContractProblem::EmptyLiteral {
                    input_key: binding.input_key.clone(),
                });
        };

        let task = self
            .get_by_number(source)
            .await
            .map_err(|error| ContractProblem::Storage {
                input_key: binding.input_key.clone(),
                message: error.to_string(),
            })?
            .ok_or(ContractProblem::SourceMissing {
                input_key: binding.input_key.clone(),
                source_task_number: source,
            })?;

        let outputs = task.outputs.ok_or(ContractProblem::SourceHasNoOutputs {
            input_key: binding.input_key.clone(),
            source_task_number: source,
        })?;

        let pointer = binding.source_pointer.as_deref().unwrap_or("");
        // RFC 6901: the empty pointer selects the whole document.
        let value = if pointer.is_empty() {
            Some(&outputs)
        } else {
            outputs.pointer(pointer)
        };

        value
            .cloned()
            .ok_or_else(|| ContractProblem::PointerMissed {
                input_key: binding.input_key.clone(),
                source_task_number: source,
                pointer: pointer.to_string(),
            })
    }

    /// Persist a task's resolved inputs.
    pub async fn set_inputs(&self, task_number: i64, inputs: &Value) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET inputs = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ?",
        )
        .bind(inputs.to_string())
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to persist task inputs")?;

        Ok(())
    }

    /// Check a proposed output against the task's declared output schema and
    /// persist it if it fits.
    ///
    /// Rejecting is the entire point. Without it a contract is a comment: the
    /// worker says it produced something, nothing checks, and the downstream
    /// task discovers the gap at runtime with no idea who broke it. The
    /// rejection carries the validation errors so the worker can correct and
    /// retry inside its own budget rather than failing the task.
    pub async fn submit_outputs(
        &self,
        task_number: i64,
        outputs: &Value,
    ) -> Result<OutputSubmission> {
        let Some(task) = self.get_by_number(task_number).await? else {
            return Ok(OutputSubmission::TaskMissing);
        };

        if let Some(schema) = &task.output_schema {
            let problems = validation_problems(schema, outputs, ContractSide::Output);
            if !problems.is_empty() {
                return Ok(OutputSubmission::Rejected { problems });
            }
        }

        sqlx::query(
            "UPDATE tasks SET outputs = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ?",
        )
        .bind(outputs.to_string())
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to persist task outputs")?;

        Ok(OutputSubmission::Accepted)
    }

    /// Submit outputs for the task a specific worker is executing.
    ///
    /// Scoped exactly like `update_worker_task`: a worker may only complete the
    /// task it was spawned for. Without this a worker could write outputs onto
    /// another agent's task, and downstream tasks would consume them as fact.
    pub async fn submit_worker_outputs(
        &self,
        worker_id: &str,
        task_number: i64,
        outputs: &Value,
    ) -> Result<WorkerOutputSubmission> {
        let owns: Option<i64> = sqlx::query_scalar(
            "SELECT task_number FROM tasks WHERE worker_id = ? AND task_number = ?",
        )
        .bind(worker_id)
        .bind(task_number)
        .fetch_optional(&self.pool)
        .await
        .context("failed to check worker task ownership")?;

        if owns.is_none() {
            let assigned: Option<i64> = sqlx::query_scalar(
                "SELECT task_number FROM tasks WHERE worker_id = ? \
                 ORDER BY task_number DESC LIMIT 1",
            )
            .bind(worker_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to look up the worker's own task")?;

            return Ok(match assigned {
                Some(assigned_task_number) => WorkerOutputSubmission::WrongTask {
                    assigned_task_number,
                },
                None => WorkerOutputSubmission::NotAssigned,
            });
        }

        Ok(WorkerOutputSubmission::Submitted(
            self.submit_outputs(task_number, outputs).await?,
        ))
    }

    /// Set or clear a task's declared contract.
    pub async fn set_contract(
        &self,
        task_number: i64,
        input_schema: Option<&Value>,
        output_schema: Option<&Value>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET input_schema = ?, output_schema = ?, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE task_number = ?",
        )
        .bind(input_schema.map(|value| value.to_string()))
        .bind(output_schema.map(|value| value.to_string()))
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to set task contract")?;

        Ok(())
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

    /// The still-open attempt for a task, if one exists.
    ///
    /// An attempt with no `ended_at` means the process died before it could
    /// close the row — the reaper uses this to write a terminal outcome rather
    /// than leaving the log claiming the work is still running.
    pub async fn open_run(&self, task_number: i64) -> Result<Option<TaskRun>> {
        let row = sqlx::query(&format!(
            "{RUN_SELECT_COLUMNS} FROM task_runs \
             WHERE task_number = ? AND ended_at IS NULL \
             ORDER BY attempt DESC LIMIT 1"
        ))
        .bind(task_number)
        .fetch_optional(&self.pool)
        .await
        .context("failed to look up open task run")?;

        row.map(task_run_from_row).transpose()
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
            "SELECT status, consecutive_failures, max_retries FROM tasks WHERE task_number = ?",
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

        // A worker runs for minutes. If a human completed, cancelled, or
        // reassigned the task in that window, the attempt's failure must not
        // drag it back to `ready`. `BEGIN IMMEDIATE` holds the write lock, so
        // this read and the update below cannot interleave with another writer.
        let current_status = row
            .try_get::<String, _>("status")
            .ok()
            .and_then(|value| TaskStatus::parse(&value));
        if current_status != Some(TaskStatus::InProgress) {
            tx.commit()
                .await
                .context("failed to commit no-op failure budget transaction")?;
            return Ok(FailureDisposition::NoLongerRunning {
                status: current_status,
            });
        }

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

        // A parked task carries a reason. Exhausting the budget is a
        // `transient` block: repeated failures nobody classified further.
        // Requeued tasks clear any stale reason so the card does not keep
        // showing why a previous attempt stopped.
        let block_kind = exhausted.then_some(BlockKind::Transient.as_str());
        let block_reason = exhausted.then_some(error);

        sqlx::query(
            "UPDATE tasks SET consecutive_failures = ?, last_error = ?, status = ?, \
             block_kind = ?, block_reason = ?, \
             worker_id = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE task_number = ? AND status = 'in_progress'",
        )
        .bind(failures)
        .bind(error)
        .bind(next_status.as_str())
        .bind(block_kind)
        .bind(block_reason)
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

/// Why a dependency edge was refused.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DependencyError {
    #[error("task #{task_number} cannot depend on itself")]
    SelfLoop { task_number: i64 },
    #[error("task #{task_number} does not exist")]
    UnknownTask { task_number: i64 },
    #[error(
        "that edge would create a cycle: {}",
        .path.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(" -> ")
    )]
    WouldCycle { path: Vec<i64> },
    #[error("dependency storage error: {0}")]
    Storage(String),
}

/// What [`TaskStore::block_task`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockOutcome {
    /// Where the task came to rest.
    pub status: TaskStatus,
    pub kind: BlockKind,
    /// Consecutive blocks for this same kind.
    pub recurrences: i64,
    /// Whether the recurrence limit forced an escalation to a human.
    pub escalated: bool,
}

/// Where one of a task's inputs comes from.
///
/// Either a pointer into an upstream task's outputs, or a literal baked into
/// the graph. `source_task_number` being `None` means literal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskInputBinding {
    pub child_task_number: i64,
    /// Key in the child's input object.
    pub input_key: String,
    /// Upstream task to read from. `None` for a literal.
    pub source_task_number: Option<i64>,
    /// RFC 6901 JSON Pointer into that task's outputs. Empty selects the whole
    /// outputs object.
    pub source_pointer: Option<String>,
    /// JSON literal, used when `source_task_number` is `None`.
    pub literal_value: Option<Value>,
}

/// Which half of a contract a problem came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractSide {
    Input,
    Output,
}

/// A specific reason a contract could not be satisfied.
///
/// Deliberately granular. "Validation failed" sends a human reading prompts and
/// guessing; naming the key and the upstream task that should have supplied it
/// points straight at the broken edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContractProblem {
    #[error("task #{task_number} does not exist")]
    TaskMissing { task_number: i64 },
    #[error("input `{input_key}` is bound to task #{source_task_number}, which does not exist")]
    SourceMissing {
        input_key: String,
        source_task_number: i64,
    },
    #[error(
        "input `{input_key}` needs output from task #{source_task_number}, which has not produced any yet"
    )]
    SourceHasNoOutputs {
        input_key: String,
        source_task_number: i64,
    },
    #[error("input `{input_key}`: task #{source_task_number} produced no value at `{pointer}`")]
    PointerMissed {
        input_key: String,
        source_task_number: i64,
        pointer: String,
    },
    #[error("input `{input_key}` is declared a literal but carries no value")]
    EmptyLiteral { input_key: String },
    #[error("{side:?} at `{path}` does not match the declared schema: {message}")]
    SchemaViolation {
        side: ContractSide,
        /// JSON Pointer to the offending value, `""` for the document root.
        path: String,
        message: String,
    },
    #[error("declared {side:?} schema is not a valid JSON Schema: {message}")]
    InvalidSchema { side: ContractSide, message: String },
    #[error("input `{input_key}` could not be read: {message}")]
    Storage { input_key: String, message: String },
}

/// The result of assembling a task's inputs.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractResolution {
    /// The task declares no contract and has no bindings — nothing to do.
    NotRequired,
    /// Inputs assembled and validated.
    Resolved { inputs: Value },
    /// The graph cannot supply what this task was promised.
    Unresolved { problems: Vec<ContractProblem> },
}

/// The result of a worker submitting outputs for its own task.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerOutputSubmission {
    Submitted(OutputSubmission),
    /// The worker is not bound to any task.
    NotAssigned,
    /// The worker tried to write to a task other than its own.
    WrongTask {
        assigned_task_number: i64,
    },
}

/// The result of a worker submitting its outputs.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputSubmission {
    Accepted,
    /// The output does not match the declared schema. The worker is told why
    /// and may correct it within its own segment budget.
    Rejected {
        problems: Vec<ContractProblem>,
    },
    TaskMissing,
}

/// Validate `value` against `schema`, converting failures into problems.
///
/// A schema that will not compile is itself reported as a problem rather than
/// silently skipped: a task declaring an unusable contract is misconfigured,
/// and quietly accepting anything would hide that.
fn validation_problems(schema: &Value, value: &Value, side: ContractSide) -> Vec<ContractProblem> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(validator) => validator,
        Err(error) => {
            return vec![ContractProblem::InvalidSchema {
                side,
                message: error.to_string(),
            }];
        }
    };

    validator
        .iter_errors(value)
        .map(|error| ContractProblem::SchemaViolation {
            side,
            path: error.instance_path().to_string(),
            message: error.to_string(),
        })
        .collect()
}

/// How many edges touch a task, and how many still gate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskEdgeSummary {
    pub task_number: i64,
    /// Tasks this one waits on.
    pub parents: i64,
    /// Tasks waiting on this one.
    pub children: i64,
    /// The subset of `parents` that has not finished — why the task is waiting.
    pub blocked_by: i64,
}

/// A task whose parents are all done but whose inputs still cannot be built.
///
/// Distinct from "waiting": nothing upstream is going to change on its own, so
/// this is a graph a person has to repair.
#[derive(Debug, Clone, PartialEq)]
pub struct StalledTask {
    pub task_number: i64,
    pub problems: Vec<ContractProblem>,
}

/// What a [`TaskStore::recompute_ready`] pass changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadySweep {
    /// Tasks whose parents all finished, now eligible for pickup.
    pub promoted: Vec<i64>,
    /// Tasks that were eligible but should not have been.
    pub demoted: Vec<i64>,
    /// Tasks held back because their inputs do not resolve. Reported rather
    /// than promoted-and-rejected, so the loop never starts.
    pub stalled: Vec<StalledTask>,
    /// Tasks otherwise ready but held by an external gate, with the reason.
    ///
    /// Reported separately from `stalled` because the recovery differs: a
    /// stalled task needs its graph fixed, a gated one needs the outside world
    /// to change — or, if the gate has failed or cannot be reached, a person.
    pub gated: Vec<GatedTask>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GatedTask {
    pub task_number: i64,
    /// One line per gate holding it, already phrased for a human.
    pub reasons: Vec<String>,
}

impl ReadySweep {
    pub fn is_empty(&self) -> bool {
        self.promoted.is_empty()
            && self.demoted.is_empty()
            && self.stalled.is_empty()
            && self.gated.is_empty()
    }
}

/// Walk the edges downward from `start`, looking for `target`.
///
/// Returns the path that reaches it, so a rejection can name the loop instead
/// of just asserting one exists. Iterative rather than recursive: the graph is
/// user-built and a deep chain must not blow the stack.
async fn reachable_path(
    tx: &mut sqlx::SqliteConnection,
    start: i64,
    target: i64,
) -> std::result::Result<Option<Vec<i64>>, DependencyError> {
    // Each entry is the path taken to reach its final node, so the answer is
    // available without a second traversal to reconstruct it.
    let mut frontier: Vec<Vec<i64>> = vec![vec![start]];
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    seen.insert(start);

    while let Some(path) = frontier.pop() {
        let node = *path.last().expect("paths are never empty");
        if node == target {
            return Ok(Some(path));
        }

        let children: Vec<i64> = sqlx::query_scalar(
            "SELECT child_task_number FROM task_dependencies WHERE parent_task_number = ?",
        )
        .bind(node)
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| DependencyError::Storage(error.to_string()))?;

        for child in children {
            if seen.insert(child) {
                let mut next = path.clone();
                next.push(child);
                frontier.push(next);
            }
        }
    }

    Ok(None)
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
    /// The task moved out of `in_progress` while the worker was in flight — a
    /// human completed, reassigned, or requeued it. Left untouched.
    NoLongerRunning { status: Option<TaskStatus> },
    /// The task row disappeared between execution and bookkeeping.
    TaskMissing,
}

/// Column list used by all SELECT queries. Kept in sync with `task_from_row`.
const SELECT_COLUMNS: &str = "SELECT id, task_number, title, description, status, priority, \
     owner_agent_id, assigned_agent_id, subtasks, metadata, source_memory_id, worker_id, \
     created_by, approved_at, approved_by, created_at, updated_at, completed_at, \
     consecutive_failures, max_retries, last_error, project_id, repo_id, worktree_id, \
     block_kind, block_reason, block_recurrences, last_block_kind, \
     input_schema, output_schema, inputs, outputs, \
     workflow_run_id, workflow_step_key, system_prompt";

const RUN_SELECT_COLUMNS: &str = "SELECT id, task_number, attempt, worker_id, outcome, \
     summary, error, started_at, ended_at";

/// The single source of truth for legal status transitions, enforced by the
/// store on every update.
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

/// Every legal `(from, to)` status move.
///
/// Exported over the API so the dashboard's drag-and-drop consumes the same
/// table the store enforces. Hermes renders a board column its PATCH handler
/// has no branch for, so dragging a card there 400s — the failure mode of
/// keeping two copies of this in two languages.
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
        block_kind: row
            .try_get::<Option<String>, _>("block_kind")
            .ok()
            .flatten()
            .as_deref()
            .and_then(BlockKind::parse),
        block_reason: row
            .try_get::<Option<String>, _>("block_reason")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        block_recurrences: row.try_get("block_recurrences").unwrap_or(0),
        last_block_kind: row
            .try_get::<Option<String>, _>("last_block_kind")
            .ok()
            .flatten()
            .as_deref()
            .and_then(BlockKind::parse),
        input_schema: read_optional_json(&row, "input_schema"),
        output_schema: read_optional_json(&row, "output_schema"),
        inputs: read_optional_json(&row, "inputs"),
        outputs: read_optional_json(&row, "outputs"),
        workflow_run_id: read_optional_id(&row, "workflow_run_id"),
        workflow_step_key: read_optional_id(&row, "workflow_step_key"),
        system_prompt: row
            .try_get::<Option<String>, _>("system_prompt")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
    })
}

/// Read a nullable TEXT column holding JSON.
///
/// A column that fails to parse is treated as absent rather than failing the
/// whole read: one malformed contract should not make a task unreadable.
fn read_optional_json(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<Value> {
    let raw = row
        .try_get::<Option<String>, _>(column)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())?;

    match serde_json::from_str(&raw) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, column, "task column held invalid JSON — treating as absent");
            None
        }
    }
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
            worktree_id TEXT,
            block_kind TEXT,
            block_reason TEXT,
            block_recurrences INTEGER NOT NULL DEFAULT 0,
            last_block_kind TEXT,
            input_schema TEXT,
            output_schema TEXT,
            inputs TEXT,
            outputs TEXT,
            workflow_run_id TEXT,
            workflow_step_key TEXT,
            system_prompt TEXT
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
        r#"
        CREATE TABLE task_dependencies (
            parent_task_number INTEGER NOT NULL,
            child_task_number INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            PRIMARY KEY (parent_task_number, child_task_number)
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("task_dependencies schema should be created");

    sqlx::query(
        r#"
        CREATE TABLE task_input_bindings (
            child_task_number INTEGER NOT NULL,
            input_key TEXT NOT NULL,
            source_task_number INTEGER,
            source_pointer TEXT,
            literal_value TEXT,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            PRIMARY KEY (child_task_number, input_key)
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("task_input_bindings schema should be created");

    sqlx::query(
        r#"
        CREATE TABLE task_gates (
            id TEXT PRIMARY KEY NOT NULL,
            task_number INTEGER NOT NULL,
            kind TEXT NOT NULL,
            config TEXT NOT NULL,
            label TEXT,
            poll_interval_secs INTEGER NOT NULL DEFAULT 60,
            last_checked_at TEXT,
            last_result TEXT NOT NULL DEFAULT 'pending',
            last_detail TEXT,
            consecutive_errors INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("task_gates schema should be created");

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

        // Rebind only the repo. The project must survive untouched — naming one
        // column in a patch must never null its siblings.
        let rebound = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    binding: TaskBindingPatch {
                        repo_id: Some(Some("repo-2".into())),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(rebound.repo_id.as_deref(), Some("repo-2"));
        assert_eq!(
            rebound.project_id.as_deref(),
            Some("proj-a"),
            "patching the repo must not unbind the project"
        );

        // A single column can also be cleared on its own.
        let repo_cleared = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    binding: TaskBindingPatch {
                        repo_id: Some(None),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert!(repo_cleared.repo_id.is_none());
        assert_eq!(repo_cleared.project_id.as_deref(), Some("proj-a"));

        // Put the repo back for the remaining assertions.
        let rebound = store
            .update(
                created.task_number,
                UpdateTaskInput {
                    binding: TaskBindingPatch {
                        repo_id: Some(Some("repo-2".into())),
                        ..Default::default()
                    },
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

        // The requeued task gets picked up again before it can fail again —
        // `record_failure` only acts on a task that is actually running.
        store
            .update(
                task.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .await
            .expect("re-claim")
            .expect("exists");

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
    async fn failure_does_not_override_a_status_changed_mid_run() {
        let store = setup_store().await;
        let task = store
            .create(self_assigned_input("long runner", TaskStatus::InProgress))
            .await
            .expect("should create");

        // A human marks it done while the worker is still in flight.
        store
            .update(
                task.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");

        // The worker then fails. The human's decision must win.
        let disposition = store
            .record_failure(task.task_number, TaskRunOutcome::Failed, "too late")
            .await
            .expect("record failure");
        assert_eq!(
            disposition,
            FailureDisposition::NoLongerRunning {
                status: Some(TaskStatus::Done)
            }
        );

        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(
            after.status,
            TaskStatus::Done,
            "a late failure must not resurrect a task somebody already closed"
        );
        assert_eq!(after.consecutive_failures, 0);
        assert!(after.last_error.is_none());
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

        // Burn the budget so the task lands in Blocked, going through a real
        // claim each round — `record_failure` only acts on a running task.
        for round in 0..DEFAULT_FAILURE_LIMIT {
            if round > 0 {
                store
                    .claim_next_ready("agent-test")
                    .await
                    .expect("claim should succeed")
                    .expect("a requeued task must be claimable again");
            }
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
    // -- Dependencies -------------------------------------------------------

    async fn task_at(store: &TaskStore, title: &str, status: TaskStatus) -> Task {
        store
            .create(self_assigned_input(title, status))
            .await
            .expect("should create")
    }

    async fn finish(store: &TaskStore, task_number: i64) {
        store
            .update(
                task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
    }

    #[tokio::test]
    async fn link_rejects_self_loops_and_unknown_tasks() {
        let store = setup_store().await;
        let task = task_at(&store, "lonely", TaskStatus::Backlog).await;

        let self_loop = store
            .link_tasks(task.task_number, task.task_number)
            .await
            .expect_err("a task must not depend on itself");
        assert!(matches!(self_loop, DependencyError::SelfLoop { .. }));

        let unknown = store
            .link_tasks(9999, task.task_number)
            .await
            .expect_err("an edge to a task that does not exist must be refused");
        assert!(matches!(unknown, DependencyError::UnknownTask { .. }));
    }

    /// A cycle found while scheduling is a deadlock nobody can see. Reject at
    /// link time, and name the path so the caller knows which edge conflicts.
    #[tokio::test]
    async fn link_rejects_a_three_node_cycle() {
        let store = setup_store().await;
        let a = task_at(&store, "a", TaskStatus::Backlog).await;
        let b = task_at(&store, "b", TaskStatus::Backlog).await;
        let c = task_at(&store, "c", TaskStatus::Backlog).await;

        store
            .link_tasks(a.task_number, b.task_number)
            .await
            .expect("a -> b");
        store
            .link_tasks(b.task_number, c.task_number)
            .await
            .expect("b -> c");

        let error = store
            .link_tasks(c.task_number, a.task_number)
            .await
            .expect_err("c -> a closes the loop");
        match error {
            DependencyError::WouldCycle { path } => {
                assert_eq!(
                    path,
                    vec![a.task_number, b.task_number, c.task_number],
                    "the rejection must name the path that would close"
                );
            }
            other => panic!("expected WouldCycle, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_child_is_not_claimable_until_every_parent_is_done() {
        let store = setup_store().await;
        let first = task_at(&store, "parent one", TaskStatus::InProgress).await;
        let second = task_at(&store, "parent two", TaskStatus::InProgress).await;
        let child = task_at(&store, "child", TaskStatus::Ready).await;

        for parent in [&first, &second] {
            store
                .link_tasks(parent.task_number, child.task_number)
                .await
                .expect("link");
        }

        assert!(
            store
                .claim_next_ready("agent-test")
                .await
                .expect("claim")
                .is_none(),
            "a ready task with unfinished parents must not be claimable"
        );

        finish(&store, first.task_number).await;
        assert!(
            store
                .claim_next_ready("agent-test")
                .await
                .expect("claim")
                .is_none(),
            "one remaining parent still gates the child"
        );

        finish(&store, second.task_number).await;
        let claimed = store
            .claim_next_ready("agent-test")
            .await
            .expect("claim")
            .expect("the child becomes claimable once every parent is done");
        assert_eq!(claimed.task_number, child.task_number);
    }

    #[tokio::test]
    async fn sweep_promotes_a_child_when_the_last_parent_finishes() {
        let store = setup_store().await;
        let parent = task_at(&store, "parent", TaskStatus::InProgress).await;
        let child = task_at(&store, "child", TaskStatus::Backlog).await;
        store
            .link_tasks(parent.task_number, child.task_number)
            .await
            .expect("link");

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert!(sweep.is_empty(), "nothing to do while the parent runs");

        finish(&store, parent.task_number).await;

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert_eq!(sweep.promoted, vec![child.task_number]);
        let after = store
            .get_by_number(child.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.status, TaskStatus::Ready);
        assert!(
            after.block_kind.is_none(),
            "promotion clears the block reason"
        );
    }

    /// Drift repair: an edge added after a task was already promoted, or a
    /// parent reopened, must pull the child back out of the ready queue.
    #[tokio::test]
    async fn sweep_demotes_a_ready_task_that_gained_an_unfinished_parent() {
        let store = setup_store().await;
        let parent = task_at(&store, "parent", TaskStatus::InProgress).await;
        let child = task_at(&store, "child", TaskStatus::Ready).await;
        store
            .link_tasks(parent.task_number, child.task_number)
            .await
            .expect("link");

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert_eq!(sweep.demoted, vec![child.task_number]);
        let after = store
            .get_by_number(child.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.status, TaskStatus::Backlog);
        assert_eq!(after.block_kind, Some(BlockKind::Dependency));
    }

    /// The reason typed blocks exist. A human parked these knowing the task
    /// could not proceed; a sweep that resurrects them hands the worker the
    /// same dead end and throws the human's decision away.
    #[tokio::test]
    async fn sweep_never_resurrects_a_sticky_block() {
        for kind in [BlockKind::NeedsInput, BlockKind::Capability] {
            let store = setup_store().await;
            let task = task_at(&store, "parked", TaskStatus::InProgress).await;
            store
                .block_task(task.task_number, kind, "needs a human")
                .await
                .expect("block")
                .expect("task exists");

            let sweep = store.recompute_ready("agent-test").await.expect("sweep");
            assert!(sweep.is_empty(), "{kind} must not be swept back to ready");

            let after = store
                .get_by_number(task.task_number)
                .await
                .expect("fetch")
                .expect("exists");
            assert_eq!(after.status, TaskStatus::Blocked);
            assert_eq!(after.block_kind, Some(kind));
        }
    }

    /// A gate holds a task exactly as an unfinished parent does.
    ///
    /// The whole feature is worthless if the sweep does not honour it: a gated
    /// task would be promoted, claimed, and run while CI was still red.
    #[tokio::test]
    async fn an_unsatisfied_gate_holds_a_task_the_sweep_would_otherwise_promote() {
        let store = setup_store().await;
        let parent = task_at(&store, "build", TaskStatus::InProgress).await;
        let child = task_at(&store, "deploy", TaskStatus::Backlog).await;
        store
            .link_tasks(parent.task_number, child.task_number)
            .await
            .expect("link");
        store
            .update(
                parent.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("finish parent");

        let gates = crate::tasks::GateStore::new(store.pool().clone());
        gates
            .create(
                child.task_number,
                crate::tasks::GateKind::Http,
                &serde_json::json!({"url": "https://ci.example/status", "expect_status": 200}),
                Some("CI on main"),
                60,
            )
            .await
            .expect("create gate");

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert!(
            sweep.promoted.is_empty(),
            "every parent is done, but the gate has not opened"
        );
        assert_eq!(
            sweep.gated.len(),
            1,
            "the hold must be reported, not silent"
        );
        assert_eq!(sweep.gated[0].task_number, child.task_number);
        assert!(
            sweep.gated[0].reasons[0].contains("CI on main"),
            "the reason should name the gate: {:?}",
            sweep.gated[0].reasons
        );

        let after = store
            .get_by_number(child.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.status, TaskStatus::Backlog);
    }

    /// And releases it the moment the gate opens — with no other change.
    #[tokio::test]
    async fn a_satisfied_gate_stops_holding() {
        let store = setup_store().await;
        let parent = task_at(&store, "build", TaskStatus::InProgress).await;
        let child = task_at(&store, "deploy", TaskStatus::Backlog).await;
        store
            .link_tasks(parent.task_number, child.task_number)
            .await
            .expect("link");
        store
            .update(
                parent.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("finish parent");

        let gates = crate::tasks::GateStore::new(store.pool().clone());
        let gate = gates
            .create(
                child.task_number,
                crate::tasks::GateKind::TaskOutput,
                &serde_json::json!({"task_number": parent.task_number, "pointer": "/state"}),
                None,
                60,
            )
            .await
            .expect("create gate");

        assert!(
            store
                .recompute_ready("agent-test")
                .await
                .expect("sweep")
                .promoted
                .is_empty()
        );

        gates
            .record_evaluation(
                &gate.id,
                &crate::tasks::Evaluation {
                    result: crate::tasks::GateResult::Satisfied,
                    detail: "`/state` is success".into(),
                },
            )
            .await
            .expect("record");

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert_eq!(sweep.promoted, vec![child.task_number]);
        assert!(sweep.gated.is_empty());
    }

    /// A dependency wait is ordinary scheduling, not an incident: it rests in
    /// the backlog rather than the blocked column a human is meant to triage.
    #[tokio::test]
    async fn a_dependency_block_rests_in_backlog_not_blocked() {
        let store = setup_store().await;
        let task = task_at(&store, "waiting", TaskStatus::InProgress).await;

        let outcome = store
            .block_task(task.task_number, BlockKind::Dependency, "waiting on #1")
            .await
            .expect("block")
            .expect("task exists");

        assert_eq!(outcome.status, TaskStatus::Backlog);
        assert!(!outcome.escalated);
        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.status, TaskStatus::Backlog);
    }

    /// The loop breaker: a sweep that unblocks and a worker that re-blocks will
    /// otherwise trade a card forever, burning a worker spawn each round.
    #[tokio::test]
    async fn repeated_blocks_for_the_same_reason_escalate_to_a_human() {
        let store = setup_store().await;
        let task = task_at(&store, "bouncing", TaskStatus::InProgress).await;

        // The first block is not a recurrence — it is just a block.
        let first = store
            .block_task(task.task_number, BlockKind::Capability, "no credential")
            .await
            .expect("block")
            .expect("exists");
        assert_eq!(first.recurrences, 0);
        assert!(!first.escalated);
        assert_eq!(first.status, TaskStatus::Blocked);

        // Hitting the same wall again is tolerated once.
        let second = store
            .block_task(task.task_number, BlockKind::Capability, "no credential")
            .await
            .expect("block")
            .expect("exists");
        assert_eq!(second.recurrences, 1);
        assert!(!second.escalated);

        // Twice over is a loop, not bad luck.
        let third = store
            .block_task(task.task_number, BlockKind::Capability, "no credential")
            .await
            .expect("block")
            .expect("exists");
        assert_eq!(third.recurrences, BLOCK_RECURRENCE_LIMIT);
        assert!(third.escalated);
        assert_eq!(
            third.status,
            TaskStatus::PendingApproval,
            "escalation must land somewhere that raises a notification"
        );
    }

    /// Different obstacles in sequence are progress, not a loop — escalating
    /// those would be noise.
    #[tokio::test]
    async fn blocks_for_different_reasons_do_not_escalate() {
        let store = setup_store().await;
        let task = task_at(&store, "varied", TaskStatus::InProgress).await;

        store
            .block_task(task.task_number, BlockKind::Capability, "no credential")
            .await
            .expect("block")
            .expect("exists");
        let second = store
            .block_task(task.task_number, BlockKind::NeedsInput, "which region?")
            .await
            .expect("block")
            .expect("exists");

        assert_eq!(second.recurrences, 0);
        assert!(!second.escalated);
        assert_eq!(second.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn unblock_lands_in_ready_or_backlog_depending_on_parents() {
        let store = setup_store().await;

        let free = task_at(&store, "free", TaskStatus::InProgress).await;
        store
            .block_task(free.task_number, BlockKind::NeedsInput, "?")
            .await
            .expect("block")
            .expect("exists");
        let released = store
            .unblock_task(free.task_number)
            .await
            .expect("unblock")
            .expect("exists");
        assert_eq!(released.status, TaskStatus::Ready);
        assert!(released.block_kind.is_none());

        let parent = task_at(&store, "parent", TaskStatus::InProgress).await;
        let gated = task_at(&store, "gated", TaskStatus::InProgress).await;
        store
            .link_tasks(parent.task_number, gated.task_number)
            .await
            .expect("link");
        store
            .block_task(gated.task_number, BlockKind::NeedsInput, "?")
            .await
            .expect("block")
            .expect("exists");
        let released = store
            .unblock_task(gated.task_number)
            .await
            .expect("unblock")
            .expect("exists");
        assert_eq!(
            released.status,
            TaskStatus::Backlog,
            "unblocking must not jump the dependency queue"
        );
    }

    #[tokio::test]
    async fn fan_in_and_fan_out_edges_round_trip() {
        let store = setup_store().await;
        let hub = task_at(&store, "hub", TaskStatus::Backlog).await;
        let mut parents = Vec::new();
        let mut children = Vec::new();

        for index in 0..3 {
            let parent = task_at(&store, &format!("parent {index}"), TaskStatus::Backlog).await;
            store
                .link_tasks(parent.task_number, hub.task_number)
                .await
                .expect("fan-in link");
            parents.push(parent.task_number);

            let child = task_at(&store, &format!("child {index}"), TaskStatus::Backlog).await;
            store
                .link_tasks(hub.task_number, child.task_number)
                .await
                .expect("fan-out link");
            children.push(child.task_number);
        }

        parents.sort_unstable();
        children.sort_unstable();
        assert_eq!(
            store.list_parents(hub.task_number).await.expect("parents"),
            parents
        );
        assert_eq!(
            store
                .list_children(hub.task_number)
                .await
                .expect("children"),
            children
        );
        assert_eq!(
            store
                .unfinished_parents(hub.task_number)
                .await
                .expect("unfinished"),
            parents,
            "no parent has finished yet"
        );
    }

    #[tokio::test]
    async fn create_with_depends_on_parks_the_task_and_links_it() {
        let store = setup_store().await;
        let parent = task_at(&store, "upstream", TaskStatus::InProgress).await;

        let child = store
            .create(CreateTaskInput {
                depends_on: vec![parent.task_number],
                ..self_assigned_input("downstream", TaskStatus::Ready)
            })
            .await
            .expect("create with dependency");

        assert_eq!(
            child.status,
            TaskStatus::Backlog,
            "a task created with unmet dependencies must not start out claimable"
        );
        assert_eq!(child.block_kind, Some(BlockKind::Dependency));
        assert_eq!(
            store
                .list_parents(child.task_number)
                .await
                .expect("parents"),
            vec![parent.task_number]
        );
    }

    /// A rejected edge must not leave a task behind: a half-linked task is
    /// worse than none, because the scheduler would happily run it early.
    #[tokio::test]
    async fn create_with_a_bad_dependency_leaves_no_orphan() {
        let store = setup_store().await;
        let before = store
            .list(TaskListFilter::default())
            .await
            .expect("list")
            .len();

        let result = store
            .create(CreateTaskInput {
                depends_on: vec![4242],
                ..self_assigned_input("doomed", TaskStatus::Ready)
            })
            .await;

        assert!(result.is_err(), "an unknown parent must fail the create");
        assert_eq!(
            store
                .list(TaskListFilter::default())
                .await
                .expect("list")
                .len(),
            before,
            "the failed create must not leave a task behind"
        );
    }

    /// F1's budget parks a task; F3 says why. Without a kind the sweep cannot
    /// tell it apart from a dependency wait.
    #[tokio::test]
    async fn an_exhausted_budget_parks_the_task_as_transient() {
        let store = setup_store().await;
        let task = task_at(&store, "doomed", TaskStatus::InProgress).await;

        for round in 0..DEFAULT_FAILURE_LIMIT {
            if round > 0 {
                store
                    .claim_next_ready("agent-test")
                    .await
                    .expect("claim")
                    .expect("requeued");
            }
            store
                .record_failure(task.task_number, TaskRunOutcome::Failed, "boom")
                .await
                .expect("record failure");
        }

        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.status, TaskStatus::Blocked);
        assert_eq!(after.block_kind, Some(BlockKind::Transient));
        assert_eq!(after.block_reason.as_deref(), Some("boom"));
    }
    #[tokio::test]
    async fn dependency_summaries_counts_both_directions_in_one_pass() {
        let store = setup_store().await;
        let done_parent = task_at(&store, "done parent", TaskStatus::InProgress).await;
        let live_parent = task_at(&store, "live parent", TaskStatus::InProgress).await;
        let hub = task_at(&store, "hub", TaskStatus::Backlog).await;
        let child = task_at(&store, "child", TaskStatus::Backlog).await;
        let lonely = task_at(&store, "lonely", TaskStatus::Backlog).await;

        for parent in [&done_parent, &live_parent] {
            store
                .link_tasks(parent.task_number, hub.task_number)
                .await
                .expect("link");
        }
        store
            .link_tasks(hub.task_number, child.task_number)
            .await
            .expect("link");
        finish(&store, done_parent.task_number).await;

        let summaries = store.dependency_summaries().await.expect("summaries");
        let by_number: std::collections::HashMap<i64, TaskEdgeSummary> = summaries
            .into_iter()
            .map(|summary| (summary.task_number, summary))
            .collect();

        let hub_summary = by_number.get(&hub.task_number).expect("hub has edges");
        assert_eq!(hub_summary.parents, 2);
        assert_eq!(hub_summary.children, 1);
        assert_eq!(
            hub_summary.blocked_by, 1,
            "only the unfinished parent still gates the hub"
        );

        let child_summary = by_number.get(&child.task_number).expect("child has edges");
        assert_eq!(child_summary.parents, 1);
        assert_eq!(child_summary.children, 0);
        assert_eq!(child_summary.blocked_by, 1);

        assert!(
            !by_number.contains_key(&lonely.task_number),
            "a task with no edges must be absent, not present with zeroes"
        );
    }
    // -- Contracts ----------------------------------------------------------

    fn tag_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["tag"],
            "properties": {"tag": {"type": "string"}},
        })
    }

    #[tokio::test]
    async fn a_binding_resolves_through_a_json_pointer_into_a_parent_output() {
        let store = setup_store().await;
        let parent = task_at(&store, "build", TaskStatus::InProgress).await;
        let child = task_at(&store, "deploy", TaskStatus::Backlog).await;

        let accepted = store
            .submit_outputs(
                parent.task_number,
                &serde_json::json!({"image": {"tag": "v1.4.2", "digest": "sha256:abc"}}),
            )
            .await
            .expect("submit");
        assert_eq!(accepted, OutputSubmission::Accepted);

        store
            .set_contract(child.task_number, Some(&tag_schema()), None)
            .await
            .expect("set contract");
        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: child.task_number,
                input_key: "tag".into(),
                source_task_number: Some(parent.task_number),
                source_pointer: Some("/image/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let resolution = store
            .resolve_inputs(child.task_number)
            .await
            .expect("resolve");
        assert_eq!(
            resolution,
            ContractResolution::Resolved {
                inputs: serde_json::json!({"tag": "v1.4.2"})
            }
        );
    }

    #[tokio::test]
    async fn a_literal_binding_needs_no_upstream_task() {
        let store = setup_store().await;
        let task = task_at(&store, "deploy", TaskStatus::Backlog).await;

        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: task.task_number,
                input_key: "environment".into(),
                source_task_number: None,
                source_pointer: None,
                literal_value: Some(serde_json::json!("staging")),
            })
            .await
            .expect("bind");

        let resolution = store
            .resolve_inputs(task.task_number)
            .await
            .expect("resolve");
        assert_eq!(
            resolution,
            ContractResolution::Resolved {
                inputs: serde_json::json!({"environment": "staging"})
            }
        );
    }

    /// The failure that actually happens in a hand-built graph: the edge exists
    /// but the upstream task never produced the field. The problem must name
    /// the key, the task, and the pointer — "validation failed" would send
    /// somebody reading prompts to guess.
    #[tokio::test]
    async fn an_unresolved_pointer_names_the_key_task_and_path() {
        let store = setup_store().await;
        let parent = task_at(&store, "build", TaskStatus::InProgress).await;
        let child = task_at(&store, "deploy", TaskStatus::Backlog).await;

        store
            .submit_outputs(
                parent.task_number,
                &serde_json::json!({"digest": "sha256:abc"}),
            )
            .await
            .expect("submit");
        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: child.task_number,
                input_key: "tag".into(),
                source_task_number: Some(parent.task_number),
                source_pointer: Some("/image/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        let resolution = store
            .resolve_inputs(child.task_number)
            .await
            .expect("resolve");
        match resolution {
            ContractResolution::Unresolved { problems } => {
                assert_eq!(
                    problems,
                    vec![ContractProblem::PointerMissed {
                        input_key: "tag".into(),
                        source_task_number: parent.task_number,
                        pointer: "/image/tag".into(),
                    }]
                );
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_parent_that_has_not_produced_output_yet_is_reported_as_such() {
        let store = setup_store().await;
        let parent = task_at(&store, "build", TaskStatus::InProgress).await;
        let child = task_at(&store, "deploy", TaskStatus::Backlog).await;

        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: child.task_number,
                input_key: "tag".into(),
                source_task_number: Some(parent.task_number),
                source_pointer: Some("/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        match store
            .resolve_inputs(child.task_number)
            .await
            .expect("resolve")
        {
            ContractResolution::Unresolved { problems } => assert_eq!(
                problems,
                vec![ContractProblem::SourceHasNoOutputs {
                    input_key: "tag".into(),
                    source_task_number: parent.task_number,
                }]
            ),
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn inputs_that_miss_the_declared_schema_are_unresolved() {
        let store = setup_store().await;
        let task = task_at(&store, "deploy", TaskStatus::Backlog).await;

        store
            .set_contract(task.task_number, Some(&tag_schema()), None)
            .await
            .expect("set contract");
        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: task.task_number,
                input_key: "tag".into(),
                source_task_number: None,
                source_pointer: None,
                literal_value: Some(serde_json::json!(42)),
            })
            .await
            .expect("bind");

        match store
            .resolve_inputs(task.task_number)
            .await
            .expect("resolve")
        {
            ContractResolution::Unresolved { problems } => {
                assert!(
                    problems.iter().any(|p| matches!(
                        p,
                        ContractProblem::SchemaViolation {
                            side: ContractSide::Input,
                            ..
                        }
                    )),
                    "a number where a string was declared must be a schema violation: {problems:?}"
                );
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    /// The whole point of the contract. Without rejection it is a comment: the
    /// worker claims it produced something, nothing checks, and the downstream
    /// task discovers the gap at runtime with no idea who broke it.
    #[tokio::test]
    async fn an_output_that_misses_its_schema_is_rejected_and_not_persisted() {
        let store = setup_store().await;
        let task = task_at(&store, "build", TaskStatus::InProgress).await;

        store
            .set_contract(task.task_number, None, Some(&tag_schema()))
            .await
            .expect("set contract");

        let submission = store
            .submit_outputs(
                task.task_number,
                &serde_json::json!({"digest": "sha256:abc"}),
            )
            .await
            .expect("submit");

        match submission {
            OutputSubmission::Rejected { problems } => {
                assert!(!problems.is_empty());
                assert!(
                    problems[0].to_string().contains("tag"),
                    "the rejection must say what is missing: {problems:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert!(
            after.outputs.is_none(),
            "a rejected output must not be readable by downstream tasks"
        );
    }

    #[tokio::test]
    async fn a_valid_output_is_accepted_and_persisted() {
        let store = setup_store().await;
        let task = task_at(&store, "build", TaskStatus::InProgress).await;
        store
            .set_contract(task.task_number, None, Some(&tag_schema()))
            .await
            .expect("set contract");

        let submission = store
            .submit_outputs(task.task_number, &serde_json::json!({"tag": "v1.4.2"}))
            .await
            .expect("submit");
        assert_eq!(submission, OutputSubmission::Accepted);

        let after = store
            .get_by_number(task.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.outputs, Some(serde_json::json!({"tag": "v1.4.2"})));
    }

    /// A task declaring an unusable schema is misconfigured. Quietly accepting
    /// anything would hide that, so it surfaces as a problem in its own right.
    #[tokio::test]
    async fn an_invalid_schema_is_reported_rather_than_ignored() {
        let store = setup_store().await;
        let task = task_at(&store, "build", TaskStatus::InProgress).await;
        store
            .set_contract(
                task.task_number,
                None,
                Some(&serde_json::json!({"type": "not-a-real-type"})),
            )
            .await
            .expect("set contract");

        match store
            .submit_outputs(task.task_number, &serde_json::json!({"anything": true}))
            .await
            .expect("submit")
        {
            OutputSubmission::Rejected { problems } => assert!(
                matches!(problems.as_slice(), [ContractProblem::InvalidSchema { .. }]),
                "expected InvalidSchema, got {problems:?}"
            ),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    /// Regression guard: the overwhelming majority of tasks have no contract
    /// and must stay on exactly the path they were on before F4.
    #[tokio::test]
    async fn a_task_without_a_contract_is_unaffected() {
        let store = setup_store().await;
        let task = task_at(&store, "ordinary", TaskStatus::InProgress).await;

        assert_eq!(
            store
                .resolve_inputs(task.task_number)
                .await
                .expect("resolve"),
            ContractResolution::NotRequired
        );
        assert_eq!(
            store
                .submit_outputs(task.task_number, &serde_json::json!({"whatever": 1}))
                .await
                .expect("submit"),
            OutputSubmission::Accepted,
            "with no declared schema any output is acceptable"
        );
    }

    #[tokio::test]
    async fn rebinding_an_input_replaces_rather_than_duplicates() {
        let store = setup_store().await;
        let task = task_at(&store, "deploy", TaskStatus::Backlog).await;

        for value in ["staging", "production"] {
            store
                .set_input_binding(&TaskInputBinding {
                    child_task_number: task.task_number,
                    input_key: "environment".into(),
                    source_task_number: None,
                    source_pointer: None,
                    literal_value: Some(serde_json::json!(value)),
                })
                .await
                .expect("bind");
        }

        let bindings = store
            .list_input_bindings(task.task_number)
            .await
            .expect("list");
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0].literal_value,
            Some(serde_json::json!("production"))
        );
    }
    // -- Promote/re-block loop ---------------------------------------------

    /// The bug this guards against was found by running the system, not by
    /// reading it: a task whose binding could not resolve cycled
    /// ready -> claimed -> blocked -> promoted nineteen times before anyone
    /// noticed, with the recurrence counter pinned at zero the whole way.
    #[tokio::test]
    async fn a_promoted_task_still_remembers_what_it_was_blocked_for() {
        let store = setup_store().await;
        let task = task_at(&store, "cycler", TaskStatus::InProgress).await;

        store
            .block_task(task.task_number, BlockKind::Dependency, "waiting")
            .await
            .expect("block")
            .expect("exists");

        // Promotion clears `block_kind` — the task is no longer parked — but
        // the counter's memory has to survive it or the limiter never fires.
        store
            .update(
                task.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Ready),
                    ..Default::default()
                },
            )
            .await
            .expect("promote")
            .expect("exists");
        sqlx::query("UPDATE tasks SET block_kind = NULL WHERE task_number = ?")
            .bind(task.task_number)
            .execute(store.pool())
            .await
            .expect("clear block_kind the way the sweep does");

        let second = store
            .block_task(task.task_number, BlockKind::Dependency, "waiting")
            .await
            .expect("block")
            .expect("exists");
        assert_eq!(
            second.recurrences, 1,
            "a re-block after promotion is a recurrence, not a first offence"
        );
    }

    #[tokio::test]
    async fn a_task_cycling_through_promotion_eventually_escalates() {
        let store = setup_store().await;
        let task = task_at(&store, "cycler", TaskStatus::InProgress).await;

        let mut last = None;
        for _ in 0..=BLOCK_RECURRENCE_LIMIT {
            last = store
                .block_task(task.task_number, BlockKind::Dependency, "still waiting")
                .await
                .expect("block");
            // Simulate the sweep releasing it again.
            sqlx::query(
                "UPDATE tasks SET status = 'in_progress', block_kind = NULL WHERE task_number = ?",
            )
            .bind(task.task_number)
            .execute(store.pool())
            .await
            .expect("release");
        }

        let last = last.expect("blocked at least once");
        assert!(
            last.escalated,
            "a task promoted and re-blocked past the limit must stop cycling"
        );
        assert_eq!(last.status, TaskStatus::PendingApproval);
    }

    /// The deeper half: `dependency` covers both "waiting on a parent", which
    /// the sweep clears, and "this binding will not resolve", which it cannot.
    /// Promoting the second kind is what started the loop.
    #[tokio::test]
    async fn the_sweep_holds_back_a_task_whose_inputs_cannot_resolve() {
        let store = setup_store().await;
        let parent = task_at(&store, "producer", TaskStatus::InProgress).await;
        let child = task_at(&store, "consumer", TaskStatus::Backlog).await;

        store
            .link_tasks(parent.task_number, child.task_number)
            .await
            .expect("link");
        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: child.task_number,
                input_key: "tag".into(),
                source_task_number: Some(parent.task_number),
                source_pointer: Some("/image/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");

        // The parent finishes, but produces nothing at the bound pointer.
        store
            .submit_outputs(parent.task_number, &serde_json::json!({"digest": "abc"}))
            .await
            .expect("submit");
        finish(&store, parent.task_number).await;

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert!(
            sweep.promoted.is_empty(),
            "a task that cannot build its inputs must not be made claimable"
        );
        assert_eq!(sweep.stalled.len(), 1);
        assert_eq!(sweep.stalled[0].task_number, child.task_number);

        let after = store
            .get_by_number(child.task_number)
            .await
            .expect("fetch")
            .expect("exists");
        assert_eq!(after.status, TaskStatus::Backlog);
    }

    /// The ordinary case must keep working: parents done, bindings resolve.
    #[tokio::test]
    async fn the_sweep_still_promotes_a_task_whose_inputs_do_resolve() {
        let store = setup_store().await;
        let parent = task_at(&store, "producer", TaskStatus::InProgress).await;
        let child = task_at(&store, "consumer", TaskStatus::Backlog).await;

        store
            .link_tasks(parent.task_number, child.task_number)
            .await
            .expect("link");
        store
            .set_input_binding(&TaskInputBinding {
                child_task_number: child.task_number,
                input_key: "tag".into(),
                source_task_number: Some(parent.task_number),
                source_pointer: Some("/image/tag".into()),
                literal_value: None,
            })
            .await
            .expect("bind");
        store
            .submit_outputs(
                parent.task_number,
                &serde_json::json!({"image": {"tag": "v1"}}),
            )
            .await
            .expect("submit");
        finish(&store, parent.task_number).await;

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert_eq!(sweep.promoted, vec![child.task_number]);
        assert!(sweep.stalled.is_empty());
    }
    /// The budget has to be adjustable *and* resettable. A plain `Option` can
    /// express "set it" but not "put it back to the default", which would make
    /// an override permanent once applied.
    #[tokio::test]
    async fn max_retries_can_be_set_cleared_and_left_alone() {
        let store = setup_store().await;
        let task = task_at(&store, "tunable", TaskStatus::Backlog).await;
        assert!(task.max_retries.is_none(), "starts on the instance default");

        let set = store
            .update(
                task.task_number,
                UpdateTaskInput {
                    max_retries: Some(Some(5)),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(set.max_retries, Some(5));

        // An unrelated update must not disturb it.
        let untouched = store
            .update(
                task.task_number,
                UpdateTaskInput {
                    title: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert_eq!(untouched.max_retries, Some(5));

        let cleared = store
            .update(
                task.task_number,
                UpdateTaskInput {
                    max_retries: Some(None),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");
        assert!(
            cleared.max_retries.is_none(),
            "an override must be removable, not just replaceable"
        );
    }

    /// The budget is what `record_failure` reads, so a change has to actually
    /// move the parking threshold rather than only the displayed number.
    #[tokio::test]
    async fn a_raised_budget_delays_parking() {
        let store = setup_store().await;
        let task = task_at(&store, "patient", TaskStatus::InProgress).await;
        store
            .update(
                task.task_number,
                UpdateTaskInput {
                    max_retries: Some(Some(3)),
                    ..Default::default()
                },
            )
            .await
            .expect("update")
            .expect("exists");

        for round in 0..2 {
            if round > 0 {
                store
                    .claim_next_ready("agent-test")
                    .await
                    .expect("claim")
                    .expect("requeued");
            }
            let disposition = store
                .record_failure(task.task_number, TaskRunOutcome::Failed, "boom")
                .await
                .expect("record failure");
            assert!(
                matches!(disposition, FailureDisposition::Requeued { .. }),
                "under the raised limit the task should still retry: {disposition:?}"
            );
        }

        // Third failure reaches the raised limit.
        store
            .claim_next_ready("agent-test")
            .await
            .expect("claim")
            .expect("requeued");
        let final_disposition = store
            .record_failure(task.task_number, TaskRunOutcome::Failed, "boom")
            .await
            .expect("record failure");
        assert!(matches!(
            final_disposition,
            FailureDisposition::Parked { limit: 3, .. }
        ));
    }
    /// A worker-filed card is already a decision to run. It must not need a
    /// second one, and it must not be strandable.
    ///
    /// `depends_on` and `input_bindings` are separate arguments, so a filed
    /// card can have bindings and no edges. If its inputs fail to resolve it is
    /// parked as `dependency` — with no parents and no run id. A promote rule
    /// keyed only on edges would leave it in the backlog permanently.
    #[tokio::test]
    async fn a_filed_card_parked_without_edges_is_still_reconsidered() {
        let store = setup_store().await;
        let filer = task_at(&store, "decomposer", TaskStatus::InProgress).await;

        let filed = store
            .create(CreateTaskInput {
                owner_agent_id: "agent-test".into(),
                assigned_agent_id: "agent-test".into(),
                title: "filed with a binding but no edge".into(),
                status: TaskStatus::Backlog,
                created_by: filer_id(filer.task_number),
                ..Default::default()
            })
            .await
            .expect("create filed card");

        // The scheduler parks it: inputs did not resolve. No edges exist.
        store
            .block_task(filed.task_number, BlockKind::Dependency, "input unresolved")
            .await
            .expect("block")
            .expect("exists");
        assert!(
            store
                .list_parents(filed.task_number)
                .await
                .expect("parents")
                .is_empty(),
            "the fixture is only meaningful with no edges"
        );

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert_eq!(
            sweep.promoted,
            vec![filed.task_number],
            "a card the scheduler parked must be one the scheduler can release"
        );
    }

    /// The other half of the same rule: a card a person parked stays parked.
    /// Without this the sweep would empty the backlog on its next tick.
    #[tokio::test]
    async fn a_hand_parked_card_is_left_alone_by_the_sweep() {
        let store = setup_store().await;
        store
            .create(CreateTaskInput {
                owner_agent_id: "agent-test".into(),
                assigned_agent_id: "agent-test".into(),
                title: "parked on purpose".into(),
                status: TaskStatus::Backlog,
                created_by: "human".into(),
                ..Default::default()
            })
            .await
            .expect("create");

        let sweep = store.recompute_ready("agent-test").await.expect("sweep");
        assert!(
            sweep.is_empty(),
            "a backlog nobody automated must survive a sweep: {sweep:?}"
        );
    }
}
