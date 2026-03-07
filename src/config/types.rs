//! Domain type definitions for Spacebot configuration.

use crate::error::{ConfigError, Result};
use crate::llm::routing::RoutingConfig;
use crate::secrets::store::{InstancePattern, SecretField, SystemSecrets};

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) const CRON_TIMEZONE_ENV_VAR: &str = "SPACEBOT_CRON_TIMEZONE";
pub(super) const USER_TIMEZONE_ENV_VAR: &str = "SPACEBOT_USER_TIMEZONE";

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
    /// Agent communication graph links.
    pub links: Vec<LinkDef>,
    /// Visual grouping of agents in the topology UI.
    pub groups: Vec<GroupDef>,
    /// Org-level humans (real people, shown in topology graph).
    pub humans: Vec<HumanDef>,
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

impl Config {
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

/// A link definition from config, connecting two nodes (agents or humans).
#[derive(Debug, Clone)]
pub struct LinkDef {
    pub from: String,
    pub to: String,
    pub direction: String,
    pub kind: String,
}

/// An org-level human definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HumanDef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
}

/// A visual group definition for the topology UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupDef {
    pub name: String,
    pub agent_ids: Vec<String>,
    #[serde(default)]
    pub color: Option<String>,
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
    pub auth_token: Option<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 19898,
            bind: "127.0.0.1".into(),
            auth_token: None,
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
    /// OpenAI Chat Completions API (`/v1/chat/completions`)
    OpenAiCompletions,
    /// OpenAI-compatible Chat Completions API (`/chat/completions`)
    OpenAiChatCompletions,
    /// Kilo Gateway API (`/chat/completions`) with required gateway headers
    KiloGateway,
    /// OpenAI Responses API (`/v1/responses`)
    OpenAiResponses,
    /// Anthropic Messages API (https://api.anthropic.com/v1/messages)
    Anthropic,
    /// Google Gemini API (https://generativelanguage.googleapis.com/v1beta/openai/chat/completions)
    Gemini,
}

impl<'de> serde::Deserialize<'de> for ApiType {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "openai_completions" => Ok(Self::OpenAiCompletions),
            "openai_chat_completions" => Ok(Self::OpenAiChatCompletions),
            "kilo_gateway" => Ok(Self::KiloGateway),
            "openai_responses" => Ok(Self::OpenAiResponses),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            other => Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(other),
                &"one of \"openai_completions\", \"openai_chat_completions\", \"kilo_gateway\", \"openai_responses\", \"anthropic\", or \"gemini\"",
            )),
        }
    }
}

