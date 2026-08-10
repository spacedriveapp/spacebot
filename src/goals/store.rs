//! Goal CRUD storage (SQLite).
//!
//! Operates against the instance-level database alongside tasks. Goals are
//! instance-scoped: one goal list shared across all agents.

use crate::error::Result;
use crate::tasks::{TaskPriority, TaskStatus};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Completed,
    Abandoned,
}

impl GoalStatus {
    pub const ALL: [GoalStatus; 4] = [
        GoalStatus::Active,
        GoalStatus::Paused,
        GoalStatus::Completed,
        GoalStatus::Abandoned,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Completed => "completed",
            GoalStatus::Abandoned => "abandoned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(GoalStatus::Active),
            "paused" => Some(GoalStatus::Paused),
            "completed" => Some(GoalStatus::Completed),
            "abandoned" => Some(GoalStatus::Abandoned),
            _ => None,
        }
    }
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: GoalStatus,
    pub priority: TaskPriority,
    pub due_date: Option<String>,
    pub notes: Option<String>,
    pub metadata: Value,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateGoalInput {
    pub title: String,
    pub description: Option<String>,
    pub priority: TaskPriority,
    pub due_date: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateGoalInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<GoalStatus>,
    pub priority: Option<TaskPriority>,
    /// New due date. `Some("")` clears the due date.
    pub due_date: Option<String>,
    /// Replacement progress notes — a full overwrite, never an append.
    /// `Some("")` clears the notes.
    pub notes: Option<String>,
    /// Object patch deep-merged into the current metadata.
    pub metadata: Option<Value>,
}

/// Filters for listing goals.
#[derive(Debug, Clone, Default)]
pub struct GoalListFilter {
    pub status: Option<GoalStatus>,
    pub limit: Option<i64>,
}

/// Linked task counts by status for a single goal.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GoalTaskCounts {
    pub pending_approval: i64,
    pub backlog: i64,
    pub ready: i64,
    pub in_progress: i64,
    pub done: i64,
    pub failed: i64,
}

impl GoalTaskCounts {
    pub fn total(&self) -> i64 {
        self.pending_approval
            + self.backlog
            + self.ready
            + self.in_progress
            + self.done
            + self.failed
    }

    fn add(&mut self, status: TaskStatus, count: i64) {
        match status {
            TaskStatus::PendingApproval => self.pending_approval += count,
            TaskStatus::Backlog => self.backlog += count,
            TaskStatus::Ready => self.ready += count,
            TaskStatus::InProgress => self.in_progress += count,
            TaskStatus::Done => self.done += count,
            TaskStatus::Failed => self.failed += count,
        }
    }

    /// One-line progress summary for prompt injection, used when a goal has
    /// no progress notes.
    pub fn summary(&self) -> String {
        let total = self.total();
        if total == 0 {
            return "No tasks yet".to_string();
        }
        if self.in_progress > 0 {
            let noun = if self.in_progress == 1 {
                "task"
            } else {
                "tasks"
            };
            return format!("{} {noun} in progress", self.in_progress);
        }
        format!("{}/{total} tasks done", self.done)
    }
}

#[derive(Debug, Clone)]
pub struct GoalStore {
    pool: SqlitePool,
}

