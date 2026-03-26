//! Webchat conversation persistence (SQLite).

use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct WebChatConversation {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub title_source: String,
    pub archived: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct WebChatConversationSummary {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub title_source: String,
    pub archived: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub last_message_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_message_preview: Option<String>,
    pub last_message_role: Option<String>,
    pub message_count: i64,
}

#[derive(Debug, Clone)]
pub struct WebChatConversationStore {
    pool: SqlitePool,
}

impl WebChatConversationStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        agent_id: &str,
        title: Option<&str>,
    ) -> crate::error::Result<WebChatConversation> {
        let id = format!("portal:chat:{agent_id}:{}", uuid::Uuid::new_v4());
        let title = normalize_title(title).unwrap_or_else(default_title);
        let title_source = if title == default_title() {
            "system"
        } else {
            "user"
        };

        sqlx::query(
            "INSERT INTO webchat_conversations (id, agent_id, title, title_source) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(agent_id)
        .bind(&title)
        .bind(title_source)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(self
            .get(agent_id, &id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("newly created webchat conversation missing"))?)
    }

    pub async fn ensure(
        &self,
        agent_id: &str,
        session_id: &str,
    ) -> crate::error::Result<WebChatConversation> {
        sqlx::query(
            "INSERT INTO webchat_conversations (id, agent_id, title, title_source) VALUES (?, ?, ?, 'system') \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(session_id)
        .bind(agent_id)
        .bind(default_title())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(self
            .get(agent_id, session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("ensured webchat conversation missing"))?)
    }

    pub async fn get(
        &self,
        agent_id: &str,
        session_id: &str,
    ) -> crate::error::Result<Option<WebChatConversation>> {
        let row = sqlx::query(
            "SELECT id, agent_id, title, title_source, archived, created_at, updated_at \
             FROM webchat_conversations WHERE agent_id = ? AND id = ?",
        )
        .bind(agent_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.map(row_to_conversation))
    }

    pub async fn list(
        &self,
        agent_id: &str,
        include_archived: bool,
        limit: i64,
    ) -> crate::error::Result<Vec<WebChatConversationSummary>> {
        self.backfill_from_messages(agent_id).await?;

        let rows = sqlx::query(
            "SELECT \
                c.id, c.agent_id, c.title, c.title_source, c.archived, c.created_at, c.updated_at, \
                (SELECT MAX(created_at) FROM conversation_messages WHERE channel_id = c.id) as last_message_at, \
                (SELECT content FROM conversation_messages WHERE channel_id = c.id ORDER BY created_at DESC LIMIT 1) as last_message_preview, \
                (SELECT role FROM conversation_messages WHERE channel_id = c.id ORDER BY created_at DESC LIMIT 1) as last_message_role, \
                (SELECT COUNT(*) FROM conversation_messages WHERE channel_id = c.id) as message_count \
             FROM webchat_conversations c \
             WHERE c.agent_id = ? AND (? = 1 OR c.archived = 0) \
             ORDER BY COALESCE((SELECT MAX(created_at) FROM conversation_messages WHERE channel_id = c.id), c.updated_at, c.created_at) DESC \
             LIMIT ?",
        )
        .bind(agent_id)
        .bind(if include_archived { 1_i64 } else { 0_i64 })
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(rows.into_iter().map(row_to_summary).collect())
    }

    pub async fn update(
        &self,
        agent_id: &str,
        session_id: &str,
        title: Option<&str>,
        archived: Option<bool>,
    ) -> crate::error::Result<Option<WebChatConversation>> {
        if title.is_none() && archived.is_none() {
            return self.get(agent_id, session_id).await;
        }

        let title = normalize_title(title);
        let title_source = title.as_ref().map(|_| "user");

        let result = sqlx::query(
            "UPDATE webchat_conversations \
             SET title = COALESCE(?, title), \
                 title_source = COALESCE(?, title_source), \
                 archived = COALESCE(?, archived), \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE agent_id = ? AND id = ?",
        )
        .bind(title.as_deref())
        .bind(title_source)
        .bind(archived.map(|value| if value { 1_i64 } else { 0_i64 }))
        .bind(agent_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }

        self.get(agent_id, session_id).await
    }

    pub async fn delete(&self, agent_id: &str, session_id: &str) -> crate::error::Result<bool> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        sqlx::query("DELETE FROM conversation_messages WHERE channel_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        let result = sqlx::query("DELETE FROM webchat_conversations WHERE agent_id = ? AND id = ?")
            .bind(agent_id)
            .bind(session_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        tx.commit().await.map_err(|error| anyhow::anyhow!(error))?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn maybe_set_generated_title(
        &self,
        agent_id: &str,
        session_id: &str,
        content: &str,
    ) -> crate::error::Result<()> {
        let generated_title = generate_title(content);

        sqlx::query(
            "UPDATE webchat_conversations \
             SET title = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE agent_id = ? AND id = ? AND title_source = 'system' AND title = ?",
        )
        .bind(&generated_title)
        .bind(agent_id)
        .bind(session_id)
        .bind(default_title())
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(())
    }

    async fn backfill_from_messages(&self, agent_id: &str) -> crate::error::Result<()> {
        let rows = sqlx::query(
            "SELECT DISTINCT channel_id FROM conversation_messages WHERE channel_id LIKE 'portal:chat:%'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        for row in rows {
            let channel_id: String = row.try_get("channel_id").unwrap_or_default();
            if channel_id.is_empty() {
                continue;
            }

            let existing = self.get(agent_id, &channel_id).await?;
            if existing.is_some() {
                continue;
            }

            let title = sqlx::query(
                "SELECT content FROM conversation_messages \
                 WHERE channel_id = ? AND role = 'user' \
                 ORDER BY created_at ASC LIMIT 1",
            )
            .bind(&channel_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?
            .and_then(|title_row| title_row.try_get::<String, _>("content").ok())
            .map(|content| generate_title(&content))
            .unwrap_or_else(default_title);

            let title_source = if title == default_title() {
                "system"
            } else {
                "user"
            };

            sqlx::query(
                "INSERT INTO webchat_conversations (id, agent_id, title, title_source) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(id) DO NOTHING",
            )
            .bind(&channel_id)
            .bind(agent_id)
            .bind(&title)
            .bind(title_source)
            .execute(&self.pool)
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        }

        Ok(())
    }
}

fn row_to_conversation(row: sqlx::sqlite::SqliteRow) -> WebChatConversation {
    WebChatConversation {
        id: row.try_get("id").unwrap_or_default(),
        agent_id: row.try_get("agent_id").unwrap_or_default(),
        title: row.try_get("title").unwrap_or_else(|_| default_title()),
        title_source: row
            .try_get("title_source")
            .unwrap_or_else(|_| "system".to_string()),
        archived: row.try_get::<i64, _>("archived").unwrap_or(0) == 1,
        created_at: row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        updated_at: row
            .try_get("updated_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
    }
}

fn row_to_summary(row: sqlx::sqlite::SqliteRow) -> WebChatConversationSummary {
    WebChatConversationSummary {
        id: row.try_get("id").unwrap_or_default(),
        agent_id: row.try_get("agent_id").unwrap_or_default(),
        title: row.try_get("title").unwrap_or_else(|_| default_title()),
        title_source: row
            .try_get("title_source")
            .unwrap_or_else(|_| "system".to_string()),
        archived: row.try_get::<i64, _>("archived").unwrap_or(0) == 1,
        created_at: row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        updated_at: row
            .try_get("updated_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        last_message_at: row.try_get("last_message_at").ok(),
        last_message_preview: row.try_get("last_message_preview").ok().flatten(),
        last_message_role: row.try_get("last_message_role").ok().flatten(),
        message_count: row.try_get("message_count").unwrap_or(0),
    }
}

fn default_title() -> String {
    "New chat".to_string()
}

fn normalize_title(title: Option<&str>) -> Option<String> {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
}

fn generate_title(content: &str) -> String {
    let cleaned = content.trim().replace('\n', " ");
    let trimmed = cleaned.trim();

    if trimmed.is_empty() {
        return default_title();
    }

    let mut title = trimmed.chars().take(72).collect::<String>();
    if trimmed.chars().count() > 72 {
        title.push_str("...");
    }
    title
}
