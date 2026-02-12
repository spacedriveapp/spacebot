//! Configuration loading and validation.

use crate::error::{ConfigError, Result};
use crate::llm::routing::RoutingConfig;
use anyhow::Context as _;
use arc_swap::ArcSwap;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Top-level Spacebot configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Instance root directory (~/.spacebot or SPACEBOT_DIR).
    pub instance_dir: PathBuf,
    /// LLM provider credentials (shared across all agents).
    pub llm: LlmConfig,
    /// Default settings inherited by all agents.
    pub defaults: DefaultsConfig,
    /// Agent definitions.
    pub agents: Vec<AgentConfig>,
    /// Messaging platform credentials.
    pub messaging: MessagingConfig,
    /// Routing bindings (maps platform conversations to agents).
    pub bindings: Vec<Binding>,
}

/// LLM provider credentials (instance-level).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub anthropic_key: Option<String>,
    pub openai_key: Option<String>,
    pub openrouter_key: Option<String>,
}

/// Defaults inherited by all agents. Individual agents can override any field.
#[derive(Debug, Clone)]
pub struct DefaultsConfig {
    pub routing: RoutingConfig,
    pub max_concurrent_branches: usize,
    pub max_turns: usize,
    pub branch_max_turns: usize,
    pub context_window: usize,
    pub compaction: CompactionConfig,
    pub memory_persistence: MemoryPersistenceConfig,
    pub ingestion: IngestionConfig,
    pub cortex: CortexConfig,
    pub browser: BrowserConfig,
    /// Brave Search API key for web search tool. Supports "env:VAR_NAME" references.
    pub brave_search_key: Option<String>,
    pub history_backfill_count: usize,
    pub heartbeats: Vec<HeartbeatDef>,
}

/// Compaction threshold configuration.
#[derive(Debug, Clone, Copy)]
pub struct CompactionConfig {
    pub background_threshold: f32,
    pub aggressive_threshold: f32,
    pub emergency_threshold: f32,
}

/// Auto-branching memory persistence configuration.
///
/// Spawns a silent branch every N messages to recall existing memories and save
/// new ones from the recent conversation. Runs without blocking the channel and
/// the result is never injected into channel history.
#[derive(Debug, Clone, Copy)]
pub struct MemoryPersistenceConfig {
    /// Whether auto memory persistence branches are enabled.
    pub enabled: bool,
    /// Number of user messages between automatic memory persistence branches.
    pub message_interval: usize,
}

impl Default for MemoryPersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            message_interval: 50,
        }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            background_threshold: 0.80,
            aggressive_threshold: 0.85,
            emergency_threshold: 0.95,
        }
    }
}

/// File-based memory ingestion configuration.
///
/// Watches a directory in the agent workspace for text files, chunks them, and
/// processes each chunk through the memory recall + save flow. Files are deleted
/// after successful ingestion.
#[derive(Debug, Clone, Copy)]
pub struct IngestionConfig {
    /// Whether file-based memory ingestion is enabled.
    pub enabled: bool,
    /// How often to scan the ingest directory for new files, in seconds.
    pub poll_interval_secs: u64,
    /// Target chunk size in characters. Chunks may be slightly larger to avoid
    /// splitting mid-line.
    pub chunk_size: usize,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 30,
            chunk_size: 4000,
        }
    }
}

/// Browser automation configuration for workers.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Whether browser tools are available to workers.
    pub enabled: bool,
    /// Run Chrome in headless mode.
    pub headless: bool,
    /// Allow JavaScript evaluation via the browser tool.
    pub evaluate_enabled: bool,
    /// Custom Chrome/Chromium executable path.
    pub executable_path: Option<String>,
    /// Directory for storing screenshots and other browser artifacts.
    pub screenshot_dir: Option<PathBuf>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            headless: true,
            evaluate_enabled: false,
            executable_path: None,
            screenshot_dir: None,
        }
    }
}

/// Cortex configuration.
#[derive(Debug, Clone, Copy)]
pub struct CortexConfig {
    pub tick_interval_secs: u64,
    pub worker_timeout_secs: u64,
    pub branch_timeout_secs: u64,
    pub circuit_breaker_threshold: u8,
}

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 30,
            worker_timeout_secs: 300,
            branch_timeout_secs: 60,
            circuit_breaker_threshold: 3,
        }
    }
}

