//! LinkStore: CRUD operations for the agent communication graph.

use crate::error::Result;
use crate::links::types::{AgentLink, LinkDirection, LinkRelationship};

use anyhow::Context as _;
use chrono::Utc;
use sqlx::SqlitePool;

/// Persistent store for agent links, backed by the instance-level SQLite database.
#[derive(Clone)]
pub struct LinkStore {
    pool: SqlitePool,
}

impl LinkStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Get all links involving this agent (as source or target).
    pub async fn get_links_for_agent(&self, agent_id: &str) -> Result<Vec<AgentLink>> {
        let rows = sqlx::query_as::<_, LinkRow>(
            "SELECT id, from_agent_id, to_agent_id, direction, relationship, enabled, created_at, updated_at
             FROM agent_links
             WHERE from_agent_id = ? OR to_agent_id = ?
             ORDER BY created_at ASC",
        )
        .bind(agent_id)
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch links for agent")?;

        rows.into_iter().map(LinkRow::into_link).collect()
    }

    /// Get a specific link by ID.
    pub async fn get(&self, link_id: &str) -> Result<Option<AgentLink>> {
        let row = sqlx::query_as::<_, LinkRow>(
            "SELECT id, from_agent_id, to_agent_id, direction, relationship, enabled, created_at, updated_at
             FROM agent_links
             WHERE id = ?",
        )
        .bind(link_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch link by id")?;

        row.map(LinkRow::into_link).transpose()
    }

    /// Get all links in the instance.
    pub async fn get_all(&self) -> Result<Vec<AgentLink>> {
        let rows = sqlx::query_as::<_, LinkRow>(
            "SELECT id, from_agent_id, to_agent_id, direction, relationship, enabled, created_at, updated_at
             FROM agent_links
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch all links")?;

        rows.into_iter().map(LinkRow::into_link).collect()
    }

    /// Get the link between two specific agents (if any).
    ///
    /// Checks both directions: from_agent_id→to_agent_id and the reverse.
    pub async fn get_between(
        &self,
        from_agent_id: &str,
        to_agent_id: &str,
    ) -> Result<Option<AgentLink>> {
        let row = sqlx::query_as::<_, LinkRow>(
            "SELECT id, from_agent_id, to_agent_id, direction, relationship, enabled, created_at, updated_at
             FROM agent_links
             WHERE (from_agent_id = ? AND to_agent_id = ?)
                OR (from_agent_id = ? AND to_agent_id = ?)
             LIMIT 1",
        )
        .bind(from_agent_id)
        .bind(to_agent_id)
        .bind(to_agent_id)
        .bind(from_agent_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch link between agents")?;

        row.map(LinkRow::into_link).transpose()
    }

    /// Create a new link. Returns error if a link already exists between these agents.
    pub async fn create(&self, link: &AgentLink) -> Result<()> {
        sqlx::query(
            "INSERT INTO agent_links (id, from_agent_id, to_agent_id, direction, relationship, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&link.id)
        .bind(&link.from_agent_id)
        .bind(&link.to_agent_id)
        .bind(link.direction.as_str())
        .bind(link.relationship.as_str())
        .bind(link.enabled)
        .bind(link.created_at)
        .bind(link.updated_at)
        .execute(&self.pool)
        .await
        .context("failed to create agent link")?;

        Ok(())
    }

    /// Update link properties (direction, relationship, enabled).
    pub async fn update(&self, link: &AgentLink) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE agent_links
             SET direction = ?, relationship = ?, enabled = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(link.direction.as_str())
        .bind(link.relationship.as_str())
        .bind(link.enabled)
        .bind(now)
        .bind(&link.id)
        .execute(&self.pool)
        .await
        .context("failed to update agent link")?;

        Ok(())
    }

    /// Delete a link by ID.
    pub async fn delete(&self, link_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM agent_links WHERE id = ?")
            .bind(link_id)
            .execute(&self.pool)
            .await
            .context("failed to delete agent link")?;

        Ok(())
    }

    /// Upsert a link from config. If a link between the same agents already exists,
    /// update its direction and relationship to match config.
    pub async fn upsert_from_config(&self, link: &AgentLink) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO agent_links (id, from_agent_id, to_agent_id, direction, relationship, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(from_agent_id, to_agent_id)
             DO UPDATE SET direction = excluded.direction,
                           relationship = excluded.relationship,
                           enabled = excluded.enabled,
                           updated_at = ?",
        )
        .bind(&link.id)
        .bind(&link.from_agent_id)
        .bind(&link.to_agent_id)
        .bind(link.direction.as_str())
        .bind(link.relationship.as_str())
        .bind(link.enabled)
        .bind(link.created_at)
        .bind(link.updated_at)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to upsert agent link from config")?;

        Ok(())
    }
}

/// Internal row type for sqlx deserialization.
#[derive(sqlx::FromRow)]
struct LinkRow {
    id: String,
    from_agent_id: String,
    to_agent_id: String,
    direction: String,
    relationship: String,
    enabled: bool,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
}

impl LinkRow {
    fn into_link(self) -> Result<AgentLink> {
        let direction: LinkDirection = self.direction.parse().map_err(|e: String| {
            anyhow::anyhow!("invalid link direction in database: {e}")
        })?;
        let relationship: LinkRelationship = self.relationship.parse().map_err(|e: String| {
            anyhow::anyhow!("invalid link relationship in database: {e}")
        })?;

        Ok(AgentLink {
            id: self.id,
            from_agent_id: self.from_agent_id,
            to_agent_id: self.to_agent_id,
            direction,
            relationship,
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
