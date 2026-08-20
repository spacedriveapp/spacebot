//! Immutable task revisions: who changed a task's specification, to what, and
//! when.
//!
//! A task row holds the current state. Every materially different state it has
//! ever held is also written here, whole, in the same transaction as the change
//! that produced it. Reading revision N reconstructs the task as it stood
//! without replaying the revisions before it, and restoring N appends a new
//! latest revision rather than rewinding the counter — history only grows.

use crate::error::{Result, TaskError};
use crate::tasks::store::{
    SELECT_COLUMNS, Task, TaskDependencyKind, TaskPriority, TaskStatus, TaskStore, TaskSubtask,
    TaskWorkerType, TaskWorktreeMode, task_from_row,
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, sqlite::SqliteRow};

/// Hard ceiling on revisions returned by a single history call.
pub const MAX_REVISION_PAGE: i64 = 200;

/// Longest edit summary accepted, in characters.
pub const MAX_EDIT_SUMMARY_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// Authorship and provenance
// ---------------------------------------------------------------------------

/// Who performed a task mutation or wrote a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskAuthorKind {
    /// A human, through the interface, the CLI, or the API.
    User,
    /// An agent process — a branch, the cortex, or an autonomy run.
    Agent,
    /// A worker reporting on its own run.
    Worker,
    /// Spacebot itself: migrations, lifecycle transitions, automation.
    System,
}

impl TaskAuthorKind {
    pub const ALL: [TaskAuthorKind; 4] = [
        TaskAuthorKind::User,
        TaskAuthorKind::Agent,
        TaskAuthorKind::Worker,
        TaskAuthorKind::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskAuthorKind::User => "user",
            TaskAuthorKind::Agent => "agent",
            TaskAuthorKind::Worker => "worker",
            TaskAuthorKind::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(TaskAuthorKind::User),
            "agent" => Some(TaskAuthorKind::Agent),
            "worker" => Some(TaskAuthorKind::Worker),
            "system" => Some(TaskAuthorKind::System),
            _ => None,
        }
    }
}

impl std::fmt::Display for TaskAuthorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Default for TaskAuthorKind {
    /// Unattributed writes are Spacebot's own.
    fn default() -> Self {
        Self::System
    }
}

/// Which surface a task mutation arrived through. Recorded per revision so
/// history reads as a sequence of decisions with their origin intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskMutationSource {
    /// The control API, called by something that did not identify itself.
    Api,
    /// The `spacebot` command line.
    Cli,
    /// The web interface.
    Portal,
    /// An agent tool call.
    Tool,
    /// A worker updating the task it is bound to.
    Worker,
    /// Restoring a historical revision.
    Restore,
    /// A schema or data migration.
    Migration,
    /// Internal automation with no external caller.
    System,
}

impl TaskMutationSource {
    pub const ALL: [TaskMutationSource; 8] = [
        TaskMutationSource::Api,
        TaskMutationSource::Cli,
        TaskMutationSource::Portal,
        TaskMutationSource::Tool,
        TaskMutationSource::Worker,
        TaskMutationSource::Restore,
        TaskMutationSource::Migration,
        TaskMutationSource::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskMutationSource::Api => "api",
            TaskMutationSource::Cli => "cli",
            TaskMutationSource::Portal => "portal",
            TaskMutationSource::Tool => "tool",
            TaskMutationSource::Worker => "worker",
            TaskMutationSource::Restore => "restore",
            TaskMutationSource::Migration => "migration",
            TaskMutationSource::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "api" => Some(TaskMutationSource::Api),
            "cli" => Some(TaskMutationSource::Cli),
            "portal" => Some(TaskMutationSource::Portal),
            "tool" => Some(TaskMutationSource::Tool),
            "worker" => Some(TaskMutationSource::Worker),
            "restore" => Some(TaskMutationSource::Restore),
            "migration" => Some(TaskMutationSource::Migration),
            "system" => Some(TaskMutationSource::System),
            _ => None,
        }
    }
}

impl std::fmt::Display for TaskMutationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Attribution and concurrency control carried by every task mutation.
#[derive(Debug, Clone)]
pub struct TaskMutationContext {
    pub author_type: TaskAuthorKind,
    pub author_id: Option<String>,
    pub source: TaskMutationSource,
    pub edit_summary: Option<String>,
    /// The revision the caller believes is current. When set and stale, the
    /// mutation fails with [`TaskError::RevisionConflict`] instead of
    /// overwriting whatever landed in between.
    pub expected_revision: Option<i64>,
}

impl Default for TaskMutationContext {
    fn default() -> Self {
        Self {
            author_type: TaskAuthorKind::System,
            author_id: None,
            source: TaskMutationSource::System,
            edit_summary: None,
            expected_revision: None,
        }
    }
}

