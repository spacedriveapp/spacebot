//! Configuration loading and validation.

use crate::error::{ConfigError, Result};
use crate::llm::routing::RoutingConfig;
use anyhow::Context as _;
use arc_swap::ArcSwap;
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// OpenTelemetry export configuration.
///
/// All fields are optional. If `otlp_endpoint` is not set (and the standard
/// `OTEL_EXPORTER_OTLP_ENDPOINT` env var is not present), OTLP export is
/// disabled and the OTel layer is omitted entirely.
#[derive(Debug, Clone, Default)]
pub struct TelemetryConfig {
    /// OTLP HTTP endpoint, e.g. `http://localhost:4318`.
    /// Falls back to the `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable.
    pub otlp_endpoint: Option<String>,
    /// Extra OTLP headers for the exporter (e.g. `Authorization`).
    /// Loaded from the `OTEL_EXPORTER_OTLP_HEADERS` environment variable.
    pub otlp_headers: HashMap<String, String>,
    /// `service.name` resource attribute sent with every span.
    pub service_name: String,
    /// Trace sample rate in the range 0.0–1.0. Defaults to 1.0 (sample all).
    pub sample_rate: f64,
}

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
    /// HTTP API server configuration.
    pub api: ApiConfig,
    /// Prometheus metrics endpoint configuration.
    pub metrics: MetricsConfig,
    /// OpenTelemetry export configuration.
    pub telemetry: TelemetryConfig,
}

/// HTTP API server configuration.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Whether the HTTP API server is enabled.
    pub enabled: bool,
    /// Port to bind the HTTP server on.
    pub port: u16,
    /// Address to bind the HTTP server on.
    pub bind: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 19898,
            bind: "127.0.0.1".into(),
        }
    }
}

/// Prometheus metrics endpoint configuration.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Whether the metrics endpoint is enabled.
    pub enabled: bool,
    /// Port to bind the metrics HTTP server on.
    pub port: u16,
    /// Address to bind the metrics HTTP server on.
    pub bind: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 9090,
            bind: "0.0.0.0".into(),
        }
    }
}

/// API types supported by LLM providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiType {
    /// OpenAI Completions API (https://api.openai.com/v1/completions)
    OpenAiCompletions,
    /// OpenAI Responses API (https://api.openai.com/v1/chat/completions)
    OpenAiResponses,
    /// Anthropic Messages API (https://api.anthropic.com/v1/messages)
    Anthropic,
}

impl<'de> serde::Deserialize<'de> for ApiType {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "openai_completions" => Ok(Self::OpenAiCompletions),
            "openai_responses" => Ok(Self::OpenAiResponses),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(other),
                &"one of \"openai_completions\", \"openai_responses\", or \"anthropic\"",
            )),
        }
    }
}

/// Configuration for a single LLM provider.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub api_type: ApiType,
    pub base_url: String,
    pub api_key: String,
    pub name: Option<String>,
}

/// LLM provider credentials (instance-level).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub anthropic_key: Option<String>,
    pub openai_key: Option<String>,
    pub openrouter_key: Option<String>,
    pub zhipu_key: Option<String>,
    pub groq_key: Option<String>,
    pub together_key: Option<String>,
    pub fireworks_key: Option<String>,
    pub deepseek_key: Option<String>,
    pub xai_key: Option<String>,
    pub mistral_key: Option<String>,
    pub ollama_key: Option<String>,
    pub ollama_base_url: Option<String>,
    pub opencode_zen_key: Option<String>,
    pub nvidia_key: Option<String>,
    pub minimax_key: Option<String>,
    pub moonshot_key: Option<String>,
    pub zai_coding_plan_key: Option<String>,
    pub providers: HashMap<String, ProviderConfig>,
}

impl LlmConfig {
    /// Check if any provider configuration is set.
    pub fn has_any_key(&self) -> bool {
        self.anthropic_key.is_some()
            || self.openai_key.is_some()
            || self.openrouter_key.is_some()
            || self.zhipu_key.is_some()
            || self.groq_key.is_some()
            || self.together_key.is_some()
            || self.fireworks_key.is_some()
            || self.deepseek_key.is_some()
            || self.xai_key.is_some()
            || self.mistral_key.is_some()
            || self.ollama_key.is_some()
            || self.ollama_base_url.is_some()
            || self.opencode_zen_key.is_some()
            || self.nvidia_key.is_some()
            || self.minimax_key.is_some()
            || self.moonshot_key.is_some()
            || self.zai_coding_plan_key.is_some()
            || !self.providers.is_empty()
    }
}

const ANTHROPIC_PROVIDER_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_PROVIDER_BASE_URL: &str = "https://api.openai.com";
const OPENROUTER_PROVIDER_BASE_URL: &str = "https://openrouter.ai/api";
const OPENCODE_ZEN_PROVIDER_BASE_URL: &str = "https://opencode.ai/zen";
const MINIMAX_PROVIDER_BASE_URL: &str = "https://api.minimax.io/anthropic";
const MOONSHOT_PROVIDER_BASE_URL: &str = "https://api.moonshot.ai";

const ZHIPU_PROVIDER_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const ZAI_CODING_PLAN_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const NVIDIA_PROVIDER_BASE_URL: &str = "https://integrate.api.nvidia.com";

/// Defaults inherited by all agents. Individual agents can override any field.
#[derive(Debug, Clone)]
pub struct DefaultsConfig {
    pub routing: RoutingConfig,
    pub max_concurrent_branches: usize,
    pub max_concurrent_workers: usize,
    pub max_turns: usize,
    pub branch_max_turns: usize,
    pub context_window: usize,
    pub compaction: CompactionConfig,
    pub memory_persistence: MemoryPersistenceConfig,
    pub coalesce: CoalesceConfig,
    pub ingestion: IngestionConfig,
    pub cortex: CortexConfig,
    pub browser: BrowserConfig,
    pub mcp: Vec<McpServerConfig>,
    /// Brave Search API key for web search tool. Supports "env:VAR_NAME" references.
    pub brave_search_key: Option<String>,
    pub history_backfill_count: usize,
    pub cron: Vec<CronDef>,
    pub opencode: OpenCodeConfig,
    /// Worker log mode: "errors_only", "all_separate", or "all_combined".
    pub worker_log_mode: crate::settings::WorkerLogMode,
}

/// MCP server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
    pub enabled: bool,
}

/// MCP transport configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
}

impl McpTransport {
    pub fn kind(&self) -> &'static str {
        match self {
            McpTransport::Stdio { .. } => "stdio",
            McpTransport::Http { .. } => "http",
        }
    }
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

/// Message coalescing configuration for handling rapid-fire messages.
///
/// When enabled, messages arriving in quick succession are accumulated and
/// presented to the LLM as a single batched turn with a hint that this is
/// a fast-moving conversation.
#[derive(Debug, Clone, Copy)]
pub struct CoalesceConfig {
    /// Enable message coalescing for multi-user channels.
    pub enabled: bool,
    /// Initial debounce window after first message (milliseconds).
    pub debounce_ms: u64,
    /// Maximum time to wait before flushing regardless (milliseconds).
    pub max_wait_ms: u64,
    /// Min messages to trigger coalesce mode (1 = always debounce, 2 = only when burst detected).
    pub min_messages: usize,
    /// Apply only to multi-user conversations (skip for DMs).
    pub multi_user_only: bool,
}

impl Default for CoalesceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_ms: 1500,
            max_wait_ms: 5000,
            min_messages: 2,
            multi_user_only: true,
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

/// OpenCode subprocess worker configuration.
#[derive(Debug, Clone)]
pub struct OpenCodeConfig {
    /// Whether OpenCode workers are available.
    pub enabled: bool,
    /// Path to the OpenCode binary. Supports "env:VAR_NAME" references.
    /// Falls back to "opencode" on PATH.
    pub path: String,
    /// Maximum concurrent OpenCode server processes.
    pub max_servers: usize,
    /// Timeout in seconds waiting for a server to become healthy.
    pub server_startup_timeout_secs: u64,
    /// Maximum restart attempts before giving up on a server.
    pub max_restart_retries: u32,
    /// Permission settings passed to OpenCode's config.
    pub permissions: crate::opencode::OpenCodePermissions,
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: "opencode".to_string(),
            max_servers: 5,
            server_startup_timeout_secs: 30,
            max_restart_retries: 5,
            permissions: crate::opencode::OpenCodePermissions::default(),
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
    /// Interval in seconds between memory bulletin refreshes.
    pub bulletin_interval_secs: u64,
    /// Target word count for the memory bulletin.
    pub bulletin_max_words: usize,
    /// Max LLM turns for bulletin generation.
    pub bulletin_max_turns: usize,
    /// Interval in seconds between association passes.
    pub association_interval_secs: u64,
    /// Minimum cosine similarity to create a RelatedTo edge.
    pub association_similarity_threshold: f32,
    /// Minimum cosine similarity to create an Updates edge (near-duplicate).
    pub association_updates_threshold: f32,
    /// Max associations to create per pass (rate limit).
    pub association_max_per_pass: usize,
}

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 30,
            worker_timeout_secs: 300,
            branch_timeout_secs: 60,
            circuit_breaker_threshold: 3,
            bulletin_interval_secs: 3600,
            bulletin_max_words: 1500,
            bulletin_max_turns: 15,
            association_interval_secs: 300,
            association_similarity_threshold: 0.85,
            association_updates_threshold: 0.95,
            association_max_per_pass: 100,
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
    pub max_concurrent_workers: Option<usize>,
    pub max_turns: Option<usize>,
    pub branch_max_turns: Option<usize>,
    pub context_window: Option<usize>,
    pub compaction: Option<CompactionConfig>,
    pub memory_persistence: Option<MemoryPersistenceConfig>,
    pub coalesce: Option<CoalesceConfig>,
    pub ingestion: Option<IngestionConfig>,
    pub cortex: Option<CortexConfig>,
    pub browser: Option<BrowserConfig>,
    pub mcp: Option<Vec<McpServerConfig>>,
    /// Per-agent Brave Search API key override. None inherits from defaults.
    pub brave_search_key: Option<String>,
    /// Cron job definitions for this agent.
    pub cron: Vec<CronDef>,
}

/// A cron job definition from config.
#[derive(Debug, Clone)]
pub struct CronDef {
    pub id: String,
    pub prompt: String,
    pub interval_secs: u64,
    /// Delivery target in "adapter:target" format (e.g. "discord:123456789").
    pub delivery_target: String,
    /// Optional active hours window (start_hour, end_hour) in 24h format.
    pub active_hours: Option<(u8, u8)>,
    pub enabled: bool,
    pub run_once: bool,
    /// Maximum wall-clock seconds to wait for the job to complete.
    /// `None` uses the default of 120 seconds.
    pub timeout_secs: Option<u64>,
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
    pub max_concurrent_workers: usize,
    pub max_turns: usize,
    pub branch_max_turns: usize,
    pub context_window: usize,
    pub compaction: CompactionConfig,
    pub memory_persistence: MemoryPersistenceConfig,
    pub coalesce: CoalesceConfig,
    pub ingestion: IngestionConfig,
    pub cortex: CortexConfig,
    pub browser: BrowserConfig,
    pub mcp: Vec<McpServerConfig>,
    pub brave_search_key: Option<String>,
    /// Number of messages to fetch from the platform when a new channel is created.
    pub history_backfill_count: usize,
    pub cron: Vec<CronDef>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            routing: RoutingConfig::default(),
            max_concurrent_branches: 5,
            max_concurrent_workers: 5,
            max_turns: 5,
            branch_max_turns: 50,
            context_window: 128_000,
            compaction: CompactionConfig::default(),
            memory_persistence: MemoryPersistenceConfig::default(),
            coalesce: CoalesceConfig::default(),
            ingestion: IngestionConfig::default(),
            cortex: CortexConfig::default(),
            browser: BrowserConfig::default(),
            mcp: Vec::new(),
            brave_search_key: None,
            history_backfill_count: 50,
            cron: Vec::new(),
            opencode: OpenCodeConfig::default(),
            worker_log_mode: crate::settings::WorkerLogMode::default(),
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
            max_concurrent_workers: self
                .max_concurrent_workers
                .unwrap_or(defaults.max_concurrent_workers),
            max_turns: self.max_turns.unwrap_or(defaults.max_turns),
            branch_max_turns: self.branch_max_turns.unwrap_or(defaults.branch_max_turns),
            context_window: self.context_window.unwrap_or(defaults.context_window),
            compaction: self.compaction.unwrap_or(defaults.compaction),
            memory_persistence: self
                .memory_persistence
                .unwrap_or(defaults.memory_persistence),
            coalesce: self.coalesce.unwrap_or(defaults.coalesce),
            ingestion: self.ingestion.unwrap_or(defaults.ingestion),
            cortex: self.cortex.unwrap_or(defaults.cortex),
            browser: self
                .browser
                .clone()
                .unwrap_or_else(|| defaults.browser.clone()),
            mcp: resolve_mcp_configs(&defaults.mcp, self.mcp.as_deref()),
            brave_search_key: self
                .brave_search_key
                .clone()
                .or_else(|| defaults.brave_search_key.clone()),
            history_backfill_count: defaults.history_backfill_count,
            cron: self.cron.clone(),
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

