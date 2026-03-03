//! Topic CRUD storage (SQLite).

use crate::error::Result;
use crate::memory::{MemoryType, SearchConfig, SearchMode, SearchSort};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TopicStatus {
    Active,
    Paused,
    Archived,
}

impl TopicStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TopicStatus::Active => "active",
            TopicStatus::Paused => "paused",
            TopicStatus::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(TopicStatus::Active),
            "paused" => Some(TopicStatus::Paused),
            "archived" => Some(TopicStatus::Archived),
            _ => None,
        }
    }
}

impl std::fmt::Display for TopicStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Structured criteria for querying memories during topic synthesis.
/// Maps to `SearchConfig` plus SQL-level filters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicCriteria {
    /// Semantic query for hybrid search (omit for pure metadata queries).
    #[serde(default)]
    pub query: Option<String>,
    /// Filter to specific memory types (empty = all types).
    #[serde(default)]
    pub memory_types: Vec<MemoryType>,
    /// Minimum importance threshold.
    #[serde(default)]
    pub min_importance: Option<f32>,
    /// Only memories from these channels (empty = all channels).
    #[serde(default)]
    pub channel_ids: Vec<String>,
    /// Only memories newer than this duration ago (e.g. "30d", "7d").
    #[serde(default)]
    pub max_age: Option<String>,
    /// Search mode override (defaults to Hybrid if query present, Recent otherwise).
    #[serde(default)]
    pub mode: Option<SearchMode>,
    /// Sort order for non-hybrid modes.
    #[serde(default)]
    pub sort_by: Option<SearchSort>,
    /// Max memories to gather before synthesis (default 30).
    #[serde(default = "default_max_memories")]
    pub max_memories: usize,
}

fn default_max_memories() -> usize {
    30
}

impl Default for TopicCriteria {
    fn default() -> Self {
        Self {
            query: None,
            memory_types: Vec::new(),
            min_importance: None,
            channel_ids: Vec::new(),
            max_age: None,
            mode: None,
            sort_by: None,
            max_memories: default_max_memories(),
        }
    }
}

