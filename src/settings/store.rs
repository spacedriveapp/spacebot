//! Key-value settings storage (redb).

use crate::error::{Result, SettingsError};
use redb::{Database, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Table definition for settings: key -> value (both strings).
const SETTINGS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("settings");

/// Default key for worker log mode setting.
pub const WORKER_LOG_MODE_KEY: &str = "worker_log_mode";
/// Key for channel listen-only mode setting.
pub const CHANNEL_LISTEN_ONLY_MODE_KEY: &str = "channel_listen_only_mode";
const CHANNEL_LISTEN_ONLY_MODE_PREFIX: &str = "channel_listen_only_mode:";
const PROMPT_CAPTURE_PREFIX: &str = "prompt_capture:";
const CHANNEL_SPOKEN_HISTORY_PREFIX: &str = "channel_spoken_history:";

/// How worker execution logs are stored.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLogMode {
    /// Only log failed worker runs (default).
    #[default]
    ErrorsOnly,
    /// Log all runs with separate directories for success/failure.
    AllSeparate,
    /// Log all runs to the same directory.
    AllCombined,
}

impl std::fmt::Display for WorkerLogMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ErrorsOnly => write!(f, "errors_only"),
            Self::AllSeparate => write!(f, "all_separate"),
            Self::AllCombined => write!(f, "all_combined"),
        }
    }
}

impl std::str::FromStr for WorkerLogMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "errors_only" => Ok(Self::ErrorsOnly),
            "all_separate" => Ok(Self::AllSeparate),
            "all_combined" => Ok(Self::AllCombined),
            _ => Err(format!("unknown worker log mode: {}", s)),
        }
    }
}

/// Settings store backed by redb.
pub struct SettingsStore {
    db: Arc<Database>,
}

impl SettingsStore {
    fn channel_listen_only_mode_key(channel_id: &str) -> String {
        format!("{CHANNEL_LISTEN_ONLY_MODE_PREFIX}{channel_id}")
    }

    fn channel_spoken_history_key(channel_id: &str) -> String {
        format!("{CHANNEL_SPOKEN_HISTORY_PREFIX}{channel_id}")
    }

    /// Create a new settings store at the given path.
    /// The database will be created if it doesn't exist.
    pub fn new(path: &Path) -> Result<Self> {
        let db = Database::create(path)
            .map_err(|e| SettingsError::Other(format!("failed to open settings db: {e}")))?;

        // Initialize the table if it doesn't exist
        let write_txn = db
            .begin_write()
            .map_err(|e| SettingsError::Other(format!("failed to begin write txn: {e}")))?;
        {
            let _ = write_txn
                .open_table(SETTINGS_TABLE)
                .map_err(|e| SettingsError::Other(format!("failed to open settings table: {e}")))?;
        }
        write_txn
            .commit()
            .map_err(|e| SettingsError::Other(format!("failed to commit write txn: {e}")))?;

        let store = Self { db: Arc::new(db) };

        // Set default values if not present
        if store.get_raw(WORKER_LOG_MODE_KEY).is_err() {
            store.set_raw(WORKER_LOG_MODE_KEY, &WorkerLogMode::default().to_string())?;
        }

        Ok(store)
    }