    /// Directory for worker execution logs written on failure.
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
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
    pub workspace_id: Option<String>, // Slack workspace (team) ID
    pub chat_id: Option<String>,
    /// Channel IDs this binding applies to. If empty, all channels in the guild/workspace are allowed.
    pub channel_ids: Vec<String>,
    /// Require explicit @mention (or reply-to-bot) for inbound messages.
    pub require_mention: bool,
    /// User IDs allowed to DM the bot through this binding.
    pub dm_allowed_users: Vec<String>,
}

impl Binding {
    /// Check if this binding matches an inbound message.
    fn matches(&self, message: &crate::InboundMessage) -> bool {
        if self.channel != message.source {
            return false;
        }

        // For webchat messages, match based on agent_id in the message
        if message.source == "webchat" {
            if let Some(message_agent_id) = &message.agent_id {
                return message_agent_id.as_ref() == &self.agent_id;
            }
        }

        // DM messages have no guild_id — match if the sender is in dm_allowed_users
        let is_dm =
            message.metadata.get("discord_guild_id").is_none() && message.source == "discord";
        if is_dm {
            return !self.dm_allowed_users.is_empty()
                && self.dm_allowed_users.contains(&message.sender_id);
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

        if let Some(workspace_id) = &self.workspace_id {
            let message_workspace = message
                .metadata
                .get("slack_workspace_id")
                .and_then(|v| v.as_str());
            if message_workspace != Some(workspace_id) {
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

            // Also check Slack and Twitch channel IDs
            let slack_channel = message
                .metadata
                .get("slack_channel_id")
                .and_then(|v| v.as_str());
            let twitch_channel = message
                .metadata
                .get("twitch_channel")
                .and_then(|v| v.as_str());

            let direct_match = message_channel
                .as_ref()
                .is_some_and(|id| self.channel_ids.contains(id))
                || slack_channel.is_some_and(|id| self.channel_ids.contains(&id.to_string()))
                || twitch_channel.is_some_and(|id| self.channel_ids.contains(&id.to_string()));
            let parent_match = parent_channel
                .as_ref()
                .is_some_and(|id| self.channel_ids.contains(id));

            if !direct_match && !parent_match {
                return false;
            }
        }

        if let Some(chat_id) = &self.chat_id {
            let message_chat = message.metadata.get("telegram_chat_id").and_then(|value| {
                value
                    .as_str()
                    .map(std::borrow::ToOwned::to_owned)
                    .or_else(|| value.as_i64().map(|id| id.to_string()))
            });
            if message_chat.as_deref() != Some(chat_id.as_str()) {
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
    pub slack: Option<SlackConfig>,
    pub telegram: Option<TelegramConfig>,
    pub webhook: Option<WebhookConfig>,
    pub twitch: Option<TwitchConfig>,
}

#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub token: String,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
    /// Whether to process messages from other bots (self-messages are always ignored).
    pub allow_bot_messages: bool,
}

/// A single slash command definition for the Slack adapter.
///
/// Maps a Slack slash command (e.g. `/ask`) to a target agent.
/// Commands not listed here are acknowledged but produce a "not configured" reply.
#[derive(Debug, Clone)]
pub struct SlackCommandConfig {
    /// The slash command string exactly as Slack sends it, e.g. `"/ask"`.
    pub command: String,
    /// ID of the agent that should handle this command.
    pub agent_id: String,
    /// Short description shown in Slack's command autocomplete hint (optional).
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SlackConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub app_token: String,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
    /// Slash command definitions. If empty, all slash commands are ignored.
    pub commands: Vec<SlackCommandConfig>,
}

/// Hot-reloadable Discord permission filters.
///
/// Derived from bindings + discord config. Shared with the Discord adapter
/// via `Arc<ArcSwap<..>>` so the file watcher can swap in new values without
/// restarting the gateway connection.
#[derive(Debug, Clone, Default)]
pub struct DiscordPermissions {
    pub guild_filter: Option<Vec<u64>>,
    pub channel_filter: std::collections::HashMap<u64, Vec<u64>>,
    pub mention_required_guilds: Vec<u64>,
    pub mention_required_channels: std::collections::HashMap<u64, Vec<u64>>,
    pub dm_allowed_users: Vec<u64>,
    pub allow_bot_messages: bool,
}

/// Hot-reloadable Slack permission filters.
///
/// Shared with the Slack adapter via `Arc<ArcSwap<..>>` for hot-reloading.
#[derive(Debug, Clone, Default)]
pub struct SlackPermissions {
    pub workspace_filter: Option<Vec<String>>, // team IDs
    pub channel_filter: std::collections::HashMap<String, Vec<String>>, // team_id -> allowed channel_ids
    pub dm_allowed_users: Vec<String>,                                  // user IDs
}

impl SlackPermissions {
    /// Build from the current config's slack settings and bindings.
    pub fn from_config(slack: &SlackConfig, bindings: &[Binding]) -> Self {
        let slack_bindings: Vec<&Binding> =
            bindings.iter().filter(|b| b.channel == "slack").collect();

        let workspace_filter = {
            let workspace_ids: Vec<String> = slack_bindings
                .iter()
                .filter_map(|b| b.workspace_id.clone())
                .collect();
            if workspace_ids.is_empty() {
                None
            } else {
                Some(workspace_ids)
            }
        };

        let channel_filter = {
            let mut filter: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for binding in &slack_bindings {
                if let Some(workspace_id) = &binding.workspace_id {
                    if !binding.channel_ids.is_empty() {
                        filter
                            .entry(workspace_id.clone())
                            .or_default()
                            .extend(binding.channel_ids.clone());
                    }
                }
            }
            filter
        };

        let dm_allowed_users = slack.dm_allowed_users.clone();

        Self {
            workspace_filter,
            channel_filter,
            dm_allowed_users,
        }
    }
}

impl DiscordPermissions {
    /// Build from the current config's discord settings and bindings.
    pub fn from_config(discord: &DiscordConfig, bindings: &[Binding]) -> Self {
        let discord_bindings: Vec<&Binding> =
            bindings.iter().filter(|b| b.channel == "discord").collect();

        let guild_filter = {
            let guild_ids: Vec<u64> = discord_bindings
                .iter()
                .filter_map(|b| b.guild_id.as_ref()?.parse::<u64>().ok())
                .collect();
            if guild_ids.is_empty() {
                None
            } else {
                Some(guild_ids)
            }
        };

        let channel_filter = {
            let mut filter: std::collections::HashMap<u64, Vec<u64>> =
                std::collections::HashMap::new();
            for binding in &discord_bindings {
                if let Some(guild_id) = binding
                    .guild_id
                    .as_ref()
                    .and_then(|g| g.parse::<u64>().ok())
                {
                    if !binding.channel_ids.is_empty() {
                        let channel_ids: Vec<u64> = binding
                            .channel_ids
                            .iter()
                            .filter_map(|id| id.parse::<u64>().ok())
                            .collect();
                        filter.entry(guild_id).or_default().extend(channel_ids);
                    }
                }
            }
            filter
        };

        let mut mention_required_guilds: Vec<u64> = Vec::new();
        let mention_required_channels = {
            let mut filter: std::collections::HashMap<u64, Vec<u64>> =
                std::collections::HashMap::new();

            for binding in &discord_bindings {
                if !binding.require_mention {
                    continue;
                }

                if let Some(guild_id) = binding
                    .guild_id
                    .as_ref()
                    .and_then(|g| g.parse::<u64>().ok())
                {
                    if binding.channel_ids.is_empty() {
                        if !mention_required_guilds.contains(&guild_id) {
                            mention_required_guilds.push(guild_id);
                        }
                        continue;
                    }

                    let channel_ids: Vec<u64> = binding
                        .channel_ids
                        .iter()
                        .filter_map(|id| id.parse::<u64>().ok())
                        .collect();
                    if !channel_ids.is_empty() {
                        filter.entry(guild_id).or_default().extend(channel_ids);
                    }
                }
            }

            filter
        };

        let mut dm_allowed_users: Vec<u64> = discord
            .dm_allowed_users
            .iter()
            .filter_map(|id| id.parse::<u64>().ok())
            .collect();

        // Also collect dm_allowed_users from bindings
        for binding in &discord_bindings {
            for id in &binding.dm_allowed_users {
                if let Ok(uid) = id.parse::<u64>() {
                    if !dm_allowed_users.contains(&uid) {
                        dm_allowed_users.push(uid);
                    }
                }
            }
        }

        Self {
            guild_filter,
            channel_filter,
            mention_required_guilds,
            mention_required_channels,
            dm_allowed_users,
            allow_bot_messages: discord.allow_bot_messages,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token: String,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
}

/// Hot-reloadable Telegram permission filters.
///
/// Shared with the Telegram adapter via `Arc<ArcSwap<..>>` for hot-reloading.
#[derive(Debug, Clone, Default)]
pub struct TelegramPermissions {
    /// Allowed chat IDs (None = all chats accepted).
    pub chat_filter: Option<Vec<i64>>,
    /// User IDs allowed in private chats.
    pub dm_allowed_users: Vec<i64>,
}

impl TelegramPermissions {
    /// Build from the current config's telegram settings and bindings.
    pub fn from_config(telegram: &TelegramConfig, bindings: &[Binding]) -> Self {
        let telegram_bindings: Vec<&Binding> = bindings
            .iter()
            .filter(|b| b.channel == "telegram")
            .collect();

        let chat_filter = {
            let chat_ids: Vec<i64> = telegram_bindings
                .iter()
                .filter_map(|b| b.chat_id.as_ref()?.parse::<i64>().ok())
                .collect();
            if chat_ids.is_empty() {
                None
            } else {
                Some(chat_ids)
            }
        };

        let mut dm_allowed_users: Vec<i64> = telegram
            .dm_allowed_users
            .iter()
            .filter_map(|id| id.parse::<i64>().ok())
            .collect();

        for binding in &telegram_bindings {
            for id in &binding.dm_allowed_users {
                if let Ok(uid) = id.parse::<i64>() {
                    if !dm_allowed_users.contains(&uid) {
                        dm_allowed_users.push(uid);
                    }
                }
            }
        }

        Self {
            chat_filter,
            dm_allowed_users,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TwitchConfig {
    pub enabled: bool,
    pub username: String,
    pub oauth_token: String,
    /// Channels to join (without the # prefix).
    pub channels: Vec<String>,
    /// Optional prefix that triggers the bot (e.g. "!ask"). If empty, all messages are processed.
    pub trigger_prefix: Option<String>,
}

/// Hot-reloadable Twitch permission filters.
///
/// Shared with the Twitch adapter via `Arc<ArcSwap<..>>` for hot-reloading.
#[derive(Debug, Clone, Default)]
pub struct TwitchPermissions {
    /// Allowed channel names (None = all joined channels accepted).
    pub channel_filter: Option<Vec<String>>,
    /// User login names allowed to interact with the bot. Empty = all users.
    pub allowed_users: Vec<String>,
}

impl TwitchPermissions {
    /// Build from the current config's twitch settings and bindings.
    pub fn from_config(_twitch: &TwitchConfig, bindings: &[Binding]) -> Self {
        let twitch_bindings: Vec<&Binding> =
            bindings.iter().filter(|b| b.channel == "twitch").collect();

        let channel_filter = {
            let channel_ids: Vec<String> = twitch_bindings
                .iter()
                .flat_map(|b| b.channel_ids.clone())
                .collect();
            if channel_ids.is_empty() {
                None
            } else {
                Some(channel_ids)
            }
        };

        let mut allowed_users: Vec<String> = Vec::new();
        for binding in &twitch_bindings {
            for id in &binding.dm_allowed_users {
                if !allowed_users.contains(id) {
                    allowed_users.push(id.clone());
                }
            }
        }

        Self {
            channel_filter,
            allowed_users,
        }
    }
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
    #[serde(default)]
    api: TomlApiConfig,
    #[serde(default)]
    metrics: TomlMetricsConfig,
    #[serde(default)]
    telemetry: TomlTelemetryConfig,
}

#[derive(Deserialize, Default)]
struct TomlTelemetryConfig {
    otlp_endpoint: Option<String>,
    otlp_headers: Option<String>,
    service_name: Option<String>,
    sample_rate: Option<f64>,
}

#[derive(Deserialize)]
struct TomlApiConfig {
    #[serde(default = "default_api_enabled")]
    enabled: bool,
    #[serde(default = "default_api_port")]
    port: u16,
    #[serde(default = "default_api_bind")]
    bind: String,
}

impl Default for TomlApiConfig {
    fn default() -> Self {
        Self {
            enabled: default_api_enabled(),
            port: default_api_port(),
            bind: default_api_bind(),
        }
    }
}

fn default_api_enabled() -> bool {
    true
}
fn default_api_port() -> u16 {
    19898
}
fn default_api_bind() -> String {
    "127.0.0.1".into()
}

fn hosted_api_bind(bind: String) -> String {
    match std::env::var("SPACEBOT_DEPLOYMENT") {
        Ok(deployment) if deployment.eq_ignore_ascii_case("hosted") => "[::]".into(),
        _ => bind,
    }
}

#[derive(Deserialize)]
struct TomlMetricsConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_metrics_port")]
    port: u16,
    #[serde(default = "default_metrics_bind")]
    bind: String,
}

impl Default for TomlMetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_metrics_port(),
            bind: default_metrics_bind(),
        }
    }
}

fn default_metrics_port() -> u16 {
    9090
}
fn default_metrics_bind() -> String {
    "0.0.0.0".into()
}

#[derive(Deserialize, Debug)]
struct TomlProviderConfig {
    api_type: ApiType,
    base_url: String,
    api_key: String,
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct TomlLlmConfigFields {
    anthropic_key: Option<String>,
    openai_key: Option<String>,
    openrouter_key: Option<String>,
    zhipu_key: Option<String>,
    groq_key: Option<String>,
    together_key: Option<String>,
    fireworks_key: Option<String>,
    deepseek_key: Option<String>,
    xai_key: Option<String>,
    mistral_key: Option<String>,
    ollama_key: Option<String>,
    ollama_base_url: Option<String>,
    opencode_zen_key: Option<String>,
    nvidia_key: Option<String>,
    minimax_key: Option<String>,
    moonshot_key: Option<String>,
    zai_coding_plan_key: Option<String>,
    #[serde(default)]
    providers: HashMap<String, TomlProviderConfig>,
    #[serde(default)]
    #[serde(flatten)]
    extra: HashMap<String, toml::Value>,
}

#[derive(Default)]
struct TomlLlmConfig {
    anthropic_key: Option<String>,
    openai_key: Option<String>,
    openrouter_key: Option<String>,
    zhipu_key: Option<String>,
    groq_key: Option<String>,
    together_key: Option<String>,
    fireworks_key: Option<String>,
    deepseek_key: Option<String>,
    xai_key: Option<String>,
    mistral_key: Option<String>,
    ollama_key: Option<String>,
    ollama_base_url: Option<String>,
    opencode_zen_key: Option<String>,
    nvidia_key: Option<String>,
    minimax_key: Option<String>,
    moonshot_key: Option<String>,
    zai_coding_plan_key: Option<String>,
    providers: HashMap<String, TomlProviderConfig>,
}

impl<'de> Deserialize<'de> for TomlLlmConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut fields = TomlLlmConfigFields::deserialize(deserializer)?;
        let mut providers = fields.providers;

        for (key, value) in fields.extra {
            if key == "provider" {
                let table = value
                    .as_table()
                    .ok_or_else(|| serde::de::Error::custom("`llm.provider` must be a table"))?;
                for (provider_id, provider_value) in table {
                    let provider_config = provider_value
                        .clone()
                        .try_into()
                        .map_err(serde::de::Error::custom)?;
                    providers.insert(provider_id.to_string(), provider_config);
                }
            }

            if let Some(provider_id) = key.strip_prefix("provider.") {
                let provider_config = value.try_into().map_err(serde::de::Error::custom)?;
                providers.insert(provider_id.to_string(), provider_config);
            }
        }

        fields.providers = providers;

        Ok(Self {
            anthropic_key: fields.anthropic_key,
            openai_key: fields.openai_key,
            openrouter_key: fields.openrouter_key,
            zhipu_key: fields.zhipu_key,
            groq_key: fields.groq_key,
            together_key: fields.together_key,
            fireworks_key: fields.fireworks_key,
            deepseek_key: fields.deepseek_key,
            xai_key: fields.xai_key,
            mistral_key: fields.mistral_key,
            ollama_key: fields.ollama_key,
            ollama_base_url: fields.ollama_base_url,
            opencode_zen_key: fields.opencode_zen_key,
            nvidia_key: fields.nvidia_key,
            minimax_key: fields.minimax_key,
            moonshot_key: fields.moonshot_key,
            zai_coding_plan_key: fields.zai_coding_plan_key,
            providers: fields.providers,
        })
    }
}

#[derive(Deserialize, Default)]
struct TomlDefaultsConfig {
    routing: Option<TomlRoutingConfig>,
    max_concurrent_branches: Option<usize>,
    max_concurrent_workers: Option<usize>,
    max_turns: Option<usize>,
    branch_max_turns: Option<usize>,
    context_window: Option<usize>,
    compaction: Option<TomlCompactionConfig>,
    memory_persistence: Option<TomlMemoryPersistenceConfig>,
    coalesce: Option<TomlCoalesceConfig>,
    ingestion: Option<TomlIngestionConfig>,
    cortex: Option<TomlCortexConfig>,
    browser: Option<TomlBrowserConfig>,
    #[serde(default)]
    mcp: Vec<TomlMcpServerConfig>,
    brave_search_key: Option<String>,
    opencode: Option<TomlOpenCodeConfig>,
    worker_log_mode: Option<String>,
}

#[derive(Deserialize, Default)]
struct TomlRoutingConfig {
    channel: Option<String>,
    branch: Option<String>,
    worker: Option<String>,
    compactor: Option<String>,
    cortex: Option<String>,
    rate_limit_cooldown_secs: Option<u64>,
    channel_thinking_effort: Option<String>,
    branch_thinking_effort: Option<String>,
    worker_thinking_effort: Option<String>,
    compactor_thinking_effort: Option<String>,
    cortex_thinking_effort: Option<String>,
    #[serde(default)]
    task_overrides: HashMap<String, String>,
    fallbacks: Option<HashMap<String, Vec<String>>>,
}

#[derive(Deserialize)]
struct TomlMemoryPersistenceConfig {
    enabled: Option<bool>,
    message_interval: Option<usize>,
}

#[derive(Deserialize)]
struct TomlCoalesceConfig {
    enabled: Option<bool>,
    debounce_ms: Option<u64>,
    max_wait_ms: Option<u64>,
    min_messages: Option<usize>,
    multi_user_only: Option<bool>,
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
    bulletin_interval_secs: Option<u64>,
    bulletin_max_words: Option<usize>,
    bulletin_max_turns: Option<usize>,
    association_interval_secs: Option<u64>,
    association_similarity_threshold: Option<f32>,
    association_updates_threshold: Option<f32>,
    association_max_per_pass: Option<usize>,
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
struct TomlOpenCodeConfig {
    enabled: Option<bool>,
    path: Option<String>,
    max_servers: Option<usize>,
    server_startup_timeout_secs: Option<u64>,
    max_restart_retries: Option<u32>,
    permissions: Option<TomlOpenCodePermissions>,
}

#[derive(Deserialize)]
struct TomlOpenCodePermissions {
    edit: Option<String>,
    bash: Option<String>,
    webfetch: Option<String>,
}

#[derive(Deserialize, Clone)]
struct TomlMcpServerConfig {
    name: String,
    transport: String,
    #[serde(default = "default_mcp_enabled")]
    enabled: bool,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
}

fn default_mcp_enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct TomlAgentConfig {
    id: String,
    #[serde(default)]
    default: bool,
    workspace: Option<String>,
    routing: Option<TomlRoutingConfig>,
    max_concurrent_branches: Option<usize>,
    max_concurrent_workers: Option<usize>,
    max_turns: Option<usize>,
    branch_max_turns: Option<usize>,
    context_window: Option<usize>,
    compaction: Option<TomlCompactionConfig>,
    memory_persistence: Option<TomlMemoryPersistenceConfig>,
    coalesce: Option<TomlCoalesceConfig>,
    ingestion: Option<TomlIngestionConfig>,
    cortex: Option<TomlCortexConfig>,
    browser: Option<TomlBrowserConfig>,
    mcp: Option<Vec<TomlMcpServerConfig>>,
    brave_search_key: Option<String>,
    #[serde(default)]
    cron: Vec<TomlCronDef>,
}

#[derive(Deserialize)]
struct TomlCronDef {
    id: String,
    prompt: String,
    interval_secs: Option<u64>,
    delivery_target: String,
    active_start_hour: Option<u8>,
    active_end_hour: Option<u8>,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    run_once: bool,
    timeout_secs: Option<u64>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Deserialize, Default)]
struct TomlMessagingConfig {
    discord: Option<TomlDiscordConfig>,
    slack: Option<TomlSlackConfig>,
    telegram: Option<TomlTelegramConfig>,
    webhook: Option<TomlWebhookConfig>,
    twitch: Option<TomlTwitchConfig>,
}

#[derive(Deserialize)]
struct TomlDiscordConfig {
    #[serde(default)]
    enabled: bool,
    token: Option<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    #[serde(default)]
    allow_bot_messages: bool,
}

#[derive(Deserialize)]
struct TomlSlackConfig {
    #[serde(default)]
    enabled: bool,
    bot_token: Option<String>,
    app_token: Option<String>,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
    #[serde(default)]
    commands: Vec<TomlSlackCommandConfig>,
}

#[derive(Deserialize)]
struct TomlSlackCommandConfig {
    command: String,
    agent_id: String,
    description: Option<String>,
}

#[derive(Deserialize)]
struct TomlTelegramConfig {
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

#[derive(Deserialize)]
struct TomlTwitchConfig {
    #[serde(default)]
    enabled: bool,
    username: Option<String>,
    oauth_token: Option<String>,
    #[serde(default)]
    channels: Vec<String>,
    trigger_prefix: Option<String>,
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
    workspace_id: Option<String>,
    chat_id: Option<String>,
    #[serde(default)]
    channel_ids: Vec<String>,
    #[serde(default)]
    require_mention: bool,
    #[serde(default)]
    dm_allowed_users: Vec<String>,
}

/// Resolve a value that might be an "env:VAR_NAME" reference.
fn resolve_env_value(value: &str) -> Option<String> {
    if let Some(var_name) = value.strip_prefix("env:") {
        std::env::var(var_name).ok()
    } else {
        Some(value.to_string())
    }
}

fn parse_otlp_headers(value: Option<String>) -> Result<HashMap<String, String>> {
    let Some(raw) = value else {
        return Ok(HashMap::new());
    };

    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(HashMap::new());
    }

    let mut headers = HashMap::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((key, value)) = entry.split_once('=') else {
            return Err(ConfigError::Invalid(format!(
                "invalid OTEL_EXPORTER_OTLP_HEADERS entry '{entry}', expected key=value"
            )))?;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            return Err(ConfigError::Invalid(
                "invalid OTEL_EXPORTER_OTLP_HEADERS entry: empty header name".into(),
            ))?;
        }
        headers.insert(key.to_string(), value.to_string());
    }

    Ok(headers)
}

fn parse_mcp_server_config(raw: TomlMcpServerConfig) -> Result<McpServerConfig> {
    if raw.name.trim().is_empty() {
        return Err(ConfigError::Invalid("mcp server name cannot be empty".into()).into());
    }

    let transport = match raw.transport.as_str() {
        "stdio" => {
            let command = raw.command.ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "mcp server '{}' with stdio transport requires 'command'",
                    raw.name
                ))
            })?;
            McpTransport::Stdio {
                command,
                args: raw.args,
                env: raw.env,
            }
        }
        "http" => {
            let url = raw.url.ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "mcp server '{}' with http transport requires 'url'",
                    raw.name
                ))
            })?;
            McpTransport::Http {
                url,
                headers: raw.headers,
            }
        }
        other => {
            return Err(ConfigError::Invalid(format!(
                "mcp server '{}' has invalid transport '{}', expected 'stdio' or 'http'",
                raw.name, other
            ))
            .into());
        }
    };

    Ok(McpServerConfig {
        name: raw.name,
        transport,
        enabled: raw.enabled,
    })
}