/// Configuration for a single LLM provider.
#[derive(Clone)]
pub struct ProviderConfig {
    pub api_type: ApiType,
    pub base_url: String,
    pub api_key: String,
    pub name: Option<String>,
    /// When true, use `Authorization: Bearer` instead of `x-api-key` for
    /// Anthropic requests. Set automatically when the key originates from
    /// `ANTHROPIC_AUTH_TOKEN` (proxy-compatible auth).
    pub use_bearer_auth: bool,
    /// Additional HTTP headers included in requests to this provider.
    /// Currently applied in `call_openai()` (the `OpenAiCompletions` path).
    pub extra_headers: Vec<(String, String)>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("api_type", &self.api_type)
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("name", &self.name)
            .field("use_bearer_auth", &self.use_bearer_auth)
            .field(
                "extra_headers",
                &self
                    .extra_headers
                    .iter()
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// LLM provider credentials (instance-level).
#[derive(Clone)]
pub struct LlmConfig {
    pub anthropic_key: Option<String>,
    pub openai_key: Option<String>,
    pub openrouter_key: Option<String>,
    pub kilo_key: Option<String>,
    pub zhipu_key: Option<String>,
    pub groq_key: Option<String>,
    pub together_key: Option<String>,
    pub fireworks_key: Option<String>,
    pub deepseek_key: Option<String>,
    pub xai_key: Option<String>,
    pub mistral_key: Option<String>,
    pub gemini_key: Option<String>,
    pub ollama_key: Option<String>,
    pub ollama_base_url: Option<String>,
    pub opencode_zen_key: Option<String>,
    pub opencode_go_key: Option<String>,
    pub nvidia_key: Option<String>,
    pub minimax_key: Option<String>,
    pub minimax_cn_key: Option<String>,
    pub moonshot_key: Option<String>,
    pub zai_coding_plan_key: Option<String>,
    pub providers: HashMap<String, ProviderConfig>,
}

impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field(
                "anthropic_key",
                &self.anthropic_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "openai_key",
                &self.openai_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "openrouter_key",
                &self.openrouter_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("kilo_key", &self.kilo_key.as_ref().map(|_| "[REDACTED]"))
            .field("zhipu_key", &self.zhipu_key.as_ref().map(|_| "[REDACTED]"))
            .field("groq_key", &self.groq_key.as_ref().map(|_| "[REDACTED]"))
            .field(
                "together_key",
                &self.together_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "fireworks_key",
                &self.fireworks_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "deepseek_key",
                &self.deepseek_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("xai_key", &self.xai_key.as_ref().map(|_| "[REDACTED]"))
            .field(
                "mistral_key",
                &self.mistral_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "gemini_key",
                &self.gemini_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "ollama_key",
                &self.ollama_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("ollama_base_url", &self.ollama_base_url)
            .field(
                "opencode_zen_key",
                &self.opencode_zen_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "opencode_go_key",
                &self.opencode_go_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "nvidia_key",
                &self.nvidia_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "minimax_key",
                &self.minimax_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "moonshot_key",
                &self.moonshot_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "zai_coding_plan_key",
                &self.zai_coding_plan_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("providers", &self.providers)
            .finish()
    }
}

impl LlmConfig {
    /// Check if any provider configuration is set.
    pub fn has_any_key(&self) -> bool {
        self.anthropic_key.is_some()
            || self.openai_key.is_some()
            || self.openrouter_key.is_some()
            || self.kilo_key.is_some()
            || self.zhipu_key.is_some()
            || self.groq_key.is_some()
            || self.together_key.is_some()
            || self.fireworks_key.is_some()
            || self.deepseek_key.is_some()
            || self.xai_key.is_some()
            || self.mistral_key.is_some()
            || self.gemini_key.is_some()
            || self.ollama_key.is_some()
            || self.ollama_base_url.is_some()
            || self.opencode_zen_key.is_some()
            || self.opencode_go_key.is_some()
            || self.nvidia_key.is_some()
            || self.minimax_key.is_some()
            || self.minimax_cn_key.is_some()
            || self.moonshot_key.is_some()
            || self.zai_coding_plan_key.is_some()
            || !self.providers.is_empty()
    }
}

impl SystemSecrets for LlmConfig {
    fn section() -> &'static str {
        "llm"
    }

    fn secret_fields() -> &'static [SecretField] {
        &[
            SecretField {
                toml_key: "anthropic_key",
                secret_name: "ANTHROPIC_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "anthropic_key",
                secret_name: "ANTHROPIC_AUTH_TOKEN",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "openai_key",
                secret_name: "OPENAI_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "openrouter_key",
                secret_name: "OPENROUTER_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "kilo_key",
                secret_name: "KILO_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "zhipu_key",
                secret_name: "ZHIPU_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "groq_key",
                secret_name: "GROQ_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "together_key",
                secret_name: "TOGETHER_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "fireworks_key",
                secret_name: "FIREWORKS_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "deepseek_key",
                secret_name: "DEEPSEEK_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "xai_key",
                secret_name: "XAI_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "mistral_key",
                secret_name: "MISTRAL_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "gemini_key",
                secret_name: "GEMINI_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "gemini_key",
                secret_name: "GOOGLE_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "ollama_key",
                secret_name: "OLLAMA_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "opencode_zen_key",
                secret_name: "OPENCODE_ZEN_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "opencode_go_key",
                secret_name: "OPENCODE_GO_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "nvidia_key",
                secret_name: "NVIDIA_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "minimax_key",
                secret_name: "MINIMAX_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "minimax_cn_key",
                secret_name: "MINIMAX_CN_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "moonshot_key",
                secret_name: "MOONSHOT_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "zai_coding_plan_key",
                secret_name: "ZAI_CODING_PLAN_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "cerebras_key",
                secret_name: "CEREBRAS_API_KEY",
                instance_pattern: None,
            },
            SecretField {
                toml_key: "sambanova_key",
                secret_name: "SAMBANOVA_API_KEY",
                instance_pattern: None,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Defaults, agent configs, and resolution helpers
// ---------------------------------------------------------------------------

/// Defaults inherited by all agents. Individual agents can override any field.
#[derive(Clone)]
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
    pub warmup: WarmupConfig,
    pub browser: BrowserConfig,
    pub channel: ChannelConfig,
    pub mcp: Vec<McpServerConfig>,
    /// Brave Search API key for web search tool. Supports "env:VAR_NAME" references.
    pub brave_search_key: Option<String>,
    /// Default timezone used when evaluating cron active hours.
    pub cron_timezone: Option<String>,
    /// Default timezone for channel/worker temporal context.
    pub user_timezone: Option<String>,
    pub history_backfill_count: usize,
    pub cron: Vec<CronDef>,
    pub opencode: OpenCodeConfig,
    /// Worker log mode: "errors_only", "all_separate", or "all_combined".
    pub worker_log_mode: crate::settings::WorkerLogMode,
    /// Projects workspace management defaults.
    pub projects: ProjectsConfig,
}

impl std::fmt::Debug for DefaultsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultsConfig")
            .field("routing", &self.routing)
            .field("max_concurrent_branches", &self.max_concurrent_branches)
            .field("max_concurrent_workers", &self.max_concurrent_workers)
            .field("max_turns", &self.max_turns)
            .field("branch_max_turns", &self.branch_max_turns)
            .field("context_window", &self.context_window)
            .field("compaction", &self.compaction)
            .field("memory_persistence", &self.memory_persistence)
            .field("coalesce", &self.coalesce)
            .field("ingestion", &self.ingestion)
            .field("cortex", &self.cortex)
            .field("warmup", &self.warmup)
            .field("browser", &self.browser)
            .field("channel", &self.channel)
            .field("mcp", &self.mcp)
            .field(
                "brave_search_key",
                &self.brave_search_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("cron_timezone", &self.cron_timezone)
            .field("user_timezone", &self.user_timezone)
            .field("history_backfill_count", &self.history_backfill_count)
            .field("cron", &self.cron)
            .field("opencode", &self.opencode)
            .field("worker_log_mode", &self.worker_log_mode)
            .field("projects", &self.projects)
            .finish()
    }
}

impl SystemSecrets for DefaultsConfig {
    fn section() -> &'static str {
        "defaults"
    }

    fn secret_fields() -> &'static [SecretField] {
        &[SecretField {
            toml_key: "brave_search_key",
            secret_name: "BRAVE_SEARCH_API_KEY",
            instance_pattern: None,
        }]
    }
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

/// What happens when a worker explicitly calls "close" on the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosePolicy {
    /// Kill the browser process and reset all state (current default behavior).
    #[default]
    CloseBrowser,
    /// Close all tabs but leave the browser process running.
    CloseTabs,
    /// Disconnect from the browser without touching tabs or the process.
    Detach,
}

impl ClosePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CloseBrowser => "close_browser",
            Self::CloseTabs => "close_tabs",
            Self::Detach => "detach",
        }
    }
}

impl std::fmt::Display for ClosePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
    /// Keep the browser alive across worker lifetimes. When true, all workers
    /// for this agent share a single browser connection and tabs survive between
    /// worker runs. Cookies, localStorage, and login sessions persist.
    pub persist_session: bool,
    /// Controls what happens when a worker calls "close" or finishes.
    pub close_policy: ClosePolicy,
    /// Directory for caching a fetcher-downloaded Chromium binary.
    /// Populated from `{instance_dir}/chrome_cache` during config resolution.
    pub chrome_cache_dir: PathBuf,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            headless: true,
            evaluate_enabled: false,
            executable_path: None,
            screenshot_dir: None,
            persist_session: false,
            close_policy: ClosePolicy::default(),
            chrome_cache_dir: PathBuf::from("chrome_cache"),
        }
    }
}

