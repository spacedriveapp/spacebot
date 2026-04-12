//! Global task CRUD storage (SQLite).
//!
//! Operates against the instance-level `tasks.db` database with globally
//! unique task numbers and explicit owner/assigned agent relationships.

use crate::error::Result;

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    PendingApproval,
    Backlog,
    Ready,
    InProgress,
    Done,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 5] = [
        TaskStatus::PendingApproval,
        TaskStatus::Backlog,
        TaskStatus::Ready,
        TaskStatus::InProgress,
        TaskStatus::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::PendingApproval => "pending_approval",
            TaskStatus::Backlog => "backlog",
            TaskStatus::Ready => "ready",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Done => "done",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending_approval" => Some(TaskStatus::PendingApproval),
            "backlog" => Some(TaskStatus::Backlog),
            "ready" => Some(TaskStatus::Ready),
            "in_progress" => Some(TaskStatus::InProgress),
            "done" => Some(TaskStatus::Done),
            _ => None,
        }
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
                    subtasks, metadata, source_memory_id, created_by
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        let Some(current) = self.get_by_number(task_number).await? else {
            return Ok(None);
        };

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

        sql.bind(input.approved_by)
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to update task")?;

        self.get_by_number(task_number).await
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
}

/// Column list used by all SELECT queries. Kept in sync with `task_from_row`.
const SELECT_COLUMNS: &str = "SELECT id, task_number, title, description, status, priority, \
     owner_agent_id, assigned_agent_id, subtasks, metadata, source_memory_id, worker_id, \
     created_by, approved_at, approved_by, created_at, updated_at, completed_at";

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
            | (TaskStatus::Backlog, TaskStatus::Ready)
            | (TaskStatus::Done, TaskStatus::Ready)
    )
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
        worker_id: row
            .try_get::<Option<String>, _>("worker_id")
            .ok()
            .flatten()
            .and_then(|value| if value.is_empty() { None } else { Some(value) }),
        created_by: row
            .try_get("created_by")
            .context("failed to read task created_by")?,
        approved_at: read_optional_timestamp(&row, "approved_at"),
        approved_by: row.try_get("approved_by").ok(),
        created_at,
        updated_at,
        completed_at: read_optional_timestamp(&row, "completed_at"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn setup_store() -> TaskStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

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
                completed_at TEXT
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("tasks schema should be created");

        sqlx::query(
            "CREATE TABLE task_number_seq (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                next_number INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .expect("task_number_seq should be created");

        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("sequence seed should be inserted");

        TaskStore::new(pool)
    }

    fn self_assigned_input(title: &str, status: TaskStatus) -> CreateTaskInput {
        CreateTaskInput {
            owner_agent_id: "agent-test".to_string(),
            assigned_agent_id: "agent-test".to_string(),
            title: title.to_string(),
            description: None,
            status,
            priority: TaskPriority::Medium,
            subtasks: Vec::new(),
            metadata: serde_json::json!({}),
            source_memory_id: None,
            created_by: "branch".to_string(),
        }
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
