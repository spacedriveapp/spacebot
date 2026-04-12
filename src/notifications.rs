//! Instance-level notification store.
//!
//! Persists actionable events (task approvals, worker failures, cortex
//! observations) to the global spacebot.db so the dashboard Inbox card always
//! has up-to-date data, even after a page reload or reconnect.

use crate::error::Result;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The class of event that triggered the notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    TaskApproval,
    WorkerFailed,
    CortexObservation,
}

impl NotificationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::TaskApproval => "task_approval",
            NotificationKind::WorkerFailed => "worker_failed",
            NotificationKind::CortexObservation => "cortex_observation",
        }
    }
}

/// Urgency level of the notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Info,
    Warn,
    Error,
}

impl NotificationSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationSeverity::Info => "info",
            NotificationSeverity::Warn => "warn",
            NotificationSeverity::Error => "error",
        }
    }
}

/// A persisted notification row.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub severity: String,
    pub title: String,
    pub body: Option<String>,
    pub agent_id: Option<String>,
    pub related_entity_type: Option<String>,
    pub related_entity_id: Option<String>,
    pub action_url: Option<String>,
    pub metadata: Option<String>,
    pub created_at: String,
    pub read_at: Option<String>,
    pub dismissed_at: Option<String>,
}

/// Input for creating a notification.
#[derive(Debug, Clone)]
pub struct NewNotification {
    pub kind: NotificationKind,
    pub severity: NotificationSeverity,
    pub title: String,
    pub body: Option<String>,
    pub agent_id: Option<String>,
    pub related_entity_type: Option<String>,
    pub related_entity_id: Option<String>,
    pub action_url: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Filters for listing notifications.
#[derive(Debug, Default)]
pub struct NotificationFilter {
    /// If true, only return notifications where `read_at IS NULL`.
    pub unread_only: bool,
    /// If false (default), exclude dismissed notifications.
    pub include_dismissed: bool,
    pub agent_id: Option<String>,
    pub kind: Option<NotificationKind>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NotificationStore {
    pool: SqlitePool,
}

const SELECT_COLUMNS: &str = "SELECT id, kind, severity, title, body, agent_id, \
    related_entity_type, related_entity_id, action_url, metadata, \
    created_at, read_at, dismissed_at";

impl NotificationStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a notification. Returns the new row, or `None` if a duplicate
    /// undismissed notification for the same entity already exists.
    pub async fn insert(&self, n: NewNotification) -> Result<Option<Notification>> {
        let id = uuid::Uuid::new_v4().to_string();
        let metadata_json = n.metadata.as_ref().map(|m| m.to_string());

        let affected = sqlx::query(
            r#"
            INSERT OR IGNORE INTO notifications
                (id, kind, severity, title, body, agent_id,
                 related_entity_type, related_entity_id, action_url, metadata)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(n.kind.as_str())
        .bind(n.severity.as_str())
        .bind(&n.title)
        .bind(&n.body)
        .bind(&n.agent_id)
        .bind(&n.related_entity_type)
        .bind(&n.related_entity_id)
        .bind(&n.action_url)
        .bind(&metadata_json)
        .execute(&self.pool)
        .await
        .context("failed to insert notification")?
        .rows_affected();

        if affected == 0 {
            return Ok(None);
        }

        self.get_by_id(&id).await.map(Some)
    }

    pub async fn get_by_id(&self, id: &str) -> Result<Notification> {
        let row = sqlx::query(&format!("{SELECT_COLUMNS} FROM notifications WHERE id = ?"))
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .context("failed to fetch notification by id")?;

        notification_from_row(row)
    }

    pub async fn list(&self, filter: NotificationFilter) -> Result<Vec<Notification>> {
        let mut query = format!("{SELECT_COLUMNS} FROM notifications WHERE 1=1");

        if filter.unread_only {
            query.push_str(" AND read_at IS NULL");
        }
        if !filter.include_dismissed {
            query.push_str(" AND dismissed_at IS NULL");
        }
        if filter.agent_id.is_some() {
            query.push_str(" AND agent_id = ?");
        }
        if filter.kind.is_some() {
            query.push_str(" AND kind = ?");
        }
        query.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");

        let mut sql = sqlx::query(&query);
        if let Some(ref agent_id) = filter.agent_id {
            sql = sql.bind(agent_id);
        }
        if let Some(kind) = filter.kind {
            sql = sql.bind(kind.as_str());
        }
        sql = sql.bind(filter.limit.unwrap_or(50).clamp(1, 500));
        sql = sql.bind(filter.offset.unwrap_or(0));

        let rows = sql
            .fetch_all(&self.pool)
            .await
            .context("failed to list notifications")?;

        rows.into_iter().map(notification_from_row).collect()
    }

    pub async fn unread_count(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notifications WHERE read_at IS NULL AND dismissed_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .context("failed to count unread notifications")?;
        Ok(count)
    }

    /// Mark a single notification as read. Returns true if it was updated.
    pub async fn mark_read(&self, id: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE notifications SET read_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE id = ? AND read_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to mark notification read")?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Mark all undismissed notifications as read. Returns the count updated.
    pub async fn mark_all_read(&self) -> Result<u64> {
        let affected = sqlx::query(
            "UPDATE notifications SET read_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE read_at IS NULL AND dismissed_at IS NULL",
        )
        .execute(&self.pool)
        .await
        .context("failed to mark all notifications read")?
        .rows_affected();
        Ok(affected)
    }

    /// Dismiss a single notification. Returns true if it was updated.
    pub async fn dismiss(&self, id: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE notifications SET \
             dismissed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), \
             read_at = COALESCE(read_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) \
             WHERE id = ? AND dismissed_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .context("failed to dismiss notification")?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Dismiss all active (undismissed) notifications for a given entity.
    /// Used to automatically clean up e.g. task_approval notifications when a task is approved.
    pub async fn dismiss_by_entity(
        &self,
        kind: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<u64> {
        let affected = sqlx::query(
            "UPDATE notifications SET \
             dismissed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), \
             read_at = COALESCE(read_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) \
             WHERE kind = ? AND related_entity_type = ? AND related_entity_id = ? \
             AND dismissed_at IS NULL",
        )
        .bind(kind)
        .bind(entity_type)
        .bind(entity_id)
        .execute(&self.pool)
        .await
        .context("failed to dismiss notifications by entity")?
        .rows_affected();
        Ok(affected)
    }

    /// Dismiss all already-read notifications. Returns the count updated.
    pub async fn dismiss_read(&self) -> Result<u64> {
        let affected = sqlx::query(
            "UPDATE notifications SET dismissed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
             WHERE read_at IS NOT NULL AND dismissed_at IS NULL",
        )
        .execute(&self.pool)
        .await
        .context("failed to dismiss read notifications")?
        .rows_affected();
        Ok(affected)
    }
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn notification_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Notification> {
    Ok(Notification {
        id: row
            .try_get("id")
            .context("failed to read notification id")?,
        kind: row
            .try_get("kind")
            .context("failed to read notification kind")?,
        severity: row
            .try_get("severity")
            .context("failed to read notification severity")?,
        title: row
            .try_get("title")
            .context("failed to read notification title")?,
        body: row.try_get("body").ok(),
        agent_id: row.try_get("agent_id").ok(),
        related_entity_type: row.try_get("related_entity_type").ok(),
        related_entity_id: row.try_get("related_entity_id").ok(),
        action_url: row.try_get("action_url").ok(),
        metadata: row.try_get("metadata").ok(),
        created_at: row
            .try_get("created_at")
            .context("failed to read notification created_at")?,
        read_at: row.try_get("read_at").ok(),
        dismissed_at: row.try_get("dismissed_at").ok(),
    })
}