impl TaskMutationContext {
    pub fn new(
        author_type: TaskAuthorKind,
        author_id: Option<String>,
        source: TaskMutationSource,
    ) -> Self {
        Self {
            author_type,
            author_id,
            source,
            edit_summary: None,
            expected_revision: None,
        }
    }

    pub fn with_summary(mut self, summary: Option<String>) -> Self {
        self.edit_summary = summary;
        self
    }

    pub fn expecting(mut self, revision: Option<i64>) -> Self {
        self.expected_revision = revision;
        self
    }

    /// Reject an over-long summary before it reaches storage.
    pub fn validate(&self) -> Result<()> {
        if let Some(summary) = &self.edit_summary
            && summary.chars().count() > MAX_EDIT_SUMMARY_CHARS
        {
            return Err(TaskError::Invalid(format!(
                "edit summary is {} characters; the limit is {MAX_EDIT_SUMMARY_CHARS}",
                summary.chars().count()
            ))
            .into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The versioned snapshot contract
// ---------------------------------------------------------------------------

/// A dependency edge as stored in a snapshot: the referenced task number, not
/// its internal id, so a snapshot stays readable after the store is rebuilt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskRevisionDependency {
    pub task: i64,
    pub kind: TaskDependencyKind,
}

/// Every field whose change needs historical reconstruction.
///
/// Deliberately excluded, because they describe a run rather than the spec:
/// `worker_id` (the currently bound worker), `approved_at`/`completed_at`
/// (derived from status transitions), `updated_at`, and the identity fields
/// that never change — `id`, `task_number`, `owner_agent_id`, `created_by`,
/// `source_memory_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskRevisionSnapshot {
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub assigned_agent_id: Option<String>,
    pub subtasks: Vec<TaskSubtask>,
    pub metadata: Value,
    pub goal_id: Option<String>,
    pub worker_type: Option<TaskWorkerType>,
    pub project_id: Option<String>,
    pub repo_id: Option<String>,
    pub worktree_mode: Option<TaskWorktreeMode>,
    pub worktree_id: Option<String>,
    pub required_skills: Vec<String>,
    pub depends_on: Vec<TaskRevisionDependency>,
}

impl TaskRevisionSnapshot {
    /// Capture a task's material state. `dependencies` comes from the same
    /// transaction as the task row, ordered by task number so two snapshots of
    /// the same edge set compare equal.
    pub fn capture(task: &Task, dependencies: &[(i64, TaskDependencyKind)]) -> Self {
        let mut depends_on: Vec<TaskRevisionDependency> = dependencies
            .iter()
            .map(|(task, kind)| TaskRevisionDependency {
                task: *task,
                kind: *kind,
            })
            .collect();
        depends_on.sort_by_key(|edge| edge.task);

        Self {
            title: task.title.clone(),
            description: task.description.clone(),
            status: task.status,
            priority: task.priority,
            assigned_agent_id: task.assigned_agent_id.clone(),
            subtasks: task.subtasks.clone(),
            metadata: task.metadata.clone(),
            goal_id: task.goal_id.clone(),
            worker_type: task.worker_type,
            project_id: task.project_id.clone(),
            repo_id: task.repo_id.clone(),
            worktree_mode: task.worktree_mode,
            worktree_id: task.worktree_id.clone(),
            required_skills: task.required_skills.clone(),
            depends_on,
        }
    }

    /// The dependency edges this snapshot restores to.
    pub fn dependency_edges(&self) -> Vec<(i64, TaskDependencyKind)> {
        self.depends_on
            .iter()
            .map(|edge| (edge.task, edge.kind))
            .collect()
    }

    /// Field-by-field comparison, in a stable order suitable for rendering.
    pub fn changes(&self, other: &Self) -> Vec<TaskFieldChange> {
        let mut changes = Vec::new();
        let mut compare = |field: &'static str, before: Value, after: Value| {
            if before != after {
                changes.push(TaskFieldChange {
                    field: field.to_string(),
                    before,
                    after,
                });
            }
        };

        compare("title", json(&self.title), json(&other.title));
        compare(
            "description",
            json(&self.description),
            json(&other.description),
        );
        compare("status", json(&self.status), json(&other.status));
        compare("priority", json(&self.priority), json(&other.priority));
        compare(
            "assigned_agent_id",
            json(&self.assigned_agent_id),
            json(&other.assigned_agent_id),
        );
        compare("subtasks", json(&self.subtasks), json(&other.subtasks));
        compare("metadata", self.metadata.clone(), other.metadata.clone());
        compare("goal_id", json(&self.goal_id), json(&other.goal_id));
        compare(
            "worker_type",
            json(&self.worker_type),
            json(&other.worker_type),
        );
        compare(
            "project_id",
            json(&self.project_id),
            json(&other.project_id),
        );
        compare("repo_id", json(&self.repo_id), json(&other.repo_id));
        compare(
            "worktree_mode",
            json(&self.worktree_mode),
            json(&other.worktree_mode),
        );
        compare(
            "worktree_id",
            json(&self.worktree_id),
            json(&other.worktree_id),
        );
        compare(
            "required_skills",
            json(&self.required_skills),
            json(&other.required_skills),
        );
        compare(
            "depends_on",
            json(&self.depends_on),
            json(&other.depends_on),
        );
        changes
    }
}

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// One field that differs between two snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskFieldChange {
    pub field: String,
    pub before: Value,
    pub after: Value,
}