    /// Get a raw string value by key.
    fn get_raw(&self, key: &str) -> Result<String> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| SettingsError::ReadFailed {
                key: key.to_string(),
                details: e.to_string(),
            })?;

        let table = read_txn
            .open_table(SETTINGS_TABLE)
            .map_err(|e| SettingsError::ReadFailed {
                key: key.to_string(),
                details: e.to_string(),
            })?;

        let value = table
            .get(key)
            .map_err(|e| SettingsError::ReadFailed {
                key: key.to_string(),
                details: e.to_string(),
            })?
            .ok_or_else(|| SettingsError::NotFound {
                key: key.to_string(),
            })?;

        Ok(value.value().to_string())
    }

    /// Set a raw string value by key.
    fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| SettingsError::WriteFailed {
                key: key.to_string(),
                details: e.to_string(),
            })?;

        {
            let mut table =
                write_txn
                    .open_table(SETTINGS_TABLE)
                    .map_err(|e| SettingsError::WriteFailed {
                        key: key.to_string(),
                        details: e.to_string(),
                    })?;

            table
                .insert(key, value)
                .map_err(|e| SettingsError::WriteFailed {
                    key: key.to_string(),
                    details: e.to_string(),
                })?;
        }

        write_txn.commit().map_err(|e| SettingsError::WriteFailed {
            key: key.to_string(),
            details: e.to_string(),
        })?;

        Ok(())
    }

    /// Get the worker log mode setting.
    pub fn worker_log_mode(&self) -> WorkerLogMode {
        match self.get_raw(WORKER_LOG_MODE_KEY) {
            Ok(raw) => raw.parse().unwrap_or_default(),
            Err(_) => WorkerLogMode::default(),
        }
    }

    /// Set the worker log mode setting.
    pub fn set_worker_log_mode(&self, mode: WorkerLogMode) -> Result<()> {
        self.set_raw(WORKER_LOG_MODE_KEY, &mode.to_string())
    }

    /// Get the channel listen-only mode, if explicitly persisted.
    pub fn channel_listen_only_mode(&self) -> Result<Option<bool>> {
        match self.get_raw(CHANNEL_LISTEN_ONLY_MODE_KEY) {
            Ok(raw) => raw.parse::<bool>().map(Some).map_err(|error| {
                SettingsError::ReadFailed {
                    key: CHANNEL_LISTEN_ONLY_MODE_KEY.to_string(),
                    details: format!("invalid boolean value '{raw}': {error}"),
                }
                .into()
            }),
            Err(crate::error::Error::Settings(settings_error)) => match *settings_error {
                SettingsError::NotFound { .. } => Ok(None),
                other => Err(other.into()),
            },
            Err(other) => Err(other),
        }
    }

    /// Persist channel listen-only mode.
    pub fn set_channel_listen_only_mode(&self, enabled: bool) -> Result<()> {
        self.set_raw(
            CHANNEL_LISTEN_ONLY_MODE_KEY,
            if enabled { "true" } else { "false" },
        )
    }

    /// Get the listen-only mode for a specific channel, if explicitly persisted.
    pub fn channel_listen_only_mode_for(&self, channel_id: &str) -> Result<Option<bool>> {
        let key = Self::channel_listen_only_mode_key(channel_id);
        match self.get_raw(&key) {
            Ok(raw) => raw.parse::<bool>().map(Some).map_err(|error| {
                SettingsError::ReadFailed {
                    key: key.clone(),
                    details: format!("invalid boolean value '{raw}': {error}"),
                }
                .into()
            }),
            Err(crate::error::Error::Settings(settings_error)) => match *settings_error {
                SettingsError::NotFound { .. } => Ok(None),
                other => Err(other.into()),
            },
            Err(other) => Err(other),
        }
    }

    /// Persist listen-only mode for a specific channel.
    pub fn set_channel_listen_only_mode_for(&self, channel_id: &str, enabled: bool) -> Result<()> {
        let key = Self::channel_listen_only_mode_key(channel_id);
        self.set_raw(&key, if enabled { "true" } else { "false" })
    }

    /// Check whether prompt capture is enabled for a specific channel.
    pub fn prompt_capture_enabled(&self, channel_id: &str) -> bool {
        let key = format!("{PROMPT_CAPTURE_PREFIX}{channel_id}");
        matches!(self.get_raw(&key), Ok(v) if v == "true")
    }

    /// Enable or disable prompt capture for a specific channel.
    pub fn set_prompt_capture(&self, channel_id: &str, enabled: bool) -> Result<()> {
        let key = format!("{PROMPT_CAPTURE_PREFIX}{channel_id}");
        self.set_raw(&key, if enabled { "true" } else { "false" })
    }

    /// Load the persisted spoken response history for a channel.
    pub fn channel_spoken_history_for(&self, channel_id: &str) -> Result<Vec<String>> {
        let key = Self::channel_spoken_history_key(channel_id);
        match self.get_raw(&key) {
            Ok(raw) => serde_json::from_str::<Vec<String>>(&raw).map_err(|error| {
                SettingsError::ReadFailed {
                    key,
                    details: format!("invalid spoken history JSON: {error}"),
                }
                .into()
            }),
            Err(crate::error::Error::Settings(settings_error)) => match *settings_error {
                SettingsError::NotFound { .. } => Ok(Vec::new()),
                other => Err(other.into()),
            },
            Err(other) => Err(other),
        }
    }

    /// Persist the spoken response history for a channel.
    pub fn set_channel_spoken_history_for(
        &self,
        channel_id: &str,
        history: &[String],
    ) -> Result<()> {
        let key = Self::channel_spoken_history_key(channel_id);
        let serialized =
            serde_json::to_string(history).map_err(|error| SettingsError::WriteFailed {
                key: key.clone(),
                details: format!("failed to serialize spoken history: {error}"),
            })?;
        self.set_raw(&key, &serialized)
    }
}

impl std::fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::SettingsStore;

    #[test]
    fn spoken_history_round_trips_per_channel() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let store = SettingsStore::new(&temp_dir.path().join("settings.redb"))
            .expect("failed to create settings store");
        let history = vec![
            "First spoken reply".to_string(),
            "Second spoken reply".to_string(),
        ];

        store
            .set_channel_spoken_history_for("portal:chat:main", &history)
            .expect("failed to persist spoken history");

        let loaded = store
            .channel_spoken_history_for("portal:chat:main")
            .expect("failed to load spoken history");

        assert_eq!(loaded, history);
        assert!(
            store
                .channel_spoken_history_for("portal:chat:other")
                .expect("failed to load empty spoken history")
                .is_empty()
        );
    }
}