/// Per-agent configuration (raw, before resolution with defaults).
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub id: String,
    pub default: bool,
    /// Custom workspace path. If None, resolved to instance_dir/agents/{id}/workspace.
    pub workspace: Option<PathBuf>,
    /// Per-agent routing overrides. None inherits from defaults.
    pub routing: Option<RoutingConfig>,
    pub max_concurrent_branches: Option<usize>,
    pub max_turns: Option<usize>,
    pub branch_max_turns: Option<usize>,
    pub context_window: Option<usize>,
    pub compaction: Option<CompactionConfig>,
    pub memory_persistence: Option<MemoryPersistenceConfig>,
    pub ingestion: Option<IngestionConfig>,
    pub cortex: Option<CortexConfig>,
    pub browser: Option<BrowserConfig>,
    /// Per-agent Brave Search API key override. None inherits from defaults.
    pub brave_search_key: Option<String>,
    /// Heartbeat definitions for this agent.
    pub heartbeats: Vec<HeartbeatDef>,
}

/// A heartbeat definition from config.
#[derive(Debug, Clone)]
pub struct HeartbeatDef {
    pub id: String,
    pub prompt: String,
    pub interval_secs: u64,
    /// Delivery target in "adapter:target" format (e.g. "discord:123456789").
    pub delivery_target: String,
    /// Optional active hours window (start_hour, end_hour) in 24h format.
    pub active_hours: Option<(u8, u8)>,
    pub enabled: bool,
}

/// Fully resolved agent config (merged with defaults, paths resolved).
#[derive(Debug, Clone)]
pub struct ResolvedAgentConfig {
    pub id: String,
    pub workspace: PathBuf,
    pub data_dir: PathBuf,
    pub archives_dir: PathBuf,
    pub routing: RoutingConfig,
    pub max_concurrent_branches: usize,
    pub max_turns: usize,
    pub branch_max_turns: usize,
    pub context_window: usize,
    pub compaction: CompactionConfig,
    pub memory_persistence: MemoryPersistenceConfig,
    pub ingestion: IngestionConfig,
    pub cortex: CortexConfig,
    pub browser: BrowserConfig,
    pub brave_search_key: Option<String>,
    /// Number of messages to fetch from the platform when a new channel is created.
    pub history_backfill_count: usize,
    pub heartbeats: Vec<HeartbeatDef>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            routing: RoutingConfig::default(),
            max_concurrent_branches: 5,
            max_turns: 5,
            branch_max_turns: 50,
            context_window: 128_000,
            compaction: CompactionConfig::default(),
            memory_persistence: MemoryPersistenceConfig::default(),
            ingestion: IngestionConfig::default(),
            cortex: CortexConfig::default(),
            browser: BrowserConfig::default(),
            brave_search_key: None,
            history_backfill_count: 50,
            heartbeats: Vec::new(),
        }
    }
}

impl AgentConfig {
    /// Resolve this agent config against instance defaults and base paths.
    pub fn resolve(&self, instance_dir: &Path, defaults: &DefaultsConfig) -> ResolvedAgentConfig {
        let agent_root = instance_dir.join("agents").join(&self.id);

        ResolvedAgentConfig {
            id: self.id.clone(),
            workspace: self
                .workspace
                .clone()
                .unwrap_or_else(|| agent_root.join("workspace")),
            data_dir: agent_root.join("data"),
            archives_dir: agent_root.join("archives"),
            routing: self
                .routing
                .clone()
                .unwrap_or_else(|| defaults.routing.clone()),
            max_concurrent_branches: self
                .max_concurrent_branches
                .unwrap_or(defaults.max_concurrent_branches),
            max_turns: self.max_turns.unwrap_or(defaults.max_turns),
            branch_max_turns: self.branch_max_turns.unwrap_or(defaults.branch_max_turns),
            context_window: self.context_window.unwrap_or(defaults.context_window),
            compaction: self.compaction.unwrap_or(defaults.compaction),
            memory_persistence: self
                .memory_persistence
                .unwrap_or(defaults.memory_persistence),
            ingestion: self.ingestion.unwrap_or(defaults.ingestion),
            cortex: self.cortex.unwrap_or(defaults.cortex),
            browser: self
                .browser
                .clone()
                .unwrap_or_else(|| defaults.browser.clone()),
            brave_search_key: self
                .brave_search_key
                .clone()
                .or_else(|| defaults.brave_search_key.clone()),
            history_backfill_count: defaults.history_backfill_count,
            heartbeats: self.heartbeats.clone(),
        }
    }
}

impl ResolvedAgentConfig {
    pub fn sqlite_path(&self) -> PathBuf {
        self.data_dir.join("spacebot.db")
    }
    pub fn lancedb_path(&self) -> PathBuf {
        self.data_dir.join("lancedb")
    }
    pub fn redb_path(&self) -> PathBuf {
        self.data_dir.join("config.redb")
    }
    pub fn history_backfill_count(&self) -> usize {
        self.history_backfill_count
    }
    /// Resolved screenshot directory, falling back to data_dir/screenshots.
    pub fn screenshot_dir(&self) -> PathBuf {
        self.browser
            .screenshot_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("screenshots"))
    }

    /// Path to agent workspace skills directory.
    pub fn skills_dir(&self) -> PathBuf {
        self.workspace.join("skills")
    }

    /// Path to the memory ingestion directory where users drop files.
    pub fn ingest_dir(&self) -> PathBuf {
        self.workspace.join("ingest")
    }
}

