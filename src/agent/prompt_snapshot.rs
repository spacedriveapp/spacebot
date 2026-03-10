//! Prompt snapshot store for debugging channel prompt construction.
//!
//! Stores per-turn snapshots of the full system prompt (broken into named
//! sections) and the conversation history at the time of each LLM call.
//! Uses a dedicated redb database (`prompt_snapshots.redb`) so it can be
//! deleted independently without affecting settings or secrets.

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Table: channel_id:timestamp_ms -> JSON-encoded PromptSnapshot
const SNAPSHOTS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("prompt_snapshots");

/// A complete snapshot of what the LLM sees on a given turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSnapshot {
    /// Channel that produced this snapshot.
    pub channel_id: String,
    /// Unix timestamp in milliseconds when the snapshot was captured.
    pub timestamp_ms: i64,
    /// The user message that triggered this turn.
    pub user_message: String,
    /// The full rendered system prompt, exactly as sent to the model.
    pub system_prompt: String,
    /// Total character count of the rendered system prompt.
    pub system_prompt_chars: usize,
    /// The conversation history as serialized rig Messages.
    pub history: serde_json::Value,
    /// Number of messages in the history.
    pub history_length: usize,
}

/// Summary of a snapshot for listing (without the full content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSnapshotSummary {
    pub timestamp_ms: i64,
    pub user_message: String,
    pub system_prompt_chars: usize,
    pub history_length: usize,
}

/// Persistent store for prompt snapshots, backed by a dedicated redb.
pub struct PromptSnapshotStore {
    db: Arc<Database>,
}

impl PromptSnapshotStore {
    /// Open or create the snapshot store at the given path.
    pub fn new(path: &Path) -> crate::error::Result<Self> {
        let db = Database::create(path).map_err(|error| {
            crate::error::SettingsError::Other(format!(
                "failed to open prompt snapshot db: {error}"
            ))
        })?;

        // Initialize the table if it doesn't exist.
        let write_txn = db.begin_write().map_err(|error| {
            crate::error::SettingsError::Other(format!("failed to begin write txn: {error}"))
        })?;
        {
            let _ = write_txn.open_table(SNAPSHOTS_TABLE).map_err(|error| {
                crate::error::SettingsError::Other(format!(
                    "failed to open snapshots table: {error}"
                ))
            })?;
        }
        write_txn.commit().map_err(|error| {
            crate::error::SettingsError::Other(format!("failed to commit write txn: {error}"))
        })?;

        Ok(Self { db: Arc::new(db) })
    }

    /// Composite key: `{channel_id}:{timestamp_ms}`.
    fn key(channel_id: &str, timestamp_ms: i64) -> String {
        format!("{channel_id}:{timestamp_ms}")
    }

    /// Store a snapshot.
    pub fn save(&self, snapshot: &PromptSnapshot) -> crate::error::Result<()> {
        let key = Self::key(&snapshot.channel_id, snapshot.timestamp_ms);
        let data = serde_json::to_vec(snapshot).map_err(|error| {
            crate::error::SettingsError::Other(format!("failed to serialize snapshot: {error}"))
        })?;

        let write_txn =
            self.db
                .begin_write()
                .map_err(|error| crate::error::SettingsError::WriteFailed {
                    key: key.clone(),
                    details: error.to_string(),
                })?;
        {
            let mut table = write_txn.open_table(SNAPSHOTS_TABLE).map_err(|error| {
                crate::error::SettingsError::WriteFailed {
                    key: key.clone(),
                    details: error.to_string(),
                }
            })?;
            table
                .insert(key.as_str(), data.as_slice())
                .map_err(|error| crate::error::SettingsError::WriteFailed {
                    key: key.clone(),
                    details: error.to_string(),
                })?;
        }
        write_txn
            .commit()
            .map_err(|error| crate::error::SettingsError::WriteFailed {
                key,
                details: error.to_string(),
            })?;

        Ok(())
    }