impl TopicCriteria {
    /// Build a `SearchConfig` from criteria fields.
    pub fn to_search_config(&self) -> SearchConfig {
        let mode = self.mode.unwrap_or_else(|| {
            if self.query.is_some() {
                SearchMode::Hybrid
            } else if !self.memory_types.is_empty() {
                SearchMode::Typed
            } else {
                SearchMode::Recent
            }
        });
        let sort_by = self.sort_by.unwrap_or(SearchSort::Recent);
        let memory_type = self.memory_types.first().copied();

        SearchConfig {
            mode,
            memory_type,
            sort_by,
            max_results: self.max_memories,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub content: String,
    pub criteria: TopicCriteria,
    pub pin_ids: Vec<String>,
    pub status: TopicStatus,
    pub max_words: usize,
    pub last_memory_at: Option<String>,
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicVersion {
    pub id: String,
    pub topic_id: String,
    pub content: String,
    pub memory_count: i64,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct TopicStore {
    pool: SqlitePool,
}

impl TopicStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, topic: &Topic) -> Result<()> {
        let criteria_json =
            serde_json::to_string(&topic.criteria).context("failed to serialize topic criteria")?;
        let pin_ids_json =
            serde_json::to_string(&topic.pin_ids).context("failed to serialize topic pin_ids")?;

        sqlx::query(
            r#"
            INSERT INTO topics (
                id, agent_id, title, content, criteria, pin_ids,
                status, max_words, last_memory_at, last_synced_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&topic.id)
        .bind(&topic.agent_id)
        .bind(&topic.title)
        .bind(&topic.content)
        .bind(&criteria_json)
        .bind(&pin_ids_json)
        .bind(topic.status.as_str())
        .bind(topic.max_words as i64)
        .bind(&topic.last_memory_at)
        .bind(&topic.last_synced_at)
        .execute(&self.pool)
        .await
        .context("failed to insert topic")?;

        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<Topic>> {
        let row = sqlx::query(
            "SELECT id, agent_id, title, content, criteria, pin_ids, \
             status, max_words, last_memory_at, last_synced_at, created_at, updated_at \
             FROM topics WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch topic")?;

        row.map(topic_from_row).transpose()
    }

    pub async fn list(&self, agent_id: &str) -> Result<Vec<Topic>> {
        let rows = sqlx::query(
            "SELECT id, agent_id, title, content, criteria, pin_ids, \
             status, max_words, last_memory_at, last_synced_at, created_at, updated_at \
             FROM topics WHERE agent_id = ? ORDER BY created_at DESC",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list topics")?;

        rows.into_iter().map(topic_from_row).collect()
    }

    pub async fn list_active(&self, agent_id: &str) -> Result<Vec<Topic>> {
        let rows = sqlx::query(
            "SELECT id, agent_id, title, content, criteria, pin_ids, \
             status, max_words, last_memory_at, last_synced_at, created_at, updated_at \
             FROM topics WHERE agent_id = ? AND status = 'active' ORDER BY created_at DESC",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list active topics")?;

        rows.into_iter().map(topic_from_row).collect()
    }

    pub async fn update(&self, topic: &Topic) -> Result<()> {
        let criteria_json =
            serde_json::to_string(&topic.criteria).context("failed to serialize topic criteria")?;
        let pin_ids_json =
            serde_json::to_string(&topic.pin_ids).context("failed to serialize topic pin_ids")?;

        sqlx::query(
            r#"
            UPDATE topics SET
                title = ?, content = ?, criteria = ?, pin_ids = ?,
                status = ?, max_words = ?, last_memory_at = ?, last_synced_at = ?,
                updated_at = datetime('now')
            WHERE id = ?
            "#,
        )
        .bind(&topic.title)
        .bind(&topic.content)
        .bind(&criteria_json)
        .bind(&pin_ids_json)
        .bind(topic.status.as_str())
        .bind(topic.max_words as i64)
        .bind(&topic.last_memory_at)
        .bind(&topic.last_synced_at)
        .bind(&topic.id)
        .execute(&self.pool)
        .await
        .context("failed to update topic")?;

        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM topics WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete topic")?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn save_version(&self, version: &TopicVersion) -> Result<()> {
        sqlx::query(
            "INSERT INTO topic_versions (id, topic_id, content, memory_count) VALUES (?, ?, ?, ?)",
        )
        .bind(&version.id)
        .bind(&version.topic_id)
        .bind(&version.content)
        .bind(version.memory_count)
        .execute(&self.pool)
        .await
        .context("failed to save topic version")?;

        Ok(())
    }

    pub async fn get_versions(&self, topic_id: &str, limit: i64) -> Result<Vec<TopicVersion>> {
        let rows = sqlx::query(
            "SELECT id, topic_id, content, memory_count, created_at \
             FROM topic_versions WHERE topic_id = ? ORDER BY created_at DESC LIMIT ?",
        )
        .bind(topic_id)
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch topic versions")?;

        rows.into_iter().map(version_from_row).collect()
    }

    /// Clear `last_synced_at` on all active topics for an agent, forcing
    /// the sync loop to re-evaluate and re-synthesize them.
    pub async fn mark_all_stale(&self, agent_id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE topics SET last_synced_at = NULL WHERE agent_id = ? AND status = 'active'",
        )
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .context("failed to mark topics stale")?;

        Ok(())
    }
}

fn parse_json_string_array(value: &str) -> Vec<String> {
    serde_json::from_str(value).unwrap_or_default()
}

fn parse_criteria(value: &str) -> TopicCriteria {
    serde_json::from_str(value).unwrap_or_default()
}

fn topic_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Topic> {
    let status_value: String = row
        .try_get("status")
        .context("failed to read topic status")?;
    let criteria_value: String = row.try_get("criteria").unwrap_or_else(|_| "{}".to_string());
    let pin_ids_value: String = row.try_get("pin_ids").unwrap_or_else(|_| "[]".to_string());

    let status = TopicStatus::parse(&status_value)
        .with_context(|| format!("invalid topic status in database: {status_value}"))?;

    Ok(Topic {
        id: row.try_get("id").context("failed to read topic id")?,
        agent_id: row
            .try_get("agent_id")
            .context("failed to read topic agent_id")?,
        title: row.try_get("title").context("failed to read topic title")?,
        content: row
            .try_get("content")
            .context("failed to read topic content")?,
        criteria: parse_criteria(&criteria_value),
        pin_ids: parse_json_string_array(&pin_ids_value),
        status,
        max_words: row
            .try_get::<i64, _>("max_words")
            .map(|value| value as usize)
            .unwrap_or(1500),
        last_memory_at: row.try_get("last_memory_at").ok(),
        last_synced_at: row.try_get("last_synced_at").ok(),
        created_at: row
            .try_get::<chrono::NaiveDateTime, _>("created_at")
            .map(|value| value.and_utc().to_rfc3339())
            .context("failed to read topic created_at")?,
        updated_at: row
            .try_get::<chrono::NaiveDateTime, _>("updated_at")
            .map(|value| value.and_utc().to_rfc3339())
            .context("failed to read topic updated_at")?,
    })
}

fn version_from_row(row: sqlx::sqlite::SqliteRow) -> Result<TopicVersion> {
    Ok(TopicVersion {
        id: row
            .try_get("id")
            .context("failed to read topic version id")?,
        topic_id: row
            .try_get("topic_id")
            .context("failed to read topic version topic_id")?,
        content: row
            .try_get("content")
            .context("failed to read topic version content")?,
        memory_count: row
            .try_get("memory_count")
            .context("failed to read topic version memory_count")?,
        created_at: row
            .try_get::<chrono::NaiveDateTime, _>("created_at")
            .map(|value| value.and_utc().to_rfc3339())
            .context("failed to read topic version created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn setup_store() -> TopicStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

        sqlx::query(
            r#"
            CREATE TABLE topics (
                id              TEXT PRIMARY KEY,
                agent_id        TEXT NOT NULL,
                title           TEXT NOT NULL,
                content         TEXT NOT NULL DEFAULT '',
                criteria        TEXT NOT NULL,
                pin_ids         TEXT NOT NULL DEFAULT '[]',
                status          TEXT NOT NULL DEFAULT 'active',
                max_words       INTEGER NOT NULL DEFAULT 1500,
                last_memory_at  TEXT,
                last_synced_at  TEXT,
                created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("topics schema should be created");

        sqlx::query(
            r#"
            CREATE TABLE topic_versions (
                id              TEXT PRIMARY KEY,
                topic_id        TEXT NOT NULL,
                content         TEXT NOT NULL,
                memory_count    INTEGER NOT NULL,
                created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (topic_id) REFERENCES topics(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("topic_versions schema should be created");

        TopicStore::new(pool)
    }

    fn make_topic(agent_id: &str, title: &str) -> Topic {
        Topic {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            title: title.to_string(),
            content: String::new(),
            criteria: TopicCriteria::default(),
            pin_ids: Vec::new(),
            status: TopicStatus::Active,
            max_words: 1500,
            last_memory_at: None,
            last_synced_at: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[tokio::test]
    async fn create_and_get() {
        let store = setup_store().await;
        let topic = make_topic("agent-1", "Test Topic");
        store.create(&topic).await.expect("create should succeed");

        let loaded = store
            .get(&topic.id)
            .await
            .expect("get should succeed")
            .expect("topic should exist");
        assert_eq!(loaded.title, "Test Topic");
        assert_eq!(loaded.status, TopicStatus::Active);
    }

    #[tokio::test]
    async fn list_and_list_active() {
        let store = setup_store().await;

        let mut active = make_topic("agent-1", "Active Topic");
        active.status = TopicStatus::Active;
        store.create(&active).await.expect("create should succeed");

        let mut paused = make_topic("agent-1", "Paused Topic");
        paused.status = TopicStatus::Paused;
        store.create(&paused).await.expect("create should succeed");

        let all = store.list("agent-1").await.expect("list should succeed");
        assert_eq!(all.len(), 2);

        let active_only = store
            .list_active("agent-1")
            .await
            .expect("list_active should succeed");
        assert_eq!(active_only.len(), 1);
        assert_eq!(active_only[0].title, "Active Topic");
    }

    #[tokio::test]
    async fn update_topic() {
        let store = setup_store().await;
        let mut topic = make_topic("agent-1", "Original");
        store.create(&topic).await.expect("create should succeed");

        topic.title = "Updated".to_string();
        topic.content = "Some content".to_string();
        store.update(&topic).await.expect("update should succeed");

        let loaded = store
            .get(&topic.id)
            .await
            .expect("get should succeed")
            .expect("topic should exist");
        assert_eq!(loaded.title, "Updated");
        assert_eq!(loaded.content, "Some content");
    }

    #[tokio::test]
    async fn delete_topic() {
        let store = setup_store().await;
        let topic = make_topic("agent-1", "Delete Me");
        store.create(&topic).await.expect("create should succeed");

        let deleted = store
            .delete(&topic.id)
            .await
            .expect("delete should succeed");
        assert!(deleted);

        let loaded = store.get(&topic.id).await.expect("get should succeed");
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn version_history() {
        let store = setup_store().await;
        let topic = make_topic("agent-1", "Versioned");
        store.create(&topic).await.expect("create should succeed");

        let version = TopicVersion {
            id: uuid::Uuid::new_v4().to_string(),
            topic_id: topic.id.clone(),
            content: "Version 1 content".to_string(),
            memory_count: 15,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        store
            .save_version(&version)
            .await
            .expect("save_version should succeed");

        let versions = store
            .get_versions(&topic.id, 10)
            .await
            .expect("get_versions should succeed");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].memory_count, 15);
    }
}