/// Routes a messaging platform conversation to a specific agent.
#[derive(Debug, Clone)]
pub struct Binding {
    pub agent_id: String,
    pub channel: String,
    pub guild_id: Option<String>,
    pub chat_id: Option<String>,
    /// Discord channel IDs this binding applies to. If empty, all channels in the guild are allowed.
    pub channel_ids: Vec<String>,
}

impl Binding {
    /// Check if this binding matches an inbound message.
    fn matches(&self, message: &crate::InboundMessage) -> bool {
        if self.channel != message.source {
            return false;
        }

        if let Some(guild_id) = &self.guild_id {
            let message_guild = message
                .metadata
                .get("discord_guild_id")
                .and_then(|v| v.as_u64())
                .map(|v| v.to_string());
            if message_guild.as_deref() != Some(guild_id) {
                return false;
            }
        }

        if !self.channel_ids.is_empty() {
            let message_channel = message
                .metadata
                .get("discord_channel_id")
                .and_then(|v| v.as_u64())
                .map(|v| v.to_string());
            let parent_channel = message
                .metadata
                .get("discord_parent_channel_id")
                .and_then(|v| v.as_u64())
                .map(|v| v.to_string());

            let direct_match = message_channel
                .as_ref()
                .is_some_and(|id| self.channel_ids.contains(id));
            let parent_match = parent_channel
                .as_ref()
                .is_some_and(|id| self.channel_ids.contains(id));

            if !direct_match && !parent_match {
                return false;
            }
        }

        if let Some(chat_id) = &self.chat_id {
            let message_chat = message
                .metadata
                .get("telegram_chat_id")
                .and_then(|v| v.as_str());
            if message_chat != Some(chat_id) {
                return false;
            }
        }

        true
    }
}

/// Resolve which agent should handle an inbound message.
///
/// Checks bindings in order. First match wins. Falls back to the default
/// agent if no binding matches.
pub fn resolve_agent_for_message(
    bindings: &[Binding],
    message: &crate::InboundMessage,
    default_agent_id: &str,
) -> crate::AgentId {
    for binding in bindings {
        if binding.matches(message) {
            return std::sync::Arc::from(binding.agent_id.as_str());
        }
    }
    std::sync::Arc::from(default_agent_id)
}

/// Messaging platform credentials (instance-level).
#[derive(Debug, Clone, Default)]
pub struct MessagingConfig {
    pub discord: Option<DiscordConfig>,
    pub webhook: Option<WebhookConfig>,
}

#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub token: String,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub port: u16,
    pub bind: String,
}

// -- TOML deserialization types --

#[derive(Deserialize)]
struct TomlConfig {
    #[serde(default)]
    llm: TomlLlmConfig,
    #[serde(default)]
    defaults: TomlDefaultsConfig,
    #[serde(default)]
    agents: Vec<TomlAgentConfig>,
    #[serde(default)]
    messaging: TomlMessagingConfig,
    #[serde(default)]
    bindings: Vec<TomlBinding>,
}

#[derive(Deserialize, Default)]
struct TomlLlmConfig {
    anthropic_key: Option<String>,
    openai_key: Option<String>,
    openrouter_key: Option<String>,
}

#[derive(Deserialize, Default)]
struct TomlDefaultsConfig {
    routing: Option<TomlRoutingConfig>,
    max_concurrent_branches: Option<usize>,
    max_turns: Option<usize>,
    branch_max_turns: Option<usize>,
    context_window: Option<usize>,
    compaction: Option<TomlCompactionConfig>,
    memory_persistence: Option<TomlMemoryPersistenceConfig>,
    ingestion: Option<TomlIngestionConfig>,
    cortex: Option<TomlCortexConfig>,
    browser: Option<TomlBrowserConfig>,
    brave_search_key: Option<String>,
}

