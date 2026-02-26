//! Task CRUD storage (SQLite).

use crate::error::Result;
use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskSubtask {
    pub title: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub agent_id: String,
    pub task_number: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
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
    pub agent_id: String,
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
}

#[derive(Debug, Clone)]
pub struct TaskStore {
    pool: SqlitePool,
}

impl TaskStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Maximum number of retries when a concurrent create races on the same
    /// `(agent_id, task_number)` UNIQUE constraint.
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

            let task_number: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(task_number), 0) + 1 FROM tasks WHERE agent_id = ?",
            )
            .bind(&input.agent_id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to allocate next task number")?;

            let task_id = uuid::Uuid::new_v4().to_string();

            let insert_result = sqlx::query(
                r#"
                INSERT INTO tasks (
                    id, agent_id, task_number, title, description, status, priority,
                    subtasks, metadata, source_memory_id, created_by
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&task_id)
            .bind(&input.agent_id)
            .bind(task_number)
            .bind(&input.title)
            .bind(&input.description)
            .bind(input.status.as_str())
            .bind(input.priority.as_str())
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
                        .get_by_number(&input.agent_id, task_number)
                        .await?
                        .context("task inserted but not found")
                        .map_err(Into::into);
                }
                Err(sqlx::Error::Database(ref db_error))
                    if db_error.code().as_deref() == Some("2067") =>
                {
                    // UNIQUE constraint violation — another concurrent create won the
                    // race for this task_number. Roll back and retry.
                    tracing::debug!(
                        attempt,
                        task_number,
                        agent_id = %input.agent_id,
                        "task_number collision, retrying"
                    );
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

    pub async fn list(
        &self,
        agent_id: &str,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        limit: i64,
    ) -> Result<Vec<Task>> {
        let mut query = String::from(
            "SELECT id, agent_id, task_number, title, description, status, priority, subtasks, metadata, source_memory_id, worker_id, created_by, approved_at, approved_by, created_at, updated_at, completed_at FROM tasks WHERE agent_id = ?",
        );

        if status.is_some() {
            query.push_str(" AND status = ?");
        }
        if priority.is_some() {
            query.push_str(" AND priority = ?");
        }
        query.push_str(" ORDER BY task_number DESC LIMIT ?");

        let mut sql = sqlx::query(&query).bind(agent_id);
        if let Some(status) = status {
            sql = sql.bind(status.as_str());
        }
        if let Some(priority) = priority {
            sql = sql.bind(priority.as_str());
        }
        sql = sql.bind(limit.clamp(1, 500));

        let rows = sql
            .fetch_all(&self.pool)
            .await
            .context("failed to list tasks")?;

        rows.into_iter().map(task_from_row).collect()
    }

    pub async fn list_ready(&self, agent_id: &str, limit: i64) -> Result<Vec<Task>> {
        self.list(agent_id, Some(TaskStatus::Ready), None, limit)
            .await
    }

    pub async fn get_by_number(&self, agent_id: &str, task_number: i64) -> Result<Option<Task>> {
        let row = sqlx::query(
            "SELECT id, agent_id, task_number, title, description, status, priority, subtasks, metadata, source_memory_id, worker_id, created_by, approved_at, approved_by, created_at, updated_at, completed_at FROM tasks WHERE agent_id = ? AND task_number = ?",
        )
        .bind(agent_id)
        .bind(task_number)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch task by number")?;

        row.map(task_from_row).transpose()
    }

    pub async fn update(
        &self,
        agent_id: &str,
        task_number: i64,
        input: UpdateTaskInput,
    ) -> Result<Option<Task>> {
        let Some(current) = self.get_by_number(agent_id, task_number).await? else {
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
        let next_worker_id = if let Some(worker_id) = input.worker_id {
            Some(worker_id)
        } else {
            current.worker_id
        };

        let approved_at = if current.approved_at.is_none() && next_status == TaskStatus::Ready {
            Some("datetime('now')")
        } else {
            None
        };

        let completed_at = if next_status == TaskStatus::Done {
            Some("datetime('now')")
        } else if current.completed_at.is_some() && next_status != TaskStatus::Done {
            Some("NULL")
        } else {
            None
        };

        let mut query = String::from(
            "UPDATE tasks SET title = ?, description = ?, status = ?, priority = ?, subtasks = ?, metadata = ?, ",
        );

        if input.clear_worker_id {
            query.push_str("worker_id = NULL, ");
        } else {
            query.push_str("worker_id = ?, ");
        }

        query.push_str("approved_by = COALESCE(?, approved_by), updated_at = datetime('now')");

        if approved_at.is_some() {
            query.push_str(", approved_at = datetime('now')");
        }
        if let Some(value) = completed_at {
            if value == "datetime('now')" {
                query.push_str(", completed_at = datetime('now')");
            } else {
                query.push_str(", completed_at = NULL");
            }
        }

        query.push_str(" WHERE agent_id = ? AND task_number = ?");

        let mut sql = sqlx::query(&query)
            .bind(input.title.unwrap_or(current.title))
            .bind(input.description.or(current.description))
            .bind(next_status.as_str())
            .bind(next_priority.as_str())
            .bind(serde_json::to_string(&subtasks).context("failed to serialize subtasks")?)
            .bind(next_metadata.to_string());

        if !input.clear_worker_id {
            sql = sql.bind(next_worker_id);
        }

        sql.bind(input.approved_by)
            .bind(agent_id)
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to update task")?;

        self.get_by_number(agent_id, task_number).await
    }

    pub async fn delete(&self, agent_id: &str, task_number: i64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM tasks WHERE agent_id = ? AND task_number = ?")
            .bind(agent_id)
            .bind(task_number)
            .execute(&self.pool)
            .await
            .context("failed to delete task")?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn claim_next_ready(&self, agent_id: &str) -> Result<Option<Task>> {
        let row = sqlx::query(
            "SELECT task_number FROM tasks WHERE agent_id = ? AND status = 'ready' \
             ORDER BY CASE priority \
               WHEN 'critical' THEN 0 \
               WHEN 'high' THEN 1 \
               WHEN 'medium' THEN 2 \
               WHEN 'low' THEN 3 \
               ELSE 4 END ASC, \
             task_number ASC \
             LIMIT 1",
        )
        .bind(agent_id)
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
            "UPDATE tasks SET status = 'in_progress', updated_at = datetime('now') WHERE agent_id = ? AND task_number = ? AND status = 'ready'",
        )
        .bind(agent_id)
        .bind(task_number)
        .execute(&self.pool)
        .await
        .context("failed to claim ready task")?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }

        self.get_by_number(agent_id, task_number).await
    }

    pub async fn get_by_worker_id(&self, worker_id: &str) -> Result<Option<Task>> {
        let row = sqlx::query(
            "SELECT id, agent_id, task_number, title, description, status, priority, subtasks, metadata, source_memory_id, worker_id, created_by, approved_at, approved_by, created_at, updated_at, completed_at FROM tasks WHERE worker_id = ? ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch task by worker id")?;

        row.map(task_from_row).transpose()
    }
}

fn can_transition(current: TaskStatus, next: TaskStatus) -> bool {
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
    )
}