    /// List snapshot summaries for a channel, newest first.
    pub fn list(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> crate::error::Result<Vec<PromptSnapshotSummary>> {
        let prefix = format!("{channel_id}:");
        let read_txn =
            self.db
                .begin_read()
                .map_err(|error| crate::error::SettingsError::ReadFailed {
                    key: prefix.clone(),
                    details: error.to_string(),
                })?;

        let table = read_txn.open_table(SNAPSHOTS_TABLE).map_err(|error| {
            crate::error::SettingsError::ReadFailed {
                key: prefix.clone(),
                details: error.to_string(),
            }
        })?;

        let mut summaries = Vec::new();
        // Scan all keys with the channel prefix. redb iterates in key order
        // (lexicographic), and our keys are `channel_id:timestamp_ms`, so
        // entries for the same channel are grouped and sorted by time.
        let range = table.range(prefix.as_str()..).map_err(|error| {
            crate::error::SettingsError::ReadFailed {
                key: prefix.clone(),
                details: error.to_string(),
            }
        })?;

        for entry in range {
            let entry = entry.map_err(|error| crate::error::SettingsError::ReadFailed {
                key: prefix.clone(),
                details: error.to_string(),
            })?;
            let key = entry.0.value();
            if !key.starts_with(&prefix) {
                break; // Past our channel's entries.
            }
            let data = entry.1.value();
            if let Ok(snapshot) = serde_json::from_slice::<PromptSnapshot>(data) {
                summaries.push(PromptSnapshotSummary {
                    timestamp_ms: snapshot.timestamp_ms,
                    user_message: snapshot.user_message,
                    system_prompt_chars: snapshot.system_prompt_chars,
                    history_length: snapshot.history_length,
                });
            }
        }

        // Reverse to get newest first, then truncate.
        summaries.reverse();
        summaries.truncate(limit);

        Ok(summaries)
    }

    /// Retrieve a specific snapshot.
    pub fn get(
        &self,
        channel_id: &str,
        timestamp_ms: i64,
    ) -> crate::error::Result<Option<PromptSnapshot>> {
        let key = Self::key(channel_id, timestamp_ms);
        let read_txn =
            self.db
                .begin_read()
                .map_err(|error| crate::error::SettingsError::ReadFailed {
                    key: key.clone(),
                    details: error.to_string(),
                })?;

        let table = read_txn.open_table(SNAPSHOTS_TABLE).map_err(|error| {
            crate::error::SettingsError::ReadFailed {
                key: key.clone(),
                details: error.to_string(),
            }
        })?;

        match table.get(key.as_str()) {
            Ok(Some(data)) => {
                let snapshot =
                    serde_json::from_slice::<PromptSnapshot>(data.value()).map_err(|error| {
                        crate::error::SettingsError::ReadFailed {
                            key,
                            details: format!("failed to deserialize snapshot: {error}"),
                        }
                    })?;
                Ok(Some(snapshot))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(crate::error::SettingsError::ReadFailed {
                key,
                details: error.to_string(),
            }
            .into()),
        }
    }

    /// Delete all snapshots for a channel.
    pub fn clear_channel(&self, channel_id: &str) -> crate::error::Result<usize> {
        let prefix = format!("{channel_id}:");
        let write_txn =
            self.db
                .begin_write()
                .map_err(|error| crate::error::SettingsError::WriteFailed {
                    key: prefix.clone(),
                    details: error.to_string(),
                })?;

        let mut removed = 0;
        {
            let mut table = write_txn.open_table(SNAPSHOTS_TABLE).map_err(|error| {
                crate::error::SettingsError::WriteFailed {
                    key: prefix.clone(),
                    details: error.to_string(),
                }
            })?;

            // Collect keys to remove (can't mutate while iterating).
            let keys: Vec<String> = {
                let range = table.range(prefix.as_str()..).map_err(|error| {
                    crate::error::SettingsError::ReadFailed {
                        key: prefix.clone(),
                        details: error.to_string(),
                    }
                })?;
                let mut result = Vec::new();
                for entry in range {
                    let entry = entry.map_err(|error| crate::error::SettingsError::ReadFailed {
                        key: prefix.clone(),
                        details: error.to_string(),
                    })?;
                    let key = entry.0.value();
                    if !key.starts_with(&prefix) {
                        break;
                    }
                    result.push(key.to_string());
                }
                result
            };

            for key in &keys {
                table.remove(key.as_str()).map_err(|error| {
                    crate::error::SettingsError::WriteFailed {
                        key: key.clone(),
                        details: error.to_string(),
                    }
                })?;
                removed += 1;
            }
        }

        write_txn
            .commit()
            .map_err(|error| crate::error::SettingsError::WriteFailed {
                key: prefix,
                details: error.to_string(),
            })?;

        Ok(removed)
    }
}

impl std::fmt::Debug for PromptSnapshotStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptSnapshotStore")
            .finish_non_exhaustive()
    }
}