#[derive(Deserialize, Default)]
struct TomlRoutingConfig {
    channel: Option<String>,
    branch: Option<String>,
    worker: Option<String>,
    compactor: Option<String>,
    cortex: Option<String>,
    rate_limit_cooldown_secs: Option<u64>,
    #[serde(default)]
    task_overrides: HashMap<String, String>,
    #[serde(default)]
    fallbacks: HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct TomlMemoryPersistenceConfig {
    enabled: Option<bool>,
    message_interval: Option<usize>,
}

#[derive(Deserialize)]
struct TomlIngestionConfig {
    enabled: Option<bool>,
    poll_interval_secs: Option<u64>,
    chunk_size: Option<usize>,
}

#[derive(Deserialize)]
struct TomlCompactionConfig {
    background_threshold: Option<f32>,
    aggressive_threshold: Option<f32>,
    emergency_threshold: Option<f32>,
}

#[derive(Deserialize)]
struct TomlCortexConfig {
    tick_interval_secs: Option<u64>,
    worker_timeout_secs: Option<u64>,
    branch_timeout_secs: Option<u64>,
    circuit_breaker_threshold: Option<u8>,
}

#[derive(Deserialize)]
struct TomlBrowserConfig {
    enabled: Option<bool>,
    headless: Option<bool>,
    evaluate_enabled: Option<bool>,
    executable_path: Option<String>,
    screenshot_dir: Option<String>,
}

#[derive(Deserialize)]
struct TomlAgentConfig {
    id: String,
    #[serde(default)]
    default: bool,
    workspace: Option<String>,
    routing: Option<TomlRoutingConfig>,
    max_concurrent_branches: Option<usize>,
    max_turns: Option<usize>,
    branch_max_turns: Option<usize>,
    context_window: Option<usize>,
    brave_search_key: Option<String>,
    #[serde(default)]
    heartbeats: Vec<TomlHeartbeatDef>,
}

#[derive(Deserialize)]
struct TomlHeartbeatDef {
    id: String,
    prompt: String,
    interval_secs: Option<u64>,
    delivery_target: String,
    active_start_hour: Option<u8>,
    active_end_hour: Option<u8>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Deserialize, Default)]
struct TomlMessagingConfig {
    discord: Option<TomlDiscordConfig>,
    webhook: Option<TomlWebhookConfig>,
}

#[derive(Deserialize)]
struct TomlDiscordConfig {
    #[serde(default)]
    enabled: bool,
    token: Option<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
}

#[derive(Deserialize)]
struct TomlWebhookConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_webhook_port")]
    port: u16,
    #[serde(default = "default_webhook_bind")]
    bind: String,
}

fn default_webhook_port() -> u16 {
    18789
}
fn default_webhook_bind() -> String {
    "127.0.0.1".into()
}

#[derive(Deserialize)]
struct TomlBinding {
    agent_id: String,
    channel: String,
    guild_id: Option<String>,
    chat_id: Option<String>,
    #[serde(default)]
    channel_ids: Vec<String>,
}

/// Resolve a value that might be an "env:VAR_NAME" reference.
fn resolve_env_value(value: &str) -> Option<String> {
    if let Some(var_name) = value.strip_prefix("env:") {
        std::env::var(var_name).ok()
    } else {
        Some(value.to_string())
    }
}

/// Resolve a TomlRoutingConfig against a base RoutingConfig.
fn resolve_routing(toml: Option<TomlRoutingConfig>, base: &RoutingConfig) -> RoutingConfig {
    let Some(t) = toml else { return base.clone() };

    let mut task_overrides = base.task_overrides.clone();
    task_overrides.extend(t.task_overrides);

    let mut fallbacks = base.fallbacks.clone();
    fallbacks.extend(t.fallbacks);

    RoutingConfig {
        channel: t.channel.unwrap_or_else(|| base.channel.clone()),
        branch: t.branch.unwrap_or_else(|| base.branch.clone()),
        worker: t.worker.unwrap_or_else(|| base.worker.clone()),
        compactor: t.compactor.unwrap_or_else(|| base.compactor.clone()),
        cortex: t.cortex.unwrap_or_else(|| base.cortex.clone()),
        task_overrides,
        fallbacks,
        rate_limit_cooldown_secs: t
            .rate_limit_cooldown_secs
            .unwrap_or(base.rate_limit_cooldown_secs),
    }
}

impl Config {
    /// Load configuration from the default config file, falling back to env vars.
    pub fn load() -> Result<Self> {
        let instance_dir = std::env::var("SPACEBOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|d| d.join(".spacebot"))
                    .unwrap_or_else(|| PathBuf::from("./.spacebot"))
            });