/// Resolve a TomlRoutingConfig against a base RoutingConfig.
fn resolve_routing(toml: Option<TomlRoutingConfig>, base: &RoutingConfig) -> RoutingConfig {
    let Some(t) = toml else { return base.clone() };

    let mut task_overrides = base.task_overrides.clone();
    task_overrides.extend(t.task_overrides);

    let fallbacks = match t.fallbacks {
        Some(f) => f,
        None => base.fallbacks.clone(),
    };

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
        channel_thinking_effort: t
            .channel_thinking_effort
            .unwrap_or_else(|| base.channel_thinking_effort.clone()),
        branch_thinking_effort: t
            .branch_thinking_effort
            .unwrap_or_else(|| base.branch_thinking_effort.clone()),
        worker_thinking_effort: t
            .worker_thinking_effort
            .unwrap_or_else(|| base.worker_thinking_effort.clone()),
        compactor_thinking_effort: t
            .compactor_thinking_effort
            .unwrap_or_else(|| base.compactor_thinking_effort.clone()),
        cortex_thinking_effort: t
            .cortex_thinking_effort
            .unwrap_or_else(|| base.cortex_thinking_effort.clone()),
    }
}

fn resolve_mcp_configs(
    default_configs: &[McpServerConfig],
    agent_configs: Option<&[McpServerConfig]>,
) -> Vec<McpServerConfig> {
    let mut merged = default_configs.to_vec();

    if let Some(agent_configs) = agent_configs {
        for agent_config in agent_configs {
            if let Some(existing_index) = merged
                .iter()
                .position(|existing| existing.name == agent_config.name)
            {
                merged[existing_index] = agent_config.clone();
            } else {
                merged.push(agent_config.clone());
            }
        }
    }

    merged
}