/// A diff between two points in a task's history.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskRevisionDiff {
    pub task_number: i64,
    pub from: i64,
    pub to: i64,
    pub changes: Vec<TaskFieldChange>,
}

// ---------------------------------------------------------------------------
// Stored revisions
// ---------------------------------------------------------------------------

/// A revision without its snapshot — what a history list renders.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskRevisionSummary {
    pub id: String,
    pub task_id: String,
    pub revision: i64,
    pub author_type: TaskAuthorKind,
    pub author_id: Option<String>,
    pub source: TaskMutationSource,
    pub edit_summary: Option<String>,
    /// Set when this revision was produced by restoring an earlier one.
    pub restored_from: Option<i64>,
    pub created_at: String,
}

/// A revision with the full material snapshot it recorded.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskRevision {
    #[serde(flatten)]
    pub summary: TaskRevisionSummary,
    pub snapshot: TaskRevisionSnapshot,
}

const REVISION_SELECT_COLUMNS: &str = "SELECT id, task_id, revision, snapshot, author_type, \
     author_id, source, edit_summary, restored_from, created_at";

fn revision_summary_from_row(row: &SqliteRow) -> Result<TaskRevisionSummary> {
    let author_value: String = row
        .try_get("author_type")
        .context("failed to read revision author_type")?;
    let author_type = TaskAuthorKind::parse(&author_value)
        .with_context(|| format!("invalid revision author kind in database: {author_value}"))?;
    let source_value: String = row
        .try_get("source")
        .context("failed to read revision source")?;
    let source = TaskMutationSource::parse(&source_value)
        .with_context(|| format!("invalid revision source in database: {source_value}"))?;

    Ok(TaskRevisionSummary {
        id: row.try_get("id").context("failed to read revision id")?,
        task_id: row
            .try_get("task_id")
            .context("failed to read revision task_id")?,
        revision: row
            .try_get("revision")
            .context("failed to read revision number")?,
        author_type,
        author_id: row
            .try_get::<Option<String>, _>("author_id")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        source,
        edit_summary: row
            .try_get::<Option<String>, _>("edit_summary")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        restored_from: row
            .try_get::<Option<i64>, _>("restored_from")
            .ok()
            .flatten(),
        created_at: row
            .try_get("created_at")
            .context("failed to read revision created_at")?,
    })
}

fn revision_from_row(row: SqliteRow) -> Result<TaskRevision> {
    let summary = revision_summary_from_row(&row)?;
    let snapshot_json: String = row
        .try_get("snapshot")
        .context("failed to read revision snapshot")?;
    let snapshot: TaskRevisionSnapshot =
        serde_json::from_str(&snapshot_json).context("failed to deserialize revision snapshot")?;
    Ok(TaskRevision { summary, snapshot })
}