        let config_path = instance_dir.join("config.toml");
        if config_path.exists() {
            Self::load_from_path(&config_path)
        } else {
            Self::load_from_env(&instance_dir)
        }
    }

    /// Load from a specific TOML config file.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let instance_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;

        let toml_config: TomlConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", path.display()))?;

        Self::from_toml(toml_config, instance_dir)
    }

    /// Load from environment variables only (no config file).
    pub fn load_from_env(instance_dir: &Path) -> Result<Self> {
        let llm = LlmConfig {
            anthropic_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            openai_key: std::env::var("OPENAI_API_KEY").ok(),
            openrouter_key: std::env::var("OPENROUTER_API_KEY").ok(),
        };

        if llm.anthropic_key.is_none() && llm.openai_key.is_none() && llm.openrouter_key.is_none() {
            return Err(ConfigError::Invalid(
                "no LLM provider API key found — set ANTHROPIC_API_KEY, OPENAI_API_KEY, or OPENROUTER_API_KEY".into(),
            )
            .into());
        }

        // Env-only routing: check for env overrides on channel/worker models
        let mut routing = RoutingConfig::default();
        if let Ok(channel_model) = std::env::var("SPACEBOT_CHANNEL_MODEL") {
            routing.channel = channel_model;
        }
        if let Ok(worker_model) = std::env::var("SPACEBOT_WORKER_MODEL") {
            routing.worker = worker_model;
        }

        let agents = vec![AgentConfig {
            id: "main".into(),
            default: true,
            workspace: None,
            routing: Some(routing),
            max_concurrent_branches: None,
            max_turns: None,
            branch_max_turns: None,
            context_window: None,
            compaction: None,
            memory_persistence: None,
            ingestion: None,
            cortex: None,
            browser: None,
            brave_search_key: None,
            heartbeats: Vec::new(),
        }];

        Ok(Self {
            instance_dir: instance_dir.to_path_buf(),
            llm,
            defaults: DefaultsConfig::default(),
            agents,
            messaging: MessagingConfig::default(),
            bindings: Vec::new(),
        })
    }

    fn from_toml(toml: TomlConfig, instance_dir: PathBuf) -> Result<Self> {
        let llm = LlmConfig {
            anthropic_key: toml
                .llm
                .anthropic_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok()),
            openai_key: toml
                .llm
                .openai_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("OPENAI_API_KEY").ok()),
            openrouter_key: toml
                .llm
                .openrouter_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("OPENROUTER_API_KEY").ok()),
        };

        if llm.anthropic_key.is_none() && llm.openai_key.is_none() && llm.openrouter_key.is_none() {
            return Err(ConfigError::Invalid(
                "no LLM provider API key found — set ANTHROPIC_API_KEY, OPENAI_API_KEY, or OPENROUTER_API_KEY".into(),
            )
            .into());
        }

        let base_defaults = DefaultsConfig::default();
        let defaults = DefaultsConfig {
            routing: resolve_routing(toml.defaults.routing, &base_defaults.routing),
            max_concurrent_branches: toml
                .defaults
                .max_concurrent_branches
                .unwrap_or(base_defaults.max_concurrent_branches),
            max_turns: toml.defaults.max_turns.unwrap_or(base_defaults.max_turns),
            branch_max_turns: toml
                .defaults
                .branch_max_turns
                .unwrap_or(base_defaults.branch_max_turns),
            context_window: toml
                .defaults
                .context_window
                .unwrap_or(base_defaults.context_window),
            compaction: toml
                .defaults
                .compaction
                .map(|c| CompactionConfig {
                    background_threshold: c
                        .background_threshold
                        .unwrap_or(base_defaults.compaction.background_threshold),
                    aggressive_threshold: c
                        .aggressive_threshold
                        .unwrap_or(base_defaults.compaction.aggressive_threshold),
                    emergency_threshold: c
                        .emergency_threshold
                        .unwrap_or(base_defaults.compaction.emergency_threshold),
                })
                .unwrap_or(base_defaults.compaction),
            memory_persistence: toml
                .defaults
                .memory_persistence
                .map(|mp| MemoryPersistenceConfig {
                    enabled: mp
                        .enabled
                        .unwrap_or(base_defaults.memory_persistence.enabled),
                    message_interval: mp
                        .message_interval
                        .unwrap_or(base_defaults.memory_persistence.message_interval),
                })
                .unwrap_or(base_defaults.memory_persistence),
            ingestion: toml
                .defaults
                .ingestion
                .map(|ig| IngestionConfig {
                    enabled: ig.enabled.unwrap_or(base_defaults.ingestion.enabled),
                    poll_interval_secs: ig
                        .poll_interval_secs
                        .unwrap_or(base_defaults.ingestion.poll_interval_secs),
                    chunk_size: ig.chunk_size.unwrap_or(base_defaults.ingestion.chunk_size),
                })
                .unwrap_or(base_defaults.ingestion),
            cortex: toml
                .defaults
                .cortex
                .map(|c| CortexConfig {
                    tick_interval_secs: c
                        .tick_interval_secs
                        .unwrap_or(base_defaults.cortex.tick_interval_secs),
                    worker_timeout_secs: c
                        .worker_timeout_secs
                        .unwrap_or(base_defaults.cortex.worker_timeout_secs),
                    branch_timeout_secs: c
                        .branch_timeout_secs
                        .unwrap_or(base_defaults.cortex.branch_timeout_secs),
                    circuit_breaker_threshold: c
                        .circuit_breaker_threshold
                        .unwrap_or(base_defaults.cortex.circuit_breaker_threshold),
                })
                .unwrap_or(base_defaults.cortex),
            browser: toml
                .defaults
                .browser
                .map(|b| {
                    let base = &base_defaults.browser;
                    BrowserConfig {
                        enabled: b.enabled.unwrap_or(base.enabled),
                        headless: b.headless.unwrap_or(base.headless),
                        evaluate_enabled: b.evaluate_enabled.unwrap_or(base.evaluate_enabled),
                        executable_path: b.executable_path.or_else(|| base.executable_path.clone()),
                        screenshot_dir: b
                            .screenshot_dir
                            .map(PathBuf::from)
                            .or_else(|| base.screenshot_dir.clone()),
                    }
                })
                .unwrap_or_else(|| base_defaults.browser.clone()),
            brave_search_key: toml
                .defaults
                .brave_search_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("BRAVE_SEARCH_API_KEY").ok()),
            history_backfill_count: base_defaults.history_backfill_count,
            heartbeats: base_defaults.heartbeats,
        };

        let mut agents: Vec<AgentConfig> = toml
            .agents
            .into_iter()
            .map(|a| {
                // Per-agent routing resolves against instance defaults
                let agent_routing = a
                    .routing
                    .map(|r| resolve_routing(Some(r), &defaults.routing));

                let heartbeats = a
                    .heartbeats
                    .into_iter()
                    .map(|h| HeartbeatDef {
                        id: h.id,
                        prompt: h.prompt,
                        interval_secs: h.interval_secs.unwrap_or(3600),
                        delivery_target: h.delivery_target,
                        active_hours: match (h.active_start_hour, h.active_end_hour) {
                            (Some(s), Some(e)) => Some((s, e)),
                            _ => None,
                        },
                        enabled: h.enabled,
                    })
                    .collect();

                AgentConfig {
                    id: a.id,
                    default: a.default,
                    workspace: a.workspace.map(PathBuf::from),
                    routing: agent_routing,
                    max_concurrent_branches: a.max_concurrent_branches,
                    max_turns: a.max_turns,
                    branch_max_turns: a.branch_max_turns,
                    context_window: a.context_window,
                    compaction: None,
                    memory_persistence: None,
                    ingestion: None,
                    cortex: None,
                    browser: None,
                    brave_search_key: a.brave_search_key.as_deref().and_then(resolve_env_value),
                    heartbeats,
                }
            })
            .collect();

        if agents.is_empty() {
            agents.push(AgentConfig {
                id: "main".into(),
                default: true,
                workspace: None,
                routing: None,
                max_concurrent_branches: None,
                max_turns: None,
                branch_max_turns: None,
                context_window: None,
                compaction: None,
                memory_persistence: None,
                ingestion: None,
                cortex: None,
                browser: None,
                brave_search_key: None,
                heartbeats: Vec::new(),
            });
        }

        if !agents.iter().any(|a| a.default) {
            if let Some(first) = agents.first_mut() {
                first.default = true;
            }
        }

        let messaging = MessagingConfig {
            discord: toml.messaging.discord.and_then(|d| {
                let token = d
                    .token
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("DISCORD_BOT_TOKEN").ok())?;
                Some(DiscordConfig {
                    enabled: d.enabled,
                    token,
                    dm_allowed_users: d.dm_allowed_users,
                })
            }),
            webhook: toml.messaging.webhook.map(|w| WebhookConfig {
                enabled: w.enabled,
                port: w.port,
                bind: w.bind,
            }),
        };

        let bindings = toml
            .bindings
            .into_iter()
            .map(|b| Binding {
                agent_id: b.agent_id,
                channel: b.channel,
                guild_id: b.guild_id,
                chat_id: b.chat_id,
                channel_ids: b.channel_ids,
            })
            .collect();

        Ok(Config {
            instance_dir,
            llm,
            defaults,
            agents,
            messaging,
            bindings,
        })
    }

    /// Get the default agent ID.
    pub fn default_agent_id(&self) -> &str {
        self.agents
            .iter()
            .find(|a| a.default)
            .map(|a| a.id.as_str())
            .unwrap_or("main")
    }

    /// Resolve all agent configs against defaults.
    pub fn resolve_agents(&self) -> Vec<ResolvedAgentConfig> {
        self.agents
            .iter()
            .map(|a| a.resolve(&self.instance_dir, &self.defaults))
            .collect()
    }

    /// Path to shared prompts directory.
    pub fn prompts_dir(&self) -> PathBuf {
        self.instance_dir.join("prompts")
    }

    /// Path to instance-level skills directory.
    pub fn skills_dir(&self) -> PathBuf {
        self.instance_dir.join("skills")
    }
}

