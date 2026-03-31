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
const PROMPT_CAPTURE_PREFIX: &str = "prompt_capture:";

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
}

impl std::fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore").finish_non_exhaustive()
    }
}
