//! Learning system configuration.

use crate::learning::observatory::ObservatoryConfig;
use serde::{Deserialize, Serialize};

/// Configuration for the learning engine.
///
/// Loaded from `[defaults.learning]` in config.toml and hot-reloadable via
/// RuntimeConfig. All fields have sensible defaults for single-agent use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LearningConfig {
    /// Whether the learning engine is active.
    pub enabled: bool,
    /// Seconds between heartbeat writes to learning_state.
    pub tick_interval_secs: u64,
    /// Seconds between batch processing passes (future use).
    pub batch_interval_secs: u64,
    /// Advisory time budget in milliseconds for per-event processing.
    pub advisory_budget_ms: u64,
    /// Platform:id pairs (e.g. "discord:123456") identifying the owner.
    /// When non-empty, only traces originating from these user IDs are learned from.
    pub owner_user_ids: Vec<String>,
    /// Number of completed episodes before predictions activate (cold start guard).
    pub cold_start_episodes: u64,
    /// Seconds after which an episode with no updates is considered stale.
    pub stale_episode_timeout_secs: u64,
    /// Tool names considered high-stakes (get richer step data).
    pub high_stakes_tools: Vec<String>,
    /// File operations considered high-stakes.
    pub high_stakes_file_operations: Vec<String>,
    /// Maximum distillation batch size per tick.
    pub distillation_batch_size: usize,
    /// Seconds between promotion checks.
    pub promotion_interval_secs: u64,
    /// Observatory configuration.
    pub observatory: ObservatoryConfig,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_interval_secs: 30,
            batch_interval_secs: 60,
            advisory_budget_ms: 4000,
            owner_user_ids: Vec::new(),
            cold_start_episodes: 50,
            stale_episode_timeout_secs: 1800,
            high_stakes_tools: vec!["shell".into(), "exec".into()],
            high_stakes_file_operations: vec!["write".into()],
            distillation_batch_size: 3,
            promotion_interval_secs: 3600,
            observatory: ObservatoryConfig::default(),
        }
    }
}