/// Live configuration that can be hot-reloaded without restarting.
///
/// All fields use ArcSwap for lock-free reads. Consumers call `.load()` on
/// individual fields to get a snapshot — cheap and contention-free.
/// The file watcher calls `.store()` to atomically swap in new values.
pub struct RuntimeConfig {
    pub routing: ArcSwap<RoutingConfig>,
    pub compaction: ArcSwap<CompactionConfig>,
    pub memory_persistence: ArcSwap<MemoryPersistenceConfig>,
    pub ingestion: ArcSwap<IngestionConfig>,
    pub max_turns: ArcSwap<usize>,
    pub branch_max_turns: ArcSwap<usize>,
    pub context_window: ArcSwap<usize>,
    pub max_concurrent_branches: ArcSwap<usize>,
    pub browser_config: ArcSwap<BrowserConfig>,
    pub brave_search_key: ArcSwap<Option<String>>,
    pub prompts: ArcSwap<crate::identity::Prompts>,
    pub identity: ArcSwap<crate::identity::Identity>,
    pub skills: ArcSwap<crate::skills::SkillSet>,
}

impl RuntimeConfig {
    /// Build from a resolved agent config, loaded prompts, identity, and skills.
    pub fn new(
        agent_config: &ResolvedAgentConfig,
        prompts: crate::identity::Prompts,
        identity: crate::identity::Identity,
        skills: crate::skills::SkillSet,
    ) -> Self {
        Self {
            routing: ArcSwap::from_pointee(agent_config.routing.clone()),
            compaction: ArcSwap::from_pointee(agent_config.compaction),
            memory_persistence: ArcSwap::from_pointee(agent_config.memory_persistence),
            ingestion: ArcSwap::from_pointee(agent_config.ingestion),
            max_turns: ArcSwap::from_pointee(agent_config.max_turns),
            branch_max_turns: ArcSwap::from_pointee(agent_config.branch_max_turns),
            context_window: ArcSwap::from_pointee(agent_config.context_window),
            max_concurrent_branches: ArcSwap::from_pointee(agent_config.max_concurrent_branches),
            browser_config: ArcSwap::from_pointee(agent_config.browser.clone()),
            brave_search_key: ArcSwap::from_pointee(agent_config.brave_search_key.clone()),
            prompts: ArcSwap::from_pointee(prompts),
            identity: ArcSwap::from_pointee(identity),
            skills: ArcSwap::from_pointee(skills),
        }
    }