impl Config {
    /// Resolve the instance directory from env or default (~/.spacebot).
    pub fn default_instance_dir() -> PathBuf {
        std::env::var("SPACEBOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|d| d.join(".spacebot"))
                    .unwrap_or_else(|| PathBuf::from("./.spacebot"))
            })
    }

    /// Check whether a first-run onboarding is needed (no config file and no env keys/providers).
    pub fn needs_onboarding() -> bool {
        let instance_dir = Self::default_instance_dir();
        let config_path = instance_dir.join("config.toml");
        if config_path.exists() {
            return false;
        }

        // OAuth credentials count as configured
        if crate::auth::credentials_path(&instance_dir).exists() {
            return false;
        }

        // Check if we have any legacy env keys configured
        let has_legacy_keys = std::env::var("ANTHROPIC_API_KEY").is_ok()
            || std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("OPENROUTER_API_KEY").is_ok()
            || std::env::var("ZHIPU_API_KEY").is_ok()
            || std::env::var("GROQ_API_KEY").is_ok()
            || std::env::var("TOGETHER_API_KEY").is_ok()
            || std::env::var("FIREWORKS_API_KEY").is_ok()
            || std::env::var("DEEPSEEK_API_KEY").is_ok()
            || std::env::var("XAI_API_KEY").is_ok()
            || std::env::var("MISTRAL_API_KEY").is_ok()
            || std::env::var("NVIDIA_API_KEY").is_ok()
            || std::env::var("OLLAMA_API_KEY").is_ok()
            || std::env::var("OLLAMA_BASE_URL").is_ok()
            || std::env::var("OPENCODE_ZEN_API_KEY").is_ok()
            || std::env::var("MINIMAX_API_KEY").is_ok()
            || std::env::var("MOONSHOT_API_KEY").is_ok()
            || std::env::var("ZAI_CODING_PLAN_API_KEY").is_ok();

        // If we have any legacy keys, no onboarding needed
        if has_legacy_keys {
            return false;
        }

        // Check if we have any provider-specific env variables (provider.<name>.*)
        let has_provider_env_vars = std::env::vars().any(|(key, _)| {
            key.starts_with("SPACEBOT_PROVIDER_")
                || key.starts_with("PROVIDER_")
                || key.contains("PROVIDER") && key.contains("API_KEY")
        });

        // Also check for specific legacy env vars that can bootstrap
        let has_legacy_bootstrap_vars = std::env::var("ANTHROPIC_API_KEY").is_ok()
            || std::env::var("ANTHROPIC_OAUTH_TOKEN").is_ok()
            || std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("OPENROUTER_API_KEY").is_ok()
            || std::env::var("OPENCODE_ZEN_API_KEY").is_ok();

        !has_provider_env_vars && !has_legacy_bootstrap_vars
    }

    /// Load configuration from the default config file, falling back to env vars.
    pub fn load() -> Result<Self> {
        let instance_dir = Self::default_instance_dir();

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
        let mut llm = LlmConfig {
            anthropic_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            openai_key: std::env::var("OPENAI_API_KEY").ok(),
            openrouter_key: std::env::var("OPENROUTER_API_KEY").ok(),
            zhipu_key: std::env::var("ZHIPU_API_KEY").ok(),
            groq_key: std::env::var("GROQ_API_KEY").ok(),
            together_key: std::env::var("TOGETHER_API_KEY").ok(),
            fireworks_key: std::env::var("FIREWORKS_API_KEY").ok(),
            deepseek_key: std::env::var("DEEPSEEK_API_KEY").ok(),
            xai_key: std::env::var("XAI_API_KEY").ok(),
            mistral_key: std::env::var("MISTRAL_API_KEY").ok(),
            ollama_key: std::env::var("OLLAMA_API_KEY").ok(),
            ollama_base_url: std::env::var("OLLAMA_BASE_URL").ok(),
            opencode_zen_key: std::env::var("OPENCODE_ZEN_API_KEY").ok(),
            nvidia_key: std::env::var("NVIDIA_API_KEY").ok(),
            minimax_key: std::env::var("MINIMAX_API_KEY").ok(),
            moonshot_key: std::env::var("MOONSHOT_API_KEY").ok(),
            zai_coding_plan_key: std::env::var("ZAI_CODING_PLAN_API_KEY").ok(),
            providers: HashMap::new(),
        };

        // Populate providers from env vars (same as from_toml does)
        if let Some(anthropic_key) = llm.anthropic_key.clone() {
            llm.providers
                .entry("anthropic".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: ANTHROPIC_PROVIDER_BASE_URL.to_string(),
                    api_key: anthropic_key,
                    name: None,
                });
        }

        if let Some(openai_key) = llm.openai_key.clone() {
            llm.providers
                .entry("openai".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENAI_PROVIDER_BASE_URL.to_string(),
                    api_key: openai_key,
                    name: None,
                });
        }

        if let Some(openrouter_key) = llm.openrouter_key.clone() {
            llm.providers
                .entry("openrouter".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENROUTER_PROVIDER_BASE_URL.to_string(),
                    api_key: openrouter_key,
                    name: None,
                });
        }

        if let Some(zhipu_key) = llm.zhipu_key.clone() {
            llm.providers
                .entry("zhipu".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: ZHIPU_PROVIDER_BASE_URL.to_string(),
                    api_key: zhipu_key,
                    name: None,
                });
        }

        if let Some(zai_coding_plan_key) = llm.zai_coding_plan_key.clone() {
            llm.providers
                .entry("zai-coding-plan".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: ZAI_CODING_PLAN_BASE_URL.to_string(),
                    api_key: zai_coding_plan_key,
                    name: None,
                });
        }

        if let Some(opencode_zen_key) = llm.opencode_zen_key.clone() {
            llm.providers
                .entry("opencode-zen".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENCODE_ZEN_PROVIDER_BASE_URL.to_string(),
                    api_key: opencode_zen_key,
                    name: None,
                });
        }

        if let Some(minimax_key) = llm.minimax_key.clone() {
            llm.providers
                .entry("minimax".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_key,
                    name: None,
                });
        }

        if let Some(moonshot_key) = llm.moonshot_key.clone() {
            llm.providers
                .entry("moonshot".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: MOONSHOT_PROVIDER_BASE_URL.to_string(),
                    api_key: moonshot_key,
                    name: None,
                });
        }

        if let Some(nvidia_key) = llm.nvidia_key.clone() {
            llm.providers
                .entry("nvidia".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: NVIDIA_PROVIDER_BASE_URL.to_string(),
                    api_key: nvidia_key,
                    name: None,
                });
        }

        // Note: We allow boot without provider keys now. System starts in setup mode.
        // Agents are initialized later when keys are added via API.

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
            max_concurrent_workers: None,
            max_turns: None,
            branch_max_turns: None,
            context_window: None,
            compaction: None,
            memory_persistence: None,
            coalesce: None,
            ingestion: None,
            cortex: None,
            browser: None,
            mcp: None,
            brave_search_key: None,
            cron: Vec::new(),
        }];

        let mut api = ApiConfig::default();
        api.bind = hosted_api_bind(api.bind);

        Ok(Self {
            instance_dir: instance_dir.to_path_buf(),
            llm,
            defaults: DefaultsConfig::default(),
            agents,
            messaging: MessagingConfig::default(),
            bindings: Vec::new(),
            api,
            metrics: MetricsConfig::default(),
            telemetry: TelemetryConfig {
                otlp_endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok(),
                otlp_headers: parse_otlp_headers(std::env::var("OTEL_EXPORTER_OTLP_HEADERS").ok())?,
                service_name: std::env::var("OTEL_SERVICE_NAME")
                    .unwrap_or_else(|_| "spacebot".into()),
                sample_rate: 1.0,
            },
        })
    }

    /// Validate a raw TOML string as a valid Spacebot config.
    /// Returns Ok(()) if the config is structurally valid, or an error describing what's wrong.
    pub fn validate_toml(content: &str) -> Result<()> {
        let toml_config: TomlConfig =
            toml::from_str(content).context("failed to parse config TOML")?;
        // Run full conversion to catch semantic errors (env resolution, defaults, etc.)
        let instance_dir = Self::default_instance_dir();
        Self::from_toml(toml_config, instance_dir)?;
        Ok(())
    }

    fn from_toml(toml: TomlConfig, instance_dir: PathBuf) -> Result<Self> {
        // Validate providers before processing
        for (provider_id, config) in &toml.llm.providers {
            // Validate provider_id
            if provider_id.is_empty() || provider_id.len() > 64 {
                return Err(ConfigError::Invalid(format!(
                    "Provider ID '{}' must be between 1 and 64 characters long",
                    provider_id
                ))
                .into());
            }
            if provider_id.contains('/') || provider_id.contains(char::is_whitespace) {
                return Err(ConfigError::Invalid(format!(
                    "Provider ID '{}' contains invalid characters (cannot contain '/' or whitespace)",
                    provider_id
                ))
                .into());
            }

            // Validate base_url
            if let Err(e) = reqwest::Url::parse(&config.base_url) {
                return Err(ConfigError::Invalid(format!(
                    "Invalid base URL '{}' for provider '{}': {}",
                    config.base_url, provider_id, e
                ))
                .into());
            }
        }

        let mut llm = LlmConfig {
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
            zhipu_key: toml
                .llm
                .zhipu_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("ZHIPU_API_KEY").ok()),
            groq_key: toml
                .llm
                .groq_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("GROQ_API_KEY").ok()),
            together_key: toml
                .llm
                .together_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("TOGETHER_API_KEY").ok()),
            fireworks_key: toml
                .llm
                .fireworks_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("FIREWORKS_API_KEY").ok()),
            deepseek_key: toml
                .llm
                .deepseek_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok()),
            xai_key: toml
                .llm
                .xai_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("XAI_API_KEY").ok()),
            mistral_key: toml
                .llm
                .mistral_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("MISTRAL_API_KEY").ok()),
            ollama_key: toml
                .llm
                .ollama_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("OLLAMA_API_KEY").ok()),
            ollama_base_url: toml
                .llm
                .ollama_base_url
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("OLLAMA_BASE_URL").ok()),
            opencode_zen_key: toml
                .llm
                .opencode_zen_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("OPENCODE_ZEN_API_KEY").ok()),
            nvidia_key: toml
                .llm
                .nvidia_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("NVIDIA_API_KEY").ok()),
            minimax_key: toml
                .llm
                .minimax_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("MINIMAX_API_KEY").ok()),
            moonshot_key: toml
                .llm
                .moonshot_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("MOONSHOT_API_KEY").ok()),
            zai_coding_plan_key: toml
                .llm
                .zai_coding_plan_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("ZAI_CODING_PLAN_API_KEY").ok()),
            providers: toml
                .llm
                .providers
                .into_iter()
                .map(|(provider_id, config)| {
                    (
                        provider_id.to_lowercase(),
                        ProviderConfig {
                            api_type: config.api_type,
                            base_url: config.base_url,
                            api_key: resolve_env_value(&config.api_key)
                                .expect("Failed to resolve API key for provider"),
                            name: config.name,
                        },
                    )
                })
                .collect(),
        };

        if let Some(anthropic_key) = llm.anthropic_key.clone() {
            llm.providers
                .entry("anthropic".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: ANTHROPIC_PROVIDER_BASE_URL.to_string(),
                    api_key: anthropic_key,
                    name: None,
                });
        }

        if let Some(openai_key) = llm.openai_key.clone() {
            llm.providers
                .entry("openai".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENAI_PROVIDER_BASE_URL.to_string(),
                    api_key: openai_key,
                    name: None,
                });
        }

        if let Some(openrouter_key) = llm.openrouter_key.clone() {
            llm.providers
                .entry("openrouter".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENROUTER_PROVIDER_BASE_URL.to_string(),
                    api_key: openrouter_key,
                    name: None,
                });
        }

        if let Some(zhipu_key) = llm.zhipu_key.clone() {
            llm.providers
                .entry("zhipu".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: ZHIPU_PROVIDER_BASE_URL.to_string(),
                    api_key: zhipu_key,
                    name: None,
                });
        }

        if let Some(zai_coding_plan_key) = llm.zai_coding_plan_key.clone() {
            llm.providers
                .entry("zai-coding-plan".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: ZAI_CODING_PLAN_BASE_URL.to_string(),
                    api_key: zai_coding_plan_key,
                    name: None,
                });
        }

        if let Some(opencode_zen_key) = llm.opencode_zen_key.clone() {
            llm.providers
                .entry("opencode-zen".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: OPENCODE_ZEN_PROVIDER_BASE_URL.to_string(),
                    api_key: opencode_zen_key,
                    name: None,
                });
        }

        if let Some(minimax_key) = llm.minimax_key.clone() {
            llm.providers
                .entry("minimax".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::Anthropic,
                    base_url: MINIMAX_PROVIDER_BASE_URL.to_string(),
                    api_key: minimax_key,
                    name: None,
                });
        }

        if let Some(moonshot_key) = llm.moonshot_key.clone() {
            llm.providers
                .entry("moonshot".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: MOONSHOT_PROVIDER_BASE_URL.to_string(),
                    api_key: moonshot_key,
                    name: None,
                });
        }

        if let Some(nvidia_key) = llm.nvidia_key.clone() {
            llm.providers
                .entry("nvidia".to_string())
                .or_insert_with(|| ProviderConfig {
                    api_type: ApiType::OpenAiCompletions,
                    base_url: NVIDIA_PROVIDER_BASE_URL.to_string(),
                    api_key: nvidia_key,
                    name: None,
                });
        }

        // Note: We allow boot without provider keys now. System starts in setup mode.
        // Agents are initialized later when keys are added via API.

        let default_mcp = toml
            .defaults
            .mcp
            .into_iter()
            .map(parse_mcp_server_config)
            .collect::<Result<Vec<_>>>()?;

        let base_defaults = DefaultsConfig::default();
        let defaults = DefaultsConfig {
            routing: resolve_routing(toml.defaults.routing, &base_defaults.routing),
            max_concurrent_branches: toml
                .defaults
                .max_concurrent_branches
                .unwrap_or(base_defaults.max_concurrent_branches),
            max_concurrent_workers: toml
                .defaults
                .max_concurrent_workers
                .unwrap_or(base_defaults.max_concurrent_workers),
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
            coalesce: toml
                .defaults
                .coalesce
                .map(|c| CoalesceConfig {
                    enabled: c.enabled.unwrap_or(base_defaults.coalesce.enabled),
                    debounce_ms: c.debounce_ms.unwrap_or(base_defaults.coalesce.debounce_ms),
                    max_wait_ms: c.max_wait_ms.unwrap_or(base_defaults.coalesce.max_wait_ms),
                    min_messages: c
                        .min_messages
                        .unwrap_or(base_defaults.coalesce.min_messages),
                    multi_user_only: c
                        .multi_user_only
                        .unwrap_or(base_defaults.coalesce.multi_user_only),
                })
                .unwrap_or(base_defaults.coalesce),
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
                    bulletin_interval_secs: c
                        .bulletin_interval_secs
                        .unwrap_or(base_defaults.cortex.bulletin_interval_secs),
                    bulletin_max_words: c
                        .bulletin_max_words
                        .unwrap_or(base_defaults.cortex.bulletin_max_words),
                    bulletin_max_turns: c
                        .bulletin_max_turns
                        .unwrap_or(base_defaults.cortex.bulletin_max_turns),
                    association_interval_secs: c
                        .association_interval_secs
                        .unwrap_or(base_defaults.cortex.association_interval_secs),
                    association_similarity_threshold: c
                        .association_similarity_threshold
                        .unwrap_or(base_defaults.cortex.association_similarity_threshold),
                    association_updates_threshold: c
                        .association_updates_threshold
                        .unwrap_or(base_defaults.cortex.association_updates_threshold),
                    association_max_per_pass: c
                        .association_max_per_pass
                        .unwrap_or(base_defaults.cortex.association_max_per_pass),
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
            mcp: default_mcp,
            brave_search_key: toml
                .defaults
                .brave_search_key
                .as_deref()
                .and_then(resolve_env_value)
                .or_else(|| std::env::var("BRAVE_SEARCH_API_KEY").ok()),
            history_backfill_count: base_defaults.history_backfill_count,
            cron: Vec::new(),
            opencode: toml
                .defaults
                .opencode
                .map(|oc| {
                    let base = &base_defaults.opencode;
                    let path_raw = oc.path.unwrap_or_else(|| base.path.clone());
                    let resolved_path =
                        resolve_env_value(&path_raw).unwrap_or_else(|| base.path.clone());
                    OpenCodeConfig {
                        enabled: oc.enabled.unwrap_or(base.enabled),
                        path: resolved_path,
                        max_servers: oc.max_servers.unwrap_or(base.max_servers),
                        server_startup_timeout_secs: oc
                            .server_startup_timeout_secs
                            .unwrap_or(base.server_startup_timeout_secs),
                        max_restart_retries: oc
                            .max_restart_retries
                            .unwrap_or(base.max_restart_retries),
                        permissions: oc
                            .permissions
                            .map(|p| crate::opencode::OpenCodePermissions {
                                edit: p.edit.unwrap_or_else(|| base.permissions.edit.clone()),
                                bash: p.bash.unwrap_or_else(|| base.permissions.bash.clone()),
                                webfetch: p
                                    .webfetch
                                    .unwrap_or_else(|| base.permissions.webfetch.clone()),
                            })
                            .unwrap_or_else(|| base.permissions.clone()),
                    }
                })
                .unwrap_or_else(|| base_defaults.opencode.clone()),
            worker_log_mode: toml
                .defaults
                .worker_log_mode
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(base_defaults.worker_log_mode),
        };

        let mut agents: Vec<AgentConfig> = toml
            .agents
            .into_iter()
            .map(|a| -> Result<AgentConfig> {
                // Per-agent routing resolves against instance defaults
                let agent_routing = a
                    .routing
                    .map(|r| resolve_routing(Some(r), &defaults.routing));

                let cron = a
                    .cron
                    .into_iter()
                    .map(|h| CronDef {
                        id: h.id,
                        prompt: h.prompt,
                        interval_secs: h.interval_secs.unwrap_or(3600),
                        delivery_target: h.delivery_target,
                        active_hours: match (h.active_start_hour, h.active_end_hour) {
                            (Some(s), Some(e)) => Some((s, e)),
                            _ => None,
                        },
                        enabled: h.enabled,
                        run_once: h.run_once,
                        timeout_secs: h.timeout_secs,
                    })
                    .collect();

                Ok(AgentConfig {
                    id: a.id,
                    default: a.default,
                    workspace: a.workspace.map(PathBuf::from),
                    routing: agent_routing,
                    max_concurrent_branches: a.max_concurrent_branches,
                    max_concurrent_workers: a.max_concurrent_workers,
                    max_turns: a.max_turns,
                    branch_max_turns: a.branch_max_turns,
                    context_window: a.context_window,
                    compaction: a.compaction.map(|c| CompactionConfig {
                        background_threshold: c
                            .background_threshold
                            .unwrap_or(defaults.compaction.background_threshold),
                        aggressive_threshold: c
                            .aggressive_threshold
                            .unwrap_or(defaults.compaction.aggressive_threshold),
                        emergency_threshold: c
                            .emergency_threshold
                            .unwrap_or(defaults.compaction.emergency_threshold),
                    }),
                    memory_persistence: a.memory_persistence.map(|mp| MemoryPersistenceConfig {
                        enabled: mp.enabled.unwrap_or(defaults.memory_persistence.enabled),
                        message_interval: mp
                            .message_interval
                            .unwrap_or(defaults.memory_persistence.message_interval),
                    }),
                    coalesce: a.coalesce.map(|c| CoalesceConfig {
                        enabled: c.enabled.unwrap_or(defaults.coalesce.enabled),
                        debounce_ms: c.debounce_ms.unwrap_or(defaults.coalesce.debounce_ms),
                        max_wait_ms: c.max_wait_ms.unwrap_or(defaults.coalesce.max_wait_ms),
                        min_messages: c.min_messages.unwrap_or(defaults.coalesce.min_messages),
                        multi_user_only: c
                            .multi_user_only
                            .unwrap_or(defaults.coalesce.multi_user_only),
                    }),
                    ingestion: a.ingestion.map(|ig| IngestionConfig {
                        enabled: ig.enabled.unwrap_or(defaults.ingestion.enabled),
                        poll_interval_secs: ig
                            .poll_interval_secs
                            .unwrap_or(defaults.ingestion.poll_interval_secs),
                        chunk_size: ig.chunk_size.unwrap_or(defaults.ingestion.chunk_size),
                    }),
                    cortex: a.cortex.map(|c| CortexConfig {
                        tick_interval_secs: c
                            .tick_interval_secs
                            .unwrap_or(defaults.cortex.tick_interval_secs),
                        worker_timeout_secs: c
                            .worker_timeout_secs
                            .unwrap_or(defaults.cortex.worker_timeout_secs),
                        branch_timeout_secs: c
                            .branch_timeout_secs
                            .unwrap_or(defaults.cortex.branch_timeout_secs),
                        circuit_breaker_threshold: c
                            .circuit_breaker_threshold
                            .unwrap_or(defaults.cortex.circuit_breaker_threshold),
                        bulletin_interval_secs: c
                            .bulletin_interval_secs
                            .unwrap_or(defaults.cortex.bulletin_interval_secs),
                        bulletin_max_words: c
                            .bulletin_max_words
                            .unwrap_or(defaults.cortex.bulletin_max_words),
                        bulletin_max_turns: c
                            .bulletin_max_turns
                            .unwrap_or(defaults.cortex.bulletin_max_turns),
                        association_interval_secs: c
                            .association_interval_secs
                            .unwrap_or(defaults.cortex.association_interval_secs),
                        association_similarity_threshold: c
                            .association_similarity_threshold
                            .unwrap_or(defaults.cortex.association_similarity_threshold),
                        association_updates_threshold: c
                            .association_updates_threshold
                            .unwrap_or(defaults.cortex.association_updates_threshold),
                        association_max_per_pass: c
                            .association_max_per_pass
                            .unwrap_or(defaults.cortex.association_max_per_pass),
                    }),
                    browser: a.browser.map(|b| BrowserConfig {
                        enabled: b.enabled.unwrap_or(defaults.browser.enabled),
                        headless: b.headless.unwrap_or(defaults.browser.headless),
                        evaluate_enabled: b
                            .evaluate_enabled
                            .unwrap_or(defaults.browser.evaluate_enabled),
                        executable_path: b
                            .executable_path
                            .or_else(|| defaults.browser.executable_path.clone()),
                        screenshot_dir: b
                            .screenshot_dir
                            .map(PathBuf::from)
                            .or_else(|| defaults.browser.screenshot_dir.clone()),
                    }),
                    mcp: match a.mcp {
                        Some(mcp_servers) => Some(
                            mcp_servers
                                .into_iter()
                                .map(parse_mcp_server_config)
                                .collect::<Result<Vec<_>>>()?,
                        ),
                        None => None,
                    },
                    brave_search_key: a.brave_search_key.as_deref().and_then(resolve_env_value),
                    cron,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        if agents.is_empty() {
            agents.push(AgentConfig {
                id: "main".into(),
                default: true,
                workspace: None,
                routing: None,
                max_concurrent_branches: None,
                max_concurrent_workers: None,
                max_turns: None,
                branch_max_turns: None,
                context_window: None,
                compaction: None,
                memory_persistence: None,
                coalesce: None,
                ingestion: None,
                cortex: None,
                browser: None,
                mcp: None,
                brave_search_key: None,
                cron: Vec::new(),
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
                    allow_bot_messages: d.allow_bot_messages,
                })
            }),
            slack: toml.messaging.slack.and_then(|s| {
                let bot_token = s
                    .bot_token
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("SLACK_BOT_TOKEN").ok())?;
                let app_token = s
                    .app_token
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("SLACK_APP_TOKEN").ok())?;
                Some(SlackConfig {
                    enabled: s.enabled,
                    bot_token,
                    app_token,
                    dm_allowed_users: s.dm_allowed_users,
                    commands: s
                        .commands
                        .into_iter()
                        .map(|c| SlackCommandConfig {
                            command: c.command,
                            agent_id: c.agent_id,
                            description: c.description,
                        })
                        .collect(),
                })
            }),
            telegram: toml.messaging.telegram.and_then(|t| {
                let token = t
                    .token
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())?;
                Some(TelegramConfig {
                    enabled: t.enabled,
                    token,
                    dm_allowed_users: t.dm_allowed_users,
                })
            }),
            webhook: toml.messaging.webhook.map(|w| WebhookConfig {
                enabled: w.enabled,
                port: w.port,
                bind: w.bind,
            }),
            twitch: toml.messaging.twitch.and_then(|t| {
                let username = t
                    .username
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("TWITCH_BOT_USERNAME").ok())?;
                let oauth_token = t
                    .oauth_token
                    .as_deref()
                    .and_then(resolve_env_value)
                    .or_else(|| std::env::var("TWITCH_OAUTH_TOKEN").ok())?;
                Some(TwitchConfig {
                    enabled: t.enabled,
                    username,
                    oauth_token,
                    channels: t.channels,
                    trigger_prefix: t.trigger_prefix,
                })
            }),
        };

        let bindings = toml
            .bindings
            .into_iter()
            .map(|b| Binding {
                agent_id: b.agent_id,
                channel: b.channel,
                guild_id: b.guild_id,
                workspace_id: b.workspace_id,
                chat_id: b.chat_id,
                channel_ids: b.channel_ids,
                require_mention: b.require_mention,
                dm_allowed_users: b.dm_allowed_users,
            })
            .collect();

        let api = ApiConfig {
            enabled: toml.api.enabled,
            port: toml.api.port,
            bind: hosted_api_bind(toml.api.bind),
        };

        let metrics = MetricsConfig {
            enabled: toml.metrics.enabled,
            port: toml.metrics.port,
            bind: toml.metrics.bind,
        };

        let telemetry = {
            // env var takes precedence over config file value
            let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .ok()
                .or(toml.telemetry.otlp_endpoint);
            let otlp_headers = parse_otlp_headers(
                std::env::var("OTEL_EXPORTER_OTLP_HEADERS")
                    .ok()
                    .or(toml.telemetry.otlp_headers),
            )?;
            let service_name = std::env::var("OTEL_SERVICE_NAME")
                .ok()
                .or(toml.telemetry.service_name)
                .unwrap_or_else(|| "spacebot".into());
            let sample_rate = toml.telemetry.sample_rate.unwrap_or(1.0);
            TelemetryConfig {
                otlp_endpoint,
                otlp_headers,
                service_name,
                sample_rate,
            }
        };

        Ok(Config {
            instance_dir,
            llm,
            defaults,
            agents,
            messaging,
            bindings,
            api,
            metrics,
            telemetry,
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
    /// Instance root directory (e.g., ~/.spacebot). Immutable after startup.
    pub instance_dir: PathBuf,
    /// Agent workspace directory (e.g., ~/.spacebot/agents/{id}/workspace). Immutable after startup.
    pub workspace_dir: PathBuf,
    pub routing: ArcSwap<RoutingConfig>,
    pub compaction: ArcSwap<CompactionConfig>,
    pub memory_persistence: ArcSwap<MemoryPersistenceConfig>,
    pub coalesce: ArcSwap<CoalesceConfig>,
    pub ingestion: ArcSwap<IngestionConfig>,
    pub max_turns: ArcSwap<usize>,
    pub branch_max_turns: ArcSwap<usize>,
    pub context_window: ArcSwap<usize>,
    pub max_concurrent_branches: ArcSwap<usize>,
    pub max_concurrent_workers: ArcSwap<usize>,
    pub browser_config: ArcSwap<BrowserConfig>,
    pub mcp: ArcSwap<Vec<McpServerConfig>>,
    pub history_backfill_count: ArcSwap<usize>,
    pub brave_search_key: ArcSwap<Option<String>>,
    pub cortex: ArcSwap<CortexConfig>,
    /// Cached memory bulletin generated by the cortex. Injected into every
    /// channel's system prompt. Empty string until the first cortex run.
    pub memory_bulletin: ArcSwap<String>,
    pub prompts: ArcSwap<crate::prompts::PromptEngine>,
    pub identity: ArcSwap<crate::identity::Identity>,
    pub skills: ArcSwap<crate::skills::SkillSet>,
    pub opencode: ArcSwap<OpenCodeConfig>,
    /// Shared pool of OpenCode server processes. Lazily initialized on first use.
    pub opencode_server_pool: Arc<crate::opencode::OpenCodeServerPool>,
    /// Cron store, set after agent initialization.
    pub cron_store: ArcSwap<Option<Arc<crate::cron::CronStore>>>,
    /// Cron scheduler, set after agent initialization.
    pub cron_scheduler: ArcSwap<Option<Arc<crate::cron::Scheduler>>>,
    /// Settings store for agent-specific configuration.
    pub settings: ArcSwap<Option<Arc<crate::settings::SettingsStore>>>,
}

impl RuntimeConfig {
    /// Build from a resolved agent config, loaded prompts, identity, and skills.
    pub fn new(
        instance_dir: &Path,
        agent_config: &ResolvedAgentConfig,
        defaults: &DefaultsConfig,
        prompts: crate::prompts::PromptEngine,
        identity: crate::identity::Identity,
        skills: crate::skills::SkillSet,
    ) -> Self {
        let opencode_config = &defaults.opencode;
        let server_pool = crate::opencode::OpenCodeServerPool::new(
            opencode_config.path.clone(),
            opencode_config.permissions.clone(),
            opencode_config.max_servers,
        );

        Self {
            instance_dir: instance_dir.to_path_buf(),
            workspace_dir: agent_config.workspace.clone(),
            routing: ArcSwap::from_pointee(agent_config.routing.clone()),
            compaction: ArcSwap::from_pointee(agent_config.compaction),
            memory_persistence: ArcSwap::from_pointee(agent_config.memory_persistence),
            coalesce: ArcSwap::from_pointee(agent_config.coalesce),
            ingestion: ArcSwap::from_pointee(agent_config.ingestion),
            max_turns: ArcSwap::from_pointee(agent_config.max_turns),
            branch_max_turns: ArcSwap::from_pointee(agent_config.branch_max_turns),
            context_window: ArcSwap::from_pointee(agent_config.context_window),
            max_concurrent_branches: ArcSwap::from_pointee(agent_config.max_concurrent_branches),
            max_concurrent_workers: ArcSwap::from_pointee(agent_config.max_concurrent_workers),
            browser_config: ArcSwap::from_pointee(agent_config.browser.clone()),
            mcp: ArcSwap::from_pointee(agent_config.mcp.clone()),
            history_backfill_count: ArcSwap::from_pointee(agent_config.history_backfill_count),
            brave_search_key: ArcSwap::from_pointee(agent_config.brave_search_key.clone()),
            cortex: ArcSwap::from_pointee(agent_config.cortex),
            memory_bulletin: ArcSwap::from_pointee(String::new()),
            prompts: ArcSwap::from_pointee(prompts),
            identity: ArcSwap::from_pointee(identity),
            skills: ArcSwap::from_pointee(skills),
            opencode: ArcSwap::from_pointee(defaults.opencode.clone()),
            opencode_server_pool: Arc::new(server_pool),
            cron_store: ArcSwap::from_pointee(None),
            cron_scheduler: ArcSwap::from_pointee(None),
            settings: ArcSwap::from_pointee(None),
        }
    }

    /// Set the cron store and scheduler after initialization.
    pub fn set_cron(
        &self,
        store: Arc<crate::cron::CronStore>,
        scheduler: Arc<crate::cron::Scheduler>,
    ) {
        self.cron_store.store(Arc::new(Some(store)));
        self.cron_scheduler.store(Arc::new(Some(scheduler)));
    }

    /// Set the settings store after initialization.
    pub fn set_settings(&self, settings: Arc<crate::settings::SettingsStore>) {
        self.settings.store(Arc::new(Some(settings)));
    }

    /// Reload tunable config values from a freshly parsed Config.
    ///
    /// Finds the matching agent by ID, re-resolves it against defaults, and
    /// swaps all reloadable fields. Does not handle API keys (those are
    /// reloaded via LlmManager), DB paths, messaging adapters, or agent
    /// topology.
    pub async fn reload_config(
        &self,
        config: &Config,
        agent_id: &str,
        mcp_manager: &crate::mcp::McpManager,
    ) {
        let agent = config.agents.iter().find(|a| a.id == agent_id);
        let Some(agent) = agent else {
            tracing::warn!(agent_id, "agent not found in reloaded config, skipping");
            return;
        };

        let resolved = agent.resolve(&config.instance_dir, &config.defaults);
        let old_mcp = (**self.mcp.load()).clone();
        let new_mcp = resolved.mcp.clone();

        self.routing.store(Arc::new(resolved.routing));
        self.compaction.store(Arc::new(resolved.compaction));
        self.memory_persistence
            .store(Arc::new(resolved.memory_persistence));
        self.coalesce.store(Arc::new(resolved.coalesce));
        self.ingestion.store(Arc::new(resolved.ingestion));
        self.max_turns.store(Arc::new(resolved.max_turns));
        self.branch_max_turns
            .store(Arc::new(resolved.branch_max_turns));
        self.context_window.store(Arc::new(resolved.context_window));
        self.max_concurrent_branches
            .store(Arc::new(resolved.max_concurrent_branches));
        self.max_concurrent_workers
            .store(Arc::new(resolved.max_concurrent_workers));
        self.browser_config.store(Arc::new(resolved.browser));
        self.mcp.store(Arc::new(new_mcp.clone()));
        self.history_backfill_count
            .store(Arc::new(resolved.history_backfill_count));
        self.brave_search_key
            .store(Arc::new(resolved.brave_search_key));
        self.cortex.store(Arc::new(resolved.cortex));

        mcp_manager.reconcile(&old_mcp, &new_mcp).await;

        tracing::info!(agent_id, "runtime config reloaded");
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
    agents: Vec<(
        String,
        PathBuf,
        Arc<RuntimeConfig>,
        Arc<crate::mcp::McpManager>,
    )>,
    discord_permissions: Option<Arc<arc_swap::ArcSwap<DiscordPermissions>>>,
    slack_permissions: Option<Arc<arc_swap::ArcSwap<SlackPermissions>>>,
    telegram_permissions: Option<Arc<arc_swap::ArcSwap<TelegramPermissions>>>,
    twitch_permissions: Option<Arc<arc_swap::ArcSwap<TwitchPermissions>>>,
    bindings: Arc<arc_swap::ArcSwap<Vec<Binding>>>,
    messaging_manager: Option<Arc<crate::messaging::MessagingManager>>,
    llm_manager: Arc<crate::llm::LlmManager>,
) -> tokio::task::JoinHandle<()> {
    use notify::{Event, RecursiveMode, Watcher};
    use std::time::Duration;

    tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();

        let mut watcher = match notify::recommended_watcher(
            move |result: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = result {
                    // Only forward data modification events, not metadata/access changes
                    use notify::EventKind;
                    match &event.kind {
                        EventKind::Create(_)
                        | EventKind::Modify(notify::event::ModifyKind::Data(_))
                        | EventKind::Remove(_) => {
                            let _ = tx.send(event);
                        }
                        // Also forward Any/Other modify events (some backends don't distinguish)
                        EventKind::Modify(notify::event::ModifyKind::Any) => {
                            let _ = tx.send(event);
                        }
                        _ => {}
                    }
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

        // Watch instance-level skills directory
        let instance_skills_dir = instance_dir.join("skills");
        if instance_skills_dir.is_dir() {
            if let Err(error) = watcher.watch(&instance_skills_dir, RecursiveMode::Recursive) {
                tracing::warn!(%error, path = %instance_skills_dir.display(), "failed to watch instance skills dir");
            }
        }

        // Watch per-agent workspace directories (skills, identity)
        for (_, workspace, _, _) in &agents {
            for subdir in &["skills"] {
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

        // Track config.toml content hash to skip no-op reloads
        let mut last_config_hash: u64 = std::fs::read(&config_path)
            .map(|bytes| {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut hasher);
                hasher.finish()
            })
            .unwrap_or(0);

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
            let mut config_changed = changed_paths.iter().any(|p| p.ends_with("config.toml"));
            let identity_changed = changed_paths.iter().any(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                matches!(name, "SOUL.md" | "IDENTITY.md" | "USER.md")
            });
            let skills_changed = changed_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("skills"));

            // Skip entirely if nothing relevant changed
            if !config_changed && !identity_changed && !skills_changed {
                continue;
            }

            // Skip config reload if file content hasn't actually changed
            if config_changed {
                let current_hash: u64 = std::fs::read(&config_path)
                    .map(|bytes| {
                        use std::hash::{Hash, Hasher};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        bytes.hash(&mut hasher);
                        hasher.finish()
                    })
                    .unwrap_or(0);
                if current_hash == last_config_hash {
                    config_changed = false;
                    // If config was the only thing that "changed", skip entirely
                    if !identity_changed && !skills_changed {
                        continue;
                    }
                } else {
                    last_config_hash = current_hash;
                }
            }

            let changed_summary: Vec<&str> = [
                config_changed.then_some("config"),
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

            // Reload instance-level bindings, provider keys, and permissions
            if let Some(config) = &new_config {
                llm_manager.reload_config(config.llm.clone());

                bindings.store(Arc::new(config.bindings.clone()));
                tracing::info!("bindings reloaded ({} entries)", config.bindings.len());

                if let Some(ref perms) = discord_permissions {
                    if let Some(discord_config) = &config.messaging.discord {
                        let new_perms =
                            DiscordPermissions::from_config(discord_config, &config.bindings);
                        perms.store(Arc::new(new_perms));
                        tracing::info!("discord permissions reloaded");
                    }
                }

                if let Some(ref perms) = slack_permissions {
                    if let Some(slack_config) = &config.messaging.slack {
                        let new_perms =
                            SlackPermissions::from_config(slack_config, &config.bindings);
                        perms.store(Arc::new(new_perms));
                        tracing::info!("slack permissions reloaded");
                    }
                }

                if let Some(ref perms) = telegram_permissions {
                    if let Some(telegram_config) = &config.messaging.telegram {
                        let new_perms =
                            TelegramPermissions::from_config(telegram_config, &config.bindings);
                        perms.store(Arc::new(new_perms));
                        tracing::info!("telegram permissions reloaded");
                    }
                }

                if let Some(ref perms) = twitch_permissions {
                    if let Some(twitch_config) = &config.messaging.twitch {
                        let new_perms =
                            TwitchPermissions::from_config(twitch_config, &config.bindings);
                        perms.store(Arc::new(new_perms));
                        tracing::info!("twitch permissions reloaded");
                    }
                }

                // Hot-start adapters that are newly enabled in the config
                if let Some(ref manager) = messaging_manager {
                    let rt = tokio::runtime::Handle::current();
                    let manager = manager.clone();
                    let config = config.clone();
                    let discord_permissions = discord_permissions.clone();
                    let slack_permissions = slack_permissions.clone();
                    let telegram_permissions = telegram_permissions.clone();
                    let twitch_permissions = twitch_permissions.clone();

                    rt.spawn(async move {
                        // Discord: start if enabled and not already running
                        if let Some(discord_config) = &config.messaging.discord {
                            if discord_config.enabled && !manager.has_adapter("discord").await {
                                let perms = match discord_permissions {
                                    Some(ref existing) => existing.clone(),
                                    None => {
                                        let perms = DiscordPermissions::from_config(discord_config, &config.bindings);
                                        Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                                    }
                                };
                                let adapter = crate::messaging::discord::DiscordAdapter::new(
                                    &discord_config.token,
                                    perms,
                                );
                                if let Err(error) = manager.register_and_start(adapter).await {
                                    tracing::error!(%error, "failed to hot-start discord adapter from config change");
                                }
                            }
                        }

                        // Slack: start if enabled and not already running
                        if let Some(slack_config) = &config.messaging.slack {
                            if slack_config.enabled && !manager.has_adapter("slack").await {
                                let perms = match slack_permissions {
                                    Some(ref existing) => existing.clone(),
                                    None => {
                                        let perms = SlackPermissions::from_config(slack_config, &config.bindings);
                                        Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                                    }
                                };
                                match crate::messaging::slack::SlackAdapter::new(
                                    &slack_config.bot_token,
                                    &slack_config.app_token,
                                    perms,
                                    slack_config.commands.clone(),
                                ) {
                                    Ok(adapter) => {
                                        if let Err(error) = manager.register_and_start(adapter).await {
                                            tracing::error!(%error, "failed to hot-start slack adapter from config change");
                                        }
                                    }
                                    Err(error) => {
                                        tracing::error!(%error, "failed to build slack adapter from config change");
                                    }
                                }
                            }
                        }

                        // Telegram: start if enabled and not already running
                        if let Some(telegram_config) = &config.messaging.telegram {
                            if telegram_config.enabled && !manager.has_adapter("telegram").await {
                                let perms = match telegram_permissions {
                                    Some(ref existing) => existing.clone(),
                                    None => {
                                        let perms = TelegramPermissions::from_config(telegram_config, &config.bindings);
                                        Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                                    }
                                };
                                let adapter = crate::messaging::telegram::TelegramAdapter::new(
                                    &telegram_config.token,
                                    perms,
                                );
                                if let Err(error) = manager.register_and_start(adapter).await {
                                    tracing::error!(%error, "failed to hot-start telegram adapter from config change");
                                }
                            }
                        }

                        // Twitch: start if enabled and not already running
                        if let Some(twitch_config) = &config.messaging.twitch {
                            if twitch_config.enabled && !manager.has_adapter("twitch").await {
                                let perms = match twitch_permissions {
                                    Some(ref existing) => existing.clone(),
                                    None => {
                                        let perms = TwitchPermissions::from_config(twitch_config, &config.bindings);
                                        Arc::new(arc_swap::ArcSwap::from_pointee(perms))
                                    }
                                };
                                let adapter = crate::messaging::twitch::TwitchAdapter::new(
                                    &twitch_config.username,
                                    &twitch_config.oauth_token,
                                    twitch_config.channels.clone(),
                                    twitch_config.trigger_prefix.clone(),
                                    perms,
                                );
                                if let Err(error) = manager.register_and_start(adapter).await {
                                    tracing::error!(%error, "failed to hot-start twitch adapter from config change");
                                }
                            }
                        }
                    });
                }
            }

            // Apply reloads to each agent's RuntimeConfig
            for (agent_id, workspace, runtime_config, mcp_manager) in &agents {
                if let Some(config) = &new_config {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(runtime_config.reload_config(config, agent_id, mcp_manager));
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

/// Interactive first-run onboarding. Creates ~/.spacebot with a minimal config.
///
/// Returns `Some(path)` if the CLI wizard created a config file, or `None` if
/// the user chose to set up via the embedded UI (setup mode).
pub fn run_onboarding() -> anyhow::Result<Option<PathBuf>> {
    use dialoguer::{Input, Password, Select};
    use std::io::Write;

    println!();
    println!("  Welcome to Spacebot");
    println!("  -------------------");
    println!();
    println!("  No configuration found. Let's set things up.");
    println!();

    let setup_method = Select::new()
        .with_prompt("How do you want to set up?")
        .items(&["Set up here (CLI)", "Set up in the browser (localhost)"])
        .default(0)
        .interact()?;

    if setup_method == 1 {
        println!();
        println!("  Starting in setup mode. Open the UI to finish configuration:");
        println!();
        println!("    http://localhost:19898");
        println!();
        return Ok(None);
    }

    println!();

    // 1. Pick a provider
    let providers = &[
        "Anthropic",
        "OpenRouter",
        "OpenAI",
        "Z.ai (GLM)",
        "Groq",
        "Together AI",
        "Fireworks AI",
        "DeepSeek",
        "xAI (Grok)",
        "Mistral AI",
        "Ollama",
        "OpenCode Zen",
        "MiniMax",
        "Moonshot AI (Kimi)",
        "Z.AI Coding Plan",
    ];
    let provider_idx = Select::new()
        .with_prompt("Which LLM provider do you want to use?")
        .items(providers)
        .default(0)
        .interact()?;

    // For Anthropic, offer OAuth login as an option
    let anthropic_oauth = if provider_idx == 0 {
        let auth_method = Select::new()
            .with_prompt("How do you want to authenticate with Anthropic?")
            .items(&[
                "Log in with Claude Pro/Max (OAuth)",
                "Log in via API Console (OAuth)",
                "Enter an API key manually",
            ])
            .default(0)
            .interact()?;

        if auth_method <= 1 {
            let mode = if auth_method == 0 {
                crate::auth::AuthMode::Max
            } else {
                crate::auth::AuthMode::Console
            };
            let instance_dir = Config::default_instance_dir();
            std::fs::create_dir_all(&instance_dir)?;

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .with_context(|| "failed to build tokio runtime")?;

            runtime.block_on(crate::auth::login_interactive(&instance_dir, mode))?;
            Some(true)
        } else {
            None
        }
    } else {
        None
    };

    let (provider_input_name, toml_key, provider_id) = match provider_idx {
        0 => ("Anthropic API key", "anthropic_key", "anthropic"),
        1 => ("OpenRouter API key", "openrouter_key", "openrouter"),
        2 => ("OpenAI API key", "openai_key", "openai"),
        3 => ("Z.ai (GLM) API key", "zhipu_key", "zhipu"),
        4 => ("Groq API key", "groq_key", "groq"),
        5 => ("Together AI API key", "together_key", "together"),
        6 => ("Fireworks AI API key", "fireworks_key", "fireworks"),
        7 => ("DeepSeek API key", "deepseek_key", "deepseek"),
        8 => ("xAI API key", "xai_key", "xai"),
        9 => ("Mistral API key", "mistral_key", "mistral"),
        10 => ("Ollama base URL (optional)", "ollama_base_url", "ollama"),
        11 => ("OpenCode Zen API key", "opencode_zen_key", "opencode-zen"),
        12 => ("MiniMax API key", "minimax_key", "minimax"),
        13 => ("Moonshot API key", "moonshot_key", "moonshot"),
        14 => (
            "Z.AI Coding Plan API key",
            "zai_coding_plan_key",
            "zai-coding-plan",
        ),
        _ => unreachable!(),
    };
    let is_secret = provider_id != "ollama";

    // 2. Get provider credential/endpoint (skip if OAuth was used)
    let provider_value = if anthropic_oauth.is_some() {
        // OAuth tokens are stored in anthropic_oauth.json, not in config.toml.
        // Use a placeholder so the config still has an [llm] section.
        String::new()
    } else if is_secret {
        let api_key: String = Password::new()
            .with_prompt(format!("Enter your {provider_input_name}"))
            .interact()?;

        let api_key = api_key.trim().to_string();
        if api_key.is_empty() {
            anyhow::bail!("API key cannot be empty");
        }
        api_key
    } else {
        let base_url: String = Input::new()
            .with_prompt(format!("Enter your {provider_input_name}"))
            .default("http://localhost:11434".to_string())
            .interact_text()?;

        let base_url = base_url.trim().to_string();
        if base_url.is_empty() {
            anyhow::bail!("Ollama base URL cannot be empty");
        }
        base_url
    };

    // 3. Agent name
    let agent_id: String = Input::new()
        .with_prompt("Agent name")
        .default("main".to_string())
        .interact_text()?;

    let agent_id = agent_id.trim().to_lowercase().replace(' ', "-");

    // 4. Optional Discord setup
    let setup_discord = Select::new()
        .with_prompt("Set up Discord integration?")
        .items(&["Not now", "Yes"])
        .default(0)
        .interact()?;

    struct DiscordSetup {
        token: String,
        guild_id: Option<String>,
        channel_ids: Vec<String>,
        dm_user_ids: Vec<String>,
    }

    let discord = if setup_discord == 1 {
        let token: String = Password::new()
            .with_prompt("Discord bot token")
            .interact()?;
        let token = token.trim().to_string();

        if token.is_empty() {
            None
        } else {
            println!();
            println!("  Tip: Right-click a server or channel in Discord with");
            println!("  Developer Mode enabled to copy IDs. Leave blank to skip.");
            println!();

            let guild_id: String = Input::new()
                .with_prompt("Server (guild) ID")
                .allow_empty(true)
                .default(String::new())
                .interact_text()?;
            let guild_id = guild_id.trim().to_string();
            let guild_id = if guild_id.is_empty() {
                None
            } else {
                Some(guild_id)
            };

            let channel_ids_raw: String = Input::new()
                .with_prompt("Channel IDs (comma-separated, or blank for all)")
                .allow_empty(true)
                .default(String::new())
                .interact_text()?;
            let channel_ids: Vec<String> = channel_ids_raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            let dm_user_ids_raw: String = Input::new()
                .with_prompt("User IDs allowed to DM the bot (comma-separated, or blank)")
                .allow_empty(true)
                .default(String::new())
                .interact_text()?;
            let dm_user_ids: Vec<String> = dm_user_ids_raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            Some(DiscordSetup {
                token,
                guild_id,
                channel_ids,
                dm_user_ids,
            })
        }
    } else {
        None
    };

    // 5. Build config.toml
    let instance_dir = Config::default_instance_dir();
    let config_path = instance_dir.join("config.toml");

    // Create directory structure
    std::fs::create_dir_all(&instance_dir)
        .with_context(|| format!("failed to create {}", instance_dir.display()))?;

    let mut config_content = String::new();
    config_content.push_str("[llm]\n");
    if anthropic_oauth.is_some() {
        config_content
            .push_str("# Anthropic authentication via OAuth (see anthropic_oauth.json)\n");
    } else {
        config_content.push_str(&format!("{toml_key} = \"{provider_value}\"\n"));
    }
    config_content.push('\n');

    // Write routing defaults for the chosen provider
    let routing = crate::llm::routing::defaults_for_provider(provider_id);
    config_content.push_str("[defaults.routing]\n");
    config_content.push_str(&format!("channel = \"{}\"\n", routing.channel));
    config_content.push_str(&format!("branch = \"{}\"\n", routing.branch));
    config_content.push_str(&format!("worker = \"{}\"\n", routing.worker));
    config_content.push_str(&format!("compactor = \"{}\"\n", routing.compactor));
    config_content.push_str(&format!("cortex = \"{}\"\n", routing.cortex));
    config_content.push('\n');

    config_content.push_str("[[agents]]\n");
    config_content.push_str(&format!("id = \"{agent_id}\"\n"));
    config_content.push_str("default = true\n");

    if let Some(discord) = &discord {
        config_content.push_str("\n[messaging.discord]\n");
        config_content.push_str("enabled = true\n");
        config_content.push_str(&format!("token = \"{}\"\n", discord.token));

        // Write the binding
        config_content.push_str("\n[[bindings]]\n");
        config_content.push_str(&format!("agent_id = \"{agent_id}\"\n"));
        config_content.push_str("channel = \"discord\"\n");
        if let Some(guild_id) = &discord.guild_id {
            config_content.push_str(&format!("guild_id = \"{guild_id}\"\n"));
        }
        if !discord.channel_ids.is_empty() {
            let ids: Vec<String> = discord
                .channel_ids
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect();
            config_content.push_str(&format!("channel_ids = [{}]\n", ids.join(", ")));
        }
        if !discord.dm_user_ids.is_empty() {
            let ids: Vec<String> = discord
                .dm_user_ids
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect();
            config_content.push_str(&format!("dm_allowed_users = [{}]\n", ids.join(", ")));
        }
    }

    let mut file = std::fs::File::create(&config_path)
        .with_context(|| format!("failed to create {}", config_path.display()))?;
    file.write_all(config_content.as_bytes())?;

    println!();
    println!("  Config written to {}", config_path.display());
    println!("  Agent '{}' created.", agent_id);
    println!();
    println!("  You can customize identity files in:");
    println!(
        "    {}/agents/{}/workspace/",
        instance_dir.display(),
        agent_id
    );
    println!();

    Ok(Some(config_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::result::Result as StdResult;
    use std::sync::{Mutex, OnceLock};

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        vars: Vec<(&'static str, Option<String>)>,
        test_dir: PathBuf,
    }

    impl EnvGuard {
        fn new() -> Self {
            const KEYS: [&str; 20] = [
                "SPACEBOT_DIR",
                "SPACEBOT_DEPLOYMENT",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_OAUTH_TOKEN",
                "OPENAI_API_KEY",
                "OPENROUTER_API_KEY",
                "ZHIPU_API_KEY",
                "GROQ_API_KEY",
                "TOGETHER_API_KEY",
                "FIREWORKS_API_KEY",
                "DEEPSEEK_API_KEY",
                "XAI_API_KEY",
                "MISTRAL_API_KEY",
                "NVIDIA_API_KEY",
                "OLLAMA_API_KEY",
                "OLLAMA_BASE_URL",
                "OPENCODE_ZEN_API_KEY",
                "MINIMAX_API_KEY",
                "MOONSHOT_API_KEY",
                "ZAI_CODING_PLAN_API_KEY",
            ];

            let vars = KEYS
                .into_iter()
                .map(|key| (key, std::env::var(key).ok()))
                .collect::<Vec<_>>();

            for key in KEYS {
                unsafe {
                    std::env::remove_var(key);
                }
            }

            let unique = format!(
                "spacebot-config-tests-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time before UNIX_EPOCH")
                    .as_nanos()
            );
            let test_dir = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&test_dir).expect("failed to create test dir");

            unsafe {
                std::env::set_var("SPACEBOT_DIR", &test_dir);
            }

            Self { vars, test_dir }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(v) => unsafe { std::env::set_var(key, v) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
            let _ = std::fs::remove_dir_all(&self.test_dir);
        }
    }

    #[test]
    fn test_api_type_deserialization() {
        let toml1 = r#"
api_type = "openai_completions"
base_url = "https://api.openai.com"
api_key = "test-key"
"#;
        let result1: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml1);
        assert!(result1.is_ok(), "Error: {:?}", result1.err());
        assert_eq!(result1.unwrap().api_type, ApiType::OpenAiCompletions);

        let toml2 = r#"
api_type = "openai_responses"
base_url = "https://api.openai.com"
api_key = "test-key"
"#;
        let result2: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml2);
        assert!(result2.is_ok(), "Error: {:?}", result2.err());
        assert_eq!(result2.unwrap().api_type, ApiType::OpenAiResponses);

        let toml3 = r#"
api_type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "test-key"
"#;
        let result3: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml3);
        assert!(result3.is_ok(), "Error: {:?}", result3.err());
        assert_eq!(result3.unwrap().api_type, ApiType::Anthropic);
    }

    #[test]
    fn test_api_type_deserialization_invalid() {
        let toml = r#"api_type = "invalid_type""#;
        let result: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("invalid value"));
        assert!(error.to_string().contains("openai_completions"));
        assert!(error.to_string().contains("openai_responses"));
        assert!(error.to_string().contains("anthropic"));
    }

    #[test]
    fn test_provider_config_deserialization() {
        let toml = r#"
api_type = "anthropic"
base_url = "https://api.anthropic.com/v1"
api_key = "sk-ant-api03-abc123"
name = "Anthropic"
"#;
        let result: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml);
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_type, ApiType::Anthropic);
        assert_eq!(config.base_url, "https://api.anthropic.com/v1");
        assert_eq!(config.api_key, "sk-ant-api03-abc123");
        assert_eq!(config.name, Some("Anthropic".to_string()));
    }

    #[test]
    fn test_provider_config_deserialization_no_name() {
        let toml = r#"
api_type = "openai_responses"
base_url = "https://api.openai.com/v1"
api_key = "sk-proj-xyz789"
"#;
        let result: StdResult<TomlProviderConfig, toml::de::Error> = toml::from_str(toml);
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_type, ApiType::OpenAiResponses);
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.api_key, "sk-proj-xyz789");
        assert_eq!(config.name, None);
    }

    #[test]
    fn test_llm_provider_tables_parse_with_env_and_lowercase_keys() {
        let toml = r#"
[llm.provider.MyProv]
api_type = "openai_responses"
base_url = "https://api.example.com/v1"
api_key = "env:PATH"

[llm.provider.SecondProvider]
api_type = "anthropic"
base_url = "https://api.anthropic.com/v1"
api_key = "static-provider-key"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert_eq!(config.llm.providers.len(), 2);
        assert!(config.llm.providers.contains_key("myprov"));
        assert!(config.llm.providers.contains_key("secondprovider"));

        let my_provider = config
            .llm
            .providers
            .get("myprov")
            .expect("myprov provider missing");
        assert_eq!(my_provider.api_type, ApiType::OpenAiResponses);
        assert_eq!(my_provider.base_url, "https://api.example.com/v1");
        assert_eq!(
            my_provider.api_key,
            std::env::var("PATH").expect("PATH must exist for test")
        );

        let second_provider = config
            .llm
            .providers
            .get("secondprovider")
            .expect("secondprovider provider missing");
        assert_eq!(second_provider.api_type, ApiType::Anthropic);
        assert_eq!(second_provider.base_url, "https://api.anthropic.com/v1");
        assert_eq!(second_provider.api_key, "static-provider-key");
    }

    #[test]
    fn test_legacy_llm_keys_auto_migrate_to_providers() {
        let toml = r#"
[llm]
anthropic_key = "legacy-anthropic-key"
openai_key = "legacy-openai-key"
openrouter_key = "legacy-openrouter-key"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        let anthropic_provider = config
            .llm
            .providers
            .get("anthropic")
            .expect("anthropic provider missing");
        assert_eq!(anthropic_provider.api_type, ApiType::Anthropic);
        assert_eq!(anthropic_provider.base_url, ANTHROPIC_PROVIDER_BASE_URL);
        assert_eq!(anthropic_provider.api_key, "legacy-anthropic-key");

        let openai_provider = config
            .llm
            .providers
            .get("openai")
            .expect("openai provider missing");
        assert_eq!(openai_provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(openai_provider.base_url, OPENAI_PROVIDER_BASE_URL);
        assert_eq!(openai_provider.api_key, "legacy-openai-key");

        let openrouter_provider = config
            .llm
            .providers
            .get("openrouter")
            .expect("openrouter provider missing");
        assert_eq!(openrouter_provider.api_type, ApiType::OpenAiCompletions);
        assert_eq!(openrouter_provider.base_url, OPENROUTER_PROVIDER_BASE_URL);
        assert_eq!(openrouter_provider.api_key, "legacy-openrouter-key");
    }

    #[test]
    fn test_explicit_provider_config_takes_priority_over_legacy_key_migration() {
        let toml = r#"
[llm]
openai_key = "legacy-openai-key"

[llm.provider.openai]
api_type = "openai_responses"
base_url = "https://custom.openai.example/v1"
api_key = "explicit-openai-key"
name = "Custom OpenAI"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        let openai_provider = config
            .llm
            .providers
            .get("openai")
            .expect("openai provider missing");
        assert_eq!(openai_provider.api_type, ApiType::OpenAiResponses);
        assert_eq!(openai_provider.base_url, "https://custom.openai.example/v1");
        assert_eq!(openai_provider.api_key, "explicit-openai-key");
        assert_eq!(openai_provider.name.as_deref(), Some("Custom OpenAI"));
        assert_eq!(config.llm.openai_key.as_deref(), Some("legacy-openai-key"));
    }

    #[test]
    fn test_needs_onboarding_without_config_or_env() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        assert!(Config::needs_onboarding());
    }

    #[test]
    fn test_needs_onboarding_with_anthropic_env_key() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        }

        assert!(!Config::needs_onboarding());
    }

    #[test]
    fn test_needs_onboarding_false_with_oauth_credentials() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        // Create an OAuth credentials file in the EnvGuard's temp dir
        let instance_dir = Config::default_instance_dir();
        let creds = crate::auth::OAuthCredentials {
            access_token: "sk-ant-oat01-test".to_string(),
            refresh_token: "sk-ant-ort01-test".to_string(),
            expires_at: chrono::Utc::now().timestamp_millis() + 3600_000,
        };
        crate::auth::save_credentials(&instance_dir, &creds).expect("failed to save credentials");

        assert!(!Config::needs_onboarding());
    }

    #[test]
    fn test_load_from_env_populates_legacy_key_and_provider() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        }

        let config = Config::load_from_env(&Config::default_instance_dir())
            .expect("failed to load config from env");

        assert_eq!(config.llm.anthropic_key.as_deref(), Some("test-key"));
        let provider = config
            .llm
            .providers
            .get("anthropic")
            .expect("missing anthropic provider from env");
        assert_eq!(provider.api_key, "test-key");
        assert_eq!(provider.base_url, ANTHROPIC_PROVIDER_BASE_URL);
    }

    #[test]
    fn test_hosted_deployment_forces_api_bind_from_toml() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("SPACEBOT_DEPLOYMENT", "hosted");
        }

        let toml = r#"
[api]
bind = "127.0.0.1"
"#;

        let parsed: TomlConfig = toml::from_str(toml).expect("failed to parse test TOML");
        let config = Config::from_toml(parsed, PathBuf::from(".")).expect("failed to build Config");

        assert_eq!(config.api.bind, "[::]");
    }

    #[test]
    fn test_hosted_deployment_forces_api_bind_from_env_defaults() {
        let _lock = env_test_lock()
            .lock()
            .expect("failed to lock env test mutex");
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("SPACEBOT_DEPLOYMENT", "hosted");
        }

        let config = Config::load_from_env(&Config::default_instance_dir())
            .expect("failed to load config from env");

        assert_eq!(config.api.bind, "[::]");
    }
}