/// Channel behavior configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelConfig {
    /// When true, unsolicited chat messages are ignored unless command/mention/reply.
    pub listen_only_mode: bool,
    /// When true, file attachments received in the channel are saved to
    /// `workspace/saved/` and tracked in the `saved_attachments` table so
    /// they can be recalled on later turns.
    pub save_attachments: bool,
}

/// OpenCode subprocess worker configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub detached_worker_timeout_retry_limit: u8,
    pub supervisor_kill_budget_per_tick: usize,
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
            worker_timeout_secs: 600,
            branch_timeout_secs: 60,
            detached_worker_timeout_retry_limit: 2,
            supervisor_kill_budget_per_tick: 8,
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

/// Warmup configuration.
#[derive(Debug, Clone, Copy)]
pub struct WarmupConfig {
    /// Enable background warmup passes.
    pub enabled: bool,
    /// Force-load the embedding model before first recall/write workloads.
    pub eager_embedding_load: bool,
    /// Interval in seconds between warmup refresh passes.
    pub refresh_secs: u64,
    /// Startup delay before the first warmup pass.
    pub startup_delay_secs: u64,
}

impl Default for WarmupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            eager_embedding_load: true,
            refresh_secs: 900,
            startup_delay_secs: 5,
        }
    }
}

/// Projects configuration — agent-level defaults for project workspace management.
#[derive(Debug, Clone)]
pub struct ProjectsConfig {
    /// Whether to use git worktrees for feature branches.
    /// When true, "start a new feature" creates a worktree for the target repo.
    /// When false, the agent works on branches within the repo directory.
    pub use_worktrees: bool,
    /// Worktree naming convention. Variables: {branch}, {feature}, {repo}.
    pub worktree_name_template: String,
    /// Whether the agent can create new worktrees autonomously
    /// or should ask for confirmation first.
    pub auto_create_worktrees: bool,
    /// Whether to auto-discover repos when a project is created by scanning
    /// the project root for git repositories.
    pub auto_discover_repos: bool,
    /// Whether to auto-discover existing worktrees by running
    /// `git worktree list` on each known repo.
    pub auto_discover_worktrees: bool,
    /// Maximum disk usage warning threshold in bytes.
    /// The UI shows a warning when a project exceeds this.
    pub disk_usage_warning_threshold: u64,
}

impl Default for ProjectsConfig {
    fn default() -> Self {
        Self {
            use_worktrees: true,
            worktree_name_template: "{branch}".to_string(),
            auto_create_worktrees: false,
            auto_discover_repos: true,
            auto_discover_worktrees: true,
            disk_usage_warning_threshold: 53_687_091_200, // 50 GB
        }
    }
}

/// Current warmup lifecycle state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WarmupState {
    Cold,
    Warming,
    Warm,
    Degraded,
}

/// Warmup runtime status snapshot for API and observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupStatus {
    pub state: WarmupState,
    pub embedding_ready: bool,
    pub last_refresh_unix_ms: Option<i64>,
    pub last_error: Option<String>,
    pub bulletin_age_secs: Option<u64>,
}

impl Default for WarmupStatus {
    fn default() -> Self {
        Self {
            state: WarmupState::Cold,
            embedding_ready: false,
            last_refresh_unix_ms: None,
            last_error: None,
            bulletin_age_secs: None,
        }
    }
}

/// Why `ready_for_work` is currently false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkReadinessReason {
    StateNotWarm,
    EmbeddingNotReady,
    BulletinMissing,
    BulletinStale,
}

impl WorkReadinessReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StateNotWarm => "state_not_warm",
            Self::EmbeddingNotReady => "embedding_not_ready",
            Self::BulletinMissing => "bulletin_missing",
            Self::BulletinStale => "bulletin_stale",
        }
    }
}

/// Derived readiness signal used to gate dispatch behavior.
#[derive(Debug, Clone, Copy)]
pub struct WorkReadiness {
    pub ready: bool,
    pub reason: Option<WorkReadinessReason>,
    pub warmup_state: WarmupState,
    pub embedding_ready: bool,
    pub bulletin_age_secs: Option<u64>,
    pub stale_after_secs: u64,
}

pub(super) fn evaluate_work_readiness(
    warmup_config: WarmupConfig,
    status: WarmupStatus,
    now_unix_ms: i64,
) -> WorkReadiness {
    let stale_after_secs = warmup_config.refresh_secs.max(1).saturating_mul(2).max(60);
    let bulletin_age_secs = status
        .last_refresh_unix_ms
        .map(|refresh_ms| {
            if now_unix_ms > refresh_ms {
                ((now_unix_ms - refresh_ms) / 1000) as u64
            } else {
                0
            }
        })
        .or(status.bulletin_age_secs);

    let reason = if status.state != WarmupState::Warm {
        Some(WorkReadinessReason::StateNotWarm)
    } else if warmup_config.eager_embedding_load && !status.embedding_ready {
        Some(WorkReadinessReason::EmbeddingNotReady)
    } else if bulletin_age_secs.is_none() {
        Some(WorkReadinessReason::BulletinMissing)
    } else if bulletin_age_secs.is_some_and(|age| age > stale_after_secs) {
        Some(WorkReadinessReason::BulletinStale)
    } else {
        None
    };

    WorkReadiness {
        ready: reason.is_none(),
        reason,
        warmup_state: status.state,
        embedding_ready: status.embedding_ready,
        bulletin_age_secs,
        stale_after_secs,
    }
}

/// Per-agent configuration (raw, before resolution with defaults).
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub id: String,
    pub default: bool,
    /// User-defined display name for the agent (shown in UI).
    pub display_name: Option<String>,
    /// User-defined role description (e.g. "handles tier 1 support").
    pub role: Option<String>,
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
    pub warmup: Option<WarmupConfig>,
    pub browser: Option<BrowserConfig>,
    pub channel: Option<ChannelConfig>,
    pub mcp: Option<Vec<McpServerConfig>>,
    /// Per-agent Brave Search API key override. None inherits from defaults.
    pub brave_search_key: Option<String>,
    /// Optional timezone override for cron active-hours evaluation.
    pub cron_timezone: Option<String>,
    /// Optional timezone override for channel/worker temporal context.
    pub user_timezone: Option<String>,
    /// Sandbox configuration for process containment.
    pub sandbox: Option<crate::sandbox::SandboxConfig>,
    /// Projects workspace management overrides.
    pub projects: Option<ProjectsConfig>,
    /// Cron job definitions for this agent.
    pub cron: Vec<CronDef>,
}