    /// Reload tunable config values from a freshly parsed Config.
    ///
    /// Finds the matching agent by ID, re-resolves it against defaults, and
    /// swaps all reloadable fields. Ignores values that require a restart
    /// (API keys, DB paths, messaging adapters, agent topology).
    pub fn reload_config(&self, config: &Config, agent_id: &str) {
        let agent = config.agents.iter().find(|a| a.id == agent_id);
        let Some(agent) = agent else {
            tracing::warn!(agent_id, "agent not found in reloaded config, skipping");
            return;
        };

        let resolved = agent.resolve(&config.instance_dir, &config.defaults);

        self.routing.store(Arc::new(resolved.routing));
        self.compaction.store(Arc::new(resolved.compaction));
        self.memory_persistence
            .store(Arc::new(resolved.memory_persistence));
        self.ingestion.store(Arc::new(resolved.ingestion));
        self.max_turns.store(Arc::new(resolved.max_turns));
        self.branch_max_turns
            .store(Arc::new(resolved.branch_max_turns));
        self.context_window.store(Arc::new(resolved.context_window));
        self.max_concurrent_branches
            .store(Arc::new(resolved.max_concurrent_branches));
        self.browser_config.store(Arc::new(resolved.browser));
        self.brave_search_key
            .store(Arc::new(resolved.brave_search_key));

        tracing::info!(agent_id, "runtime config reloaded");
    }

    /// Reload prompts from disk.
    pub fn reload_prompts(&self, prompts: crate::identity::Prompts) {
        self.prompts.store(Arc::new(prompts));
        tracing::info!("prompts reloaded");
    }

    /// Reload identity files from disk.
    pub fn reload_identity(&self, identity: crate::identity::Identity) {
        self.identity.store(Arc::new(identity));
        tracing::info!("identity reloaded");
    }

    /// Reload skills from disk.
    pub fn reload_skills(&self, skills: crate::skills::SkillSet) {
        self.skills.store(Arc::new(skills));
        tracing::info!("skills reloaded");
    }
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig").finish_non_exhaustive()
    }
}