fn merge_json_object(current: Value, patch: Option<Value>) -> Value {
    let Some(patch) = patch else {
        return current;
    };

    let mut merged = current.as_object().cloned().unwrap_or_default();
    if let Some(patch_object) = patch.as_object() {
        for (key, value) in patch_object {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
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

    Ok(Task {
        id: row.try_get("id").context("failed to read task id")?,
        agent_id: row
            .try_get("agent_id")
            .context("failed to read task agent_id")?,
        task_number: row
            .try_get("task_number")
            .context("failed to read task_number")?,
        title: row.try_get("title").context("failed to read task title")?,
        description: row.try_get("description").ok(),
        status,
        priority,
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
        approved_at: row
            .try_get::<Option<chrono::NaiveDateTime>, _>("approved_at")
            .ok()
            .flatten()
            .map(|v| v.and_utc().to_rfc3339()),
        approved_by: row.try_get("approved_by").ok(),
        created_at: row
            .try_get::<chrono::NaiveDateTime, _>("created_at")
            .map(|v| v.and_utc().to_rfc3339())
            .context("failed to read task created_at")?,
        updated_at: row
            .try_get::<chrono::NaiveDateTime, _>("updated_at")
            .map(|v| v.and_utc().to_rfc3339())
            .context("failed to read task updated_at")?,
        completed_at: row
            .try_get::<chrono::NaiveDateTime, _>("completed_at")
            .ok()
            .map(|v| v.and_utc().to_rfc3339()),
    })
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
                agent_id TEXT NOT NULL,
                task_number INTEGER NOT NULL,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL DEFAULT 'backlog',
                priority TEXT NOT NULL DEFAULT 'medium',
                subtasks TEXT,
                metadata TEXT,
                source_memory_id TEXT,
                worker_id TEXT,
                created_by TEXT NOT NULL,
                approved_at TIMESTAMP,
                approved_by TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP,
                UNIQUE(agent_id, task_number)
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("tasks schema should be created");

        TaskStore::new(pool)
    }

    #[tokio::test]
    async fn rejects_invalid_status_transition() {
        let store = setup_store().await;
        let created = store
            .create(CreateTaskInput {
                agent_id: "agent-test".to_string(),
                title: "pending task".to_string(),
                description: None,
                status: TaskStatus::PendingApproval,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "cortex".to_string(),
            })
            .await
            .expect("task should be created");

        let error = store
            .update(
                "agent-test",
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
            .create(CreateTaskInput {
                agent_id: "agent-test".to_string(),
                title: "ready task".to_string(),
                description: None,
                status: TaskStatus::Ready,
                priority: TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata: serde_json::json!({}),
                source_memory_id: None,
                created_by: "branch".to_string(),
            })
            .await
            .expect("task should be created");

        let in_progress = store
            .update(
                "agent-test",
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
                "agent-test",
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
}