/// A cron job definition from config.
#[derive(Debug, Clone)]
pub struct CronDef {
    pub id: String,
    pub prompt: String,
    /// Optional cron expression (wall-clock schedule) in standard 5-field format.
    /// When set, this takes precedence over `interval_secs`.
    pub cron_expr: Option<String>,
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
    pub display_name: Option<String>,
    pub role: Option<String>,
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
    pub warmup: WarmupConfig,
    pub browser: BrowserConfig,
    pub channel: ChannelConfig,
    pub mcp: Vec<McpServerConfig>,
    pub brave_search_key: Option<String>,
    pub cron_timezone: Option<String>,
    pub user_timezone: Option<String>,
    /// Sandbox configuration for process containment.
    pub sandbox: crate::sandbox::SandboxConfig,
    /// Projects workspace management settings.
    pub projects: ProjectsConfig,
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
            warmup: WarmupConfig::default(),
            browser: BrowserConfig::default(),
            channel: ChannelConfig::default(),
            mcp: Vec::new(),
            brave_search_key: None,
            cron_timezone: None,
            user_timezone: None,
            history_backfill_count: 50,
            cron: Vec::new(),
            opencode: OpenCodeConfig::default(),
            worker_log_mode: crate::settings::WorkerLogMode::default(),
            projects: ProjectsConfig::default(),
        }
    }
}