/// Watches config, prompt, identity, and skill files for changes and triggers
/// hot reload on the corresponding RuntimeConfig.
///
/// Returns a JoinHandle that runs until dropped. File events are debounced
/// to 2 seconds so rapid edits (e.g. :w in vim hitting multiple writes) are
/// collapsed into a single reload.
pub fn spawn_file_watcher(
    config_path: PathBuf,
    instance_dir: PathBuf,
    agents: Vec<(String, PathBuf, Arc<RuntimeConfig>)>,
) -> tokio::task::JoinHandle<()> {
    use notify::{Event, RecursiveMode, Watcher};
    use std::time::Duration;

    tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();

        let mut watcher = match notify::recommended_watcher(
            move |result: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = result {
                    let _ = tx.send(event);
                }
            },
        ) {
            Ok(w) => w,
            Err(error) => {
                tracing::error!(%error, "failed to create file watcher");
                return;
            }
        };

        // Watch config.toml
        if let Err(error) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
            tracing::warn!(%error, path = %config_path.display(), "failed to watch config file");
        }

        // Watch shared prompts directory
        let shared_prompts_dir = instance_dir.join("prompts");
        if shared_prompts_dir.is_dir() {
            if let Err(error) = watcher.watch(&shared_prompts_dir, RecursiveMode::Recursive) {
                tracing::warn!(%error, path = %shared_prompts_dir.display(), "failed to watch prompts dir");
            }
        }

        // Watch instance-level skills directory
        let instance_skills_dir = instance_dir.join("skills");
        if instance_skills_dir.is_dir() {
            if let Err(error) = watcher.watch(&instance_skills_dir, RecursiveMode::Recursive) {
                tracing::warn!(%error, path = %instance_skills_dir.display(), "failed to watch instance skills dir");
            }
        }

        // Watch per-agent workspace directories (prompts, identity, skills)
        for (_, workspace, _) in &agents {
            for subdir in &["prompts", "skills"] {
                let path = workspace.join(subdir);
                if path.is_dir() {
                    if let Err(error) = watcher.watch(&path, RecursiveMode::Recursive) {
                        tracing::warn!(%error, path = %path.display(), "failed to watch agent dir");
                    }
                }
            }
            // Identity files are in the workspace root
            if let Err(error) = watcher.watch(workspace, RecursiveMode::NonRecursive) {
                tracing::warn!(%error, path = %workspace.display(), "failed to watch workspace");
            }
        }

        tracing::info!("file watcher started");

        // Debounce loop: collect events for 2 seconds, then reload
        let debounce = Duration::from_secs(2);

        loop {
            // Block until the first event arrives
            let first = match rx.recv() {
                Ok(event) => event,
                Err(_) => break,
            };

            // Drain any additional events within the debounce window
            let mut changed_paths: Vec<PathBuf> = first.paths;
            while let Ok(event) = rx.recv_timeout(debounce) {
                changed_paths.extend(event.paths);
            }

            // Categorize what changed
            let config_changed = changed_paths.iter().any(|p| p.ends_with("config.toml"));
            let prompts_changed = changed_paths.iter().any(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("md")
                    && p.to_string_lossy().contains("prompts")
            });
            let identity_changed = changed_paths.iter().any(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                matches!(name, "SOUL.md" | "IDENTITY.md" | "USER.md")
            });
            let skills_changed = changed_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("skills"));

            let changed_summary: Vec<&str> = [
                config_changed.then_some("config"),
                prompts_changed.then_some("prompts"),
                identity_changed.then_some("identity"),
                skills_changed.then_some("skills"),
            ]
            .into_iter()
            .flatten()
            .collect();

            tracing::info!(
                changed = %changed_summary.join(", "),
                "file change detected, reloading"
            );

            // Reload config.toml if it changed
            let new_config = if config_changed {
                match Config::load_from_path(&config_path) {
                    Ok(config) => Some(config),
                    Err(error) => {
                        tracing::error!(%error, "failed to reload config.toml, keeping previous values");
                        None
                    }
                }
            } else {
                None
            };

            // Apply reloads to each agent's RuntimeConfig
            for (agent_id, workspace, runtime_config) in &agents {
                if let Some(config) = &new_config {
                    runtime_config.reload_config(config, agent_id);
                }

                if prompts_changed {
                    let prompts_dir = instance_dir.join("prompts");
                    let rt = tokio::runtime::Handle::current();
                    match rt.block_on(crate::identity::Prompts::load(workspace, &prompts_dir)) {
                        Ok(prompts) => runtime_config.reload_prompts(prompts),
                        Err(error) => tracing::error!(%error, agent_id, "failed to reload prompts"),
                    }
                }

                if identity_changed {
                    let rt = tokio::runtime::Handle::current();
                    let identity = rt.block_on(crate::identity::Identity::load(workspace));
                    runtime_config.reload_identity(identity);
                }

                if skills_changed {
                    let rt = tokio::runtime::Handle::current();
                    let skills = rt.block_on(crate::skills::SkillSet::load(
                        &instance_dir.join("skills"),
                        &workspace.join("skills"),
                    ));
                    runtime_config.reload_skills(skills);
                }
            }
        }

        tracing::info!("file watcher stopped");
    })
}
