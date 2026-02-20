//! Channel tracking and metadata (SQLite).

use sqlx::{Row as _, SqlitePool};
use std::collections::HashMap;

/// Tracks known channels in SQLite.
///
/// Handles upsert on channel open, activity timestamps, and channel lookups.
/// All write methods are fire-and-forget — they spawn a tokio task and return
/// immediately so the caller never blocks on a DB write.
#[derive(Debug, Clone)]
pub struct ChannelStore {
    pool: SqlitePool,
}

/// A tracked channel with its metadata.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub id: String,
    pub platform: String,
    pub display_name: Option<String>,
    pub platform_meta: Option<serde_json::Value>,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity_at: chrono::DateTime<chrono::Utc>,
}

impl ChannelStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Upsert a channel when it's first seen or when metadata changes.
    ///
    /// Extracts platform from the channel ID prefix (e.g. "discord" from
    /// "discord:123:456"). Updates display_name and platform_meta if the
    /// channel already exists. Fire-and-forget.
    pub fn upsert(&self, channel_id: &str, metadata: &HashMap<String, serde_json::Value>) {
        let pool = self.pool.clone();
        let channel_id = channel_id.to_string();
        let platform = extract_platform(&channel_id);
        let display_name = extract_display_name(&platform, &channel_id, metadata);
        let platform_meta = extract_platform_meta(&platform, metadata);

        tokio::spawn(async move {
            if let Err(error) = sqlx::query(
                "INSERT INTO channels (id, platform, display_name, platform_meta, last_activity_at) \
                 VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP) \
                 ON CONFLICT(id) DO UPDATE SET \
                     display_name = COALESCE(excluded.display_name, channels.display_name), \
                     platform_meta = COALESCE(excluded.platform_meta, channels.platform_meta), \
                     last_activity_at = CURRENT_TIMESTAMP"
            )
            .bind(&channel_id)
            .bind(&platform)
            .bind(&display_name)
            .bind(&platform_meta)
            .execute(&pool)
            .await
            {
                tracing::warn!(%error, %channel_id, "failed to upsert channel");
            }
        });
    }

    /// Update last_activity_at for a channel. Fire-and-forget.
    pub fn touch(&self, channel_id: &str) {
        let pool = self.pool.clone();
        let channel_id = channel_id.to_string();

        tokio::spawn(async move {
            if let Err(error) =
                sqlx::query("UPDATE channels SET last_activity_at = CURRENT_TIMESTAMP WHERE id = ?")
                    .bind(&channel_id)
                    .execute(&pool)
                    .await
            {
                tracing::warn!(%error, %channel_id, "failed to touch channel");
            }
        });
    }

    /// List all active channels, most recently active first.
    pub async fn list_active(&self) -> crate::error::Result<Vec<ChannelInfo>> {
        let rows = sqlx::query(
            "SELECT id, platform, display_name, platform_meta, is_active, created_at, last_activity_at \
             FROM channels \
             WHERE is_active = 1 \
             ORDER BY last_activity_at DESC"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

        Ok(rows.into_iter().map(row_to_channel_info).collect())
    }

    /// Find a channel by partial name or ID match.
    ///
    /// Match priority: exact name > prefix > contains > channel ID contains.
    pub async fn find_by_name(&self, name: &str) -> crate::error::Result<Option<ChannelInfo>> {
        let channels = self.list_active().await?;
        let name_lower = name.to_lowercase();

        // Exact name match
        if let Some(channel) = channels.iter().find(|c| {
            c.display_name
                .as_ref()
                .is_some_and(|n| n.to_lowercase() == name_lower)
        }) {
            return Ok(Some(channel.clone()));
        }

        // Prefix match
        if let Some(channel) = channels.iter().find(|c| {
            c.display_name
                .as_ref()
                .is_some_and(|n| n.to_lowercase().starts_with(&name_lower))
        }) {
            return Ok(Some(channel.clone()));
        }

        // Contains match
        if let Some(channel) = channels.iter().find(|c| {
            c.display_name
                .as_ref()
                .is_some_and(|n| n.to_lowercase().contains(&name_lower))
        }) {
            return Ok(Some(channel.clone()));
        }

        // Match against the raw channel ID
        if let Some(channel) = channels.iter().find(|c| c.id.contains(&name_lower)) {
            return Ok(Some(channel.clone()));
        }

        Ok(None)
    }

    /// Get a single channel by exact ID.
    pub async fn get(&self, channel_id: &str) -> crate::error::Result<Option<ChannelInfo>> {
        let row = sqlx::query(
            "SELECT id, platform, display_name, platform_meta, is_active, created_at, last_activity_at \
             FROM channels \
             WHERE id = ?"
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

        Ok(row.map(row_to_channel_info))
    }

    /// Resolve a channel's display name by ID.
    pub async fn resolve_name(&self, channel_id: &str) -> Option<String> {
        self.get(channel_id)
            .await
            .ok()
            .flatten()
            .and_then(|c| c.display_name)
    }
}

fn row_to_channel_info(row: sqlx::sqlite::SqliteRow) -> ChannelInfo {
    let platform_meta_str: Option<String> = row.try_get("platform_meta").ok().flatten();
    let platform_meta = platform_meta_str.and_then(|s| serde_json::from_str(&s).ok());

    ChannelInfo {
        id: row.try_get("id").unwrap_or_default(),
        platform: row.try_get("platform").unwrap_or_default(),
        display_name: row.try_get("display_name").ok().flatten(),
        platform_meta,
        is_active: row.try_get::<i32, _>("is_active").unwrap_or(1) == 1,
        created_at: row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
        last_activity_at: row
            .try_get("last_activity_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
    }
}

/// Extract the platform name from a channel ID.
///
/// "discord:123:456" -> "discord", "slack:T01:C01" -> "slack", "cron:daily" -> "cron"
fn extract_platform(channel_id: &str) -> String {
    channel_id
        .split(':')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Pull the best display name from inbound message metadata.
fn extract_display_name(
    platform: &str,
    channel_id: &str,
    metadata: &HashMap<String, serde_json::Value>,
) -> Option<String> {
    match platform {
        "discord" => metadata
            .get("discord_channel_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "slack" => metadata
            .get("slack_channel_name")
            .and_then(|v| v.as_str())
            .map(|name| {
                if channel_id.contains(":D") || name.starts_with("dm-") {
                    name.to_string()
                } else {
                    format!("#{name}")
                }
            }),
        "telegram" => metadata
            .get("display_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Build a JSON blob of platform-specific metadata worth persisting.
fn extract_platform_meta(
    platform: &str,
    metadata: &HashMap<String, serde_json::Value>,
) -> Option<String> {
    let mut meta = serde_json::Map::new();

    match platform {
        "discord" => {
            for key in [
                "discord_guild_id",
                "discord_guild_name",
                "discord_channel_id",
                "discord_is_thread",
                "discord_parent_channel_id",
            ] {
                if let Some(value) = metadata.get(key) {
                    meta.insert(key.to_string(), value.clone());
                }
            }
        }
        "slack" => {
            for key in ["slack_workspace_id", "slack_channel_id", "slack_thread_ts"] {
                if let Some(value) = metadata.get(key) {
                    meta.insert(key.to_string(), value.clone());
                }
            }
        }
        "telegram" => {
            for key in ["telegram_chat_id", "telegram_chat_type"] {
                if let Some(value) = metadata.get(key) {
                    meta.insert(key.to_string(), value.clone());
                }
            }
        }
        _ => {}
    }

    if meta.is_empty() {
        None
    } else {
        serde_json::to_string(&serde_json::Value::Object(meta)).ok()
    }
}