impl AgentConfig {
    /// Resolve this agent config against instance defaults and base paths.
    pub fn resolve(&self, instance_dir: &Path, defaults: &DefaultsConfig) -> ResolvedAgentConfig {
        let agent_root = instance_dir.join("agents").join(&self.id);
        let resolved_cron_timezone = resolve_cron_timezone(
            &self.id,
            self.cron_timezone.as_deref(),
            defaults.cron_timezone.as_deref(),
        );
        let resolved_user_timezone = resolve_user_timezone(
            &self.id,
            self.user_timezone.as_deref(),
            defaults.user_timezone.as_deref(),
            resolved_cron_timezone.as_deref(),
        );

        ResolvedAgentConfig {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            role: self.role.clone(),
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
            warmup: self.warmup.unwrap_or(defaults.warmup),
            browser: self
                .browser
                .clone()
                .unwrap_or_else(|| defaults.browser.clone()),
            channel: self.channel.unwrap_or(defaults.channel),
            mcp: resolve_mcp_configs(&defaults.mcp, self.mcp.as_deref()),
            brave_search_key: self
                .brave_search_key
                .clone()
                .or_else(|| defaults.brave_search_key.clone()),
            cron_timezone: resolved_cron_timezone,
            user_timezone: resolved_user_timezone,
            sandbox: self.sandbox.clone().unwrap_or_default(),
            projects: self
                .projects
                .clone()
                .unwrap_or_else(|| defaults.projects.clone()),
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

    /// Path to the saved attachments directory for persisted channel files.
    pub fn saved_dir(&self) -> PathBuf {
        self.workspace.join("saved")
    }
}

// ---------------------------------------------------------------------------
// Timezone and MCP resolution helpers
// ---------------------------------------------------------------------------

fn normalize_timezone(value: &str) -> Option<String> {
    let timezone = value.trim();
    if timezone.is_empty() {
        return None;
    }
    Some(timezone.to_string())
}

fn resolve_cron_timezone(
    agent_id: &str,
    agent_timezone: Option<&str>,
    default_timezone: Option<&str>,
) -> Option<String> {
    let env_timezone = std::env::var(CRON_TIMEZONE_ENV_VAR)
        .ok()
        .and_then(|value| normalize_timezone(&value));

    for timezone in [
        agent_timezone.and_then(normalize_timezone),
        default_timezone.and_then(normalize_timezone),
        env_timezone,
    ] {
        let Some(timezone) = timezone else {
            continue;
        };

        if timezone.parse::<Tz>().is_ok() {
            return Some(timezone);
        }

        tracing::warn!(
            agent_id,
            cron_timezone = %timezone,
            "invalid cron timezone configured, falling back to system local timezone"
        );
    }

    None
}

fn resolve_user_timezone(
    agent_id: &str,
    agent_timezone: Option<&str>,
    default_timezone: Option<&str>,
    fallback_timezone: Option<&str>,
) -> Option<String> {
    let env_timezone = std::env::var(USER_TIMEZONE_ENV_VAR)
        .ok()
        .and_then(|value| normalize_timezone(&value));

    for (source, timezone) in [
        ("agent", agent_timezone.and_then(normalize_timezone)),
        ("defaults", default_timezone.and_then(normalize_timezone)),
        ("env", env_timezone),
        (
            "cron_or_system",
            fallback_timezone.and_then(normalize_timezone),
        ),
    ] {
        let Some(timezone) = timezone else {
            continue;
        };
        if timezone.parse::<Tz>().is_ok() {
            return Some(timezone);
        }
        tracing::warn!(
            agent_id,
            user_timezone = %timezone,
            user_timezone_source = source,
            "invalid user timezone configured, trying next fallback"
        );
    }

    None
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

// ---------------------------------------------------------------------------
// Binding types and adapter validation
// ---------------------------------------------------------------------------

/// Normalize an adapter selector: trim whitespace, return `None` if empty.
pub(super) fn normalize_adapter(adapter: Option<String>) -> Option<String> {
    adapter
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Routes a messaging platform conversation to a specific agent.
#[derive(Debug, Clone)]
pub struct Binding {
    pub agent_id: String,
    pub channel: String,
    /// Optional named adapter selector (platform-scoped).
    ///
    /// `None` targets the default adapter for this platform.
    pub adapter: Option<String>,
    pub guild_id: Option<String>,
    pub workspace_id: Option<String>, // Slack workspace (team) ID
    pub chat_id: Option<String>,      // Telegram group ID
    /// Signal group IDs. If empty, all groups are blocked unless listed.
    pub group_ids: Vec<String>,
    /// Channel IDs this binding applies to. If empty, all channels in the guild/workspace are allowed.
    pub channel_ids: Vec<String>,
    /// Require explicit @mention (or reply-to-bot) for inbound messages.
    pub require_mention: bool,
    /// User IDs allowed to DM the bot through this binding.
    pub dm_allowed_users: Vec<String>,
    /// User IDs allowed to message in Signal groups through this binding.
    pub group_allowed_users: Vec<String>,
}

impl Binding {
    /// Runtime adapter key for this binding.
    pub fn runtime_adapter_key(&self) -> String {
        binding_runtime_adapter_key(self.channel.as_str(), self.adapter.as_deref())
    }

    /// Whether this binding targets the default adapter for its platform.
    pub fn uses_default_adapter(&self) -> bool {
        self.adapter.is_none()
    }

    /// Check if this binding matches an inbound message.
    fn matches(&self, message: &crate::InboundMessage) -> bool {
        if self.channel != message.source {
            return false;
        }

        if !binding_adapter_matches(self, message) {
            return false;
        }

        // For webchat messages, match based on agent_id in the message
        if message.source == "webchat"
            && let Some(message_agent_id) = &message.agent_id
        {
            return message_agent_id.as_ref() == self.agent_id;
        }

        // DM messages have no guild_id — match if the sender is in dm_allowed_users
        let is_dm =
            !message.metadata.contains_key("discord_guild_id") && message.source == "discord";
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

        if self.channel == "discord" && self.require_mention {
            let is_guild_message = message
                .metadata
                .get("discord_guild_id")
                .and_then(|v| v.as_u64())
                .is_some();
            if is_guild_message {
                let mentions_or_replies_to_bot = message
                    .metadata
                    .get("discord_mentions_or_replies_to_bot")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !mentions_or_replies_to_bot {
                    return false;
                }
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

        // Signal-specific matching logic
        if self.channel == "signal" {
            let chat_type = message
                .metadata
                .get("signal_chat_type")
                .and_then(|v| v.as_str());

            match chat_type {
                Some("group") => {
                    // For group chats: check group_id is allowed OR sender is in group_allowed_users
                    let group_allowed = if !self.group_ids.is_empty() {
                        message
                            .metadata
                            .get("signal_group_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|gid| self.group_ids.contains(&gid.to_string()))
                    } else {
                        false
                    };

                    let sender_allowed = if !self.group_allowed_users.is_empty() {
                        self.group_allowed_users.contains(&message.sender_id)
                    } else {
                        false
                    };

                    if !group_allowed && !sender_allowed {
                        return false;
                    }
                }
                Some("dm") | Some("private") => {
                    // For DMs: sender must be in dm_allowed_users (if specified)
                    if !self.dm_allowed_users.is_empty()
                        && !self.dm_allowed_users.contains(&message.sender_id)
                    {
                        return false;
                    }
                }
                _ => {
                    // Unknown chat type, can't match
                    return false;
                }
            }
        }

        true
    }
}

/// Build a runtime adapter key from platform and optional named selector.
pub fn binding_runtime_adapter_key(platform: &str, adapter: Option<&str>) -> String {
    if let Some(name) = adapter
        && !name.is_empty()
    {
        return format!("{platform}:{name}");
    }
    platform.to_string()
}

/// Match a binding's adapter selector against an inbound message adapter.
pub(super) fn binding_adapter_matches(binding: &Binding, message: &crate::InboundMessage) -> bool {
    match (&binding.adapter, message.adapter_selector()) {
        (None, None) => true,
        (Some(expected), Some(actual)) => expected == actual,
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub(super) struct AdapterValidationState {
    default_present: bool,
    named_instances: std::collections::HashSet<String>,
}

pub(super) fn is_named_adapter_platform(platform: &str) -> bool {
    matches!(
        platform,
        "discord" | "slack" | "telegram" | "twitch" | "email" | "signal"
    )
}

pub(super) fn validate_named_messaging_adapters(
    messaging: &MessagingConfig,
    bindings: &[Binding],
) -> Result<()> {
    let adapter_states = build_adapter_validation_states(messaging)?;

    for binding in bindings {
        if !is_named_adapter_platform(binding.channel.as_str()) {
            if binding.adapter.is_some() {
                return Err(ConfigError::Invalid(format!(
                    "binding for channel '{}' can't set adapter: this platform does not support named adapters",
                    binding.channel
                ))
                .into());
            }
            continue;
        }

        let state = adapter_states.get(binding.channel.as_str()).ok_or_else(|| {
            ConfigError::Invalid(format!(
                "binding for channel '{}' can't be resolved: no messaging config exists for that platform",
                binding.channel
            ))
        })?;

        // adapter is already normalized at ingest time via normalize_adapter().
        match binding.adapter.as_deref() {
            Some(adapter_name) => {
                if !state.named_instances.contains(adapter_name) {
                    return Err(ConfigError::Invalid(format!(
                        "binding for channel '{}' references missing adapter '{}'",
                        binding.channel, adapter_name
                    ))
                    .into());
                }
            }
            None => {
                if !state.default_present {
                    return Err(ConfigError::Invalid(format!(
                        "binding for channel '{}' requires the default adapter, but no default credentials are configured",
                        binding.channel
                    ))
                    .into());
                }
            }
        }
    }

    Ok(())
}

pub(super) fn build_adapter_validation_states(
    messaging: &MessagingConfig,
) -> Result<std::collections::HashMap<&'static str, AdapterValidationState>> {
    let mut states = std::collections::HashMap::new();

    if let Some(discord) = &messaging.discord {
        let named_instances = validate_instance_names(
            "discord",
            discord
                .instances
                .iter()
                .map(|instance| instance.name.as_str()),
        )?;
        validate_runtime_keys(
            "discord",
            !discord.token.trim().is_empty(),
            &named_instances,
        )?;
        states.insert(
            "discord",
            AdapterValidationState {
                default_present: !discord.token.trim().is_empty(),
                named_instances,
            },
        );
    }

    if let Some(slack) = &messaging.slack {
        let named_instances = validate_instance_names(
            "slack",
            slack
                .instances
                .iter()
                .map(|instance| instance.name.as_str()),
        )?;
        let default_present =
            !slack.bot_token.trim().is_empty() && !slack.app_token.trim().is_empty();
        validate_runtime_keys("slack", default_present, &named_instances)?;
        states.insert(
            "slack",
            AdapterValidationState {
                default_present,
                named_instances,
            },
        );
    }

    if let Some(telegram) = &messaging.telegram {
        let named_instances = validate_instance_names(
            "telegram",
            telegram
                .instances
                .iter()
                .map(|instance| instance.name.as_str()),
        )?;
        let default_present = !telegram.token.trim().is_empty();
        validate_runtime_keys("telegram", default_present, &named_instances)?;
        states.insert(
            "telegram",
            AdapterValidationState {
                default_present,
                named_instances,
            },
        );
    }

    if let Some(twitch) = &messaging.twitch {
        let named_instances = validate_instance_names(
            "twitch",
            twitch
                .instances
                .iter()
                .map(|instance| instance.name.as_str()),
        )?;
        let default_present =
            !twitch.username.trim().is_empty() && !twitch.oauth_token.trim().is_empty();
        validate_runtime_keys("twitch", default_present, &named_instances)?;
        states.insert(
            "twitch",
            AdapterValidationState {
                default_present,
                named_instances,
            },
        );
    }

    if let Some(email) = &messaging.email {
        let named_instances = validate_instance_names(
            "email",
            email
                .instances
                .iter()
                .map(|instance| instance.name.as_str()),
        )?;
        let default_present = !email.imap_host.trim().is_empty()
            && !email.imap_username.trim().is_empty()
            && !email.imap_password.trim().is_empty()
            && !email.smtp_host.trim().is_empty();
        validate_runtime_keys("email", default_present, &named_instances)?;
        states.insert(
            "email",
            AdapterValidationState {
                default_present,
                named_instances,
            },
        );
    }

    if let Some(signal) = &messaging.signal {
        let named_instances = validate_instance_names(
            "signal",
            signal
                .instances
                .iter()
                .map(|instance| instance.name.as_str()),
        )?;
        let default_present =
            !signal.http_url.trim().is_empty() && !signal.account.trim().is_empty();
        validate_runtime_keys("signal", default_present, &named_instances)?;
        states.insert(
            "signal",
            AdapterValidationState {
                default_present,
                named_instances,
            },
        );
    }

    Ok(states)
}

pub(super) fn validate_instance_names<'a>(
    platform: &str,
    names: impl Iterator<Item = &'a str>,
) -> Result<std::collections::HashSet<String>> {
    let mut seen = std::collections::HashSet::new();

    for name in names {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "messaging.{platform}.instances name can't be empty"
            ))
            .into());
        }
        if trimmed != name {
            return Err(ConfigError::Invalid(format!(
                "messaging.{platform}.instances name '{}' can't contain leading or trailing whitespace",
                name
            ))
            .into());
        }
        if trimmed.eq_ignore_ascii_case("default") {
            return Err(ConfigError::Invalid(format!(
                "messaging.{platform}.instances name '{}' is reserved",
                name
            ))
            .into());
        }
        if trimmed.contains(':') {
            return Err(ConfigError::Invalid(format!(
                "messaging.{platform}.instances name '{}' can't contain ':'",
                name
            ))
            .into());
        }
        if !seen.insert(trimmed.to_string()) {
            return Err(ConfigError::Invalid(format!(
                "messaging.{platform}.instances has duplicate name '{}'",
                name
            ))
            .into());
        }
    }

    Ok(seen)
}

fn validate_runtime_keys(
    platform: &str,
    default_present: bool,
    named_instances: &std::collections::HashSet<String>,
) -> Result<()> {
    let mut runtime_keys = std::collections::HashSet::new();

    if default_present && !runtime_keys.insert(platform.to_string()) {
        return Err(ConfigError::Invalid(format!(
            "messaging.{platform} has duplicate runtime adapter key '{platform}'"
        ))
        .into());
    }

    for instance_name in named_instances {
        let runtime_key = format!("{platform}:{instance_name}");
        if !runtime_keys.insert(runtime_key.clone()) {
            return Err(ConfigError::Invalid(format!(
                "messaging.{platform} has duplicate runtime adapter key '{runtime_key}'"
            ))
            .into());
        }
    }

    Ok(())
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

// ---------------------------------------------------------------------------
// Messaging platform configs
// ---------------------------------------------------------------------------

/// Messaging platform credentials (instance-level).
#[derive(Debug, Clone, Default)]
pub struct MessagingConfig {
    pub discord: Option<DiscordConfig>,
    pub slack: Option<SlackConfig>,
    pub telegram: Option<TelegramConfig>,
    pub email: Option<EmailConfig>,
    pub webhook: Option<WebhookConfig>,
    pub twitch: Option<TwitchConfig>,
    pub signal: Option<SignalConfig>,
}

#[derive(Clone)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub token: String,
    /// Additional named Discord bot instances for this platform.
    pub instances: Vec<DiscordInstanceConfig>,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
    /// Whether to process messages from other bots (self-messages are always ignored).
    pub allow_bot_messages: bool,
}

#[derive(Clone)]
pub struct DiscordInstanceConfig {
    pub name: String,
    pub enabled: bool,
    pub token: String,
    /// User IDs allowed to DM this bot instance.
    pub dm_allowed_users: Vec<String>,
    /// Whether this bot instance processes messages from other bots.
    pub allow_bot_messages: bool,
}

impl std::fmt::Debug for DiscordInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("token", &"[REDACTED]")
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("allow_bot_messages", &self.allow_bot_messages)
            .finish()
    }
}

impl std::fmt::Debug for DiscordConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordConfig")
            .field("enabled", &self.enabled)
            .field("token", &"[REDACTED]")
            .field("instances", &self.instances)
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("allow_bot_messages", &self.allow_bot_messages)
            .finish()
    }
}