impl GoalStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, input: CreateGoalInput) -> Result<Goal> {
        let due_date = validate_due_date(input.due_date)?;
        let goal_id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO goals (id, title, description, priority, due_date, metadata) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&goal_id)
        .bind(&input.title)
        .bind(&input.description)
        .bind(input.priority.as_str())
        .bind(&due_date)
        .bind(input.metadata.to_string())
        .execute(&self.pool)
        .await
        .context("failed to insert goal")?;

        self.get(&goal_id)
            .await?
            .context("goal inserted but not found")
            .map_err(Into::into)
    }

    pub async fn get(&self, goal_id: &str) -> Result<Option<Goal>> {
        let row = sqlx::query(&format!("{SELECT_COLUMNS} FROM goals WHERE id = ?"))
            .bind(goal_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch goal by id")?;

        row.map(goal_from_row).transpose()
    }

    /// List goals, optionally filtered by status. Ordered by priority then
    /// creation time so the most important goals render first.
    pub async fn list(&self, filter: GoalListFilter) -> Result<Vec<Goal>> {
        let mut query = format!("{SELECT_COLUMNS} FROM goals WHERE 1=1");
        if filter.status.is_some() {
            query.push_str(" AND status = ?");
        }
        query.push_str(
            " ORDER BY CASE priority \
               WHEN 'critical' THEN 0 \
               WHEN 'high' THEN 1 \
               WHEN 'medium' THEN 2 \
               WHEN 'low' THEN 3 \
               ELSE 4 END ASC, \
             created_at ASC LIMIT ?",
        );

        let mut sql = sqlx::query(&query);
        if let Some(status) = filter.status {
            sql = sql.bind(status.as_str());
        }
        sql = sql.bind(filter.limit.unwrap_or(100).clamp(1, 500));

        let rows = sql
            .fetch_all(&self.pool)
            .await
            .context("failed to list goals")?;

        rows.into_iter().map(goal_from_row).collect()
    }

    pub async fn update(&self, goal_id: &str, input: UpdateGoalInput) -> Result<Option<Goal>> {
        let due_date = validate_due_date(input.due_date)?;

        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .context("failed to open goal update transaction")?;

        let row = sqlx::query(&format!("{SELECT_COLUMNS} FROM goals WHERE id = ?"))
            .bind(goal_id)
            .fetch_optional(&mut *tx)
            .await
            .context("failed to fetch goal by id for update")?;

        let Some(row) = row else {
            tx.commit()
                .await
                .context("failed to commit empty goal update transaction")?;
            return Ok(None);
        };

        let current = goal_from_row(row)?;

        if let Some(next_status) = input.status
            && !can_transition(current.status, next_status)
        {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "invalid goal status transition: {} -> {}",
                current.status,
                next_status
            )));
        }

        let next_status = input.status.unwrap_or(current.status);
        let next_priority = input.priority.unwrap_or(current.priority);
        let next_due_date = match due_date {
            Some(value) if value.is_empty() => None,
            Some(value) => Some(value),
            None => current.due_date,
        };
        // Notes are a single replace-not-append field: the agent's current
        // assessment of where the goal stands, not a history log.
        let next_notes = match input.notes {
            Some(value) if value.is_empty() => None,
            Some(value) => Some(value),
            None => current.notes,
        };
        let next_metadata =
            crate::tasks::store::merge_json_object(current.metadata, input.metadata);

        let mut query = String::from(
            "UPDATE goals SET title = ?, description = ?, status = ?, priority = ?, \
             due_date = ?, notes = ?, metadata = ?, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        );
        if next_status == GoalStatus::Completed && current.completed_at.is_none() {
            query.push_str(", completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')");
        }
        query.push_str(" WHERE id = ?");

        sqlx::query(&query)
            .bind(input.title.unwrap_or(current.title))
            .bind(input.description.or(current.description))
            .bind(next_status.as_str())
            .bind(next_priority.as_str())
            .bind(&next_due_date)
            .bind(&next_notes)
            .bind(next_metadata.to_string())
            .bind(goal_id)
            .execute(&mut *tx)
            .await
            .context("failed to update goal")?;

        let updated = sqlx::query(&format!("{SELECT_COLUMNS} FROM goals WHERE id = ?"))
            .bind(goal_id)
            .fetch_one(&mut *tx)
            .await
            .context("failed to fetch updated goal")?;

        tx.commit()
            .await
            .context("failed to commit goal update transaction")?;

        Ok(Some(goal_from_row(updated)?))
    }

    /// Count tasks linked to a goal, grouped by task status.
    pub async fn linked_task_counts(&self, goal_id: &str) -> Result<GoalTaskCounts> {
        let rows = sqlx::query(
            "SELECT status, COUNT(*) AS count FROM tasks WHERE goal_id = ? GROUP BY status",
        )
        .bind(goal_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to count linked tasks")?;

        let mut counts = GoalTaskCounts::default();
        for row in rows {
            let status_value: String = row
                .try_get("status")
                .context("failed to read linked task status")?;
            let count: i64 = row
                .try_get("count")
                .context("failed to read linked task count")?;
            let status = TaskStatus::parse(&status_value)
                .with_context(|| format!("invalid task status in database: {status_value}"))?;
            counts.add(status, count);
        }

        Ok(counts)
    }
}

/// Column list used by all SELECT queries. Kept in sync with `goal_from_row`.
const SELECT_COLUMNS: &str = "SELECT id, title, description, status, priority, due_date, notes, \
     metadata, created_at, updated_at, completed_at";