impl TaskStore {
    /// Write a revision inside an open transaction and advance the task's
    /// revision counter. Returns the new revision number.
    ///
    /// The caller has already established that the snapshot differs from the
    /// previous one; a no-op update never reaches here.
    pub(crate) async fn insert_revision_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_id: &str,
        current_revision: i64,
        snapshot: &TaskRevisionSnapshot,
        context: &TaskMutationContext,
        restored_from: Option<i64>,
    ) -> Result<i64> {
        let next = current_revision + 1;
        let snapshot_json = serde_json::to_string(snapshot)
            .context("failed to serialize task revision snapshot")?;

        sqlx::query(
            "INSERT INTO task_revisions \
             (id, task_id, revision, snapshot, author_type, author_id, source, edit_summary, restored_from) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(task_id)
        .bind(next)
        .bind(&snapshot_json)
        .bind(context.author_type.as_str())
        .bind(&context.author_id)
        .bind(context.source.as_str())
        .bind(&context.edit_summary)
        .bind(restored_from)
        .execute(&mut **tx)
        .await
        .context("failed to insert task revision")?;

        sqlx::query("UPDATE tasks SET revision = ? WHERE id = ?")
            .bind(next)
            .bind(task_id)
            .execute(&mut **tx)
            .await
            .context("failed to advance task revision counter")?;

        Ok(next)
    }

    /// Dependency edges as task numbers, read inside an open transaction so a
    /// snapshot and the row it describes come from the same read.
    pub(crate) async fn dependency_pairs_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_id: &str,
    ) -> Result<Vec<(i64, TaskDependencyKind)>> {
        let rows = sqlx::query(
            "SELECT dep.task_number, d.kind FROM task_dependencies d \
             JOIN tasks dep ON dep.id = d.depends_on_task_id \
             WHERE d.task_id = ? ORDER BY dep.task_number ASC",
        )
        .bind(task_id)
        .fetch_all(&mut **tx)
        .await
        .context("failed to load task dependency edges")?;

        rows.into_iter()
            .map(|row| {
                let kind_value: String = row.try_get("kind").context("dependency kind")?;
                let kind = TaskDependencyKind::parse(&kind_value).with_context(|| {
                    format!("invalid dependency kind in database: {kind_value}")
                })?;
                Ok((
                    row.try_get("task_number")
                        .context("dependency task_number")?,
                    kind,
                ))
            })
            .collect()
    }

    /// Revision history for a task, newest first.
    pub async fn list_revisions(
        &self,
        task_number: i64,
        limit: i64,
    ) -> Result<Vec<TaskRevisionSummary>> {
        let rows = sqlx::query(&format!(
            "{REVISION_SELECT_COLUMNS} FROM task_revisions \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             ORDER BY revision DESC LIMIT ?"
        ))
        .bind(task_number)
        .bind(limit.clamp(1, MAX_REVISION_PAGE))
        .fetch_all(self.pool())
        .await
        .context("failed to list task revisions")?;

        rows.iter().map(revision_summary_from_row).collect()
    }

    /// Read one historical revision with its snapshot.
    pub async fn get_revision(
        &self,
        task_number: i64,
        revision: i64,
    ) -> Result<Option<TaskRevision>> {
        let row = sqlx::query(&format!(
            "{REVISION_SELECT_COLUMNS} FROM task_revisions \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) AND revision = ?"
        ))
        .bind(task_number)
        .bind(revision)
        .fetch_optional(self.pool())
        .await
        .context("failed to fetch task revision")?;

        row.map(revision_from_row).transpose()
    }

    /// Complete revision snapshots for an internal task execution briefing.
    pub(crate) async fn all_revisions(&self, task_number: i64) -> Result<Vec<TaskRevision>> {
        let rows = sqlx::query(&format!(
            "{REVISION_SELECT_COLUMNS} FROM task_revisions \
             WHERE task_id = (SELECT id FROM tasks WHERE task_number = ?) \
             ORDER BY revision ASC"
        ))
        .bind(task_number)
        .fetch_all(self.pool())
        .await
        .context("failed to load complete task revision history")?;

        rows.into_iter().map(revision_from_row).collect()
    }

    /// Diff two revisions of a task. `to` defaults to the task's current
    /// revision, which is how "what changed since I last looked" is asked.
    pub async fn diff_revisions(
        &self,
        task_number: i64,
        from: i64,
        to: Option<i64>,
    ) -> Result<TaskRevisionDiff> {
        let task = self
            .get_by_number(task_number)
            .await?
            .ok_or(TaskError::NotFound { task_number })?;
        let to = to.unwrap_or(task.revision);

        let before =
            self.get_revision(task_number, from)
                .await?
                .ok_or(TaskError::RevisionNotFound {
                    task_number,
                    revision: from,
                })?;
        let after =
            self.get_revision(task_number, to)
                .await?
                .ok_or(TaskError::RevisionNotFound {
                    task_number,
                    revision: to,
                })?;

        Ok(TaskRevisionDiff {
            task_number,
            from,
            to,
            changes: before.snapshot.changes(&after.snapshot),
        })
    }

    /// Give every task without history a baseline revision 1 snapshotting it
    /// exactly as it stands.
    ///
    /// Idempotent: tasks that already carry a revision are skipped, and the
    /// `(task_id, revision)` uniqueness makes a retry after a partial run a
    /// no-op rather than a duplicate. It does not reconstruct history that
    /// predates the feature, and says so in the summary it records.
    pub async fn backfill_baseline_revisions(&self) -> Result<usize> {
        let numbers: Vec<i64> =
            sqlx::query_scalar("SELECT task_number FROM tasks WHERE revision = 0 ORDER BY id")
                .fetch_all(self.pool())
                .await
                .context("failed to list tasks needing a baseline revision")?;

        if numbers.is_empty() {
            return Ok(0);
        }

        let context = TaskMutationContext {
            author_type: TaskAuthorKind::System,
            author_id: Some("migration".to_string()),
            source: TaskMutationSource::Migration,
            edit_summary: Some(
                "Baseline snapshot at migration; earlier history is not available".to_string(),
            ),
            expected_revision: None,
        };

        let mut written = 0usize;
        for task_number in numbers {
            let mut tx = self
                .pool()
                .begin_with("BEGIN IMMEDIATE")
                .await
                .context("failed to open baseline revision transaction")?;

            let row = sqlx::query(&format!(
                "{SELECT_COLUMNS} FROM tasks WHERE task_number = ? AND revision = 0"
            ))
            .bind(task_number)
            .fetch_optional(&mut *tx)
            .await
            .context("failed to reload task for baseline revision")?;

            let Some(row) = row else {
                // Another writer gave this task its first revision in between.
                tx.rollback()
                    .await
                    .context("failed to roll back baseline revision transaction")?;
                continue;
            };

            let task = task_from_row(row)?;
            let dependencies = Self::dependency_pairs_in_tx(&mut tx, &task.id).await?;
            let snapshot = TaskRevisionSnapshot::capture(&task, &dependencies);
            Self::insert_revision_in_tx(&mut tx, &task.id, 0, &snapshot, &context, None).await?;

            tx.commit()
                .await
                .context("failed to commit baseline revision transaction")?;
            written += 1;
        }

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::store::{
        CreateTaskInput, TaskListFilter, TaskStore, UpdateTaskInput, setup_test_store,
    };
    use crate::tasks::{CreateTaskCommentInput, TaskSubtask};

    fn user_context(summary: &str) -> TaskMutationContext {
        TaskMutationContext::new(
            TaskAuthorKind::User,
            Some("jamie".to_string()),
            TaskMutationSource::Cli,
        )
        .with_summary(Some(summary.to_string()))
    }

    fn task_input(title: &str) -> CreateTaskInput {
        CreateTaskInput {
            owner_agent_id: "agent-test".to_string(),
            assigned_agent_id: Some("agent-test".to_string()),
            title: title.to_string(),
            description: Some("original spec".to_string()),
            status: TaskStatus::Backlog,
            created_by: "human".to_string(),
            context: user_context("Task created"),
            ..Default::default()
        }
    }

    async fn store_with_task() -> (TaskStore, i64) {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("history"))
            .await
            .expect("task should be created");
        (store, task.task_number)
    }

    #[tokio::test]
    async fn create_commits_revision_one_with_the_initial_snapshot() {
        let (store, number) = store_with_task().await;

        let task = store
            .get_by_number(number)
            .await
            .expect("task should load")
            .expect("task should exist");
        assert_eq!(task.revision, 1);

        let revisions = store
            .list_revisions(number, 10)
            .await
            .expect("history should load");
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].revision, 1);
        assert_eq!(revisions[0].author_type, TaskAuthorKind::User);
        assert_eq!(revisions[0].source, TaskMutationSource::Cli);

        let first = store
            .get_revision(number, 1)
            .await
            .expect("revision should load")
            .expect("revision 1 should exist");
        assert_eq!(first.snapshot.description.as_deref(), Some("original spec"));
    }

    #[tokio::test]
    async fn multi_field_edit_produces_one_revision_and_no_ops_produce_none() {
        let (store, number) = store_with_task().await;

        let update = store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    title: Some("renamed".to_string()),
                    description: Some(Some("rewritten spec".to_string())),
                    priority: Some(TaskPriority::Critical),
                    context: user_context("Scope tightened"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");
        assert_eq!(update.new_revision, Some(2));

        // Re-applying the same values changes nothing material.
        let repeat = store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    title: Some("renamed".to_string()),
                    description: Some(Some("rewritten spec".to_string())),
                    priority: Some(TaskPriority::Critical),
                    context: user_context("Same values again"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");
        assert_eq!(repeat.new_revision, None);
        assert_eq!(repeat.task.revision, 2);

        let revisions = store
            .list_revisions(number, 10)
            .await
            .expect("history should load");
        assert_eq!(revisions.len(), 2, "a no-op must not append a revision");
    }

    #[tokio::test]
    async fn binding_a_worker_is_not_a_material_change() {
        let (store, number) = store_with_task().await;

        let update = store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    worker_id: Some("worker-1".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");

        assert_eq!(update.new_revision, None);
        assert_eq!(update.task.worker_id.as_deref(), Some("worker-1"));
    }

    #[tokio::test]
    async fn stale_writes_fail_with_the_current_revision() {
        let (store, number) = store_with_task().await;

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    description: Some(Some("first writer wins".to_string())),
                    context: user_context("First edit"),
                    ..Default::default()
                },
            )
            .await
            .expect("first update should succeed");

        // A second writer still holding revision 1 must not clobber revision 2.
        let error = store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    description: Some(Some("second writer loses".to_string())),
                    context: user_context("Stale edit").expecting(Some(1)),
                    ..Default::default()
                },
            )
            .await
            .expect_err("stale write should be rejected");

        match error {
            crate::error::Error::Task(task_error) => match *task_error {
                TaskError::RevisionConflict {
                    expected, current, ..
                } => {
                    assert_eq!(expected, 1);
                    assert_eq!(current, 2);
                }
                other => panic!("expected a revision conflict, got {other}"),
            },
            other => panic!("expected a task error, got {other}"),
        }

        let task = store
            .get_by_number(number)
            .await
            .expect("task should load")
            .expect("task should exist");
        assert_eq!(
            task.description.as_deref(),
            Some("first writer wins"),
            "the losing write must not have landed"
        );
    }

    #[tokio::test]
    async fn restore_appends_a_new_revision_and_leaves_history_intact() {
        let (store, number) = store_with_task().await;

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    description: Some(Some("agent overwrote the spec".to_string())),
                    subtasks: Some(vec![TaskSubtask {
                        title: "added later".to_string(),
                        completed: false,
                    }]),
                    context: user_context("Overwrite"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let restored = store
            .restore_revision(
                number,
                1,
                user_context("Put the original spec back").expecting(Some(2)),
            )
            .await
            .expect("restore should succeed");

        assert_eq!(restored.new_revision, Some(3));
        assert_eq!(
            restored.task.description.as_deref(),
            Some("original spec"),
            "restore must reinstate the older description"
        );
        assert!(
            restored.task.subtasks.is_empty(),
            "restore must reinstate the older subtask list"
        );

        let revisions = store
            .list_revisions(number, 10)
            .await
            .expect("history should load");
        assert_eq!(revisions.len(), 3);
        assert_eq!(revisions[0].revision, 3);
        assert_eq!(revisions[0].restored_from, Some(1));

        // The revision that was restored is untouched, and so is the one it
        // replaced.
        let overwritten = store
            .get_revision(number, 2)
            .await
            .expect("revision should load")
            .expect("revision 2 should exist");
        assert_eq!(
            overwritten.snapshot.description.as_deref(),
            Some("agent overwrote the spec")
        );
    }

    #[tokio::test]
    async fn restore_clears_fields_the_target_revision_did_not_have() {
        let (store, number) = store_with_task().await;

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    project_id: Some(Some("proj-1".to_string())),
                    metadata: Some(serde_json::json!({ "github": { "number": 7 } })),
                    context: user_context("Attach project and metadata"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let restored = store
            .restore_revision(number, 1, user_context("Back to the start"))
            .await
            .expect("restore should succeed");

        assert_eq!(restored.task.project_id, None);
        assert_eq!(restored.task.metadata, serde_json::json!({}));
    }

    /// The backfill reconnects a task to the `task-<number>` worktree that was
    /// provisioned for it before the binding was recorded.
    #[tokio::test]
    async fn worktree_backfill_binds_by_the_conventional_name() {
        let (store, number) = store_with_task().await;
        let candidates = vec![
            (format!("task-{number}"), "wt-for-this-task".to_string()),
            ("task-9999".to_string(), "wt-for-another-task".to_string()),
        ];

        let bound = store
            .backfill_worktree_bindings(&candidates)
            .await
            .expect("backfill should succeed");

        assert_eq!(bound, 1);
        let task = store
            .get_by_number(number)
            .await
            .expect("load should succeed")
            .expect("task should exist");
        assert_eq!(task.worktree_id.as_deref(), Some("wt-for-this-task"));

        // The revision records that the binding was inferred rather than
        // observed, which is what keeps the two kinds distinguishable.
        let history = store
            .list_revisions(number, 10)
            .await
            .expect("history should load");
        let latest = history.first().expect("a revision was appended");
        assert!(
            latest
                .edit_summary
                .as_deref()
                .is_some_and(|summary| summary.contains("inferred")),
            "the backfill revision should say the binding was inferred"
        );
    }

    /// Running it twice must not append a second revision, and a task that
    /// already has a binding is never touched.
    #[tokio::test]
    async fn worktree_backfill_is_idempotent_and_skips_bound_tasks() {
        let (store, number) = store_with_task().await;
        let candidates = vec![(format!("task-{number}"), "wt-1".to_string())];

        assert_eq!(
            store
                .backfill_worktree_bindings(&candidates)
                .await
                .expect("first pass"),
            1
        );
        let after_first = store
            .list_revisions(number, 10)
            .await
            .expect("history should load")
            .len();

        assert_eq!(
            store
                .backfill_worktree_bindings(&candidates)
                .await
                .expect("second pass"),
            0,
            "a bound task must not be revisited"
        );
        assert_eq!(
            store
                .list_revisions(number, 10)
                .await
                .expect("history should load")
                .len(),
            after_first,
            "the second pass must not append a revision"
        );

        // A rename must not steal the binding the task already holds.
        let renamed = vec![(format!("task-{number}"), "wt-2".to_string())];
        assert_eq!(
            store
                .backfill_worktree_bindings(&renamed)
                .await
                .expect("third pass"),
            0
        );
        let task = store
            .get_by_number(number)
            .await
            .expect("load should succeed")
            .expect("task should exist");
        assert_eq!(task.worktree_id.as_deref(), Some("wt-1"));
    }

    /// A task with no matching worktree is left alone rather than guessed at.
    #[tokio::test]
    async fn worktree_backfill_ignores_tasks_with_no_matching_worktree() {
        let (store, number) = store_with_task().await;

        let bound = store
            .backfill_worktree_bindings(&[("task-4242".to_string(), "wt-x".to_string())])
            .await
            .expect("backfill should succeed");

        assert_eq!(bound, 0);
        let task = store
            .get_by_number(number)
            .await
            .expect("load should succeed")
            .expect("task should exist");
        assert_eq!(task.worktree_id, None);
    }

    /// `goal_id` is in the snapshot and `changes` diffs it, so a restore that
    /// left the current goal in place would report success while producing a
    /// task that does not match the revision it claims to have restored.
    #[tokio::test]
    async fn restore_reinstates_the_goal_it_recorded() {
        let (store, number) = store_with_task().await;

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    goal_id: Some(Some("goal-original".to_string())),
                    context: user_context("Attach the original goal"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let moved = store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    goal_id: Some(Some("goal-moved".to_string())),
                    context: user_context("Move to another goal"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("task should exist");
        assert_eq!(moved.task.goal_id.as_deref(), Some("goal-moved"));

        let restored = store
            .restore_revision(number, 2, user_context("Back to the original goal"))
            .await
            .expect("restore should succeed");

        assert_eq!(restored.task.goal_id.as_deref(), Some("goal-original"));

        // The revision the restore appended must match what it restored, or a
        // diff against it still reports a goal change.
        let diff = store
            .diff_revisions(number, 2, None)
            .await
            .expect("diff should compute");
        assert!(
            !diff.changes.iter().any(|change| change.field == "goal_id"),
            "the restored revision should agree with revision 2 on goal_id"
        );
    }

    /// A restore back to a revision that predates any goal must clear it,
    /// matching how the other patch fields behave.
    #[tokio::test]
    async fn restore_clears_a_goal_the_target_revision_did_not_have() {
        let (store, number) = store_with_task().await;

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    goal_id: Some(Some("goal-1".to_string())),
                    context: user_context("Attach a goal"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let restored = store
            .restore_revision(number, 1, user_context("Back to the start"))
            .await
            .expect("restore should succeed");

        assert_eq!(restored.task.goal_id, None);
    }

    #[tokio::test]
    async fn diff_reports_only_the_fields_that_changed() {
        let (store, number) = store_with_task().await;

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    description: Some(Some("new spec".to_string())),
                    priority: Some(TaskPriority::High),
                    context: user_context("Rewrite"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let diff = store
            .diff_revisions(number, 1, None)
            .await
            .expect("diff should compute");

        assert_eq!(diff.from, 1);
        assert_eq!(diff.to, 2);
        let fields: Vec<&str> = diff.changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, vec!["description", "priority"]);
    }

    #[tokio::test]
    async fn restore_respects_status_transition_rules() {
        let store = setup_test_store().await;
        let task = store
            .create(CreateTaskInput {
                status: TaskStatus::Ready,
                ..task_input("gated")
            })
            .await
            .expect("task should be created");

        store
            .update_with_status_transition(
                task.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::InProgress),
                    context: user_context("Start work"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        // in_progress -> ready is legal; in_progress -> pending_approval is not,
        // so a restore that would require it must fail rather than bypass the
        // board rules.
        let store2 = setup_test_store().await;
        let pending = store2
            .create(CreateTaskInput {
                status: TaskStatus::PendingApproval,
                ..task_input("pending")
            })
            .await
            .expect("task should be created");
        store2
            .update_with_status_transition(
                pending.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::Ready),
                    context: user_context("Approve"),
                    ..Default::default()
                },
            )
            .await
            .expect("approval should succeed");
        store2
            .update_with_status_transition(
                pending.task_number,
                UpdateTaskInput {
                    status: Some(TaskStatus::InProgress),
                    context: user_context("Start"),
                    ..Default::default()
                },
            )
            .await
            .expect("start should succeed");

        assert!(
            store2
                .restore_revision(pending.task_number, 1, user_context("Rewind"))
                .await
                .is_err(),
            "restore must not bypass status transition rules"
        );
    }

    #[tokio::test]
    async fn baseline_backfill_is_idempotent_and_leaves_task_data_alone() {
        let store = setup_test_store().await;
        let task = store
            .create(task_input("already versioned"))
            .await
            .expect("task should be created");

        // Simulate a task that predates revision history.
        sqlx::query("UPDATE tasks SET revision = 0")
            .execute(store.pool())
            .await
            .expect("revision reset");
        sqlx::query("DELETE FROM task_revisions")
            .execute(store.pool())
            .await
            .expect("history reset");

        let written = store
            .backfill_baseline_revisions()
            .await
            .expect("backfill should succeed");
        assert_eq!(written, 1);

        let again = store
            .backfill_baseline_revisions()
            .await
            .expect("backfill should be idempotent");
        assert_eq!(again, 0);

        let revisions = store
            .list_revisions(task.task_number, 10)
            .await
            .expect("history should load");
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].revision, 1);
        assert_eq!(revisions[0].source, TaskMutationSource::Migration);

        let reloaded = store
            .get_by_number(task.task_number)
            .await
            .expect("task should load")
            .expect("task should exist");
        assert_eq!(reloaded.revision, 1);
        assert_eq!(reloaded.title, task.title);
        assert_eq!(reloaded.description, task.description);
    }

    #[tokio::test]
    async fn backfill_on_an_empty_store_writes_nothing() {
        let store = setup_test_store().await;
        assert_eq!(
            store
                .backfill_baseline_revisions()
                .await
                .expect("backfill should succeed"),
            0
        );
    }

    #[tokio::test]
    async fn comments_are_append_only_and_survive_independently_of_edits() {
        let (store, number) = store_with_task().await;

        store
            .add_comment(CreateTaskCommentInput {
                task_number: number,
                author_type: TaskAuthorKind::User,
                author_id: Some("jamie".to_string()),
                body: "Scope this to notes/ first.".to_string(),
                worker_id: None,
                metadata: serde_json::json!({}),
            })
            .await
            .expect("comment should be written");

        store
            .update_with_status_transition(
                number,
                UpdateTaskInput {
                    description: Some(Some("scoped to notes/".to_string())),
                    context: user_context("Applied the comment"),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed");

        let comments = store
            .list_comments(number, 50, None)
            .await
            .expect("comments should load");
        assert_eq!(comments.len(), 1);
        assert_eq!(store.count_comments(number).await.expect("count"), 1);

        // Comments are not revisions and revisions are not comments.
        let revisions = store
            .list_revisions(number, 10)
            .await
            .expect("history should load");
        assert_eq!(revisions.len(), 2);
    }

    #[tokio::test]
    async fn short_and_oversized_comment_bodies_are_rejected() {
        let (store, number) = store_with_task().await;

        let too_short = store
            .add_comment(CreateTaskCommentInput {
                task_number: number,
                author_type: TaskAuthorKind::User,
                author_id: None,
                body: "ok".to_string(),
                worker_id: None,
                metadata: serde_json::json!({}),
            })
            .await;
        assert!(too_short.is_err());

        let too_long = store
            .add_comment(CreateTaskCommentInput {
                task_number: number,
                author_type: TaskAuthorKind::Agent,
                author_id: None,
                body: "x".repeat(crate::tasks::MAX_COMMENT_BODY_BYTES + 1),
                worker_id: None,
                metadata: serde_json::json!({}),
            })
            .await;
        assert!(too_long.is_err());

        assert_eq!(store.count_comments(number).await.expect("count"), 0);
    }

    #[tokio::test]
    async fn deleting_a_task_clears_its_comments_and_revisions() {
        let (store, number) = store_with_task().await;

        store
            .add_comment(CreateTaskCommentInput {
                task_number: number,
                author_type: TaskAuthorKind::User,
                author_id: None,
                body: "a durable thought".to_string(),
                worker_id: None,
                metadata: serde_json::json!({}),
            })
            .await
            .expect("comment should be written");

        assert!(store.delete(number).await.expect("delete should succeed"));

        let orphan_comments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_comments")
            .fetch_one(store.pool())
            .await
            .expect("count comments");
        let orphan_revisions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_revisions")
            .fetch_one(store.pool())
            .await
            .expect("count revisions");
        assert_eq!(orphan_comments, 0);
        assert_eq!(orphan_revisions, 0);
        assert!(
            store
                .list(TaskListFilter::default())
                .await
                .expect("list should succeed")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_edits_produce_distinct_sequential_revisions() {
        let (store, number) = store_with_task().await;
        let store = std::sync::Arc::new(store);

        let mut handles = Vec::new();
        for index in 0..5 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .update_with_status_transition(
                        number,
                        UpdateTaskInput {
                            description: Some(Some(format!("writer {index}"))),
                            context: user_context(&format!("edit {index}")),
                            ..Default::default()
                        },
                    )
                    .await
            }));
        }

        for handle in handles {
            handle.await.expect("task should join").expect("update");
        }

        let revisions = store
            .list_revisions(number, 50)
            .await
            .expect("history should load");
        let numbers: Vec<i64> = revisions.iter().map(|r| r.revision).collect();
        assert_eq!(
            numbers,
            vec![6, 5, 4, 3, 2, 1],
            "revision numbering must stay monotonic and gapless under concurrency"
        );

        let task = store
            .get_by_number(number)
            .await
            .expect("task should load")
            .expect("task should exist");
        assert_eq!(task.revision, 6);
    }
}