impl SystemSecrets for DiscordConfig {
    fn section() -> &'static str {
        "discord"
    }

    fn is_messaging_adapter() -> bool {
        true
    }

    fn secret_fields() -> &'static [SecretField] {
        &[SecretField {
            toml_key: "token",
            secret_name: "DISCORD_BOT_TOKEN",
            instance_pattern: Some(InstancePattern {
                platform_prefix: "DISCORD",
                field_suffix: "BOT_TOKEN",
            }),
        }]
    }
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

#[derive(Clone)]
pub struct SlackConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub app_token: String,
    /// Additional named Slack app instances for this platform.
    pub instances: Vec<SlackInstanceConfig>,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
    /// Slash command definitions. If empty, all slash commands are ignored.
    pub commands: Vec<SlackCommandConfig>,
}

#[derive(Clone)]
pub struct SlackInstanceConfig {
    pub name: String,
    pub enabled: bool,
    pub bot_token: String,
    pub app_token: String,
    /// User IDs allowed to DM this app instance.
    pub dm_allowed_users: Vec<String>,
    /// Slash command definitions for this app instance.
    pub commands: Vec<SlackCommandConfig>,
}

impl std::fmt::Debug for SlackInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("bot_token", &"[REDACTED]")
            .field("app_token", &"[REDACTED]")
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("commands", &self.commands)
            .finish()
    }
}

