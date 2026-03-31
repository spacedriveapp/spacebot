//! Per-channel settings persistence (SQLite).
//!
//! Stores `ConversationSettings` for platform channels (Discord, Slack, etc.).
//! Portal conversations store settings in `portal_conversations.settings` instead.

use super::settings::ConversationSettings;
use sqlx::{Row as _, SqlitePool};

#[derive(Debug, Clone)]
pub struct ChannelSettingsStore {
    pool: SqlitePool,
}

impl ChannelSettingsStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Get settings for a specific channel, if any have been persisted.
    pub async fn get(
        &self,
        agent_id: &str,
        conversation_id: &str,
    ) -> crate::error::Result<Option<ConversationSettings>> {
        let row = sqlx::query(
            "SELECT settings FROM channel_settings WHERE agent_id = ? AND conversation_id = ?",
        )
        .bind(agent_id)
        .bind(conversation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(row.and_then(|r| {
            r.try_get::<String, _>("settings").ok().and_then(|s| {
                if s.is_empty() || s == "{}" {
                    None
                } else {
                    serde_json::from_str(&s).ok()
                }
            })
        }))
    }

    /// Insert or update settings for a channel.
    pub async fn upsert(
        &self,
        agent_id: &str,
        conversation_id: &str,
        settings: &ConversationSettings,
    ) -> crate::error::Result<()> {
        let settings_json = serde_json::to_string(settings).map_err(|e| anyhow::anyhow!(e))?;

        sqlx::query(
            "INSERT INTO channel_settings (agent_id, conversation_id, settings, updated_at) \
             VALUES (?, ?, ?, CURRENT_TIMESTAMP) \
             ON CONFLICT (agent_id, conversation_id) \
             DO UPDATE SET settings = excluded.settings, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(agent_id)
        .bind(conversation_id)
        .bind(&settings_json)
        .execute(&self.pool)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok(())
    }
}