/// Goal lifecycle: active ↔ paused, active → completed, active → abandoned.
/// Completed and abandoned are terminal.
pub fn can_transition(current: GoalStatus, next: GoalStatus) -> bool {
    if current == next {
        return true;
    }

    matches!(
        (current, next),
        (GoalStatus::Active, GoalStatus::Paused)
            | (GoalStatus::Paused, GoalStatus::Active)
            | (GoalStatus::Active, GoalStatus::Completed)
            | (GoalStatus::Active, GoalStatus::Abandoned)
    )
}

/// Validate an optional due date. Empty strings pass through (they clear the
/// field on update); anything else must be a day-granularity ISO 8601 date.
fn validate_due_date(due_date: Option<String>) -> Result<Option<String>> {
    if let Some(value) = &due_date
        && !value.is_empty()
    {
        chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .with_context(|| format!("invalid due_date (expected YYYY-MM-DD): {value}"))?;
    }
    Ok(due_date)
}

fn parse_metadata(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

fn goal_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Goal> {
    let status_value: String = row
        .try_get("status")
        .context("failed to read goal status")?;
    let priority_value: String = row
        .try_get("priority")
        .context("failed to read goal priority")?;
    let metadata_value: String = row.try_get("metadata").unwrap_or_else(|_| "{}".to_string());

    let status = GoalStatus::parse(&status_value)
        .with_context(|| format!("invalid goal status in database: {status_value}"))?;
    let priority = TaskPriority::parse(&priority_value)
        .with_context(|| format!("invalid goal priority in database: {priority_value}"))?;

    Ok(Goal {
        id: row.try_get("id").context("failed to read goal id")?,
        title: row.try_get("title").context("failed to read goal title")?,
        description: row.try_get("description").ok(),
        status,
        priority,
        due_date: row
            .try_get::<Option<String>, _>("due_date")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        notes: row
            .try_get::<Option<String>, _>("notes")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
        metadata: parse_metadata(&metadata_value),
        created_at: row
            .try_get("created_at")
            .context("failed to read goal created_at")?,
        updated_at: row
            .try_get("updated_at")
            .context("failed to read goal updated_at")?,
        completed_at: row
            .try_get::<Option<String>, _>("completed_at")
            .ok()
            .flatten()
            .filter(|value| !value.is_empty()),
    })
}

#[cfg(test)]
pub(crate) async fn setup_test_store() -> GoalStore {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite should connect");

    // Goals live in the instance database — run the real global migrations so
    // the tasks table (with goal_id) is available for linked-task counts.
    sqlx::migrate!("./migrations/global")
        .run(&pool)
        .await
        .expect("global migrations should run");

    GoalStore::new(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_input(title: &str) -> CreateGoalInput {
        CreateGoalInput {
            title: title.to_string(),
            description: None,
            priority: TaskPriority::Medium,
            due_date: None,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn create_defaults_to_active() {
        let store = setup_test_store().await;
        let goal = store
            .create(basic_input("Ship v2"))
            .await
            .expect("goal should be created");

        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.priority, TaskPriority::Medium);
        assert!(goal.completed_at.is_none());
    }

    #[tokio::test]
    async fn create_rejects_invalid_due_date() {
        let store = setup_test_store().await;
        let error = store
            .create(CreateGoalInput {
                due_date: Some("May 1st".to_string()),
                ..basic_input("bad date")
            })
            .await
            .expect_err("non-ISO due date must fail");

        assert!(error.to_string().contains("invalid due_date"));
    }

    #[tokio::test]
    async fn list_filters_by_status_and_orders_by_priority() {
        let store = setup_test_store().await;
        store
            .create(CreateGoalInput {
                priority: TaskPriority::Low,
                ..basic_input("low goal")
            })
            .await
            .expect("should create");
        store
            .create(CreateGoalInput {
                priority: TaskPriority::Critical,
                ..basic_input("critical goal")
            })
            .await
            .expect("should create");
        let paused = store
            .create(basic_input("paused goal"))
            .await
            .expect("should create");
        store
            .update(
                &paused.id,
                UpdateGoalInput {
                    status: Some(GoalStatus::Paused),
                    ..Default::default()
                },
            )
            .await
            .expect("pause should succeed");

        let active = store
            .list(GoalListFilter {
                status: Some(GoalStatus::Active),
                ..Default::default()
            })
            .await
            .expect("list should succeed");
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].title, "critical goal");
        assert_eq!(active[1].title, "low goal");

        let all = store
            .list(GoalListFilter::default())
            .await
            .expect("list should succeed");
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn update_replaces_notes_instead_of_appending() {
        let store = setup_test_store().await;
        let goal = store
            .create(basic_input("notes goal"))
            .await
            .expect("goal should be created");

        let first = store
            .update(
                &goal.id,
                UpdateGoalInput {
                    notes: Some("Audit complete".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("goal should exist");
        assert_eq!(first.notes.as_deref(), Some("Audit complete"));

        let second = store
            .update(
                &goal.id,
                UpdateGoalInput {
                    notes: Some("Migration started".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("goal should exist");
        assert_eq!(second.notes.as_deref(), Some("Migration started"));

        let cleared = store
            .update(
                &goal.id,
                UpdateGoalInput {
                    notes: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("goal should exist");
        assert!(cleared.notes.is_none());
    }

    #[tokio::test]
    async fn metadata_updates_deep_merge() {
        let store = setup_test_store().await;
        let goal = store
            .create(CreateGoalInput {
                metadata: serde_json::json!({"github_milestone": "v2.0"}),
                ..basic_input("metadata goal")
            })
            .await
            .expect("goal should be created");

        let updated = store
            .update(
                &goal.id,
                UpdateGoalInput {
                    metadata: Some(serde_json::json!({"linear_id": "OBJ-12"})),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("goal should exist");

        assert_eq!(
            updated.metadata,
            serde_json::json!({"github_milestone": "v2.0", "linear_id": "OBJ-12"})
        );
    }

    #[tokio::test]
    async fn completion_sets_completed_at() {
        let store = setup_test_store().await;
        let goal = store
            .create(basic_input("completable goal"))
            .await
            .expect("goal should be created");

        let completed = store
            .update(
                &goal.id,
                UpdateGoalInput {
                    status: Some(GoalStatus::Completed),
                    ..Default::default()
                },
            )
            .await
            .expect("update should succeed")
            .expect("goal should exist");

        assert_eq!(completed.status, GoalStatus::Completed);
        assert!(completed.completed_at.is_some());
    }

    #[tokio::test]
    async fn rejects_invalid_status_transitions() {
        let store = setup_test_store().await;
        let goal = store
            .create(basic_input("terminal goal"))
            .await
            .expect("goal should be created");

        store
            .update(
                &goal.id,
                UpdateGoalInput {
                    status: Some(GoalStatus::Abandoned),
                    ..Default::default()
                },
            )
            .await
            .expect("abandon should succeed");

        let error = store
            .update(
                &goal.id,
                UpdateGoalInput {
                    status: Some(GoalStatus::Active),
                    ..Default::default()
                },
            )
            .await
            .expect_err("abandoned -> active must fail");

        assert!(error.to_string().contains("invalid goal status transition"));
    }

    #[test]
    fn transition_matrix_matches_lifecycle() {
        use GoalStatus::*;

        // Same-status updates are always allowed.
        for status in GoalStatus::ALL {
            assert!(can_transition(status, status));
        }

        assert!(can_transition(Active, Paused));
        assert!(can_transition(Paused, Active));
        assert!(can_transition(Active, Completed));
        assert!(can_transition(Active, Abandoned));

        assert!(!can_transition(Paused, Completed));
        assert!(!can_transition(Paused, Abandoned));
        assert!(!can_transition(Completed, Active));
        assert!(!can_transition(Completed, Paused));
        assert!(!can_transition(Abandoned, Active));
        assert!(!can_transition(Completed, Abandoned));
        assert!(!can_transition(Abandoned, Completed));
    }

    #[tokio::test]
    async fn linked_task_counts_cover_all_statuses() {
        let store = setup_test_store().await;
        let goal = store
            .create(basic_input("tracked goal"))
            .await
            .expect("goal should be created");

        let statuses = [
            "pending_approval",
            "backlog",
            "ready",
            "in_progress",
            "done",
            "done",
            "failed",
        ];
        for (index, status) in statuses.iter().enumerate() {
            sqlx::query(
                "INSERT INTO tasks (id, task_number, title, status, owner_agent_id, created_by, goal_id) \
                 VALUES (?, ?, ?, ?, 'agent-test', 'branch', ?)",
            )
            .bind(format!("task-{index}"))
            .bind(index as i64 + 1)
            .bind(format!("task {index}"))
            .bind(status)
            .bind(&goal.id)
            .execute(&store.pool)
            .await
            .expect("linked task should insert");
        }

        let counts = store
            .linked_task_counts(&goal.id)
            .await
            .expect("counts should succeed");

        assert_eq!(counts.pending_approval, 1);
        assert_eq!(counts.backlog, 1);
        assert_eq!(counts.ready, 1);
        assert_eq!(counts.in_progress, 1);
        assert_eq!(counts.done, 2);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.total(), 7);

        let unlinked = store
            .linked_task_counts("missing-goal")
            .await
            .expect("counts should succeed");
        assert_eq!(unlinked.total(), 0);
        assert_eq!(unlinked.summary(), "No tasks yet");
    }

    #[tokio::test]
    async fn render_active_goals_uses_notes_then_task_summary() {
        let store = setup_test_store().await;
        store
            .create(CreateGoalInput {
                priority: TaskPriority::High,
                due_date: Some("2026-05-01".to_string()),
                ..basic_input("Migrate auth to Clerk")
            })
            .await
            .expect("goal should be created");
        let noted = store
            .create(basic_input("Ship v2 documentation"))
            .await
            .expect("goal should be created");
        store
            .update(
                &noted.id,
                UpdateGoalInput {
                    notes: Some("Outline drafted\nMore detail below".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("notes update should succeed");

        let rendered = crate::goals::render_active_goals(&store)
            .await
            .expect("render should succeed");

        assert!(rendered.starts_with("## Active Goals"));
        assert!(
            rendered.contains("- [HIGH] Migrate auth to Clerk (due: 2026-05-01) — No tasks yet")
        );
        assert!(rendered.contains("- [MEDIUM] Ship v2 documentation — Outline drafted"));
        assert!(!rendered.contains("More detail below"));
    }

    /// Insert a task linked to `goal_id` with the given status.
    async fn linked_task(store: &GoalStore, goal_id: &str, number: i64, status: &str) {
        sqlx::query(
            "INSERT INTO tasks (id, task_number, title, status, owner_agent_id, created_by, goal_id) \
             VALUES (?, ?, ?, ?, 'agent-test', 'autonomy', ?)",
        )
        .bind(format!("task-{number}"))
        .bind(number)
        .bind(format!("linked task {number}"))
        .bind(status)
        .bind(goal_id)
        .execute(&store.pool)
        .await
        .expect("linked task should insert");
    }

    #[tokio::test]
    async fn goals_are_flagged_for_review_only_when_every_linked_task_is_done() {
        let store = setup_test_store().await;
        let complete = store.create(basic_input("all done")).await.expect("create");
        let partial = store
            .create(basic_input("still going"))
            .await
            .expect("create");
        let untouched = store.create(basic_input("no tasks")).await.expect("create");
        let failing = store
            .create(basic_input("has a failure"))
            .await
            .expect("create");

        linked_task(&store, &complete.id, 1, "done").await;
        linked_task(&store, &complete.id, 2, "done").await;
        linked_task(&store, &partial.id, 3, "done").await;
        linked_task(&store, &partial.id, 4, "in_progress").await;
        linked_task(&store, &failing.id, 5, "done").await;
        linked_task(&store, &failing.id, 6, "failed").await;

        store
            .update(
                &complete.id,
                UpdateGoalInput {
                    notes: Some("Migration finished, docs updated.".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("notes should set");

        let marked = crate::goals::mark_goals_ready_for_review(&store)
            .await
            .expect("review pass should succeed");
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].id, complete.id);

        let notes = marked[0].notes.as_deref().expect("notes should be set");
        assert!(notes.starts_with(crate::goals::GOAL_READY_FOR_REVIEW_NOTE));
        assert!(
            notes.contains("Migration finished, docs updated."),
            "the agent's own assessment must survive the marker"
        );
        // The signal never closes the goal.
        assert_eq!(marked[0].status, GoalStatus::Active);

        for untouched_goal in [&partial.id, &untouched.id, &failing.id] {
            let goal = store
                .get(untouched_goal)
                .await
                .expect("get")
                .expect("goal exists");
            assert!(goal.notes.is_none(), "only completed goals are flagged");
        }

        // Running again is a no-op rather than stacking markers.
        let repeat = crate::goals::mark_goals_ready_for_review(&store)
            .await
            .expect("review pass should succeed");
        assert!(repeat.is_empty());
    }

    #[tokio::test]
    async fn render_active_goals_is_empty_without_active_goals() {
        let store = setup_test_store().await;
        let rendered = crate::goals::render_active_goals(&store)
            .await
            .expect("render should succeed");
        assert!(rendered.is_empty());
    }
}