impl std::fmt::Debug for SlackConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackConfig")
            .field("enabled", &self.enabled)
            .field("bot_token", &"[REDACTED]")
            .field("app_token", &"[REDACTED]")
            .field("instances", &self.instances)
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("commands", &self.commands)
            .finish()
    }
}

impl SystemSecrets for SlackConfig {
    fn section() -> &'static str {
        "slack"
    }

    fn is_messaging_adapter() -> bool {
        true
    }

    fn secret_fields() -> &'static [SecretField] {
        &[
            SecretField {
                toml_key: "bot_token",
                secret_name: "SLACK_BOT_TOKEN",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "SLACK",
                    field_suffix: "BOT_TOKEN",
                }),
            },
            SecretField {
                toml_key: "app_token",
                secret_name: "SLACK_APP_TOKEN",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "SLACK",
                    field_suffix: "APP_TOKEN",
                }),
            },
        ]
    }
}

#[derive(Clone)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token: String,
    /// Additional named Telegram bot instances for this platform.
    pub instances: Vec<TelegramInstanceConfig>,
    /// User IDs allowed to DM the bot. If empty, DMs are ignored entirely.
    pub dm_allowed_users: Vec<String>,
}

#[derive(Clone)]
pub struct TelegramInstanceConfig {
    pub name: String,
    pub enabled: bool,
    pub token: String,
    /// User IDs allowed to DM this bot instance.
    pub dm_allowed_users: Vec<String>,
}

impl std::fmt::Debug for TelegramInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("token", &"[REDACTED]")
            .field("dm_allowed_users", &self.dm_allowed_users)
            .finish()
    }
}

impl std::fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("enabled", &self.enabled)
            .field("token", &"[REDACTED]")
            .field("instances", &self.instances)
            .field("dm_allowed_users", &self.dm_allowed_users)
            .finish()
    }
}

impl SystemSecrets for TelegramConfig {
    fn section() -> &'static str {
        "telegram"
    }

    fn is_messaging_adapter() -> bool {
        true
    }

    fn secret_fields() -> &'static [SecretField] {
        &[SecretField {
            toml_key: "token",
            secret_name: "TELEGRAM_BOT_TOKEN",
            instance_pattern: Some(InstancePattern {
                platform_prefix: "TELEGRAM",
                field_suffix: "BOT_TOKEN",
            }),
        }]
    }
}

#[derive(Clone)]
pub struct EmailConfig {
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_username: String,
    pub imap_password: String,
    pub imap_use_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_use_starttls: bool,
    pub from_address: String,
    pub from_name: Option<String>,
    pub poll_interval_secs: u64,
    pub folders: Vec<String>,
    pub allowed_senders: Vec<String>,
    pub max_body_bytes: usize,
    pub max_attachment_bytes: usize,
    pub instances: Vec<EmailInstanceConfig>,
}

/// Per-instance config for a named email adapter.
#[derive(Clone)]
pub struct EmailInstanceConfig {
    pub name: String,
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_username: String,
    pub imap_password: String,
    pub imap_use_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_use_starttls: bool,
    pub from_address: String,
    pub from_name: Option<String>,
    pub poll_interval_secs: u64,
    pub folders: Vec<String>,
    pub allowed_senders: Vec<String>,
    pub max_body_bytes: usize,
    pub max_attachment_bytes: usize,
}

impl std::fmt::Debug for EmailInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_username", &"[REDACTED]")
            .field("imap_password", &"[REDACTED]")
            .field("imap_use_tls", &self.imap_use_tls)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &"[REDACTED]")
            .field("smtp_password", &"[REDACTED]")
            .field("smtp_use_starttls", &self.smtp_use_starttls)
            .field("from_address", &"[REDACTED]")
            .field("from_name", &self.from_name)
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("folders", &self.folders)
            .field("allowed_senders", &"[REDACTED]")
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_attachment_bytes", &self.max_attachment_bytes)
            .finish()
    }
}

impl std::fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailConfig")
            .field("enabled", &self.enabled)
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_username", &"[REDACTED]")
            .field("imap_password", &"[REDACTED]")
            .field("imap_use_tls", &self.imap_use_tls)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &"[REDACTED]")
            .field("smtp_password", &"[REDACTED]")
            .field("smtp_use_starttls", &self.smtp_use_starttls)
            .field("from_address", &"[REDACTED]")
            .field("from_name", &self.from_name)
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("folders", &self.folders)
            .field("allowed_senders", &"[REDACTED]")
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_attachment_bytes", &self.max_attachment_bytes)
            .finish()
    }
}

impl SystemSecrets for EmailConfig {
    fn section() -> &'static str {
        "email"
    }

    fn is_messaging_adapter() -> bool {
        true
    }

    fn secret_fields() -> &'static [SecretField] {
        &[
            SecretField {
                toml_key: "imap_username",
                secret_name: "EMAIL_IMAP_USERNAME",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "EMAIL",
                    field_suffix: "IMAP_USERNAME",
                }),
            },
            SecretField {
                toml_key: "imap_password",
                secret_name: "EMAIL_IMAP_PASSWORD",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "EMAIL",
                    field_suffix: "IMAP_PASSWORD",
                }),
            },
            SecretField {
                toml_key: "smtp_username",
                secret_name: "EMAIL_SMTP_USERNAME",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "EMAIL",
                    field_suffix: "SMTP_USERNAME",
                }),
            },
            SecretField {
                toml_key: "smtp_password",
                secret_name: "EMAIL_SMTP_PASSWORD",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "EMAIL",
                    field_suffix: "SMTP_PASSWORD",
                }),
            },
        ]
    }
}

#[derive(Clone)]
pub struct TwitchConfig {
    pub enabled: bool,
    pub username: String,
    pub oauth_token: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub refresh_token: Option<String>,
    /// Additional named Twitch bot instances for this platform.
    pub instances: Vec<TwitchInstanceConfig>,
    /// Channels to join (without the # prefix).
    pub channels: Vec<String>,
    /// Optional prefix that triggers the bot (e.g. "!ask"). If empty, all messages are processed.
    pub trigger_prefix: Option<String>,
}

#[derive(Clone)]
pub struct TwitchInstanceConfig {
    pub name: String,
    pub enabled: bool,
    pub username: String,
    pub oauth_token: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub refresh_token: Option<String>,
    /// Channels to join (without the # prefix).
    pub channels: Vec<String>,
    /// Optional prefix that triggers the bot for this instance.
    pub trigger_prefix: Option<String>,
}

impl std::fmt::Debug for TwitchInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwitchInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("username", &self.username)
            .field("oauth_token", &"[REDACTED]")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("channels", &self.channels)
            .field("trigger_prefix", &self.trigger_prefix)
            .finish()
    }
}

impl std::fmt::Debug for TwitchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwitchConfig")
            .field("enabled", &self.enabled)
            .field("username", &self.username)
            .field("oauth_token", &"[REDACTED]")
            .field("instances", &self.instances)
            .field("channels", &self.channels)
            .field("trigger_prefix", &self.trigger_prefix)
            .finish()
    }
}

impl SystemSecrets for TwitchConfig {
    fn section() -> &'static str {
        "twitch"
    }

    fn is_messaging_adapter() -> bool {
        true
    }

    fn secret_fields() -> &'static [SecretField] {
        &[
            SecretField {
                toml_key: "oauth_token",
                secret_name: "TWITCH_OAUTH_TOKEN",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "TWITCH",
                    field_suffix: "OAUTH_TOKEN",
                }),
            },
            SecretField {
                toml_key: "client_id",
                secret_name: "TWITCH_CLIENT_ID",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "TWITCH",
                    field_suffix: "CLIENT_ID",
                }),
            },
            SecretField {
                toml_key: "client_secret",
                secret_name: "TWITCH_CLIENT_SECRET",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "TWITCH",
                    field_suffix: "CLIENT_SECRET",
                }),
            },
            SecretField {
                toml_key: "refresh_token",
                secret_name: "TWITCH_REFRESH_TOKEN",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "TWITCH",
                    field_suffix: "REFRESH_TOKEN",
                }),
            },
        ]
    }
}

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub port: u16,
    pub bind: String,
    pub auth_token: Option<String>,
}

/// Signal messaging via signal-cli JSON-RPC daemon.
///
/// Connects to a running `signal-cli daemon --http` instance for sending and
/// receiving Signal messages. Supports both direct messages and group chats.
#[derive(Clone)]
pub struct SignalConfig {
    pub enabled: bool,
    /// Base URL of the signal-cli JSON-RPC HTTP daemon (e.g. `http://127.0.0.1:8686`).
    /// May contain embedded credentials which are redacted in debug output.
    pub http_url: String,
    /// E.164 phone number of the bot's Signal account (e.g. `+1234567890`).
    pub account: String,
    /// Additional named Signal adapter instances.
    pub instances: Vec<SignalInstanceConfig>,
    /// Phone numbers or UUIDs allowed to DM the bot. If empty, DMs are ignored.
    pub dm_allowed_users: Vec<String>,
    /// Group IDs allowed for this adapter. If empty, all groups are blocked
    /// (same as `None` in the permission filter — groups are opt-in only).
    pub group_ids: Vec<String>,
    /// User IDs allowed to message in Signal groups.
    pub group_allowed_users: Vec<String>,
    /// Whether to silently drop story messages (default: true).
    pub ignore_stories: bool,
}

/// Per-instance config for a named Signal adapter.
#[derive(Clone)]
pub struct SignalInstanceConfig {
    pub name: String,
    pub enabled: bool,
    /// Base URL of this instance's signal-cli daemon.
    pub http_url: String,
    /// E.164 phone number for this instance's Signal account.
    pub account: String,
    /// Phone numbers or UUIDs allowed to DM this instance.
    pub dm_allowed_users: Vec<String>,
    /// Group IDs allowed for this instance.
    pub group_ids: Vec<String>,
    /// User IDs allowed to message in Signal groups for this instance.
    pub group_allowed_users: Vec<String>,
    /// Whether this instance drops story messages.
    pub ignore_stories: bool,
}

impl std::fmt::Debug for SignalInstanceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalInstanceConfig")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("http_url", &"[REDACTED]")
            .field("account", &"[REDACTED]")
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("group_ids", &self.group_ids)
            .field("group_allowed_users", &self.group_allowed_users)
            .field("ignore_stories", &self.ignore_stories)
            .finish()
    }
}

impl std::fmt::Debug for SignalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalConfig")
            .field("enabled", &self.enabled)
            .field("http_url", &"[REDACTED]")
            .field("account", &"[REDACTED]")
            .field("instances", &self.instances)
            .field("dm_allowed_users", &self.dm_allowed_users)
            .field("group_ids", &self.group_ids)
            .field("group_allowed_users", &self.group_allowed_users)
            .field("ignore_stories", &self.ignore_stories)
            .finish()
    }
}

impl SystemSecrets for SignalConfig {
    fn section() -> &'static str {
        "signal"
    }

    fn is_messaging_adapter() -> bool {
        true
    }

    fn secret_fields() -> &'static [SecretField] {
        &[
            SecretField {
                toml_key: "http_url",
                secret_name: "SIGNAL_HTTP_URL",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "SIGNAL",
                    field_suffix: "HTTP_URL",
                }),
            },
            SecretField {
                toml_key: "account",
                secret_name: "SIGNAL_ACCOUNT",
                instance_pattern: Some(InstancePattern {
                    platform_prefix: "SIGNAL",
                    field_suffix: "ACCOUNT",
                }),
            },
        ]
    }
}
